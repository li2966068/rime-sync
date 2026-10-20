//! transport / apply 两个入口：只搬文件 vs 真正合并部署。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::index::{sha256_hex, FileIndex, FileMeta};
use crate::merge3::{merge3_digests, MergeResult};
use crate::plan;
use crate::platform::RimeTrigger;
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
    dav: DavClient,
    index: FileIndex,
    trigger: Box<dyn RimeTrigger>,
    progress: Option<ProgressSink>,
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
        let index = FileIndex::load(&index_path)?;
        let sync_dir = rime_dir::sync_dir(&rime_dir);
        Ok(Self {
            config,
            rime_dir,
            sync_dir,
            index_path,
            dav,
            index,
            trigger,
            progress: None,
        })
    }

    pub fn with_progress(mut self, sink: ProgressSink) -> Self {
        self.progress = Some(sink);
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn rime_dir(&self) -> &Path {
        &self.rime_dir
    }

    /// 只搬文件：pull → merge → push。
    pub fn transport(&mut self) -> Report {
        let started = Instant::now();
        let mut report = Report {
            ok: true,
            ..Report::default()
        };
        self.emit("start", 0, 0, "transport");
        if let Err(error) = self.do_transport(&mut report) {
            report.ok = false;
            report.error = Some(format!("{error:#}"));
            tracing::error!("transport failed: {error:#}");
        }
        if let Err(error) = self.index.save(&self.index_path) {
            report.ok = false;
            if report.error.is_none() {
                report.error = Some(format!("save index: {error:#}"));
            }
        }
        dedupe(&mut report.conflicts);
        dedupe(&mut report.skipped);
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
    pub fn apply(&mut self) -> Report {
        let started = Instant::now();
        let mut report = self.transport();
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
        report.duration_ms = started.elapsed().as_millis() as u64;
        self.emit(
            "done",
            report.pulled + report.pushed,
            report.pulled + report.pushed,
            format!("apply ok={} duration={}ms", report.ok, report.duration_ms),
        );
        report
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

        self.emit("pull", 0, 0, "ensure remote layout");
        self.dav.ensure_base()?;
        self.dav.ensure_dir("sync")?;
        self.dav.ensure_dir(&remote_sync)?;
        self.dav.ensure_dir("shared")?;
        self.dav.ensure_dir("shared/files")?;
        self.write_owner(&remote_sync)?;

        self.pull_snapshots(report)?;
        self.sync_shared(report)?;
        self.push_snapshots(report)?;
        Ok(())
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
        self.dav.put(&format!("{remote_sync}/owner.json"), &bytes, None)?;
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
                    report
                        .skipped
                        .push(format!("{}: {error:#}", entry.path));
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
                if fs::read(&local)
                    .map(|current| current == bytes)
                    .unwrap_or(false)
                {
                    continue;
                }
                write_bytes(&local, &bytes)?;
                report.pulled += 1;
            }
        }
        Ok(())
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
                    report.conflicts.push(format!("{rel}: remote changed during upload"));
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

    fn sync_shared(&mut self, report: &mut Report) -> Result<()> {
        let manifest_bytes = self.dav.get("shared/manifest.json")?;
        let mut manifest = match &manifest_bytes {
            Some(bytes) => serde_json::from_slice::<FileIndex>(bytes)
                .context("parse shared/manifest.json")?,
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
                    let bytes = local_bytes
                        .ok_or_else(|| anyhow!("local file disappeared: {rel}"))?;
                    let if_match = remote_meta
                        .as_ref()
                        .and_then(|meta| meta.etag.as_deref());
                    let remote_path = format!("shared/files/{rel}");
                    match self.dav.put(&remote_path, &bytes, if_match)? {
                        PutOutcome::Written => {
                            let meta = FileMeta::local(&bytes);
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
                    let meta = remote_meta
                        .ok_or_else(|| anyhow!("missing remote metadata for {rel}"))?;
                    let remote_path = format!("shared/files/{rel}");
                    let Some(bytes) = self.dav.get(&remote_path)? else {
                        report
                            .conflicts
                            .push(format!("{rel}: manifest entry has no remote file"));
                        continue;
                    };
                    if sha256_hex(&bytes) != meta.sha256 {
                        report.conflicts.push(format!(
                            "{rel}: remote hash does not match manifest"
                        ));
                        continue;
                    }
                    write_file(&self.rime_dir, rel, &bytes)?;
                    self.index.files.insert(rel.clone(), meta);
                    report.pulled += 1;
                    report.merged += 1;
                }
                MergeResult::Conflict => {
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
                        fs::remove_file(&local_path)
                            .with_context(|| format!("remove {rel}"))?;
                    }
                    self.index.files.remove(rel);
                    manifest.files.remove(rel);
                    report.merged += 1;
                }
                MergeResult::LocalDeleted => {
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

        let bytes = serde_json::to_vec_pretty(&manifest)?;
        if manifest_bytes.as_deref() != Some(bytes.as_slice()) {
            self.dav.put("shared/manifest.json", &bytes, None)?;
        }
        Ok(())
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
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", parent.display()))
            }
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
        };
        let json = serde_json::to_value(&report).expect("json");
        assert_eq!(json["pushed"], 2);
        assert_eq!(json["conflicts"][0], "a");
    }
}
