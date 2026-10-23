//! 维护 / 去重：远端设备目录、远端快照、本地备份三段预览与执行。
//!
//! 分组规则在 rime_sync_core::dedupe（纯函数 + 单测）；这里负责 WebDAV 扫描、
//! 本地备份指纹生成，以及把失败按条聚合后返回给 UI。

use std::collections::{BTreeMap, HashSet};
use std::fs;

use rime_sync_core::backup::{now_secs, BackupManifest};
use rime_sync_core::config::Config;
use rime_sync_core::dedupe::{self, BackupFingerprint, DedupeGroup, DeviceDirEntry, SnapshotFile};
use rime_sync_core::index::sha256_hex;
use rime_sync_core::plan;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::remote_tree;
use crate::{backup_manager, dav_client, ensure_config, push_log, AppState};

/// 与备份一致：sync/ 树最多扫描 6 层。
const SNAPSHOT_DEPTH: usize = 6;
const SNAPSHOT_MAX_FILES: usize = 512;

#[derive(Serialize)]
pub struct DedupeSection {
    pub groups: Vec<DedupeGroup>,
    pub suggested: Vec<String>,
    pub reclaimable: u64,
    pub suggested_bytes: u64,
    pub warnings: Vec<String>,
}

impl DedupeSection {
    fn from_groups(groups: Vec<DedupeGroup>, warnings: Vec<String>) -> Self {
        let suggested: Vec<String> = groups
            .iter()
            .flat_map(|group| group.suggested_ids())
            .collect();
        let reclaimable = groups.iter().map(DedupeGroup::reclaimable).sum();
        let suggested_bytes = groups.iter().map(DedupeGroup::suggested_bytes).sum();
        Self {
            groups,
            suggested,
            reclaimable,
            suggested_bytes,
            warnings,
        }
    }

    fn error(message: String) -> Self {
        Self::from_groups(Vec::new(), vec![message])
    }
}

#[derive(Serialize)]
pub struct DedupePreview {
    pub self_id: String,
    pub generated_at: u64,
    pub devices: DedupeSection,
    pub snapshots: DedupeSection,
    pub backups: DedupeSection,
}

#[derive(Serialize)]
pub struct DedupeFailure {
    pub id: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct DedupeResult {
    pub removed: Vec<String>,
    pub freed_bytes: u64,
    pub failed: Vec<DedupeFailure>,
}

/// 逐条执行、逐条记录失败：单个目标失败不影响其余目标。
#[derive(Default)]
pub(crate) struct ApplyAccumulator {
    pub(crate) removed: Vec<String>,
    pub(crate) freed_bytes: u64,
    pub(crate) failed: Vec<DedupeFailure>,
}

impl ApplyAccumulator {
    pub(crate) fn ok(&mut self, id: impl Into<String>, freed: u64) {
        self.removed.push(id.into());
        self.freed_bytes += freed;
    }

    pub(crate) fn fail(&mut self, id: impl Into<String>, error: impl Into<String>) {
        self.failed.push(DedupeFailure {
            id: id.into(),
            error: error.into(),
        });
    }

