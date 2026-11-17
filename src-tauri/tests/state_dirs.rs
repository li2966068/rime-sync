//! 回归：桌面端的状态目录/备份目录必须与 CLI 落在**同一处**，旧目录要一次性搬过来。
//!
//! 缺陷：桌面端原来把 index.json（基线）、retry.json、锁与 backups 放在**自己的应用
//! 配置目录**（Linux `~/.config/com.rimesync.app`、Windows `%APPDATA%\com.rimesync.app`），
//! 而 CLI 放在 `~/.config/rime-sync`。两端各存一份基线 ⇒ 同一台机器上「桌面端刚改过的
//! 文件」在 CLI 眼里还是旧的 ⇒ 合并时被当成冲突（假冲突），还原点也各有一批。
//!
//! 现在：两端都用同一个共享状态目录（`~/.config/rime-sync`，Windows
//! `%APPDATA%\rime-sync`），**配置文件本身的位置不动**（搬配置等于丢用户配置），
//! 旧目录里的状态与备份在启动时搬一次，搬不动也不影响应用启动。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_app_lib::paths;

/// 环境变量是进程级的，测试并行跑时会互相踩 ⇒ 串行化。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 受影响的环境变量（测完必须原样恢复）。
const ENV_KEYS: [&str; 4] = ["HOME", "XDG_CONFIG_HOME", "APPDATA", "RIME_SYNC_CONFIG"];

const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-state-dirs-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

