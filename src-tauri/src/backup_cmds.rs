//! 备份区：列出 / 新建 / 回滚。备份根目录与 core 引擎一致（state_dir/backups）。

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{backup_manager, push_log, AppState};

#[derive(Serialize)]
pub struct BackupView {
    pub id: String,
    pub created_at: u64,
    pub created_at_text: String,
    pub files: usize,
    pub bytes: u64,
    pub reason: String,
    pub is_pre_restore: bool,
}

#[derive(Serialize)]
pub struct CreatedView {
    pub id: String,
}

#[derive(Serialize)]
pub struct RestoreView {
    pub backup_id: String,
    pub pre_restore_id: Option<String>,
    pub restored: usize,
}

#[tauri::command]
pub fn cmd_list_backups(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<BackupView>, String> {
    let manager = backup_manager(&app, &state.0)?;
    let items = manager
        .list()
        .map_err(|error| format!("读取备份列表失败：{error:#}"))?;
    Ok(items
        .into_iter()
        .map(|item| BackupView {
            is_pre_restore: item.reason == "pre-restore",
            files: item.file_count,
            bytes: item.total_bytes,
            created_at_text: item.created_at_text,
            created_at: item.created_at,
            reason: item.reason,
            id: item.id,
        })
        .collect())
}

#[tauri::command]
pub fn cmd_create_backup(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<CreatedView, String> {
    let manager = backup_manager(&app, &state.0)?;
    let id = manager
        .create("manual")
        .map_err(|error| format!("创建备份失败：{error:#}"))?;
    push_log(&state.0, format!("已创建备份 {id}"));
    Ok(CreatedView { id })
}

#[tauri::command]
pub fn cmd_restore_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RestoreView, String> {
    let manager = backup_manager(&app, &state.0)?;
    let report = manager
        .restore(id.trim())
        .map_err(|error| format!("回滚失败：{error:#}"))?;
    push_log(
        &state.0,
        format!(
            "已回滚到备份 {}（写回 {} 个文件，pre-restore={}）",
            report.backup_id,
            report.restored,
            report.pre_restore_id.as_deref().unwrap_or("none")
        ),
    );
    Ok(RestoreView {
        backup_id: report.backup_id,
        pre_restore_id: report.pre_restore_id,
        restored: report.restored,
    })
}
