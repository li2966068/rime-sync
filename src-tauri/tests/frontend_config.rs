//! 回归（缺陷 D2）：交给前端的配置里不许有 `user:pass` 形式的地址，
//! 也不许有明文密码。
//!
//! 改前：`cmd_get_config` 把整份配置原样交给前端 —— 降级模式（密码后端不可用）
//! 时磁盘上的地址还带着 `https://alice:s3cr3t@host/dav`，于是**密码**就跟着
//! 地址进了界面（DOM、截屏、开发者工具），前端回传时又把它原样带回后端。
//!
//! 现在：前端只拿到剥掉凭据的地址 + 「密码已设置」这个事实，密码本身
//! 一律不外泄（后端另有 `cmd_secret_status` 告诉前端密码在不在）。
//!
//! `cmd_get_config` 用的就是这里被测的 `sanitize` / `clean_url`。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_app_lib::frontend_config::{clean_url, sanitize};
use rime_sync_core::config::Config;

const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-frontend-cfg-{tag}-{}-{}",
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

/// 磁盘上确实是「密码写在地址里」的老配置（降级模式下的磁盘形态）。
fn write_legacy_config(path: &PathBuf) {
    fs::write(
        path,
        r#"{
  "webdav": {"url": "https://alice:s3cr3t@example.invalid/dav", "username": "", "remote_dir": "rime-sync"},
  "password_ref": "none",
  "device_id": "devA"
}
"#,
    )
    .expect("write config");
}

/// 前端拿到的配置：没有 `user:pass` 形式的地址，也没有明文密码。
#[test]
fn the_frontend_config_never_carries_credentials() {
    let dir = temp_dir("no-credentials");
    let path = dir.join("config.json");
    write_legacy_config(&path);

    // 后端读配置 → 脱敏 → 交给前端（与 cmd_get_config 同一套处理）。
    let config = Config::load(&path).expect("load config");
    let safe = sanitize(config);

    assert_eq!(
        safe.webdav.url, "https://example.invalid/dav",
        "地址里的凭据必须被剥掉"
    );
    assert_eq!(
        safe.webdav.username, "alice",
        "用户名不是秘密：剥掉地址里的凭据后要补上，用户才知道连的是哪个账号"
    );
    assert!(safe.webdav.password.is_empty(), "密码字段不许是明文");

    // 真正过线的是 JSON：里面既不许有密码，也不许有带凭据的地址。
    let text = serde_json::to_string(&safe).expect("serialize");
    assert!(!text.contains("s3cr3t"), "JSON 里不许出现明文密码：{text}");
    assert!(
        !text.contains("alice:s3cr3t"),
        "JSON 里不许出现 user:pass 形式的地址：{text}"
    );
    assert!(
        !text.contains("alice@") && !text.contains("@example.invalid"),
        "JSON 里不许出现带凭据的地址：{text}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// 内存里带着密码的配置也一样要脱敏（保存接口的返回值走同一条路）。
#[test]
fn a_config_with_a_live_password_is_sanitized_too() {
    let mut config = Config::default();
    config.webdav.url = "https://alice:s3cr3t@example.invalid/dav".to_string();
    config.webdav.username = "alice".to_string();
    config.webdav.password = "s3cr3t".to_string();
    config.proxy.password = "proxy-secret".to_string();

    let safe = sanitize(config);
    assert_eq!(safe.webdav.url, "https://example.invalid/dav");
    assert!(safe.webdav.password.is_empty());
    assert!(safe.proxy.password.is_empty());

    let text = serde_json::to_string(&safe).expect("serialize");
    assert!(!text.contains("s3cr3t"), "JSON 里不许出现明文密码：{text}");
    assert!(
        !text.contains("proxy-secret"),
        "代理密码同样不许外泄：{text}"
    );
    let _ = fs::remove_dir_all(temp_dir("live-password"));
}

/// 地址脱敏的三种情形。
#[test]
fn urls_are_cleaned_without_breaking_the_normal_ones() {
    // 带凭据 ⇒ 剥掉。
    assert_eq!(
        clean_url("https://alice:s3cr3t@example.invalid/dav"),
        "https://example.invalid/dav"
    );
    // 端口、路径、查询串、片段原样保留。
    assert_eq!(
        clean_url("https://alice:s3cr3t@example.invalid:8443/dav/sub?x=1#frag"),
        "https://example.invalid:8443/dav/sub?x=1#frag"
    );
    // 本来就没有凭据 ⇒ 原样。
    assert_eq!(
        clean_url("https://example.invalid/dav"),
        "https://example.invalid/dav"
    );
    // 拆不出用户名的畸形写法 ⇒ 整段 userinfo 去掉（不给前端 `***@host`）。
    assert_eq!(
        clean_url("https://:s3cr3t@example.invalid/dav"),
        "https://example.invalid/dav"
    );
    // 不是 URL ⇒ 原样。
    assert_eq!(clean_url("not a url"), "not a url");
}
