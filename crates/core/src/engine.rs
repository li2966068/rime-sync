//! transport / apply 两个入口：只搬文件 vs 真正合并部署。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::backup::BackupManager;
use crate::cancel::CancelToken;
use crate::config::Config;
use crate::history::{self, HistoryEntry};
use crate::index::{sha256_hex, FileIndex, FileMeta};
use crate::lock::{self, LockGuard};
use crate::merge3::{
    custom_yaml_digest, is_custom_yaml, merge3_digests, merge_yaml_patch, MergeOutcome, MergeResult,
};
use crate::plan;
use crate::platform::RimeTrigger;
use crate::retry::RetryState;
use crate::rime_dir;
use crate::webdav::{DavClient, PutOutcome};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub pulled: usize,
    pub pushed: usize,
    pub merged: usize,
    pub conflicts: Vec<String>,
    pub skipped: Vec<String>,
    pub duration_ms: u64,
    pub ok: bool,
    pub error: Option<String>,
    /// apply 前生成的备份 id（apply 失败时也可能已有备份）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
    /// 失败退避：下一次允许尝试的时间（Unix 秒），成功后为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<u64>,
    /// 首次接入保护（SPEC-4 §A1）：本次是首次接入。
    #[serde(default)]
    pub first_sync: bool,
    /// 本次运行模式：normal / pull-only。首次接入按策略可能只拉不推。
    #[serde(default)]
    pub mode: String,
    /// *.userdb.txt 词条级冲突统计；统计失败时整体降级为 null（不影响主流程）。
    #[serde(default)]
    pub conflict_details: Option<Vec<ConflictDetail>>,
    /// 本轮有 *.custom.yaml 被拉取 / 合并 / 写入本地：Windows 上需要重新部署
    /// （WeaselDeployer /deploy），否则改动不会生效。
    #[serde(default)]
    pub config_changed: bool,
    /// 本轮拉到新的/变化的 *.userdb.txt：transport 只搬文件，必须再跑 apply
    /// （rime_dict_manager --sync）才算真正合并进本地词库。
    #[serde(default)]
    pub pending_merge: bool,
    /// apply 合并后本地词库的条目数（merge 后 userdb 导出）；导出失败或未 apply 时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_entries: Option<usize>,
}

/// 预演里的单个动作类型。语义与真实 transport 的写操作一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    /// 将写入本地（远端 -> 本地）。
    Pull,
    /// 将写入远端（本地 -> 远端）。
    Push,
    /// 两侧都改过，需要用户处理（预览不写冲突副本）。
    Conflict,
    /// 因策略/分类/路径规则跳过。
    Skip,
    /// 将删除本地文件（远端已删，本地未改）。
    DeleteLocal,
    /// 将删除远端文件（本地已删，远端未改）。
    DeleteRemote,
}

impl PlanKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanKind::Pull => "pull",
            PlanKind::Push => "push",
            PlanKind::Conflict => "conflict",
            PlanKind::Skip => "skip",
            PlanKind::DeleteLocal => "delete_local",
            PlanKind::DeleteRemote => "delete_remote",
        }
    }
}

/// 「这次会做什么」的一条记录。path 是动作真正作用的那一侧路径：
/// 快照是 sync/<installation_id>/<schema>.userdb.txt，shared 文件是 shared/files/<rel>。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAction {
    pub kind: PlanKind,
    pub path: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanCounts {
    pub pull: usize,
    pub push: usize,
    pub conflict: usize,
    pub skip: usize,
    pub delete_local: usize,
    pub delete_remote: usize,
}

/// 预演结果：只有读操作发生过，actions 就是接下来 transport 会做的写动作。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanReport {
    pub actions: Vec<PlanAction>,
    pub counts: PlanCounts,
    /// actions.len()，避免前端再算一次。
    pub total: usize,
    /// 本次判定采用的策略模式（normal / preview / pull-only）。
    pub mode: String,
    pub first_sync: bool,
    /// 恒为 true：预演不会真的改任何东西。
    pub dry_run: bool,
    pub ok: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// 单份 userdb 快照的词条差异统计（只统计，不自动合并）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictDetail {
    pub path: String,
    pub only_local: usize,
    pub only_remote: usize,
    pub both_changed: usize,
    pub sample: Vec<String>,
}

/// apply 前后导出词库条目数用的 schema 名。
const USERDB_EXPORT_SCHEMA: &str = "luna_pinyin";

/// 临时导出文件：离开作用域即删除（清理失败的告警不能反过来影响 apply）。
struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("remove temp file {}: {error}", self.0.display());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

#[derive(Clone)]
pub struct ProgressSink(Arc<dyn Fn(&Progress) + Send + Sync>);

impl ProgressSink {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(&Progress) + Send + Sync + 'static,
    {
        Self(Arc::new(callback))
    }

    pub fn emit(&self, phase: &str, current: usize, total: usize, message: impl Into<String>) {
        (self.0)(&Progress {
            phase: phase.to_string(),
            current,
            total,
            message: message.into(),
        });
    }
}

impl std::fmt::Debug for ProgressSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProgressSink(..)")
    }
}

pub struct Engine {
    config: Config,
    rime_dir: PathBuf,
    sync_dir: PathBuf,
    state_dir: PathBuf,
    index_path: PathBuf,
    lock_path: PathBuf,
    backup_root: PathBuf,
    base_dir: PathBuf,
    dav: DavClient,
    index: FileIndex,
    /// Engine::new 时本地是否已有 index.json（首次接入判定的第一半）。
    index_existed: bool,
    history_path: PathBuf,
    retry: RetryState,
    retry_path: PathBuf,
    trigger: Box<dyn RimeTrigger>,
    progress: Option<ProgressSink>,
    cancel: Option<CancelToken>,
    /// 预演模式：写操作只记录进 actions，不发 PUT/DELETE、不落盘、不动 index。
    dry_run: bool,
    /// 本次预演记录的动作，顺序与真实执行顺序一致。
    actions: Vec<PlanAction>,
}

