//! 维护 / 去重：远端设备目录、远端快照、本地备份三段预览与执行。
//!
//! 分组规则在 rime_sync_core::dedupe（纯函数 + 单测）；这里负责 WebDAV 扫描、
//! 本地备份指纹生成，以及把失败按条聚合后返回给 UI。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use rime_sync_core::backup::{now_secs, BackupManifest};
use rime_sync_core::config::Config;
use rime_sync_core::dedupe::{self, BackupFingerprint, DedupeGroup, DeviceDirEntry, SnapshotFile};
use rime_sync_core::index::sha256_hex;
use rime_sync_core::plan;
use rime_sync_core::profile::{self, Profile};
use rime_sync_core::rime_dir;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::remote_tree;
use crate::{backup_manager, dav_client, ensure_config, push_log, AppState};

/// 与备份一致：sync/ 树最多扫描 6 层。
const SNAPSHOT_DEPTH: usize = 6;
const SNAPSHOT_MAX_FILES: usize = 512;

/// 本机目录名：installation_id 优先（缺 installation.yaml 才回退 device_id）。
/// 回退时返回 warning，避免 UI 静默按错误标识判定「本机」。
pub(crate) fn resolve_self_id(config: &Config) -> (String, Option<String>) {
    let detection = crate::detect_rime(config).ok();
    resolve_self_id_at(
        detection.as_ref().map(|detection| detection.path.as_path()),
        config,
    )
}

/// 纯函数版本（单测直接喂 rime 目录）：None 表示 rime 目录不可用。
pub(crate) fn resolve_self_id_at(rime: Option<&Path>, config: &Config) -> (String, Option<String>) {
    let Some(rime) = rime else {
        return (
            config.device_id.clone(),
            Some("未能定位 Rime 用户目录，去重按 device_id 判定本机目录".to_string()),
        );
    };
    let id = rime_dir::local_sync_id(rime, &config.device_id);
    if id == config.device_id && rime_dir::installation_id(rime).is_none() {
        return (
            id,
            Some(format!(
                "installation.yaml 缺少 installation_id，去重按 device_id（{}）判定本机目录",
                config.device_id
            )),
        );
    }
    (id, None)
}

/// 单个设备目录的删除闸门（纯函数）：名字合法 + 不得指向本机目录。
pub(crate) fn check_device_delete(id: &str, self_id: &str) -> Result<(), String> {
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
    dedupe::validate_remote_delete_path(&format!("sync/{id}"), self_id)
        .map_err(|error| format!("{error:#}"))
}

