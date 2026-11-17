//! 回归（缺陷 D2）：解决冲突时必须留下可回滚的备份，冲突副本不许被无声删除。
//!
//! 改前：解决冲突直接覆盖用户目录里的文件（不走 BackupManager），覆盖完就把
//! 冲突副本 `fs::remove_file` 永久删掉 —— 用户既没有还原点，副本内容也没了。
//!
//! 现在：覆盖前先把旧内容存进一轮备份会话；删副本之前先把副本内容存进同一轮
//! 备份，备份失败就不许覆盖。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_app_lib::conflict_files::{resolve_conflict_files, Resolution};
use rime_sync_core::backup::BackupManager;

const TARGET: &str = "default.custom.yaml";
const CONFLICT: &str = "default.custom.yaml.conflict-devA-1700000000";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-conflict-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// 造一个 rime 目录：原版 + 冲突副本。
fn scene(tag: &str) -> (PathBuf, PathBuf) {
    let base = temp_dir(tag);
    let rime = base.join("rime");
    fs::create_dir_all(&rime).expect("mkdir");
    fs::write(rime.join(TARGET), b"old").expect("write target");
    fs::write(rime.join(CONFLICT), b"conflict").expect("write conflict");
    (rime, base.join("backups"))
}

/// 解决冲突会留下可回滚的备份：旧内容与冲突副本都能找回来。
#[test]
fn resolving_conflict_leaves_rollback_backup() {
    let (rime, backups) = scene("rollback");

    let outcome = resolve_conflict_files(
        &rime,
        &backups,
        TARGET,
        CONFLICT,
        &Resolution::UseConflict(b"new".to_vec()),
    )
    .expect("resolve");

    // 覆盖生效。
    assert_eq!(fs::read(rime.join(TARGET)).expect("read"), b"new");
    // 副本从 rime 目录里消失了，但内容进了备份（不是无声删除）。
    assert!(!rime.join(CONFLICT).exists());
    assert!(outcome.backed_up.contains(&TARGET.to_string()));
    assert!(outcome.backed_up.contains(&CONFLICT.to_string()));

    // 备份可列出、可回滚。
    let manager = BackupManager::new(&rime, &backups);
    let list = manager.list().expect("list");
    assert_eq!(list.len(), 1, "应当正好留下一轮备份：{list:?}");
    assert_eq!(list[0].id, outcome.backup_id);
    assert!(list[0].file_count >= 2, "旧内容与副本都要在备份里");

    manager.restore(&outcome.backup_id).expect("restore");
    assert_eq!(fs::read(rime.join(TARGET)).expect("read"), b"old");
    assert_eq!(
        fs::read(rime.join(CONFLICT)).expect("read"),
        b"conflict",
        "冲突副本必须能从备份里找回来"
    );
}

/// 保留本地版本时也不许无声删除副本。
#[test]
fn keeping_local_still_archives_conflict_copy() {
    let (rime, backups) = scene("keep-local");

    let outcome = resolve_conflict_files(&rime, &backups, TARGET, CONFLICT, &Resolution::KeepLocal)
        .expect("resolve");

    assert_eq!(fs::read(rime.join(TARGET)).expect("read"), b"old");
    assert!(!rime.join(CONFLICT).exists());
    assert!(outcome.backed_up.contains(&CONFLICT.to_string()));

    let manager = BackupManager::new(&rime, &backups);
    manager.restore(&outcome.backup_id).expect("restore");
    assert_eq!(fs::read(rime.join(CONFLICT)).expect("read"), b"conflict");
}

/// 备份失败就不许覆盖用户文件。
#[test]
fn failing_backup_blocks_overwrite() {
    let (rime, backups) = scene("blocked");
    // 把备份根占成一个普通文件：备份会话建不出来。
    fs::write(&backups, b"not-a-dir").expect("write blocker");

    let error = resolve_conflict_files(
        &rime,
        &backups,
        TARGET,
        CONFLICT,
        &Resolution::UseConflict(b"new".to_vec()),
    )
    .expect_err("备份失败时必须拒绝覆盖");

    assert!(error.contains("备份"), "错误信息要说清是备份失败：{error}");
    assert_eq!(
        fs::read(rime.join(TARGET)).expect("read"),
        b"old",
        "备份失败时用户文件不许被覆盖"
    );
    assert!(rime.join(CONFLICT).exists(), "冲突副本也不许被删");
}