impl Engine {
    pub fn new(
        config: Config,
        rime_dir: PathBuf,
        state_dir: PathBuf,
        trigger: Box<dyn RimeTrigger>,
    ) -> Result<Self> {
        config.validate()?;
        if !rime_dir.is_dir() {
            bail!("rime dir is not a directory: {}", rime_dir.display());
        }
        let dav = DavClient::new(
            &config.webdav.url,
            &config.webdav.username,
            &config.webdav.password,
            &config.webdav.remote_dir,
        )?;
        let index_path = state_dir.join("index.json");
        let index_existed = index_path.is_file();
        let index = FileIndex::load(&index_path)?;
        let retry_path = state_dir.join("retry.json");
        let retry = RetryState::load(&retry_path).unwrap_or_default();
        let sync_dir = rime_dir::sync_dir(&rime_dir);
        Ok(Self {
            config,
            rime_dir,
            sync_dir,
            state_dir: state_dir.clone(),
            index_path,
            lock_path: state_dir.join("sync.lock"),
            backup_root: state_dir.join("backups"),
            base_dir: state_dir.join("base"),
            dav,
            index,
            index_existed,
            history_path: state_dir.join("history.jsonl"),
            retry,
            retry_path,
            trigger,
            progress: None,
            cancel: None,
            dry_run: false,
            actions: Vec::new(),
        })
    }

    /// 覆盖备份根目录（默认 <state_dir>/backups）。
    pub fn with_backup_root(mut self, root: PathBuf) -> Self {
        self.backup_root = root;
        self
    }

    pub fn with_progress(mut self, sink: ProgressSink) -> Self {
        self.progress = Some(sink);
        self
    }