    pub(crate) fn finish(self) -> DedupeResult {
        DedupeResult {
            removed: self.removed,
            freed_bytes: self.freed_bytes,
            failed: self.failed,
        }
    }
}

#[tauri::command]
pub fn cmd_dedupe_preview(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DedupePreview, String> {
    let config = ensure_config(&app, &state.0)?;
    let (devices, snapshots) = match dav_client(&app, &state.0) {
        Ok((config, client)) => (
            preview_devices(&client, &config),
            preview_snapshots(&client),
        ),
        Err(error) => (
            DedupeSection::error(error.clone()),
            DedupeSection::error(error),
        ),
    };
    let backups = preview_backups(&app, &state.0)?;
    Ok(DedupePreview {
        self_id: config.device_id,
        generated_at: now_secs(),
        devices,
        snapshots,
        backups,
    })
}

#[tauri::command]
pub fn cmd_dedupe_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
    ids: Vec<String>,
) -> Result<DedupeResult, String> {
    match kind.as_str() {
        "devices" => apply_devices(&app, &state.0, &ids),
        "snapshots" => apply_snapshots(&app, &state.0, &ids),
        "backups" => apply_backups(&app, &state.0, &ids),
        other => Err(format!("未知的去重类型：{other}")),
    }
}

fn scan_sync_dirs(client: &DavClient) -> Result<Vec<String>, String> {
    let entries = client
        .propfind("sync", 1)
        .map_err(|error| format!("读取远端设备失败：{error:#}"))?;
    let mut dirs = Vec::new();
    for entry in entries {
        if !entry.is_dir {
            continue;
        }
        let id = entry.path.rsplit('/').next().unwrap_or("");
        if id.is_empty() || id == "sync" || id == "shared" || id.starts_with('.') {
            continue;
        }
        if entry.path != format!("sync/{id}") {
            continue;
        }
        if plan::validate_rel_path(&entry.path).is_err() {
            continue;
        }
        dirs.push(id.to_string());
    }
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

fn preview_devices(client: &DavClient, config: &Config) -> DedupeSection {
    let dirs = match scan_sync_dirs(client) {
        Ok(dirs) => dirs,
        Err(error) => return DedupeSection::error(error),
    };
    let mut warnings = Vec::new();
    let mut entries = Vec::new();
    for id in dirs {
        let rel = format!("sync/{id}");
        let size = match remote_tree::walk(
            client,
            &rel,
            remote_tree::MAX_DEPTH,
            remote_tree::MAX_ENTRIES,
        ) {
            Ok(entries) => remote_tree::tree_size(&entries),
            Err(error) => {
                warnings.push(format!("{rel}: {error:#}"));
                0
            }
        };
        let owner = client
            .get(&format!("{rel}/owner.json"))
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let (hostname, os, heartbeat_ts) = match owner {
            Some(owner) => (
                text_field(&owner, "hostname"),
                text_field(&owner, "os"),
                owner
                    .get("heartbeat_ts")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
            ),
            None => (None, None, 0),
        };
        entries.push(DeviceDirEntry {
            id,
            hostname,
            os,
            heartbeat_ts,
            size_bytes: size,
        });
    }
    let groups = dedupe::group_device_dirs(&entries, &config.device_id)
        .into_iter()
        .filter(|group| group.tag == dedupe::TAG_UNKNOWN || group.items.len() > 1)
        .collect();
    DedupeSection::from_groups(groups, warnings)
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::to_string)
}

struct SnapshotScan {
    files: Vec<SnapshotFile>,
    warnings: Vec<String>,
}

fn preview_snapshots(client: &DavClient) -> DedupeSection {
    match scan_snapshots(client) {
        Ok(scan) => DedupeSection::from_groups(dedupe::group_snapshots(&scan.files), scan.warnings),
        Err(error) => DedupeSection::error(error),
    }
}

fn scan_snapshots(client: &DavClient) -> Result<SnapshotScan, String> {
    let dirs = scan_sync_dirs(client)?;
    let mut warnings = Vec::new();
    let mut files = Vec::new();
    'outer: for id in dirs {
        let rel = format!("sync/{id}");
        let entries =
            match remote_tree::walk(client, &rel, SNAPSHOT_DEPTH, remote_tree::MAX_ENTRIES) {
                Ok(entries) => entries,
                Err(error) => {
                    warnings.push(format!("{rel}: {error:#}"));
                    continue;
                }
            };
        for entry in entries.into_iter().filter(|entry| !entry.is_dir) {
            if !entry.path.ends_with(".userdb.txt") {
                continue;
            }
            if plan::validate_rel_path(&entry.path).is_err() {
                continue;
            }
            if files.len() >= SNAPSHOT_MAX_FILES {
                warnings.push(format!(
                    "快照数量超过 {SNAPSHOT_MAX_FILES}，其余未扫描（可分批处理后重试）"
                ));
                break 'outer;
            }
            let bytes = match client.get(&entry.path) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(error) => {
                    warnings.push(format!("{}: {error:#}", entry.path));
                    continue;
                }
            };
            let size = if entry.size > 0 {
                entry.size
            } else {
                bytes.len() as u64
            };
            files.push(SnapshotFile {
                sha256: sha256_hex(&bytes),
                size,
                modified_ts: parse_http_date(entry.last_modified.as_deref()),
                path: entry.path,
            });
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(SnapshotScan { files, warnings })
}

fn parse_http_date(value: Option<&str>) -> u64 {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return 0;
    };
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(text) {
        return date.timestamp().max(0) as u64;
    }
    let naive = text.trim_end_matches(" GMT").trim_end_matches(" UTC");
    if let Ok(date) = chrono::NaiveDateTime::parse_from_str(naive, "%a, %d %b %Y %H:%M:%S") {
        return date.and_utc().timestamp().max(0) as u64;
    }
    0
}

