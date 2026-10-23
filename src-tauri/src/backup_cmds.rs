//! 备份区：列出 / 新建 / 回滚 / 删除 / 按配置清理。备份根目录与 core 引擎一致（state_dir/backups）。

use serde::Serialize;
use tauri::{AppHandle, State};

use rime_sync_core::config::clamp_backup_keep;

use crate::{backup_manager, ensure_config, push_log, AppState};

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

#[derive(Serialize)]
pub struct PruneView {
    /// 本次实际执行的保留份数（配置值夹到 1..=100 后）。
    pub keep: u32,
    /// 被删除的备份 id（与 list 顺序一致：新 → 旧）。
    pub removed: Vec<String>,
    /// 清理后剩余的备份数量。
    pub remaining: usize,
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

#[tauri::command]
pub fn cmd_delete_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let id = id.trim();
    let manager = backup_manager(&app, &state.0)?;
    manager
        .delete(id)
        .map_err(|error| format!("删除备份失败：{error:#}"))?;
    push_log(&state.0, format!("已删除备份 {id}"));
    Ok(())
}

/// 按当前配置的保留份数清理旧备份（与合并生效后的自动清理同一套规则）。
#[tauri::command]
pub fn cmd_prune_backups(app: AppHandle, state: State<'_, AppState>) -> Result<PruneView, String> {
    let config = ensure_config(&app, &state.0)?;
    let keep = clamp_backup_keep(config.backup_keep);
    let manager = backup_manager(&app, &state.0)?;
    let removed = manager
        .prune(keep as usize)
        .map_err(|error| format!("清理备份失败：{error:#}"))?;
    let remaining = manager
        .list()
        .map_err(|error| format!("读取备份列表失败：{error:#}"))?
        .len();
    push_log(
        &state.0,
        format!(
            "已清理 {} 份旧备份（保留 {keep} 份，剩余 {remaining} 份）",
            removed.len()
        ),
    );
    Ok(PruneView {
        keep,
        removed,
        remaining,
    })
}
