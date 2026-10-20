//! 冲突副本：扫描、读取两侧内容、一键取舍。
//!
//! 冲突文件名由 core 生成：\`<原名>.conflict-<device>-<unix秒>\`。
//! 这里不重新发明命名规则，目标文件由文件名去掉 \`.conflict-*\` 后缀得到。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rime_sync_core::plan;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{push_log, rime_dir_for, AppState};

/// 超过该大小或不是 UTF-8 的文件不回传内容（前端显示“无法预览”）。
const MAX_PREVIEW_BYTES: u64 = 512 * 1024;
const MAX_DEPTH: usize = 6;

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
}

#[derive(Serialize)]
pub struct ResolveView {
    pub target: String,
    pub choice: String,
    pub removed: bool,
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("读取目录 {} 失败：{error}", dir.display()))?;
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

fn preview(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PREVIEW_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// 去掉 \`.conflict-...\` 后缀，返回原文件名；不是冲突副本则 None。
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

#[tauri::command]
pub fn cmd_list_conflicts(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ConflictView>, String> {
    let root = rime_dir_for(&app, &state.0)?;
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
        out.push(ConflictView {
            path: rel_path(&root, &path),
            target: target_rel,
            conflict_file: path.display().to_string(),
            target_content: preview(&target),
            conflict_content: preview(&path),
            modified_at: modified_at(&path),
        });
    }
    out.sort_by(|left, right| right.modified_at.cmp(&left.modified_at));
    Ok(out)
}

/// 采用本地原版（删冲突副本）或采用冲突副本（覆盖原版后删除副本）。
#[tauri::command]
pub fn cmd_resolve_conflict(
    app: AppHandle,
    state: State<'_, AppState>,
    target_path: String,
    choice: String,
) -> Result<ResolveView, String> {
    let root = rime_dir_for(&app, &state.0)?;
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

    match choice.trim() {
        "local" => {}
        "conflict" => {
            let bytes = fs::read(&conflict_canonical)
                .map_err(|error| format!("读取冲突副本失败：{error}"))?;
            if let Some(parent) = target_abs.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("创建目录 {} 失败：{error}", parent.display()))?;
            }
            fs::write(&target_abs, &bytes)
                .map_err(|error| format!("写入 {} 失败：{error}", target_abs.display()))?;
        }
        other => return Err(format!("未知的处理方式：{other}")),
    }

    fs::remove_file(&conflict_canonical)
        .map_err(|error| format!("删除冲突副本失败：{error}"))?;
    let verb = if choice.trim() == "conflict" {
        "采用冲突副本"
    } else {
        "保留本地版本"
    };
    push_log(&state.0, format!("{verb}：{target_rel}"));
    Ok(ResolveView {
        target: target_rel,
        choice: choice.trim().to_string(),
        removed: true,
    })
}