    pub fn with_cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    fn cancelled(&self) -> Result<()> {
        if let Some(token) = &self.cancel {
            token.check()?;
        }
        Ok(())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn rime_dir(&self) -> &Path {
        &self.rime_dir
    }

    /// 只搬文件：pull → merge → push（持锁执行）。
    ///
    /// 首次接入（本地无 index 且远端已有数据）按 first_sync_policy 只拉不推。
    pub fn transport(&mut self) -> Report {
        let report = match self.lock() {
            Ok(_guard) => self.transport_inner(),
            Err(error) => self.locked_report(error),
        };
        self.record_history("transport", &report);
        report
    }

    fn lock(&self) -> Result<LockGuard> {
        lock::acquire(&self.lock_path)
    }

    fn locked_report(&self, error: anyhow::Error) -> Report {
        self.emit("done", 0, 0, "aborted: another sync holds the lock");
        Report {
            ok: false,
            error: Some(format!("{error:#}")),
            ..Report::default()
        }
    }

    fn transport_inner(&mut self) -> Report {
        let started = Instant::now();
        let mut report = Report {
            ok: true,
            mode: "normal".to_string(),
            conflict_details: Some(Vec::new()),
            ..Report::default()
        };
        self.emit("start", 0, 0, "transport");
        if let Err(error) = self.do_transport(&mut report) {
            report.ok = false;
            report.error = Some(format!("{error:#}"));
            tracing::error!("transport failed: {error:#}");
        }
        match self.index.save(&self.index_path) {
            Ok(()) => self.index_existed = true,
            Err(error) => {
                report.ok = false;
                if report.error.is_none() {
                    report.error = Some(format!("save index: {error:#}"));
                }
            }
        }
        // Windows：配置类文件（*.custom.yaml）落到本地后必须让 Weasel 重新部署，
        // 否则用户要手动「重新部署」才生效。Linux/macOS 不触发（会打断输入法）。
        if report.ok && report.config_changed && cfg!(windows) {
            self.emit("deploy", 0, 0, "deploying weasel configuration");
            match self.trigger.deploy(&self.rime_dir) {
                Ok(()) => tracing::info!("weasel deploy triggered after custom yaml update"),
                Err(error) => tracing::warn!(
                    "weasel deploy after config sync failed (config files are still on disk): {error:#}"
                ),
            }
        }
        dedupe(&mut report.conflicts);
        dedupe(&mut report.skipped);
        self.record_retry(&mut report);
        report.duration_ms = started.elapsed().as_millis() as u64;
        self.emit(
            "done",
            report.pulled + report.pushed,
            report.pulled + report.pushed,
            format!(
                "pulled={} pushed={} conflicts={}",
                report.pulled,
                report.pushed,
                report.conflicts.len()
            ),
        );
        report
    }

    /// 预演（dry-run）：完整跑一遍 transport 的判定链路，但所有写动作只记录不执行。
    ///
    /// 只发 GET / HEAD / PROPFIND；不 PUT、不 DELETE、不 MKCOL、不写本地文件，
    /// 也不写 index.json / base 缓存 / history / retry，不触发输入法部署。
    pub fn plan(&mut self) -> PlanReport {
        let started = Instant::now();
        self.actions.clear();
        // 预演会就地推进内存基线（否则同一轮里会被反复判定），结束后整体回滚，
        // 保证 Engine 之后仍按「什么都没发生过」的状态执行真实同步。
        let saved_index = self.index.clone();
        self.dry_run = true;
        let mut report = Report {
            ok: true,
            mode: "normal".to_string(),
            conflict_details: Some(Vec::new()),
            ..Report::default()
        };
        self.emit("plan", 0, 0, "dry run");
        let outcome = self.do_transport(&mut report);
        self.dry_run = false;
        self.index = saved_index;
        if let Err(error) = outcome {
            report.ok = false;
            report.error = Some(format!("{error:#}"));
            tracing::warn!("plan failed: {error:#}");
        }
        dedupe(&mut report.skipped);
        let mut actions = std::mem::take(&mut self.actions);
        // 跳过原因与真实同步写进 report.skipped 的完全同一句，统一投影成 Skip。
        for entry in &report.skipped {
            let (path, detail) = entry.split_once(": ").unwrap_or((entry.as_str(), ""));
            actions.push(PlanAction {
                kind: PlanKind::Skip,
                path: path.to_string(),
                detail: detail.to_string(),
            });
        }
        let mut counts = PlanCounts::default();
        for action in &actions {
            match action.kind {
                PlanKind::Pull => counts.pull += 1,
                PlanKind::Push => counts.push += 1,
                PlanKind::Conflict => counts.conflict += 1,
                PlanKind::Skip => counts.skip += 1,
                PlanKind::DeleteLocal => counts.delete_local += 1,
                PlanKind::DeleteRemote => counts.delete_remote += 1,
            }
        }
        self.emit(
            "done",
            0,
            actions.len(),
            format!(
                "plan: pull={} push={} conflict={} skip={}",
                counts.pull, counts.push, counts.conflict, counts.skip
            ),
        );
        PlanReport {
            total: actions.len(),
            actions,
            counts,
            mode: report.mode,
            first_sync: report.first_sync,
            dry_run: true,
            ok: report.ok,
            error: report.error,
            duration_ms: started.elapsed().as_millis() as u64,
        }
    }

    fn record(&mut self, kind: PlanKind, path: impl Into<String>, detail: impl Into<String>) {
        self.actions.push(PlanAction {
            kind,
            path: path.into(),
            detail: detail.into(),
        });
    }

    /// PUT 闸门：预演时只记一条 Push 并假装成功，让后续判定沿真实路径继续。
    fn put(
        &mut self,
        path: &str,
        bytes: &[u8],
        if_match: Option<&str>,
        detail: &str,
    ) -> Result<PutOutcome> {
        if self.dry_run {
            self.record(PlanKind::Push, path, detail);
            return Ok(PutOutcome::Written);
        }
        self.dav.put(path, bytes, if_match)
    }

    /// DELETE 闸门（远端）。
    fn delete_remote(&mut self, path: &str, detail: &str) -> Result<()> {
        if self.dry_run {
            self.record(PlanKind::DeleteRemote, path, detail);
            return Ok(());
        }
        self.dav.delete(path)
    }

    /// 本地写入闸门：远端 -> 本地，以及合并结果落盘。
    fn write_pulled(&mut self, rel: &str, bytes: &[u8], detail: &str) -> Result<()> {
        if self.dry_run {
            self.record(PlanKind::Pull, rel, detail);
            return Ok(());
        }
        write_file(&self.rime_dir, rel, bytes)
    }

    /// 合并 + 部署 + 再 push（librime 会产出新快照）。
    ///
    /// 顺序：拿锁 → 备份（失败即中止）→ transport → 停输入法 → sync → deploy → 再 transport。
    pub fn apply(&mut self) -> Report {
        let report = self.apply_inner();
        self.record_history("apply", &report);
        report
    }

    fn apply_inner(&mut self) -> Report {
        let started = Instant::now();
        let _guard = match self.lock() {
            Ok(guard) => guard,
            Err(error) => return self.locked_report(error),
        };
        let backups = BackupManager::new(&self.rime_dir, &self.backup_root);
        let backup_id = match backups.create("pre-apply") {
            Ok(id) => id,
            Err(error) => {
                // 备份失败 → 中止 apply，一个字都不写（宁可不合并）。
                self.emit("done", 0, 0, "apply aborted: backup failed");
                tracing::error!("apply aborted, backup failed: {error:#}");
                return Report {
                    ok: false,
                    error: Some(format!(
                        "backup failed, apply aborted before any write: {error:#}"
                    )),
                    duration_ms: started.elapsed().as_millis() as u64,
                    ..Report::default()
                };
            }
        };
        if let Err(error) = backups.prune(self.config.backup_keep as usize) {
            tracing::warn!("prune backups: {error:#}");
        }
        let mut report = self.transport_inner();
        report.backup_id = Some(backup_id);
        if !report.ok {
            report.duration_ms = started.elapsed().as_millis() as u64;
            return report;
        }
        self.emit("deploy", 0, 4, "stopping input method");
        tracing::info!(
            "apply: stopping input method before rime_dict_manager (trigger={})",
            self.trigger.name()
        );
        let mut deploy_error: Option<anyhow::Error> = None;
        if let Err(error) = self.trigger.stop_ime() {
            deploy_error = Some(error);
        }
        let mut before_entries: Option<usize> = None;
        if deploy_error.is_none() {
            // 合并前基线：顺带证明 LevelDB 已可读（fcitx5 确实退出）。
            before_entries = self.export_userdb_count("pre-merge");
            self.emit("deploy", 1, 4, "rime_dict_manager --sync");
            match self.trigger.sync_userdb(&self.rime_dir) {
                Ok(()) => {
                    // 快照已导入 LevelDB：pending_merge 的债还清了。
                    // 之后的 post-deploy transport 若又拉到新快照会重新置位。
                    report.pending_merge = false;
                }
                Err(error) => deploy_error = Some(error),
            }
        }
        let after_entries = if deploy_error.is_none() {
            self.export_userdb_count("post-merge")
        } else {
            None
        };
        report.merged_entries = after_entries;
        match (before_entries, after_entries) {
            (Some(before), Some(after)) => tracing::info!(
                "apply: userdb entries {before} -> {after} (delta {:+})",
                after as i64 - before as i64
            ),
            _ => tracing::info!(
                "apply: userdb entry counts unavailable (before={before_entries:?} after={after_entries:?})"
            ),
        }
        if deploy_error.is_none() {
            self.emit("deploy", 2, 4, "rime_deployer --build");
            if let Err(error) = self.trigger.deploy(&self.rime_dir) {
                deploy_error = Some(error);
            }
        }
        tracing::info!("apply: restarting input method");
        let start_result = self.trigger.start_ime();
        if let Some(error) = deploy_error {
            report.ok = false;
            report.error = Some(format!("{error:#}"));
            tracing::error!("apply deploy failed: {error:#}");
        }
        if let Err(error) = start_result {
            report.ok = false;
            if report.error.is_none() {
                report.error = Some(format!("restart input method: {error:#}"));
            }
        }
        if report.ok {
            // 部署会生成 user.yaml / 新快照，再走一遍 pull→merge→push 才能上传。
            if let Err(error) = self.do_transport(&mut report) {
                report.ok = false;
                report.error = Some(format!("post-deploy transport: {error:#}"));
            }
            if let Err(error) = self.index.save(&self.index_path) {
                report.ok = false;
                report.error = Some(format!("save index: {error:#}"));
            }
        }
        dedupe(&mut report.conflicts);
        dedupe(&mut report.skipped);
        self.record_retry(&mut report);
        report.duration_ms = started.elapsed().as_millis() as u64;
        self.emit(
            "done",
            report.pulled + report.pushed,
            report.pulled + report.pushed,
            format!("apply ok={} duration={}ms", report.ok, report.duration_ms),
        );
        report
    }

    /// 连续失败退避：1→2→4→8→16→30 分钟，成功即复位（SPEC-3 §B8）。
    fn record_retry(&mut self, report: &mut Report) {
        if report.ok {
            if self.retry.failures > 0 {
                self.retry.record_success();
                let _ = self.retry.save(&self.retry_path);
            }
            report.next_retry_at = None;
            return;
        }
        report.next_retry_at = Some(self.retry.record_failure(now_secs()));
        let _ = self.retry.save(&self.retry_path);
    }

    pub fn retry_state(&self) -> &RetryState {
        &self.retry
    }

    fn emit(&self, phase: &str, current: usize, total: usize, message: impl Into<String>) {
        if let Some(sink) = &self.progress {
            sink.emit(phase, current, total, message);
        }
    }

    /// sync/ 下本机目录名：优先 installation.yaml 的 installation_id，缺失时回退 device_id。
    fn local_sync_id(&self) -> String {
        rime_dir::local_sync_id_in(&self.rime_dir, &self.config.device_id)
    }

    /// 导出本地词库并统计条目数（rime_dict_manager -e，要求输入法已停止）。
    /// 失败只 warn：统计是加固信息，不阻断 apply。
    fn export_userdb_count(&self, label: &str) -> Option<usize> {
        let dir = self.state_dir.join("tmp");
        if let Err(error) = fs::create_dir_all(&dir) {
            tracing::warn!("create {}: {error:#}", dir.display());
            return None;
        }
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        let dest = TempPath(dir.join(format!(
            "userdb-{label}-{}-{unique}.txt",
            std::process::id()
        )));
        if let Err(error) =
            self.trigger
                .export_userdb(&self.rime_dir, USERDB_EXPORT_SCHEMA, &dest.0)
        {
            tracing::warn!("userdb export ({label}) failed, continuing without counts: {error:#}");
            return None;
        }
        let bytes = match fs::read(&dest.0) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    "read userdb export ({label}) {}: {error:#}",
                    dest.0.display()
                );
                return None;
            }
        };
        let count = crate::userdb::entry_count(&bytes);
        tracing::info!("userdb export ({label}): entries={count:?}");
        count
    }

    fn do_transport(&mut self, report: &mut Report) -> Result<()> {
        let device = self.config.device_id.clone();
        plan::validate_rel_path(&device).context("invalid device_id")?;
        if device.contains('/') {
            bail!("device_id must be a single path segment: {device}");
        }
        // librime / 小狼毫把快照写在 sync/<installation_id>/，不是 sync/<device_id>/。
        let local_id = self.local_sync_id();
        plan::validate_rel_path(&local_id).context("invalid installation_id")?;
        let remote_sync = format!("sync/{local_id}");
        self.cancelled()?;

        report.first_sync = self.detect_first_sync(&local_id, &device)?;
        if self.config.first_sync_policy.pull_only() {
            // 只读策略：policy != normal 时永远只拉不推（与是否首轮无关）。
            // 不 ensure_base / ensure_dir、不写 owner.json、不写 shared、不上传任何文件。
            report.mode = self.config.first_sync_policy.id().to_string();
            if report.first_sync {
                tracing::info!(
                    "first sync detected; policy={} -> read-only (pull only)",
                    self.config.first_sync_policy.id()
                );
                self.emit("pull", 0, 0, "first sync: pull only");
            } else {
                tracing::debug!(
                    "policy={} -> read-only (pull only)",
                    self.config.first_sync_policy.id()
                );
            }
            self.pull_snapshots(report)?;
            self.sync_shared(report, true)?;
            return Ok(());
        }
        if report.first_sync {
            tracing::warn!(
                "first sync detected but policy={} allows bidirectional sync",
                self.config.first_sync_policy.id()
            );
        }

        // MKCOL / owner.json 都是写操作：预演一律不做（目录不存在也不影响后续读取）。
        if !self.dry_run {
            self.emit("pull", 0, 0, "ensure remote layout");
            self.dav.ensure_base()?;
            self.dav.ensure_dir("sync")?;
            self.dav.ensure_dir(&remote_sync)?;
            self.dav.ensure_dir("shared")?;
            self.dav.ensure_dir("shared/files")?;
            self.write_owner(&remote_sync)?;
        }

        self.pull_snapshots(report)?;
        self.sync_shared(report, false)?;
        self.push_snapshots(report)?;
        Ok(())
    }

    /// SPEC-4 §A1：首次接入 = 本地无 index.json 且远端已有别的设备目录或 shared 数据。
    ///
    /// 只读探测：这里不创建任何远端目录/文件。
    fn detect_first_sync(&self, local_id: &str, device: &str) -> Result<bool> {
        if self.index_existed {
            return Ok(false);
        }
        self.cancelled()?;
        let entries = self.dav.propfind("sync", 1)?;
        let mut other_devices = 0usize;
        for entry in entries {
            if !entry.is_dir || !entry.path.starts_with("sync/") {
                continue;
            }
            let name = entry.path.rsplit('/').next().unwrap_or_default();
            // 自己的目录：installation_id 或旧版 device_id 都算自己。
            if name.is_empty() || name == local_id || name == device || name.starts_with('.') {
                continue;
            }
            other_devices += 1;
        }
        let mut shared_files = 0usize;
        if other_devices == 0 {
            let files = self.dav.propfind("shared/files", 1)?;
            shared_files = files.iter().filter(|entry| !entry.is_dir).count();
            if shared_files == 0 {
                if let Some(bytes) = self.dav.get("shared/manifest.json")? {
                    shared_files = serde_json::from_slice::<FileIndex>(&bytes)
                        .map(|manifest| manifest.files.len())
                        .unwrap_or(0);
                }
            }
        }
        Ok(first_sync_needed(
            self.index_existed,
            other_devices,
            shared_files,
        ))
    }

    /// 追加一条同步历史（失败只 warn，不影响同步结果）。
    fn record_history(&self, mode: &str, report: &Report) {
        let entry = HistoryEntry {
            ts: now_secs(),
            mode: mode.to_string(),
            ok: report.ok,
            pulled: report.pulled,
            pushed: report.pushed,
            merged: report.merged,
            conflicts: report.conflicts.len(),
            duration_ms: report.duration_ms,
            backup_id: report.backup_id.clone(),
            error: report.error.clone(),
        };
        if let Err(error) = history::append(&self.history_path, &entry) {
            tracing::warn!("append history {}: {error:#}", self.history_path.display());
        }
    }

    fn write_owner(&self, remote_sync: &str) -> Result<()> {
        let now = now_secs();
        let owner = serde_json::json!({
            "device_id": self.config.device_id,
            "hostname": std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
            "os": std::env::consts::OS,
            "session": format!("{}-{now}", self.config.device_id),
            "heartbeat_ts": now,
        });
        let bytes = serde_json::to_vec_pretty(&owner)?;
        self.dav
            .put(&format!("{remote_sync}/owner.json"), &bytes, None)?;
        Ok(())
    }

    fn pull_snapshots(&mut self, report: &mut Report) -> Result<()> {
        let local_id = self.local_sync_id();
        let device = self.config.device_id.clone();
        let entries = self.dav.propfind("sync", 1)?;
        let mut dirs = Vec::new();
        for entry in entries {
            if entry.is_dir && entry.path.starts_with("sync/") {
                dirs.push(entry.path);
            }
        }
        let total = dirs.len();
        for (index, dir) in dirs.iter().enumerate() {
            self.cancelled()?;
            self.emit("pull", index, total, format!("scan {dir}"));
            let Some(name) = dir.rsplit('/').next() else {
                continue;
            };
            // 自己的目录：installation_id 或旧版 device_id 都算自己。
            if name.is_empty() || name == local_id || name == device || name.starts_with('.') {
                continue;
            }
            if let Err(error) = plan::validate_rel_path(dir) {
                report.skipped.push(format!("{dir}: {error:#}"));
                continue;
            }
            let entries = match self.dav.propfind(dir, 1) {
                Ok(entries) => entries,
                Err(error) => {
                    report.skipped.push(format!("{dir}: {error:#}"));
                    continue;
                }
            };
            for entry in entries {
                if entry.is_dir {
                    continue;
                }
                if entry.path.ends_with("/owner.json") {
                    continue;
                }
                if let Err(error) = plan::assert_remote_snapshot_path(&entry.path) {
                    report.skipped.push(format!("{}: {error:#}", entry.path));
                    continue;
                }
                let local = match safe_join(&self.rime_dir, &entry.path) {
                    Ok(path) => path,
                    Err(error) => {
                        report.skipped.push(format!("{}: {error:#}", entry.path));
                        continue;
                    }
                };
                let Some(bytes) = self.dav.get(&entry.path)? else {
                    continue;
                };
                let is_userdb = entry.path.ends_with(".userdb.txt");
                if let Ok(current) = fs::read(&local) {
                    if current == bytes {
                        continue;
                    }
                    if is_userdb {
                        self.note_userdb_conflict(&entry.path, &current, &bytes, report);
                    }
                }
                if self.dry_run {
                    self.record(PlanKind::Pull, &entry.path, "snapshot download");
                } else {
                    write_bytes(&local, &bytes)?;
                }
                report.pulled += 1;
                if is_userdb {
                    // 新/变化的 *.userdb.txt 只是落盘，还需 apply 才会导入 LevelDB。
                    report.pending_merge = true;
                }
            }
        }
        Ok(())
    }

    /// SPEC-4 §A5：只统计两侧 *.userdb.txt 的词条差异，绝不自动合并。
    fn note_userdb_conflict(&self, path: &str, local: &[u8], remote: &[u8], report: &mut Report) {
        if report.conflict_details.is_none() {
            return;
        }
        match crate::userdb::diff_stats(local, remote) {
            Some(stats) => {
                let details = report.conflict_details.get_or_insert_with(Vec::new);
                if !details.iter().any(|item| item.path == path) {
                    details.push(ConflictDetail {
                        path: path.to_string(),
                        only_local: stats.only_local,
                        only_remote: stats.only_remote,
                        both_changed: stats.both_changed,
                        sample: stats.sample,
                    });
                }
            }
            None => {
                // 统计失败 → 降级为 null，主流程继续。
                tracing::debug!("userdb stats unavailable for {path}; conflict_details=null");
                report.conflict_details = None;
            }
        }
    }

    fn push_snapshots(&mut self, report: &mut Report) -> Result<()> {
        // 远端路径由本地 rel 路径推导，所以目录名换成 installation_id 后
        // 上传目标自然是 sync/<installation_id>/。
        let local_id = self.local_sync_id();
        let local_root = self.sync_dir.join(&local_id);
        if !local_root.is_dir() {
            return Ok(());
        }
        let mut files = Vec::new();
        collect_files(&local_root, 0, &mut files)?;
        let total = files.len();
        for (index, path) in files.iter().enumerate() {
            self.cancelled()?;
            let Ok(relative) = path.strip_prefix(&self.rime_dir) else {
                continue;
            };
            let rel = relative.to_string_lossy().replace('\\', "/");
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.ends_with(".userdb.txt") {
                report.skipped.push(format!("{rel}: not a rime snapshot"));
                continue;
            }
            if let Err(error) = plan::assert_upload_allowed(&rel)
                .and_then(|_| plan::assert_remote_snapshot_path(&rel))
            {
                report.skipped.push(format!("{rel}: {error:#}"));
                continue;
            }
            let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let hash = sha256_hex(&bytes);
            let previous = self.index.snapshots.get(&rel).cloned();
            let already = match previous {
                Some(meta) if meta.sha256 == hash => {
                    self.dav.remote_unchanged(&rel, &meta).unwrap_or(false)
                }
                _ => false,
            };
            if already {
                continue;
            }
            self.emit("push", index, total, format!("upload {rel}"));
            match self.put(&rel, &bytes, None, "snapshot upload")? {
                PutOutcome::Written => {}
                PutOutcome::PreconditionFailed => {
                    report
                        .conflicts
                        .push(format!("{rel}: remote changed during upload"));
                    continue;
                }
            }
            let mut meta = FileMeta::local(&bytes);
            if let Ok(Some(remote)) = self.dav.head(&rel) {
                meta.etag = remote.etag;
                if let Some(size) = remote.size {
                    meta.size = size;
                }
                if let Some(last_modified) = remote.last_modified {
                    meta.mtime = last_modified;
                }
            }
            self.index.snapshots.insert(rel, meta);
            report.pushed += 1;
        }
        Ok(())
    }

    /// read_only=true 时只做远端 -> 本地方向的合并，绝不 PUT / MKCOL / DELETE。
    fn sync_shared(&mut self, report: &mut Report, read_only: bool) -> Result<()> {
        let manifest_bytes = self.dav.get("shared/manifest.json")?;
        let mut manifest = match &manifest_bytes {
            Some(bytes) => {
                serde_json::from_slice::<FileIndex>(bytes).context("parse shared/manifest.json")?
            }
            None => FileIndex::default(),
        };
        if manifest.version == 0 {
            manifest.version = crate::index::INDEX_VERSION;
        }

        for key in manifest.files.keys() {
            let class = plan::classify(key);
            plan::assert_shared_allowed(key)
                .with_context(|| format!("remote manifest contains forbidden path: {key}"))?;
            if !self.config.wants(class.category) {
                report
                    .skipped
                    .push(format!("{key}: kind disabled by sync_kinds"));
            }
        }

        let local_files = self.scan_local_shared()?;
        let mut keys: BTreeSet<String> = BTreeSet::new();
        keys.extend(manifest.files.keys().cloned());
        keys.extend(self.index.files.keys().cloned());
        keys.extend(local_files.keys().cloned());

        let total = keys.len();
        for (index, rel) in keys.iter().enumerate() {
            self.cancelled()?;
            plan::validate_rel_path(rel).with_context(|| format!("unsafe path: {rel}"))?;
            let class = plan::classify(rel);
            if !class.category.is_shared() || !self.config.wants(class.category) {
                report
                    .skipped
                    .push(format!("{rel}: kind disabled by sync_kinds"));
                continue;
            }
            plan::assert_shared_allowed(rel)?;

            let local_path = safe_join(&self.rime_dir, rel)?;
            let local_bytes = fs::read(&local_path).ok();
            let local_hash = local_bytes.as_ref().map(|bytes| sha256_hex(bytes));
            let base = self.index.files.get(rel).cloned();
            let remote_meta = manifest.files.get(rel).cloned();
            let base_hash = base.as_ref().map(|meta| meta.sha256.clone());
            let remote_hash = remote_meta.as_ref().map(|meta| meta.sha256.clone());

            // *.custom.yaml 用「patch 段规范化摘要」做三向判定：小狼毫每次改设置都会
            // 重写 customization 头，整文件摘要会把纯 metadata 变化当成内容变化。
            let custom = is_custom_yaml(rel);
            let mut remote_bytes: Option<Vec<u8>> = None;
            let mut metadata_only = false;
            let decision = if custom
                && local_hash.is_some()
                && remote_hash.is_some()
                && local_hash != remote_hash
            {
                let Some(bytes) = self.fetch_shared_remote(rel, remote_meta.as_ref(), report)?
                else {
                    continue;
                };
                let local = local_bytes.as_deref().ok_or_else(|| {
                    anyhow!(
                        "local hash present but file unreadable: {rel} ({})",
                        local_path.display()
                    )
                })?;
                let normalized_local = custom_yaml_digest(local);
                let normalized_remote = custom_yaml_digest(&bytes);
                let normalized_base = self
                    .load_base(base_hash.as_deref())
                    .as_deref()
                    .map(custom_yaml_digest);
                remote_bytes = Some(bytes);
                if normalized_local == normalized_remote {
                    // patch 完全一致，只有文件头不同：不是冲突，采用远端字节让本地彻底对齐。
                    metadata_only = true;
                    MergeResult::Unchanged
                } else {
                    match normalized_base {
                        Some(digest) if digest == normalized_local => MergeResult::TakeRemote,
                        Some(digest) if digest == normalized_remote => MergeResult::TakeLocal,
                        Some(_) => MergeResult::Conflict,
                        // 基线内容不可用（升级后首轮）：退回原始摘要，结论只会更保守。
                        None => merge3_digests(
                            base_hash.as_deref(),
                            local_hash.as_deref(),
                            remote_hash.as_deref(),
                        ),
                    }
                }
            } else {
                merge3_digests(
                    base_hash.as_deref(),
                    local_hash.as_deref(),
                    remote_hash.as_deref(),
                )
            };
            self.emit(
                "merge",
                index,
                total,
                format!("{rel}: {}", decision_label(decision)),
            );

            match decision {
                MergeResult::Unchanged => {
                    if metadata_only {
                        // 纯 metadata 差异：把远端字节写成本地文件并更新 index/基线，
                        // 下一轮本地与远端整文件一致，不再反复进入这条路径。
                        let bytes = remote_bytes.ok_or_else(|| {
                            anyhow!("metadata-only merge without fetched bytes: {rel}")
                        })?;
                        let meta = remote_meta.clone().ok_or_else(|| {
                            anyhow!("metadata-only merge without remote metadata: {rel}")
                        })?;
                        self.write_pulled(
                            rel,
                            &bytes,
                            "shared merge: metadata-only, adopt remote",
                        )?;
                        self.remember_base(Some(&meta.sha256), Some(&bytes));
                        self.index.files.insert(rel.clone(), meta);
                        report.merged += 1;
                        report.config_changed = true;
                        tracing::info!(
                            "{rel}: only the customization header differs (metadata-only); adopting remote bytes"
                        );
                        continue;
                    }
                    self.remember_base(local_hash.as_deref(), local_bytes.as_deref());
                    match remote_meta {
                        Some(meta) => {
                            self.index.files.insert(rel.clone(), meta);
                        }
                        None => {
                            if local_hash.is_none() {
                                self.index.files.remove(rel);
                            }
                        }
                    }
                }
                MergeResult::TakeLocal => {
                    if read_only {
                        report
                            .skipped
                            .push(format!("{rel}: read-only policy, local change not pushed"));
                        continue;
                    }
                    let bytes =
                        local_bytes.ok_or_else(|| anyhow!("local file disappeared: {rel}"))?;
                    let if_match = remote_meta.as_ref().and_then(|meta| meta.etag.as_deref());
                    let remote_path = format!("shared/files/{rel}");
                    match self.put(
                        &remote_path,
                        &bytes,
                        if_match,
                        "shared upload: local changed",
                    )? {
                        PutOutcome::Written => {
                            let meta = FileMeta::local(&bytes);
                            self.remember_base(Some(&meta.sha256), Some(&bytes));
                            manifest.files.insert(rel.clone(), meta.clone());
                            self.index.files.insert(rel.clone(), meta);
                            report.pushed += 1;
                            report.merged += 1;
                        }
                        PutOutcome::PreconditionFailed => {
                            report
                                .conflicts
                                .push(format!("{rel}: remote changed during upload"));
                        }
                    }
                }
                MergeResult::TakeRemote => {
                    let meta =
                        remote_meta.ok_or_else(|| anyhow!("missing remote metadata for {rel}"))?;
                    let remote_path = format!("shared/files/{rel}");
                    let bytes = match remote_bytes.take() {
                        Some(bytes) => bytes,
                        None => {
                            let Some(bytes) = self.dav.get(&remote_path)? else {
                                report
                                    .conflicts
                                    .push(format!("{rel}: manifest entry has no remote file"));
                                continue;
                            };
                            bytes
                        }
                    };
                    if sha256_hex(&bytes) != meta.sha256 {
                        report
                            .conflicts
                            .push(format!("{rel}: remote hash does not match manifest"));
                        continue;
                    }
                    // 本地字节即将被替换：Windows 上需要重新部署。
                    if custom {
                        report.config_changed = true;
                    }
                    self.write_pulled(rel, &bytes, "shared merge: take remote")?;
                    self.remember_base(Some(&meta.sha256), Some(&bytes));
                    self.index.files.insert(rel.clone(), meta);
                    report.pulled += 1;
                    report.merged += 1;
                }
                MergeResult::Conflict => {
                    // *.custom.yaml 先试段落级合并；任何不适配都静默回落文件级冲突。
                    if !read_only
                        && custom
                        && self.merge_custom_yaml(
                            rel,
                            local_bytes.as_deref(),
                            remote_meta.as_ref(),
                            remote_bytes.as_deref(),
                            base.as_ref(),
                            &mut manifest,
                            report,
                        )?
                    {
                        continue;
                    }
                    // 预演不写冲突副本：只报冲突，等用户决定后再由真实同步落盘。
                    if !self.dry_run {
                        if let Some(bytes) = &local_bytes {
                            if !self.has_conflict_copy(rel)? {
                                let conflict_rel = conflict_path(rel, &self.config.device_id)?;
                                write_file(&self.rime_dir, &conflict_rel, bytes)?;
                            }
                        }
                    } else {
                        self.record(PlanKind::Conflict, rel, "both sides changed");
                    }
                    report.conflicts.push(rel.clone());
                }
                MergeResult::DeleteLocal => {
                    if local_path.exists() {
                        if self.dry_run {
                            self.record(
                                PlanKind::DeleteLocal,
                                rel,
                                "deleted remotely, local unchanged",
                            );
                        } else {
                            fs::remove_file(&local_path)
                                .with_context(|| format!("remove {rel}"))?;
                        }
                    }
                    self.index.files.remove(rel);
                    manifest.files.remove(rel);
                    report.merged += 1;
                }
                MergeResult::LocalDeleted => {
                    if read_only {
                        report.skipped.push(format!(
                            "{rel}: read-only policy, remote deletion not propagated"
                        ));
                        continue;
                    }
                    self.delete_remote(
                        &format!("shared/files/{rel}"),
                        "shared delete: removed locally",
                    )?;
                    manifest.files.remove(rel);
                    self.index.files.remove(rel);
                    report.pushed += 1;
                    report.merged += 1;
                }
                MergeResult::RemoteDeletedLocalModified => {
                    report
                        .conflicts
                        .push(format!("{rel}: deleted remotely, kept local copy"));
                }
            }
        }

        if !read_only {
            let bytes = serde_json::to_vec_pretty(&manifest)?;
            if manifest_bytes.as_deref() != Some(bytes.as_slice()) {
                self.put(
                    "shared/manifest.json",
                    &bytes,
                    None,
                    "shared manifest update",
                )?;
            }
        }
        Ok(())
    }

    /// 取 shared/files/<rel> 并按 manifest 校验摘要；不一致时记冲突并返回 None。
    fn fetch_shared_remote(
        &self,
        rel: &str,
        meta: Option<&FileMeta>,
        report: &mut Report,
    ) -> Result<Option<Vec<u8>>> {
        let Some(meta) = meta else {
            return Ok(None);
        };
        let Some(bytes) = self.dav.get(&format!("shared/files/{rel}"))? else {
            report
                .conflicts
                .push(format!("{rel}: manifest entry has no remote file"));
            return Ok(None);
        };
        if sha256_hex(&bytes) != meta.sha256 {
            report
                .conflicts
                .push(format!("{rel}: remote hash does not match manifest"));
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    /// *.custom.yaml 段落级合并：成功返回 true（调用方继续下一项），不适配返回 false。
    ///
    /// known_remote 是调用方已经取回并校验过的远端字节，避免同一轮重复 GET。
    #[allow(clippy::too_many_arguments)]
    fn merge_custom_yaml(
        &mut self,
        rel: &str,
        local: Option<&[u8]>,
        remote_meta: Option<&FileMeta>,
        known_remote: Option<&[u8]>,
        base: Option<&FileMeta>,
        manifest: &mut FileIndex,
        report: &mut Report,
    ) -> Result<bool> {
        let (Some(local_bytes), Some(meta)) = (local, remote_meta) else {
            return Ok(false);
        };
        let remote_path = format!("shared/files/{rel}");
        let remote_bytes = match known_remote {
            Some(bytes) => bytes.to_vec(),
            None => {
                let Some(bytes) = self.dav.get(&remote_path)? else {
                    return Ok(false);
                };
                bytes
            }
        };
        if sha256_hex(&remote_bytes) != meta.sha256 {
            return Ok(false);
        }
        let base_bytes = self.load_base(base.map(|item| item.sha256.as_str()));
        let outcome = merge_yaml_patch(base_bytes.as_deref(), local_bytes, Some(&remote_bytes))?;
        let (bytes, conflicts) = match outcome {
            MergeOutcome::Patch { bytes, conflicts } => (bytes, conflicts),
            MergeOutcome::Fallback { reason } => {
                tracing::debug!("patch merge not applicable for {rel}: {reason}");
                return Ok(false);
            }
        };
        let hash = sha256_hex(&bytes);
        if hash != sha256_hex(local_bytes) {
            self.write_pulled(rel, &bytes, "patch merge result")?;
            report.config_changed = true;
        }
        if hash != meta.sha256 {
            match self.put(
                &remote_path,
                &bytes,
                meta.etag.as_deref(),
                "patch merge upload",
            )? {
                PutOutcome::Written => {
                    let new_meta = FileMeta::local(&bytes);
                    self.remember_base(Some(&hash), Some(&bytes));
                    manifest.files.insert(rel.to_string(), new_meta.clone());
                    self.index.files.insert(rel.to_string(), new_meta);
                    report.pushed += 1;
                }
                PutOutcome::PreconditionFailed => {
                    report
                        .conflicts
                        .push(format!("{rel}: remote changed during upload"));
                    return Ok(true);
                }
            }
        }
        report.merged += 1;
        if conflicts.is_empty() {
            tracing::debug!("patch merge resolved {rel}");
        } else {
            // 段落级合并成功但仍有键冲突：真实同步会额外写冲突副本，预览要说清楚。
            if self.dry_run {
                self.record(PlanKind::Conflict, rel, conflicts.join(", "));
            }
            report
                .conflicts
                .push(format!("{rel}: {}", conflicts.join(", ")));
            if !self.dry_run && !self.has_conflict_copy(rel)? {
                let conflict_rel = conflict_path(rel, &self.config.device_id)?;
                write_file(&self.rime_dir, &conflict_rel, local_bytes)?;
            }
        }
        Ok(true)
    }

    /// 基线内容缓存（内容寻址，只用于 *.custom.yaml 的段落级合并）。
    fn remember_base(&self, hash: Option<&str>, bytes: Option<&[u8]>) {
        // 基线缓存写 state_dir，也是磁盘副作用：预演不写。
        if self.dry_run {
            return;
        }
        let (Some(hash), Some(bytes)) = (hash, bytes) else {
            return;
        };
        if !is_hex_sha256(hash) {
            return;
        }
        let path = self.base_dir.join(hash);
        if path.is_file() {
            return;
        }
        if let Err(error) = fs::create_dir_all(&self.base_dir) {
            tracing::debug!("create base cache {}: {error}", self.base_dir.display());
            return;
        }
        if let Err(error) = fs::write(&path, bytes) {
            tracing::debug!("write base cache {hash}: {error}");
        }
    }

    fn load_base(&self, hash: Option<&str>) -> Option<Vec<u8>> {
        let hash = hash?;
        if !is_hex_sha256(hash) {
            return None;
        }
        let bytes = fs::read(self.base_dir.join(hash)).ok()?;
        if sha256_hex(&bytes) == hash {
            Some(bytes)
        } else {
            None
        }
    }

    /// 同一冲突在用户处理前不应每轮都生成新副本。
    fn has_conflict_copy(&self, rel: &str) -> Result<bool> {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let parent = match rel.rsplit_once('/') {
            Some((parent, _)) => self.rime_dir.join(parent),
            None => self.rime_dir.clone(),
        };
        let prefix = format!("{name}.conflict-{}-", self.config.device_id);
        let entries = match fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).with_context(|| format!("read {}", parent.display())),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn scan_local_shared(&self) -> Result<BTreeMap<String, PathBuf>> {
        let mut out = BTreeMap::new();
        self.walk_local(&self.rime_dir, 0, &mut out)?;
        Ok(out)
    }

    fn walk_local(
        &self,
        dir: &Path,
        depth: usize,
        out: &mut BTreeMap<String, PathBuf>,
    ) -> Result<()> {
        if depth > 4 {
            return Ok(());
        }
        let entries = fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                if plan::is_excluded_dir(&name) {
                    continue;
                }
                self.walk_local(&entry.path(), depth + 1, out)?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let file_path = entry.path();
            let Ok(relative) = file_path.strip_prefix(&self.rime_dir) else {
                continue;
            };
            let rel = relative.to_string_lossy().replace('\\', "/");
            let class = plan::classify(&rel);
            if !class.category.is_shared()
                || !self.config.wants(class.category)
                || plan::is_conflict_copy(&name)
                || self.config.excluded(&rel)
            {
                continue;
            }
            out.insert(rel, entry.path());
        }
        Ok(())
    }
}

