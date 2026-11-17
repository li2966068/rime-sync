//! 方案裁剪（prune）的「先预演、后确认」三个入口。
//!
//! 为什么要有这三个命令：桌面版首启自举以前会直接调用 core 的 prune_auto()，
//! 把不在保留名单里的 <id>.custom.yaml 删掉、重写 schema_list —— 用户既没
//! 确认过，也没法预演，「我正在用的方案凭空消失」就是这么来的。
//!
//! 现在拆成两次动作：
//! 1. cmd_prune_preview：只读地算出「将被删什么、将改成什么 schema_list、
//!    哪些方案会因此消失」，一个字节都不落盘；
//! 2. cmd_prune_confirm：用户看过清单并确认后，凭预演令牌执行。
//! 3. cmd_prune_cancel：用户反悔时作废这次预演（作废后即便再点确认也会被拒）。
//!
//! 令牌里编了这次预演的指纹：预演之后用户又改了配置时，旧令牌会被拒绝，
//! 不会出现「预演的是 A，删掉的是 B」。
//!
//! CLI 的 prune-schemas --dry-run 行为不变（CLI 仍是显式命令行操作）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rime_sync_core::prune;

/// 已被用户取消的预演令牌。
///
/// 为什么单独记一份：取消是「用户明确不要这次裁剪」，光是前端把清单藏起来
/// 不算数 —— 令牌还在手上时再点一次确认就又删了。把作废的令牌记下来，确认
/// 时一律拒绝。
static CANCELLED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// 预演方案裁剪：返回清单，不动磁盘。
#[tauri::command]
pub async fn cmd_prune_preview() -> Result<prune::PrunePreview, String> {
    // 扫目录要读盘，放到阻塞线程池：别占着 IPC 线程。
    tauri::async_runtime::spawn_blocking(prune::prune_preview_auto)
        .await
        .map_err(|error| format!("预演方案清理时后台任务异常结束：{error}"))
}

/// 凭预演令牌确认执行方案裁剪。
#[tauri::command]
pub async fn cmd_prune_confirm(token: String) -> Result<prune::PruneReport, String> {
    if is_cancelled(&token) {
        return Err("这次预演已被取消，请重新预演再确认".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || prune::prune_confirm(&token))
        .await
        .map_err(|error| format!("确认方案清理时后台任务异常结束：{error}"))?
}

/// 取消这次预演（作废令牌）：不碰磁盘，也不删任何东西。
#[tauri::command]
pub fn cmd_prune_cancel(token: String) -> Result<(), String> {
    cancel_token(&token)
}

/// 预演（显式指定目录）：给集成测试与排错用。
pub fn preview_at(data_dirs: Option<&[PathBuf]>, user_dir: Option<&Path>) -> prune::PrunePreview {
    prune::prune_preview_with(data_dirs, user_dir)
}

/// 确认执行（显式指定目录）：给集成测试与排错用。
pub fn confirm_at(
    token: &str,
    data_dirs: Option<&[PathBuf]>,
    user_dir: Option<&Path>,
) -> Result<prune::PruneReport, String> {
    if is_cancelled(token) {
        return Err("这次预演已被取消，请重新预演再确认".to_string());
    }
    prune::prune_confirm_with(token, data_dirs, user_dir)
}

/// 作废一个预演令牌。
pub fn cancel_token(token: &str) -> Result<(), String> {
    let mut guard = CANCELLED
        .lock()
        .map_err(|_| "取消预演失败：内部状态锁不可用".to_string())?;
    guard
        .get_or_insert_with(HashSet::new)
        .insert(token.to_string());
    Ok(())
}

/// 这个令牌是不是已经被用户取消了。
pub fn is_cancelled(token: &str) -> bool {
    match CANCELLED.lock() {
        Ok(guard) => guard
            .as_ref()
            .map(|set| set.contains(token))
            .unwrap_or(false),
        // 锁坏了就当成「没取消」：确认时 core 那边还有指纹校验兜底。
        Err(_) => false,
    }
}