fn preview_backups(app: &AppHandle, inner: &crate::Inner) -> Result<DedupeSection, String> {
    let manager = backup_manager(app, inner)?;
    let infos = manager
        .list()
        .map_err(|error| format!("读取备份列表失败：{error:#}"))?;
    let mut warnings = Vec::new();
    let mut fingerprints = Vec::new();
    for info in infos {
        let path = manager.root().join(&info.id).join("manifest.json");
        let manifest = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<BackupManifest>(&text).ok());
        let Some(manifest) = manifest else {
            warnings.push(format!("{}: manifest.json 不可读，已跳过", info.id));
            continue;
        };
        let pairs: Vec<(String, String)> = manifest
            .files
            .iter()
            .map(|file| (file.rel.clone(), file.sha256.clone()))
            .collect();
        fingerprints.push(BackupFingerprint {
            content_hash: dedupe::backup_content_hash(&pairs),
            id: info.id,
            bytes: info.total_bytes,
            created_at: info.created_at,
            label: info.created_at_text,
        });
    }
    Ok(DedupeSection::from_groups(
        dedupe::group_backups(&fingerprints),
        warnings,
    ))
}

fn apply_devices(
    app: &AppHandle,
    inner: &crate::Inner,
    ids: &[String],
) -> Result<DedupeResult, String> {
    let (config, client) = dav_client(app, inner)?;
    let mut acc = ApplyAccumulator::default();
    let mut seen = HashSet::new();
    for id in ids {
        let id = id.trim();
        if !seen.insert(id.to_string()) {
            continue;
        }
        let result: Result<u64, String> = (|| {
            if id.is_empty()
                || id.contains('/')
                || id.contains('\\')
                || id == "."
                || id == ".."
                || id.starts_with('.')
                || id == "shared"
            {
                return Err("设备目录名无效".to_string());
            }
            let rel = format!("sync/{id}");
            dedupe::validate_remote_delete_path(&rel, &config.device_id)
                .map_err(|error| format!("{error:#}"))?;
            let report =
                remote_tree::delete_tree(&client, &rel).map_err(|error| format!("{error:#}"))?;
            Ok(report.freed_bytes)
        })();
        match result {
            Ok(bytes) => acc.ok(id, bytes),
            Err(error) => acc.fail(id, error),
        }
    }
    push_log(
        inner,
        format!(
            "去重：删除 {} 个远端设备目录，释放 {} 字节，失败 {} 个",
            acc.removed.len(),
            acc.freed_bytes,
            acc.failed.len()
        ),
    );
    Ok(acc.finish())
}

