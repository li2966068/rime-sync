//! D1 回归：GUI 不许再静默改掉用户手填的 Rime 目录。
//!
//! 缺陷现象（改前）：
//! * `detect_rime` 在配置目录不存在时悄悄改用探测到的目录 ⇒ 用户不知道自己填错了；
//! * `cmd_get_config` 把 `rime_dir` 清空并写回磁盘 ⇒ 用户手填的路径被吃掉。
//!
//! 现在两端（CLI / GUI）共用同一份判据：目录不存在就是填错了 —— 明确报错，
//! 配置原样保留。

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::config::{self, Config};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-d1-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// 配置里的目录不存在 → 明确报错，且报错要可操作（说清是哪个路径、怎么修）。
#[test]
fn configured_missing_directory_is_an_error_not_a_silent_fallback() {
    let root = scratch("missing");
    let missing = root.join("no-such-rime-dir");

    let error = config::resolve_configured_rime_dir(Some(&missing.display().to_string()))
        .expect_err("目录不存在时必须报错，不能悄悄换成别的目录");
    let text = format!("{error:#}");

    assert!(
        text.contains(&missing.display().to_string()),
        "报错必须点名那个目录：{text}"
    );
    assert!(
        text.contains("清空") || text.contains("检查"),
        "报错必须给出可操作的下一步：{text}"
    );
    fs::remove_dir_all(&root).ok();
}

/// 没填目录 ≠ 填错：留空是合法的（走自动探测）。
#[test]
fn empty_configuration_is_reported_as_missing_value() {
    let error = config::resolve_configured_rime_dir(None).expect_err("没填目录时不能凭空解析");
    let text = format!("{error:#}");
    assert!(text.contains("Rime"), "{text}");
    assert!(
        config::resolve_configured_rime_dir(Some("   ")).is_err(),
        "只有空白的值等同于没填"
    );
}

/// 软链目录：两端解析出来的路径必须一致（CLI / GUI 同口径）。
#[test]
fn symlinked_directory_resolves_to_the_same_path_on_both_sides() {
    let root = scratch("symlink");
    let real = root.join("real");
    fs::create_dir_all(&real).expect("mkdir real");
    let link = root.join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    #[cfg(windows)]
    {
        // Windows 上建软链要管理员权限，测不了就跳过（口径在 Unix 上验证）。
        fs::remove_dir_all(&root).ok();
        return;
    }

    let via_link = config::resolve_configured_rime_dir(Some(&link.display().to_string()))
        .expect("软链目录能解析");
    let via_real = config::resolve_configured_rime_dir(Some(&real.display().to_string()))
        .expect("真实目录能解析");

    assert_eq!(
        via_link, via_real,
        "软链与真实路径必须解析到同一个目录（CLI / GUI 显示同一个路径）"
    );
    fs::remove_dir_all(&root).ok();
}

/// GUI 读配置时**不许**改写 config.json：用户填的值必须原样留在盘上。
#[test]
fn reading_the_config_never_rewrites_the_users_value() {
    let root = scratch("no-rewrite");
    let path = root.join("config.json");
    let missing = root.join("gone").display().to_string();

    let config = Config {
        rime_dir: Some(missing.clone()),
        device_id: "device-d1".to_string(),
        ..Config::default()
    };
    config.save(&path).expect("save config");
    let before = fs::read(&path).expect("read config");

    // 读配置 + 目录体检（GUI 的 cmd_get_config 走的就是这两步）。
    let loaded = Config::load(&path).expect("load config");
    let problem =
        config::rime_dir_problem(loaded.rime_dir.as_deref()).expect("目录不存在时必须报出问题");
    assert!(problem.contains(&missing), "{problem}");

    let after = fs::read(&path).expect("read config again");
    assert_eq!(before, after, "读配置不许改写 config.json");
    assert_eq!(
        loaded.rime_dir.as_deref(),
        Some(missing.as_str()),
        "用户填的路径必须原样保留，不能被清空"
    );
    fs::remove_dir_all(&root).ok();
}

/// 目录存在时不该报问题（否则设置页会一直弹告警）。
#[test]
fn existing_directory_has_no_problem() {
    let root = scratch("ok");
    let dir = root.join("rime");
    fs::create_dir_all(&dir).expect("mkdir");

    assert!(config::rime_dir_problem(Some(&dir.display().to_string())).is_none());
    assert!(config::rime_dir_problem(None).is_none());
    assert!(
        config::resolve_configured_rime_dir(Some(&dir.display().to_string())).is_ok(),
        "存在的目录必须能解析"
    );
    fs::remove_dir_all(&root).ok();
}

/// 体检只看目录在不在，不看权限（权限问题由具体操作报）。
#[test]
fn problem_check_does_not_touch_the_filesystem() {
    let root = scratch("readonly");
    let dir = root.join("rime");
    fs::create_dir_all(&dir).expect("mkdir");
    let marker = dir.join("marker");
    fs::write(&marker, b"x").expect("write");

    let problem = config::rime_dir_problem(Some(&dir.display().to_string()));
    assert!(problem.is_none());
    assert!(Path::new(&marker).is_file(), "体检不许动用户的目录");
    fs::remove_dir_all(&root).ok();
}
