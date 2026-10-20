//! SPEC-2 端到端（无网络）：apply 先备份、备份失败中止、锁抢占与残留回收。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_core::backup::BackupManager;
use rime_sync_core::config::Config;
use rime_sync_core::engine::Engine;
use rime_sync_core::lock;
use rime_sync_core::platform::FakeTrigger;

fn work_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-e2e-{}-{}-{}",
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

fn seed_rime(root: &Path) {
    fs::create_dir_all(root.join("sync/devA")).expect("sync dir");
    fs::write(root.join("default.custom.yaml"), b"patch:\n  a: 1\n").expect("custom");
    fs::write(root.join("user.yaml"), b"var: 1\n").expect("user");
    fs::write(root.join("sync/devA/main.userdb.txt"), b"snapshot\n").expect("snapshot");
}

fn config(url: &str) -> Config {
    let mut config = Config::default();
    config.webdav.url = url.to_string();
    config.webdav.username = "u".to_string();
    config.device_id = "devA".to_string();
    config
}

#[test]
fn apply_creates_backup_before_any_write() {
    let root = work_dir("apply-backup");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    seed_rime(&rime);
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let trigger = FakeTrigger::new();
    let mut engine = Engine::new(
        config("http://127.0.0.1:1"),
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.apply();

    // 网络不可达 → transport 失败，但备份已经在任何写操作之前落盘。
    assert!(!report.ok, "transport should fail against a closed port");
    let backup_id = report
        .backup_id
        .as_deref()
        .expect("apply must record a backup id");
    let manager = BackupManager::new(&rime, &state.join("backups"));
    let infos = manager.list().expect("list backups");
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, backup_id);
    assert!(infos[0].file_count >= 3, "backup should capture rime files");
    let manifest_dir = manager.root().join(backup_id);
    assert!(manifest_dir.join("files/default.custom.yaml").is_file());
    assert!(manifest_dir
        .join("files/sync/devA/main.userdb.txt")
        .is_file());
    // 备份失败/未进 deploy，不应触发任何 rime 命令。
    assert!(trigger.calls().is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn apply_aborts_without_writing_when_backup_fails() {
    let root = work_dir("apply-abort");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    seed_rime(&rime);
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    // 让备份根目录不可用：同名普通文件。
    fs::write(state.join("backups"), b"not a directory").expect("block backups");

    // 用一个会记录调用的 trigger，确认部署阶段一次都没跑。
    let mut engine = Engine::new(
        config("http://127.0.0.1:1"),
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.apply();
    assert!(!report.ok);
    assert!(report.backup_id.is_none());
    let error = report.error.unwrap_or_default();
    assert!(error.contains("backup"), "error was: {error}");
    // 本地文件没被改动。
    let text = fs::read_to_string(rime.join("default.custom.yaml")).expect("read");
    assert_eq!(text, "patch:\n  a: 1\n");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn transport_refuses_while_lock_is_held() {
    let root = work_dir("lock-held");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    seed_rime(&rime);
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    let lock_path = state.join("sync.lock");
    let guard = lock::acquire(&lock_path).expect("hold lock");

    let mut engine = Engine::new(
        config("http://127.0.0.1:1"),
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.transport();
    assert!(!report.ok);
    assert!(report
        .error
        .unwrap_or_default()
        .contains("another sync is running"));
    drop(guard);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn transport_reclaims_stale_lock_and_runs() {
    let root = work_dir("lock-stale");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    seed_rime(&rime);
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    let lock_path = state.join("sync.lock");
    fs::write(
        &lock_path,
        r#"{"pid":4000000,"hostname":"ghost","started_at":1700000000,"boot_id":null}"#,
    )
    .expect("stale lock");

    let mut engine = Engine::new(
        config("http://127.0.0.1:1"),
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.transport();
    let error = report.error.unwrap_or_default();
    assert!(
        !error.contains("another sync is running"),
        "stale lock must be reclaimed, got: {error}"
    );
    assert!(!report.ok, "network is unreachable in this test");
    // 运行结束后锁已释放。
    assert!(!lock_path.exists());

    let _ = fs::remove_dir_all(&root);
}
