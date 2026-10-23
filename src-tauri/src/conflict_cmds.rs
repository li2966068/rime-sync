//! 冲突副本：扫描、读取两侧内容、一键取舍。
//!
//! 冲突文件名由 core 生成：原名 + .conflict-<device>-<unix秒>。
//! 这里不重新发明命名规则，目标文件由文件名去掉 .conflict-* 后缀得到。
//!
//! 三种处理方式：
//! - local：删除冲突副本，保留本地原版（内容不变）
//! - conflict：用冲突副本覆盖原版后删除副本（副本就是本地拷贝，内容等价于保留本地）
//! - remote：从 WebDAV 取 shared/files/<rel> 覆盖本地，并把 index.json 的
//!   基线更新为远端 meta；网络失败时不改动任何本地文件。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rime_sync_core::index::{FileIndex, FileMeta};
use rime_sync_core::plan;
use rime_sync_core::webdav::DavClient;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::{dav_client, push_log, rime_dir_for, state_dir, AppState};

/// 超过该大小或不是 UTF-8 的文件不回传内容（前端显示“无法预览”）。
const MAX_PREVIEW_BYTES: u64 = 512 * 1024;
const MAX_DEPTH: usize = 6;
/// 最近一次同步的冲突明细，供冲突页显示键级信息（state_dir/last-conflicts.json）。
const REPORT_FILE: &str = "last-conflicts.json";