/// 同一内容的远端副本不得一次删光：至少留一份（含本机目录下的快照）。
pub(crate) fn ensure_one_copy_left(total: usize, chosen: usize) -> Result<(), String> {
    if total > 0 && chosen >= total {
        return Err("不能删除同一内容的全部副本".to_string());
    }
    Ok(())
}

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
    let (self_id, self_warning) = resolve_self_id(&config);
    let self_warnings: Vec<String> = self_warning.into_iter().collect();
    let (devices, snapshots) = match dav_client(&app, &state.0) {
        // 本机目录名与传输一致：installation_id（不是 device_id 标签）。
        Ok((_, client)) => {
            let mut devices = preview_devices(&client, &self_id);
            let mut snapshots = preview_snapshots(&client);
            devices.warnings.extend(self_warnings.iter().cloned());
            snapshots.warnings.extend(self_warnings);
            (devices, snapshots)
        }
        Err(error) => (
            DedupeSection::error(error.clone()),
            DedupeSection::error(error),
        ),
    };
    let backups = preview_backups(&app, &state.0)?;
    Ok(DedupePreview {
        self_id,
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

fn scan_dir_ids(client: &DavClient, root: &str) -> Result<Vec<String>, String> {
    let entries = client
        .propfind(root, 1)
        .map_err(|error| format!("读取远端 {root} 失败：{error:#}"))?;
    let mut dirs = Vec::new();
    for entry in entries {
        if !entry.is_dir {
            continue;
        }
        let id = entry.path.rsplit('/').next().unwrap_or("");
        if id.is_empty() || id == root || id == "shared" || id.starts_with('.') {
            continue;
        }
        if entry.path != format!("{root}/{id}") {
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

fn scan_sync_dirs(client: &DavClient) -> Result<Vec<String>, String> {
    scan_dir_ids(client, "sync")
}

fn preview_devices(client: &DavClient, self_id: &str) -> DedupeSection {
    // 新布局的设备足迹只有 sync/<id>（配置在 sync/<id>/config 里）；0.6.4 开发期间
    // 的旧布局 profiles/<id> 若还没迁移，也算同一台设备的占用，一并扫描、一并计费。
    let mut dirs = match scan_sync_dirs(client) {
        Ok(dirs) => dirs,
        Err(error) => return DedupeSection::error(error),
    };
    match scan_dir_ids(client, profile::LEGACY_PROFILES_DIR) {
        Ok(legacy) => dirs.extend(legacy),
        Err(error) => return DedupeSection::error(error),
    }
    dirs.retain(|id| id != self_id);
    dirs.sort();
    dirs.dedup();
    let mut warnings = Vec::new();
    let mut entries = Vec::new();
    for id in dirs {
        let Ok(device) = Profile::new(&id) else {
            continue;
        };
        let rel = device.remote_sync();
        let legacy_rel = device.legacy_remote_root();
        let mut size = 0u64;
        // sync/<id> 已含 config/；旧布局 profiles/<id> 若还在，一并计费。
        for tree in [&rel, &legacy_rel] {
            match remote_tree::walk(
                client,
                tree,
                remote_tree::MAX_DEPTH,
                remote_tree::MAX_ENTRIES,
            ) {
                Ok(found) => size += remote_tree::tree_size(&found),
                Err(error) => warnings.push(format!("{tree}: {error:#}")),
            }
        }
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
    let groups = dedupe::group_device_dirs(&entries, self_id)
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
    let (self_id, warning) = resolve_self_id(&config);
    if let Some(warning) = warning {
        push_log(inner, warning);
    }
    let mut acc = ApplyAccumulator::default();
    let mut seen = HashSet::new();
    for id in ids {
        let id = id.trim();
        if !seen.insert(id.to_string()) {
            continue;
        }
        let result: Result<u64, String> = (|| {
            // 闸门按 installation_id 认本机目录；旧 device_id 目录可以删。
            check_device_delete(id, &self_id)?;
            let Ok(device) = Profile::new(id) else {
                return Err("设备 ID 无效".to_string());
            };
            let rel = device.remote_sync();
            let mut freed = remote_tree::delete_tree(&client, &rel)
                .map_err(|error| format!("{error:#}"))?
                .freed_bytes;
            // 0.6.4：配置在 sync/<id>/config（已随设备目录删除），再显式删一次兜底。
            let profile_rel = device.remote_root();
            if dedupe::validate_profile_delete_path(&profile_rel, &self_id).is_ok()
                && !client
                    .propfind(&profile_rel, 0)
                    .map_err(|error| format!("{error:#}"))?
                    .is_empty()
            {
                freed += remote_tree::delete_tree(&client, &profile_rel)
                    .map_err(|error| format!("{error:#}"))?
                    .freed_bytes;
            }
            // 0.6.4 旧布局 profiles/<id>（迁移失败残留）也删掉，与预览计费口径一致。
            let legacy_rel = device.legacy_remote_root();
            if dedupe::validate_remote_delete_path(&legacy_rel, &self_id).is_ok()
                && !client
                    .propfind(&legacy_rel, 0)
                    .map_err(|error| format!("{error:#}"))?
                    .is_empty()
            {
                freed += remote_tree::delete_tree(&client, &legacy_rel)
                    .map_err(|error| format!("{error:#}"))?
                    .freed_bytes;
            }
            Ok(freed)
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
    let (self_id, warning) = resolve_self_id(&config);
    if let Some(warning) = warning {
        push_log(inner, warning);
    }
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
            // 同一内容至少留一份（本机目录下的快照同样受保护）。
            ensure_one_copy_left(total, chosen)?;
            dedupe::validate_snapshot_delete_path(id, &self_id)
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

    fn config_with(device_id: &str) -> Config {
        Config {
            device_id: device_id.to_string(),
            ..Config::default()
        }
    }

    fn temp_rime(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-dedupe-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn self_id_prefers_installation_id_over_device_label() {
        let dir = temp_rime("self-id");
        fs::write(
            dir.join("installation.yaml"),
            "installation_id: \"e6169df8-uuid\"\n",
        )
        .expect("write");
        let (self_id, warning) = resolve_self_id_at(Some(&dir), &config_with("5095"));
        assert_eq!(
            self_id, "e6169df8-uuid",
            "去重必须按 installation_id 认本机目录"
        );
        assert!(warning.is_none(), "读得到 installation_id 时不应告警");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn self_id_falls_back_to_device_label_with_warning() {
        let (self_id, warning) = resolve_self_id_at(None, &config_with("5095"));
        assert_eq!(self_id, "5095");
        assert!(warning.expect("warning").contains("device_id"));

        let dir = temp_rime("fallback");
        let (self_id, warning) = resolve_self_id_at(Some(&dir), &config_with("5095"));
        assert_eq!(self_id, "5095");
        let warning = warning.expect("warning");
        assert!(warning.contains("installation_id"), "{warning}");
        fs::remove_dir_all(&dir).ok();
    }

    /// 模拟 apply_devices 对单个 id 的处理：闸门失败必须计入 failed。
    fn apply_one_device(acc: &mut ApplyAccumulator, id: &str, self_id: &str) {
        match check_device_delete(id, self_id) {
            Ok(()) => acc.ok(id, 0),
            Err(error) => acc.fail(id, error),
        }
    }

    #[test]
    fn self_installation_id_dir_is_refused_and_counted_as_failed() {
        let self_id = "e6169df8-uuid";
        let mut acc = ApplyAccumulator::default();
        apply_one_device(&mut acc, self_id, self_id);
        apply_one_device(&mut acc, "5095", self_id);
        let result = acc.finish();
        assert_eq!(
            result.removed,
            vec!["5095"],
            "旧 device_id 目录是普通设备，可以删除"
        );
        assert_eq!(result.failed.len(), 1, "删除本机目录必须失败而不是静默跳过");
        assert_eq!(result.failed[0].id, self_id);
        assert!(
            result.failed[0]
                .error
                .contains("refusing to delete the local device directory"),
            "{}",
            result.failed[0].error
        );
    }

    #[test]
    fn snapshot_guard_keeps_at_least_one_copy_including_own_dir() {
        assert!(ensure_one_copy_left(0, 0).is_ok(), "没有副本时不必拦截");
        assert!(ensure_one_copy_left(3, 2).is_ok());
        let error = ensure_one_copy_left(3, 3).expect_err("must keep one copy");
        assert!(error.contains("全部副本"), "{error}");

        // 本机 sync/<installation_id>/ 下的快照：路径闸门允许删除单个文件，
        // 但同一内容被全选时仍必须至少保留一份。
        let self_id = "e6169df8-uuid";
        let own = format!("sync/{self_id}/main.userdb.txt");
        assert!(dedupe::validate_snapshot_delete_path(&own, self_id).is_ok());
        assert!(
            ensure_one_copy_left(2, 2).is_err(),
            "本机目录的快照也不能删光"
        );
        assert!(ensure_one_copy_left(2, 1).is_ok());
    }
}
