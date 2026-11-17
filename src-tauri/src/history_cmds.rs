//! 同步历史：把 `<状态目录>/history.jsonl` 里的记录读出来给前端看。
//!
//! 只读：这个命令一个字节都不写。它读的是 core 写下的那一份历史
//! （每次 transport/apply 追加一条），桌面端以前只能看实时日志，
//! 同步完发生了什么、失败在哪、用了多久都留不住 —— 现在补上这块。
//!
//! 状态目录与 CLI 同一处（`~/.config/rime-sync/history.jsonl`，
//! Windows `%APPDATA%\rime-sync\history.jsonl`），所以两端看到的记录一致。

use serde::Serialize;
use tauri::AppHandle;

use rime_sync_core::history::{self, HistoryEntry, HISTORY_LIMIT};

use crate::state_dir;

/// 默认显示多少条（最近 50 条，够看又不刷屏）。
pub const DEFAULT_LIMIT: usize = 50;

/// 给前端的一条历史记录。
///
/// 与 core 的 `HistoryEntry` 字段一致，但 `Option` 一律序列化成 `null`
/// （core 那边对 `None` 直接省略字段，前端拿到的是 `undefined`）。
#[derive(Serialize)]
pub struct HistoryView {
    /// 同步发生的时间（Unix 秒）。
    pub ts: u64,
    /// transport（只搬运）/ apply（合并生效）。
    pub mode: String,
    pub ok: bool,
    pub pulled: usize,
    pub pushed: usize,
    pub merged: usize,
    pub conflicts: usize,
    pub duration_ms: u64,
    pub backup_id: Option<String>,
    pub error: Option<String>,
}

impl From<HistoryEntry> for HistoryView {
    fn from(entry: HistoryEntry) -> Self {
        Self {
            ts: entry.ts,
            mode: entry.mode,
            ok: entry.ok,
            pulled: entry.pulled,
            pushed: entry.pushed,
            merged: entry.merged,
            conflicts: entry.conflicts,
            duration_ms: entry.duration_ms,
            backup_id: entry.backup_id,
            error: entry.error,
        }
    }
}

/// 读最近的同步历史（时间正序，最后一条是最近一次）。
///
/// `limit` 缺省 50 条，最多 500 条（core 那边本来就只留 500 条）。
#[tauri::command]
pub async fn cmd_list_history(
    app: AppHandle,
    limit: Option<usize>,
) -> Result<Vec<HistoryView>, String> {
    let path = state_dir(&app)?.join("history.jsonl");
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, HISTORY_LIMIT);
    // 读盘放到阻塞线程池：别占着 IPC 线程。
    let entries = tauri::async_runtime::spawn_blocking(move || history::read(&path, limit))
        .await
        .map_err(|error| format!("读取同步历史时后台任务异常结束：{error}"))?
        .map_err(|error| format!("读取同步历史失败：{error:#}"))?;
    Ok(entries.into_iter().map(HistoryView::from).collect())
}
