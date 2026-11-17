//! 回归：CLI 的状态目录/备份目录必须与桌面端落在**同一处**。
//!
//! 缺陷：桌面端把 index.json（基线）与 backups 放在应用配置目录
//! （`~/.config/com.rimesync.app`），CLI 放在 `~/.config/rime-sync` ——
//! 两端各存一份基线 ⇒ 同一台机器上「桌面端刚改过、CLI 又改回来」的假冲突，
//! 还原点也各有一批。
//!
//! 现在：CLI 的默认状态目录必须是 `~/.config/rime-sync`
//! （Windows `%APPDATA%\rime-sync`），与桌面端一致。
//! 显式 `--config` 时状态目录仍跟着配置文件走（E2E/多实例要隔离，不能动）。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-cli-shared-{tag}-{}-{}",
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

/// 跑一次 CLI，环境清干净（只有 HOME），模拟一台刚装好的机器。
fn run(home: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"))
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA")
        .env_remove("RIME_SYNC_CONFIG")
        .env("RIMESYNC_PREBUILT", "0")
        .output()
        .expect("运行 rime-sync-cli")
}

/// 默认配置下，CLI 的备份根必须落在共享状态目录里。
///
/// 验证手段：仍然存在的 `backup` 命令会在普通输出里打印
/// `path=<备份根>/<id>`（`--json` 模式则是 JSON 的 `path` 字段），
/// 从这条路径就能看出备份根落在哪儿。
#[test]
fn cli_backups_live_in_the_shared_state_dir() {
    let home = temp_dir("backups");
    let rime = home.join("rime");
    fs::create_dir_all(&rime).expect("建 rime 目录");

    let output = run(
        &home,
        &["backup", "--rime-dir", rime.to_str().expect("路径")],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = home
        .join(".config")
        .join("rime-sync")
        .join("backups")
        .display()
        .to_string();
    assert!(
        stdout.contains(&expected),
        "CLI 的备份根应是 {expected}，实际输出：{stdout}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// 显式 `--config` 时状态目录仍跟着配置文件走：E2E 与多实例要隔离，
/// 不能因为「统一到一处」就把它们挤到同一个基线上。
#[test]
fn explicit_config_keeps_its_own_state_dir() {
    let home = temp_dir("explicit");
    let rime = home.join("rime");
    let config = home.join("isolated").join("config.json");
    fs::create_dir_all(&rime).expect("建 rime 目录");
    fs::create_dir_all(config.parent().expect("父目录")).expect("建配置目录");
    fs::write(&config, "{}").expect("写配置");

    let output = Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"))
        .arg("--config")
        .arg(&config)
        .args(["backup", "--rime-dir"])
        .arg(&rime)
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA")
        .env_remove("RIME_SYNC_CONFIG")
        .env("RIMESYNC_PREBUILT", "0")
        .output()
        .expect("运行 rime-sync-cli");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = config
        .parent()
        .expect("父目录")
        .join("backups")
        .display()
        .to_string();
    assert!(
        stdout.contains(&expected),
        "显式 --config 时备份根应跟着配置文件走（{expected}），实际输出：{stdout}"
    );

    let _ = fs::remove_dir_all(&home);
}