/// 在「只有 HOME」的干净环境里跑一段逻辑，并把环境变量恢复回去。
fn with_home<T>(home: &Path, body: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let saved: Vec<(&str, Option<OsString>)> = ENV_KEYS
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    std::env::set_var("HOME", home);
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("APPDATA");
    std::env::remove_var("RIME_SYNC_CONFIG");
    let outcome = body();
    for (key, value) in saved {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    outcome
}

/// 桌面端与 CLI 的状态目录、备份目录必须是同一处（路径字符串断言）。
#[test]
fn desktop_and_cli_resolve_the_same_state_dir() {
    let home = temp_dir("same-dir");
    with_home(&home, || {
        let expected = home.join(".config").join("rime-sync");

        // 桌面端：src-tauri/src/paths.rs::shared_state_dir
        let desktop = paths::shared_state_dir().expect("桌面端状态目录");
        assert_eq!(
            desktop, expected,
            "桌面端状态目录应落在 ~/.config/rime-sync"
        );

        // CLI：状态目录 = 配置文件所在目录（crates/cli/src/main.rs::state_dir），
        // 默认配置文件由 core::config::default_config_path 给出。
        let cli_config = rime_sync_core::config::default_config_path();
        let cli_state = cli_config.parent().expect("CLI 状态目录").to_path_buf();
        assert_eq!(cli_config, expected.join("config.json"));
        assert_eq!(desktop, cli_state, "桌面端与 CLI 的状态目录必须同一处");

        // 备份根：两端都是 <状态目录>/backups（CLI: app_data/backups）。
        assert_eq!(
            paths::backup_root(&desktop),
            expected.join("backups"),
            "备份根必须落在同一个状态目录下"
        );
    });
    let _ = fs::remove_dir_all(&home);
}

/// 老数据要搬过去，而且只搬一次（重复启动不再搬）。
#[test]
fn legacy_state_and_backups_are_migrated_once() {
    let root = temp_dir("migrate");
    let legacy = root.join("com.rimesync.app");
    let shared = root.join("rime-sync");
    fs::create_dir_all(legacy.join("backups").join("20240101-000000")).expect("建旧目录");
    fs::write(legacy.join("index.json"), b"{\"version\":1,\"files\":{}}").expect("写基线");
    fs::write(legacy.join("retry.json"), b"{}").expect("写重试状态");
    fs::write(
        legacy
            .join("backups")
            .join("20240101-000000")
            .join("manifest.json"),
        b"{\"id\":\"20240101-000000\"}",
    )
    .expect("写备份清单");
    // 用户的配置留在原地：搬它等于丢用户配置。
    fs::write(legacy.join("config.json"), b"{\"webdav\":{}}").expect("写配置");

    let report = paths::migrate_legacy_state(&legacy, &shared);
    assert!(
        report.failures.is_empty(),
        "迁移不该失败：{:?}",
        report.failure_lines()
    );
    assert_eq!(report.moved.len(), 3, "应搬走基线、重试状态与备份");

    // 新位置有它们。
    assert!(shared.join("index.json").is_file(), "基线没搬过来");
    assert!(shared.join("retry.json").is_file(), "重试状态没搬过来");
    assert!(
        shared
            .join("backups")
            .join("20240101-000000")
            .join("manifest.json")
            .is_file(),
        "备份没搬过来"
    );

    // 旧位置不再有（搬走了，不是复制）。
    assert!(
        !legacy.join("index.json").exists(),
        "旧目录里不该还留着基线"
    );
    assert!(!legacy.join("retry.json").exists());
    assert!(!legacy.join("backups").exists(), "旧目录里不该还留着备份");

    // 配置文件没动。
    assert!(legacy.join("config.json").is_file(), "配置文件必须留在原地");

    // 重复迁移是空操作。
    let again = paths::migrate_legacy_state(&legacy, &shared);
    assert!(again.is_noop(), "重复迁移应当是空操作：{again:?}");

    let _ = fs::remove_dir_all(&root);
}

/// 新位置已经有东西时**不许覆盖**：那是另一端算好的基线。
#[test]
fn existing_state_in_the_new_location_is_never_overwritten() {
    let root = temp_dir("no-overwrite");
    let legacy = root.join("com.rimesync.app");
    let shared = root.join("rime-sync");
    fs::create_dir_all(&legacy).expect("建旧目录");
    fs::create_dir_all(&shared).expect("建新目录");
    fs::write(legacy.join("index.json"), b"old").expect("写旧基线");
    fs::write(shared.join("index.json"), b"new").expect("写新基线");

    let report = paths::migrate_legacy_state(&legacy, &shared);
    assert!(report.moved.is_empty(), "不该搬：新位置已有同名文件");
    assert_eq!(report.skipped, vec!["index.json".to_string()]);
    assert_eq!(
        fs::read(shared.join("index.json")).expect("读新基线"),
        b"new".to_vec(),
        "新位置的基线不许被旧文件覆盖"
    );
    assert!(
        legacy.join("index.json").is_file(),
        "跳过的旧文件应留在原地，用户想自己处理也不拦着"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 迁移失败（旧目录读不动）时应用照常启动，只是用新目录，并留下可读的日志。
#[test]
fn unreadable_legacy_dir_does_not_break_startup() {
    let root = temp_dir("unreadable");
    let legacy = root.join("com.rimesync.app");
    let shared = root.join("rime-sync");

    // 旧目录不是目录（读不动）：任何用户身份下都会失败。
    fs::create_dir_all(&root).expect("建根目录");
    fs::write(&legacy, b"not a directory").expect("写占位文件");

    let report = paths::migrate_legacy_state(&legacy, &shared);
    assert!(
        !report.failures.is_empty(),
        "读不动旧目录必须报告失败，而不是假装搬过了"
    );
    let lines = report.failure_lines();
    assert!(!lines.is_empty(), "失败必须有可读的原因");
    assert!(
        lines[0].contains("读取旧目录失败"),
        "失败原因要能看懂：{lines:?}"
    );

    // 应用照常启动：新目录照常解析、照常可用。
    let home = root.join("home");
    fs::create_dir_all(&home).expect("建 home");
    with_home(&home, || {
        let state = paths::shared_state_dir().expect("状态目录仍可解析");
        assert_eq!(state, home.join(".config").join("rime-sync"));
        fs::create_dir_all(&state).expect("新目录照常可用");
        assert!(state.is_dir());
    });

    let _ = fs::remove_dir_all(&root);
}

/// 旧目录没有权限读时（普通用户遇到权限问题）同样只降级、不中断。
#[test]
fn permission_denied_on_legacy_dir_degrades_gracefully() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_dir("denied");
    let legacy = root.join("com.rimesync.app");
    let shared = root.join("rime-sync");
    fs::create_dir_all(&legacy).expect("建旧目录");
    fs::write(legacy.join("index.json"), b"{}").expect("写基线");

    // root 能无视权限 —— 那种情况下这条用例没有意义，直接跳过。
    if fs::read_dir(&legacy).is_err() {
        let _ = fs::remove_dir_all(&root);
        return;
    }
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o000)).expect("改权限");
    let report = paths::migrate_legacy_state(&legacy, &shared);
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).expect("恢复权限");

    assert!(!report.failures.is_empty(), "读不动旧目录必须报告失败");
    assert!(legacy.join("index.json").is_file(), "迁移失败不许删旧数据");

    let _ = fs::remove_dir_all(&root);
}