/// SPEC-4 §A1 判定核心：本地无基线，且远端已有别的设备目录或 shared 文件。
pub fn first_sync_needed(index_existed: bool, other_devices: usize, shared_files: usize) -> bool {
    !index_existed && (other_devices > 0 || shared_files > 0)
}

fn is_hex_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn dedupe(items: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

fn decision_label(decision: MergeResult) -> &'static str {
    match decision {
        MergeResult::Unchanged => "unchanged",
        MergeResult::TakeLocal => "local-changed",
        MergeResult::TakeRemote => "remote-changed",
        MergeResult::Conflict => "conflict",
        MergeResult::DeleteLocal => "remote-deleted",
        MergeResult::LocalDeleted => "local-deleted",
        MergeResult::RemoteDeletedLocalModified => "remote-deleted-local-modified",
    }
}

fn conflict_path(rel: &str, device: &str) -> Result<String> {
    let (parent, name) = match rel.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None => (None, rel),
    };
    let conflict_name = format!("{name}.conflict-{device}-{}", now_secs());
    let conflict_rel = match parent {
        Some(parent) => format!("{parent}/{conflict_name}"),
        None => conflict_name,
    };
    plan::validate_rel_path(&conflict_rel)?;
    Ok(conflict_rel)
}

fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    plan::validate_rel_path(rel)?;
    Ok(root.join(rel))
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let path = safe_join(root, rel)?;
    write_bytes(&path, bytes)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if depth > 4 {
        return Ok(());
    }
    let entries = fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            if plan::is_excluded_dir(&name) {
                continue;
            }
            collect_files(&entry.path(), depth + 1, out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_name_lands_next_to_original() {
        let path = conflict_path("sub/default.custom.yaml", "devA").expect("conflict");
        assert!(path.starts_with("sub/default.custom.yaml.conflict-devA-"));
        assert!(plan::validate_rel_path(&path).is_ok());
    }

    #[test]
    fn report_serializes() {
        let report = Report {
            pulled: 1,
            pushed: 2,
            merged: 3,
            conflicts: vec!["a".into()],
            skipped: vec![],
            duration_ms: 9,
            ok: true,
            error: None,
            backup_id: Some("20240101-000000".into()),
            next_retry_at: Some(1_700_000_000),
            first_sync: true,
            mode: "pull-only".into(),
            conflict_details: Some(vec![ConflictDetail {
                path: "sync/devB/main.userdb.txt".into(),
                only_local: 1,
                only_remote: 2,
                both_changed: 3,
                sample: vec!["-a".into()],
            }]),
            config_changed: true,
            pending_merge: true,
            merged_entries: Some(42),
        };
        let json = serde_json::to_value(&report).expect("json");
        assert_eq!(json["pushed"], 2);
        assert_eq!(json["pending_merge"], true);
        assert_eq!(json["config_changed"], true);
        assert_eq!(json["merged_entries"], 42);
        assert_eq!(json["conflicts"][0], "a");
        assert_eq!(json["backup_id"], "20240101-000000");
        assert_eq!(json["first_sync"], true);
        assert_eq!(json["mode"], "pull-only");
        assert_eq!(json["conflict_details"][0]["both_changed"], 3);
    }

    #[test]
    fn first_sync_needs_missing_index_and_remote_data() {
        // (本地有 index, 其他设备目录数, shared 文件数) → 是否首次接入
        assert!(first_sync_needed(false, 1, 0), "no index + other device");
        assert!(first_sync_needed(false, 0, 3), "no index + shared files");
        assert!(
            !first_sync_needed(false, 0, 0),
            "no index but empty remote is a fresh start, not first sync"
        );
        assert!(
            !first_sync_needed(true, 1, 5),
            "existing index means not first sync"
        );
    }

    #[test]
    fn plan_report_serializes_actions() {
        let report = PlanReport {
            actions: vec![
                PlanAction {
                    kind: PlanKind::Push,
                    path: "sync/devA/main.userdb.txt".into(),
                    detail: "snapshot upload".into(),
                },
                PlanAction {
                    kind: PlanKind::DeleteLocal,
                    path: "default.custom.yaml".into(),
                    detail: "deleted remotely".into(),
                },
            ],
            counts: PlanCounts {
                push: 1,
                delete_local: 1,
                ..PlanCounts::default()
            },
            total: 2,
            mode: "normal".into(),
            dry_run: true,
            ok: true,
            ..PlanReport::default()
        };
        let json = serde_json::to_value(&report).expect("json");
        assert_eq!(json["actions"][0]["kind"], "push");
        assert_eq!(json["actions"][1]["kind"], "delete_local");
        assert_eq!(json["counts"]["push"], 1);
        assert_eq!(json["total"], 2);
        assert_eq!(json["dry_run"], true);
    }

    #[test]
    fn pull_only_policy_covers_preview_and_pull_only() {
        use crate::config::FirstSyncPolicy;
        assert!(FirstSyncPolicy::Preview.pull_only());
        assert!(FirstSyncPolicy::PullOnly.pull_only());
        assert!(!FirstSyncPolicy::Normal.pull_only());
    }
}