fn apply_snapshots(
    app: &AppHandle,
    inner: &crate::Inner,
    ids: &[String],
) -> Result<DedupeResult, String> {
    let (config, client) = dav_client(app, inner)?;
    let scan = scan_snapshots(&client)?;
    let by_path: BTreeMap<String, SnapshotFile> = scan
        .files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect();
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    for file in by_path.values() {
        *totals.entry(file.sha256.clone()).or_default() += 1;
    }
    let mut selected: BTreeMap<String, usize> = BTreeMap::new();
    let mut unique = HashSet::new();
    for id in ids {
        let id = id.trim();
        if !unique.insert(id.to_string()) {
            continue;
        }
        if let Some(file) = by_path.get(id) {
            *selected.entry(file.sha256.clone()).or_default() += 1;
        }
    }

    let mut acc = ApplyAccumulator::default();
    for id in unique {
        let id = id.as_str();
        let result: Result<u64, String> = (|| {
            let Some(file) = by_path.get(id) else {
                return Err("远端快照不存在或不在本次扫描范围内".to_string());
            };
            let total = totals.get(&file.sha256).copied().unwrap_or(0);
            let chosen = selected.get(&file.sha256).copied().unwrap_or(0);
            if total > 0 && chosen >= total {
                return Err("不能删除同一内容的全部副本".to_string());
            }
            dedupe::validate_snapshot_delete_path(id, &config.device_id)
                .map_err(|error| format!("{error:#}"))?;
            client.delete(id).map_err(|error| format!("{error:#}"))?;
            Ok(file.size)
        })();
        match result {
            Ok(bytes) => acc.ok(id, bytes),
            Err(error) => acc.fail(id, error),
        }
    }
    push_log(
        inner,
        format!(
            "去重：删除 {} 个远端快照，释放 {} 字节，失败 {} 个",
            acc.removed.len(),
            acc.freed_bytes,
            acc.failed.len()
        ),
    );
    Ok(acc.finish())
}

fn apply_backups(
    app: &AppHandle,
    inner: &crate::Inner,
    ids: &[String],
) -> Result<DedupeResult, String> {
    let manager = backup_manager(app, inner)?;
    let infos = manager
        .list()
        .map_err(|error| format!("读取备份列表失败：{error:#}"))?;
    let sizes: BTreeMap<String, u64> = infos
        .into_iter()
        .map(|info| (info.id, info.total_bytes))
        .collect();
    let mut acc = ApplyAccumulator::default();
    let mut seen = HashSet::new();
    for id in ids {
        let id = id.trim();
        if !seen.insert(id.to_string()) {
            continue;
        }
        let Some(bytes) = sizes.get(id).copied() else {
            acc.fail(id, "备份不存在");
            continue;
        };
        match manager.delete(id) {
            Ok(()) => acc.ok(id, bytes),
            Err(error) => acc.fail(id, format!("{error:#}")),
        }
    }
    push_log(
        inner,
        format!(
            "去重：删除 {} 份备份，释放 {} 字节，失败 {} 个",
            acc.removed.len(),
            acc.freed_bytes,
            acc.failed.len()
        ),
    );
    Ok(acc.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_aggregated_without_aborting_other_ids() {
        let mut acc = ApplyAccumulator::default();
        acc.ok("device-a", 10);
        acc.fail("device-b", "PROPFIND failed");
        acc.ok("backup-1", 5);
        acc.fail("backup-2", "not found");
        let result = acc.finish();
        assert_eq!(result.removed, vec!["device-a", "backup-1"]);
        assert_eq!(result.freed_bytes, 15);
        assert_eq!(result.failed.len(), 2);
        assert_eq!(result.failed[0].id, "device-b");
        assert_eq!(result.failed[0].error, "PROPFIND failed");
        assert_eq!(result.failed[1].id, "backup-2");
    }

    #[test]
    fn empty_apply_returns_empty_result() {
        let result = ApplyAccumulator::default().finish();
        assert!(result.removed.is_empty());
        assert_eq!(result.freed_bytes, 0);
        assert!(result.failed.is_empty());
    }
}
