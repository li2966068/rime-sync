//! 冲突解决时的文件操作：覆盖前先备份、删冲突副本前先把它存进备份。
//!
//! 口径与 core 引擎的 `write_local_guarded` 一致：
//! * 覆盖用户文件之前，旧内容必须先进备份会话；备份失败就不许覆盖。
//! * 冲突副本不许无声地永久删除：先把副本内容存进同一轮备份，再删本地那份。
//!   用户可以在「备份」页回滚找回副本内容（回滚会把副本放回原路径）。

use std::fs;
use std::path::Path;

use rime_sync_core::backup::BackupManager;

/// 一次冲突处理的结果。
#[derive(Debug, Clone)]
pub struct ResolutionOutcome {
    /// 本轮留下的备份 id（可在 App 的备份页回滚）。
    pub backup_id: String,
    /// 被存进备份的文件（相对 rime 根目录）。
    pub backed_up: Vec<String>,
    /// 冲突副本是否已从 rime 目录删除（内容已在备份里）。
    pub conflict_archived: bool,
}

/// 冲突的处理方式。
#[derive(Debug, Clone)]
pub enum Resolution {
    /// 保留本地原版，删掉冲突副本。
    KeepLocal,
    /// 用冲突副本覆盖本地原版，然后删掉副本。
    UseConflict(Vec<u8>),
    /// 用远端内容覆盖本地原版，然后删掉副本。
    UseRemote(Vec<u8>),
}

/// 冲突处理的文件流程：备份 → 覆盖 → 删副本。
///
/// * `target_rel` / `conflict_rel` 都是相对 rime 根目录的路径（调用方已校验）。
/// * 任何一步失败都会提前返回，**不会**留下「覆盖了但没备份」的中间状态。
pub fn resolve_conflict_files(
    rime_dir: &Path,
    backup_root: &Path,
    target_rel: &str,
    conflict_rel: &str,
    resolution: &Resolution,
) -> Result<ResolutionOutcome, String> {
    let manager = BackupManager::new(rime_dir, backup_root);
    let mut session = manager
        .start_session("conflict-resolve")
        .map_err(|error| format!("创建备份失败：{error:#}。本地文件未改动。"))?;
    let mut backed_up = Vec::new();

    // ① 覆盖前先把旧内容存下来（文件不存在 ⇒ 没有旧内容要留）。
    let target_path = rime_dir.join(target_rel);
    if path_exists(&target_path) {
        session
            .snapshot(target_rel)
            .map_err(|error| format!("备份 {} 失败：{error:#}。本地文件未改动。", target_rel))?;
        backed_up.push(target_rel.to_string());
    }

    // ② 删冲突副本之前先把副本内容存进同一轮备份：副本不许被无声删掉。
    let conflict_path = rime_dir.join(conflict_rel);
    session.snapshot(conflict_rel).map_err(|error| {
        format!(
            "备份冲突副本 {} 失败：{error:#}。冲突副本保留。",
            conflict_rel
        )
    })?;
    backed_up.push(conflict_rel.to_string());

    // ③ 覆盖（备份已完成，失败也不丢旧内容）。
    let payload = match resolution {
        Resolution::KeepLocal => None,
        Resolution::UseConflict(bytes) | Resolution::UseRemote(bytes) => Some(bytes),
    };
    if let Some(bytes) = payload {
        write_local(&target_path, bytes)?;
    }

    // ④ 副本内容已经在备份里，可以安全删除本地这份。
    fs::remove_file(&conflict_path)
        .map_err(|error| format!("删除冲突副本失败：{error}。副本内容已在备份里。"))?;

    // ⑤ 收尾：确保 manifest 落盘（幂等）。
    let backup_id = session.id().to_string();
    session
        .finalize()
        .map_err(|error| format!("写备份清单失败：{error:#}"))?;

    Ok(ResolutionOutcome {
        backup_id,
        backed_up,
        conflict_archived: true,
    })
}

/// 文件是否存在（读不出来时也算存在：宁可多备份一次，也不要静默覆盖）。
fn path_exists(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// 先写同目录临时文件再 rename，避免写一半破坏原文件。
pub fn write_local(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