#[derive(Serialize)]
pub struct ConflictView {
    /// 冲突副本的相对路径（前端处理时的唯一标识）。
    pub path: String,
    /// 原始目标文件的相对路径。
    pub target: String,
    /// 冲突副本的绝对路径。
    pub conflict_file: String,
    pub target_content: Option<String>,
    pub conflict_content: Option<String>,
    pub modified_at: u64,
    /// 本地原版大小（字节）。
    pub target_size: u64,
    /// 冲突副本大小（字节）。
    pub conflict_size: u64,
    /// 远端 shared/files/<target> 的大小；列表命令先给 None，由 cmd_conflict_remote_sizes 补齐。
    pub remote_size: Option<u64>,
    /// merge 报告里的键级冲突（如 patch.menu/page_size）。
    pub keys: Vec<String>,
    /// 非键级冲突原因（如 remote changed during upload）。
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct ResolveView {
    pub target: String,
    pub choice: String,
    pub removed: bool,
    /// 本次写入的字节数（仅 remote）。
    pub bytes: Option<u64>,
}

/// 最近一轮同步的冲突明细（与 Report.conflicts 同形）。
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ConflictReport {
    #[serde(default)]
    pub ts: u64,
    #[serde(default)]
    pub conflicts: Vec<String>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 同步结束后把本轮冲突写入 state_dir：冲突页据此显示“哪个键冲突”。
pub(crate) fn record_conflicts(app: &AppHandle, conflicts: &[String]) {
    let Ok(dir) = state_dir(app) else {
        return;
    };
    let report = ConflictReport {
        ts: now_secs(),
        conflicts: conflicts.to_vec(),
    };
    let Ok(text) = serde_json::to_string_pretty(&report) else {
        return;
    };
    let path = dir.join(REPORT_FILE);
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, format!("{text}\n")).is_err() || fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

fn load_conflict_report(app: &AppHandle) -> ConflictReport {
    let Ok(dir) = state_dir(app) else {
        return ConflictReport::default();
    };
    fs::read_to_string(dir.join(REPORT_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 从 "<rel>: patch.a, patch.b" 形式的报告行里拆出键与其它说明。
fn keys_for(report: &ConflictReport, rel: &str) -> (Vec<String>, Option<String>) {
    let mut keys: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for entry in &report.conflicts {
        let (entry_rel, detail) = match entry.split_once(':') {
            Some((entry_rel, detail)) => (entry_rel.trim(), detail.trim()),
            None => (entry.trim(), ""),
        };
        if entry_rel != rel || detail.is_empty() {
            continue;
        }
        for item in detail.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            if item.starts_with("patch.") {
                if !keys.iter().any(|known| known == item) {
                    keys.push(item.to_string());
                }
            } else if !notes.iter().any(|known| known == item) {
                notes.push(item.to_string());
            }
        }
    }
    let note = if notes.is_empty() {
        None
    } else {
        Some(notes.join("; "))
    };
    (keys, note)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries =
        fs::read_dir(dir).map_err(|error| format!("读取目录 {} 失败：{error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取目录项失败：{error}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("读取文件类型失败：{error}"))?;
        if file_type.is_dir() {
            if name == "build"
                || name == ".git"
                || name.starts_with('.')
                || name.ends_with(".userdb")
            {
                continue;
            }
            walk(&entry.path(), depth + 1, out)?;
            continue;
        }
        if file_type.is_file() && plan::is_conflict_copy(&name) {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn modified_at(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn preview(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PREVIEW_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// 去掉 .conflict-... 后缀，返回原文件名；不是冲突副本则 None。
fn split_conflict_name(name: &str) -> Option<&str> {
    let (target, _) = name.rsplit_once(".conflict-")?;
    if target.is_empty() {
        return None;
    }
    Some(target)
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 冲突页用到的远端文件大小（一次 PROPFIND；单独命令，避免拖慢本地列表）。
#[derive(Serialize)]
pub struct RemoteConflictSizes {
    /// 是否成功列出远端 shared/files；false 时前端保持“远端未知”。
    pub reachable: bool,
    /// rel -> 字节数（服务器没给 size 的文件省略）。
    pub sizes: BTreeMap<String, u64>,
}

#[tauri::command]
pub async fn cmd_conflict_remote_sizes(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RemoteConflictSizes, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || remote_sizes(&app, &inner))
        .await
        .map_err(|error| format!("读取远端大小异常结束：{error}"))
}

fn remote_sizes(app: &AppHandle, inner: &crate::Inner) -> RemoteConflictSizes {
    let Ok((_, client)) = dav_client(app, inner) else {
        return RemoteConflictSizes {
            reachable: false,
            sizes: BTreeMap::new(),
        };
    };
    match client.propfind("shared/files", 1) {
        Ok(entries) => RemoteConflictSizes {
            reachable: true,
            sizes: entries
                .into_iter()
                .filter(|entry| !entry.is_dir)
                .filter_map(|entry| {
                    let rel = entry.path.strip_prefix("shared/files/")?.to_string();
                    entry.size.map(|size| (rel, size))
                })
                .collect(),
        },
        Err(_) => RemoteConflictSizes {
            reachable: false,
            sizes: BTreeMap::new(),
        },
    }
}

#[tauri::command]
pub async fn cmd_list_conflicts(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ConflictView>, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || list_conflicts(&app, &inner))
        .await
        .map_err(|error| format!("读取冲突列表异常结束：{error}"))?
}

fn list_conflicts(app: &AppHandle, inner: &crate::Inner) -> Result<Vec<ConflictView>, String> {
    let root = rime_dir_for(app, inner)?;
    let report = load_conflict_report(app);
    let mut files = Vec::new();
    walk(&root, 0, &mut files)?;
    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(target_name) = split_conflict_name(name) else {
            continue;
        };
        let Some(parent) = path.parent() else {
            continue;
        };
        let target = parent.join(target_name);
        let target_rel = rel_path(&root, &target);
        if plan::validate_rel_path(&target_rel).is_err() {
            continue;
        }
        let (keys, note) = keys_for(&report, &target_rel);
        out.push(ConflictView {
            path: rel_path(&root, &path),
            target: target_rel.clone(),
            conflict_file: path.display().to_string(),
            target_content: preview(&target),
            conflict_content: preview(&path),
            modified_at: modified_at(&path),
            target_size: file_size(&target),
            conflict_size: file_size(&path),
            // 远端大小由 cmd_conflict_remote_sizes 单独补（避免本地列表等网络）。
            remote_size: None,
            keys,
            note,
        });
    }
    out.sort_by_key(|entry| std::cmp::Reverse(entry.modified_at));
    Ok(out)
}

/// 把冲突副本路径拆成原版路径（两者都做路径校验）。
fn split_paths(root: &Path, target_path: &str) -> Result<(PathBuf, String, PathBuf), String> {
    let rel = target_path.trim().replace('\\', "/");
    plan::validate_rel_path(&rel).map_err(|error| format!("冲突路径无效：{error:#}"))?;
    let Some(name) = rel.rsplit('/').next() else {
        return Err("冲突路径无效".to_string());
    };
    let Some(target_name) = split_conflict_name(name) else {
        return Err("该文件不是冲突副本".to_string());
    };
    let parent = rel.rsplit_once('/').map(|(parent, _)| parent);
    let target_rel = match parent {
        Some(parent) => format!("{parent}/{target_name}"),
        None => target_name.to_string(),
    };
    plan::validate_rel_path(&target_rel).map_err(|error| format!("目标路径无效：{error:#}"))?;

    let conflict_abs = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let target_abs = root.join(target_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    let root_canonical = root
        .canonicalize()
        .map_err(|error| format!("无法解析 Rime 目录：{error}"))?;
    let conflict_canonical = conflict_abs
        .canonicalize()
        .map_err(|error| format!("冲突副本不存在或不可读：{error}"))?;
    if !conflict_canonical.starts_with(&root_canonical) {
        return Err("冲突副本不在 Rime 目录内，已拒绝处理".to_string());
    }
    Ok((conflict_canonical, target_rel, target_abs))
}

#[tauri::command]
pub async fn cmd_resolve_conflict(
    app: AppHandle,
    state: State<'_, AppState>,
    target_path: String,
    choice: String,
) -> Result<ResolveView, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || resolve(&app, &inner, &target_path, &choice))
        .await
        .map_err(|error| format!("处理冲突异常结束：{error}"))?
}

/// 采用本地原版（删冲突副本）、采用冲突副本（覆盖原版后删副本）或用远端覆盖本地。
fn resolve(
    app: &AppHandle,
    inner: &crate::Inner,
    target_path: &str,
    choice: &str,
) -> Result<ResolveView, String> {
    let root = rime_dir_for(app, inner)?;
    let (conflict_abs, target_rel, target_abs) = split_paths(&root, target_path)?;

    match choice.trim() {
        "local" => {}
        "conflict" => {
            let bytes =
                fs::read(&conflict_abs).map_err(|error| format!("读取冲突副本失败：{error}"))?;
            write_local(&target_abs, &bytes)?;
        }
        "remote" => {
            return resolve_with_remote(app, inner, &target_rel, &target_abs, &conflict_abs);
        }
        other => return Err(format!("未知的处理方式：{other}")),
    }

    fs::remove_file(&conflict_abs).map_err(|error| format!("删除冲突副本失败：{error}"))?;
    let verb = if choice.trim() == "conflict" {
        "采用冲突副本"
    } else {
        "保留本地版本"
    };
    push_log(inner, format!("{verb}：{target_rel}"));
    Ok(ResolveView {
        target: target_rel,
        choice: choice.trim().to_string(),
        removed: true,
        bytes: None,
    })
}

/// 从远端下载并覆盖本地：先下载、再写本地、最后更新 index.json 基线，
/// 任何一步失败都保留冲突副本；本地写入成功但基线更新失败时回滚本地文件。
fn resolve_with_remote(
    app: &AppHandle,
    inner: &crate::Inner,
    target_rel: &str,
    target_abs: &Path,
    conflict_abs: &Path,
) -> Result<ResolveView, String> {
    let (_, client) = dav_client(app, inner)?;
    let remote_path = format!("shared/files/{target_rel}");
    let bytes = client
        .get(&remote_path)
        .map_err(|error| format!("下载远端 {remote_path} 失败：{error:#}。本地文件未改动。"))?
        .ok_or_else(|| format!("远端不存在 {remote_path}，无法用远端覆盖本地。本地文件未改动。"))?;

    let meta = remote_meta(&client, &remote_path, &bytes);
    let previous = fs::read(target_abs).ok();
    if let Err(error) = write_local(target_abs, &bytes) {
        restore_local(target_abs, previous.as_deref());
        return Err(format!("{error}。本地文件未改动。"));
    }

    let index_path = state_dir(app)?.join("index.json");
    if let Err(error) = update_baseline(&index_path, target_rel, &meta) {
        restore_local(target_abs, previous.as_deref());
        return Err(format!("{error}。已恢复本地文件，冲突副本保留，可重试。"));
    }
    remember_base(app, &meta, &bytes);
    fs::remove_file(conflict_abs).map_err(|error| format!("删除冲突副本失败：{error}"))?;
    push_log(
        inner,
        format!(
            "用远端覆盖本地：{target_rel}（{} 字节，来自 {remote_path}）",
            bytes.len()
        ),
    );
    Ok(ResolveView {
        target: target_rel.to_string(),
        choice: "remote".to_string(),
        removed: true,
        bytes: Some(bytes.len() as u64),
    })
}

/// 远端内容的基线 meta：sha256/size 来自实际下载的字节，mtime/etag 尽量取 HEAD。
fn remote_meta(client: &DavClient, remote_path: &str, bytes: &[u8]) -> FileMeta {
    let mut meta = FileMeta::local(bytes);
    if let Ok(Some(entry)) = client.head(remote_path) {
        if let Some(last_modified) = entry.last_modified {
            meta.mtime = last_modified;
        }
        meta.etag = entry.etag;
    }
    meta
}

/// 基线更新：index.json 的 files[rel] 换成远端 meta。
fn update_baseline(index_path: &Path, rel: &str, meta: &FileMeta) -> Result<(), String> {
    let mut index = FileIndex::load(index_path)
        .map_err(|error| format!("读取同步基线 {} 失败：{error:#}", index_path.display()))?;
    update_baseline_index(&mut index, rel, meta);
    index
        .save(index_path)
        .map_err(|error| format!("写入同步基线 {} 失败：{error:#}", index_path.display()))
}

fn update_baseline_index(index: &mut FileIndex, rel: &str, meta: &FileMeta) {
    index.files.insert(rel.to_string(), meta.clone());
}

/// 与 core 的基线缓存同址：内容寻址，供后续 *.custom.yaml 段落级合并使用。
fn remember_base(app: &AppHandle, meta: &FileMeta, bytes: &[u8]) {
    let Ok(dir) = state_dir(app) else {
        return;
    };
    let base_dir = dir.join("base");
    if fs::create_dir_all(&base_dir).is_err() {
        return;
    }
    let path = base_dir.join(&meta.sha256);
    if path.is_file() {
        return;
    }
    let _ = fs::write(path, bytes);
}

/// 先写同目录临时文件再 rename，避免写一半破坏原文件。
fn write_local(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建目录 {} 失败：{error}", parent.display()))?;
    }
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "conflict".to_string());
    let tmp = path.with_file_name(format!(".{name}.rime-sync.tmp"));
    fs::write(&tmp, bytes).map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!("写入 {} 失败：{error}", path.display())
    })
}

fn restore_local(path: &Path, previous: Option<&[u8]>) {
    match previous {
        Some(bytes) => {
            let _ = write_local(path, bytes);
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_sync_core::index::sha256_hex;

    #[test]
    fn baseline_meta_matches_downloaded_bytes() {
        let bytes = b"patch: menu/page_size 7";
        let meta = FileMeta::local(bytes);
        assert_eq!(meta.sha256, sha256_hex(bytes));
        assert_eq!(meta.size, bytes.len() as u64);
    }

    #[test]
    fn update_baseline_index_replaces_existing_entry() {
        let mut index = FileIndex::default();
        index
            .files
            .insert("default.custom.yaml".into(), FileMeta::local(b"old"));
        index
            .files
            .insert("weasel.custom.yaml".into(), FileMeta::local(b"untouched"));
        let bytes = b"patch: style/color_scheme dark";
        let meta = FileMeta::local(bytes);

        update_baseline_index(&mut index, "default.custom.yaml", &meta);

        let entry = &index.files["default.custom.yaml"];
        assert_eq!(entry.sha256, sha256_hex(bytes));
        assert_eq!(entry.size, bytes.len() as u64);
        assert_eq!(
            index.files["weasel.custom.yaml"].sha256,
            sha256_hex(b"untouched")
        );
    }

    #[test]
    fn update_baseline_writes_index_file() {
        let dir = std::env::temp_dir().join(format!("rime-sync-baseline-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("index.json");
        update_baseline(
            &path,
            "default.custom.yaml",
            &FileMeta::local(b"remote-bytes"),
        )
        .expect("update");
        let loaded = FileIndex::load(&path).expect("load");
        assert_eq!(
            loaded.files["default.custom.yaml"].sha256,
            sha256_hex(b"remote-bytes")
        );

        update_baseline(&path, "weasel.custom.yaml", &FileMeta::local(b"second")).expect("update2");
        let loaded = FileIndex::load(&path).expect("load");
        assert_eq!(loaded.files.len(), 2);
        assert_eq!(loaded.files["weasel.custom.yaml"].size, 6);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn split_conflict_name_strips_device_suffix() {
        assert_eq!(
            split_conflict_name("default.custom.yaml.conflict-pc-a-1699999999"),
            Some("default.custom.yaml")
        );
        assert_eq!(split_conflict_name("default.custom.yaml"), None);
        assert_eq!(split_conflict_name(".conflict-x"), None);
    }

    #[test]
    fn keys_for_extracts_patch_keys_and_notes() {
        let report = ConflictReport {
            ts: 0,
            conflicts: vec![
                "default.custom.yaml: patch.menu/page_size, patch.style/color_scheme".into(),
                "weasel.custom.yaml: remote changed during upload".into(),
                "other.yaml: patch.unrelated".into(),
            ],
        };
        let (keys, note) = keys_for(&report, "default.custom.yaml");
        assert_eq!(
            keys,
            vec![
                "patch.menu/page_size".to_string(),
                "patch.style/color_scheme".to_string()
            ]
        );
        assert!(note.is_none());

        let (keys, note) = keys_for(&report, "weasel.custom.yaml");
        assert!(keys.is_empty());
        assert_eq!(note.as_deref(), Some("remote changed during upload"));

        let (keys, note) = keys_for(&report, "missing.yaml");
        assert!(keys.is_empty());
        assert!(note.is_none());
    }

    #[test]
    fn write_local_is_atomic_and_creates_parents() {
        let dir = std::env::temp_dir().join(format!("rime-sync-conflict-{}", std::process::id()));
        let path = dir.join("nested").join("a.yaml");
        write_local(&path, b"remote").expect("write");
        assert_eq!(fs::read(&path).expect("read"), b"remote");
        restore_local(&path, Some(b"local"));
        assert_eq!(fs::read(&path).expect("read"), b"local");
        fs::remove_dir_all(&dir).ok();
    }
}
