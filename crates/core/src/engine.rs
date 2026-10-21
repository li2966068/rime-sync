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
use crate::merge3::{merge3_digests, merge_yaml_patch, MergeOutcome, MergeResult};
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
        if let Err(error) = backups.prune(crate::backup::DEFAULT_KEEP) {
            tracing::warn!("prune backups: {error:#}");
        }
        let mut report = self.transport_inner();
        report.backup_id = Some(backup_id);
        if !report.ok {
            report.duration_ms = started.elapsed().as_millis() as u64;
            return report;
        }
        self.emit("deploy", 0, 3, "stopping input method");
        let mut deploy_error: Option<anyhow::Error> = None;
        if let Err(error) = self.trigger.stop_ime() {
            deploy_error = Some(error);
        }
        if deploy_error.is_none() {
            self.emit("deploy", 1, 3, "rime_dict_manager --sync");
            if let Err(error) = self.trigger.sync_userdb(&self.rime_dir) {
                deploy_error = Some(error);
            }
        }
        if deploy_error.is_none() {
            self.emit("deploy", 2, 3, "rime_deployer --build");
            if let Err(error) = self.trigger.deploy(&self.rime_dir) {
                deploy_error = Some(error);
            }
        }
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

    fn do_transport(&mut self, report: &mut Report) -> Result<()> {
        let device = self.config.device_id.clone();
        plan::validate_rel_path(&device).context("invalid device_id")?;
        if device.contains('/') {
            bail!("device_id must be a single path segment: {device}");
        }
        let remote_sync = format!("sync/{device}");
        self.cancelled()?;

        report.first_sync = self.detect_first_sync(&device)?;
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

        self.emit("pull", 0, 0, "ensure remote layout");
        self.dav.ensure_base()?;
        self.dav.ensure_dir("sync")?;
        self.dav.ensure_dir(&remote_sync)?;
        self.dav.ensure_dir("shared")?;
        self.dav.ensure_dir("shared/files")?;
        self.write_owner(&remote_sync)?;

        self.pull_snapshots(report)?;
        self.sync_shared(report, false)?;
        self.push_snapshots(report)?;
        Ok(())
    }

    /// SPEC-4 §A1：首次接入 = 本地无 index.json 且远端已有别的设备目录或 shared 数据。
    ///
    /// 只读探测：这里不创建任何远端目录/文件。
    fn detect_first_sync(&self, device: &str) -> Result<bool> {
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
            if name.is_empty() || name == device || name.starts_with('.') {
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
            if name == device || name.is_empty() || name.starts_with('.') {
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
                if let Ok(current) = fs::read(&local) {
                    if current == bytes {
                        continue;
                    }
                    if entry.path.ends_with(".userdb.txt") {
                        self.note_userdb_conflict(&entry.path, &current, &bytes, report);
                    }
                }
                write_bytes(&local, &bytes)?;
                report.pulled += 1;
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
        let device = self.config.device_id.clone();
        let local_root = self.sync_dir.join(&device);
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
            match self.dav.put(&rel, &bytes, None)? {
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
            let decision = merge3_digests(
                base.as_ref().map(|meta| meta.sha256.as_str()),
                local_hash.as_deref(),
                remote_meta.as_ref().map(|meta| meta.sha256.as_str()),
            );
            self.emit(
                "merge",
                index,
                total,
                format!("{rel}: {}", decision_label(decision)),
            );

            match decision {
                MergeResult::Unchanged => {
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
                    match self.dav.put(&remote_path, &bytes, if_match)? {
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
                    let Some(bytes) = self.dav.get(&remote_path)? else {
                        report
                            .conflicts
                            .push(format!("{rel}: manifest entry has no remote file"));
                        continue;
                    };
                    if sha256_hex(&bytes) != meta.sha256 {
                        report
                            .conflicts
                            .push(format!("{rel}: remote hash does not match manifest"));
                        continue;
                    }
                    write_file(&self.rime_dir, rel, &bytes)?;
                    self.remember_base(Some(&meta.sha256), Some(&bytes));
                    self.index.files.insert(rel.clone(), meta);
                    report.pulled += 1;
                    report.merged += 1;
                }
                MergeResult::Conflict => {
                    // *.custom.yaml 先试段落级合并；任何不适配都静默回落文件级冲突。
                    if !read_only
                        && rel.ends_with(".custom.yaml")
                        && self.merge_custom_yaml(
                            rel,
                            local_bytes.as_deref(),
                            remote_meta.as_ref(),
                            base.as_ref(),
                            &mut manifest,
                            report,
                        )?
                    {
                        continue;
                    }
                    if let Some(bytes) = &local_bytes {
                        if !self.has_conflict_copy(rel)? {
                            let conflict_rel = conflict_path(rel, &self.config.device_id)?;
                            write_file(&self.rime_dir, &conflict_rel, bytes)?;
                        }
                    }
                    report.conflicts.push(rel.clone());
                }
                MergeResult::DeleteLocal => {
                    if local_path.exists() {
                        fs::remove_file(&local_path).with_context(|| format!("remove {rel}"))?;
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
                    self.dav.delete(&format!("shared/files/{rel}"))?;
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
                self.dav.put("shared/manifest.json", &bytes, None)?;
            }
        }
        Ok(())
    }

    /// *.custom.yaml 段落级合并：成功返回 true（调用方继续下一项），不适配返回 false。
    fn merge_custom_yaml(
        &mut self,
        rel: &str,
        local: Option<&[u8]>,
        remote_meta: Option<&FileMeta>,
        base: Option<&FileMeta>,
        manifest: &mut FileIndex,
        report: &mut Report,
    ) -> Result<bool> {
        let (Some(local_bytes), Some(meta)) = (local, remote_meta) else {
            return Ok(false);
        };
        let remote_path = format!("shared/files/{rel}");
        let Some(remote_bytes) = self.dav.get(&remote_path)? else {
            return Ok(false);
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
            write_file(&self.rime_dir, rel, &bytes)?;
        }
        if hash != meta.sha256 {
            match self.dav.put(&remote_path, &bytes, meta.etag.as_deref())? {
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
            report
                .conflicts
                .push(format!("{rel}: {}", conflicts.join(", ")));
            if !self.has_conflict_copy(rel)? {
                let conflict_rel = conflict_path(rel, &self.config.device_id)?;
                write_file(&self.rime_dir, &conflict_rel, local_bytes)?;
            }
        }
        Ok(true)
    }

    /// 基线内容缓存（内容寻址，只用于 *.custom.yaml 的段落级合并）。
    fn remember_base(&self, hash: Option<&str>, bytes: Option<&[u8]>) {
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
        };
        let json = serde_json::to_value(&report).expect("json");
        assert_eq!(json["pushed"], 2);
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
    fn pull_only_policy_covers_preview_and_pull_only() {
        use crate::config::FirstSyncPolicy;
        assert!(FirstSyncPolicy::Preview.pull_only());
        assert!(FirstSyncPolicy::PullOnly.pull_only());
        assert!(!FirstSyncPolicy::Normal.pull_only());
    }
}
