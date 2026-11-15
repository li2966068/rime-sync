//! CLI 侧的 D1 回归：配置里手填的 Rime 目录不存在时，CLI 必须**明确报错**。
//!
//! 改前：CLI 会用探测到的另一个目录接着跑（GUI 更是把用户填的值清空写回磁盘），
//! 用户以为在同步 A 机器、其实在同步 B 机器，而且完全不知情。
//!
//! 现在：CLI 与 GUI 共用 core::config::resolve_configured_rime_dir 的同一份判据。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-cli-d1-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create dir");
        Self { root }
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("state/config.json")
    }

    /// 写一份配置：rime_dir 指向一个**不存在**的目录。
    fn write_config_with_missing_dir(&self, missing: &str) {
        fs::create_dir_all(self.root.join("state")).expect("create state");
        let text = format!(
            "{{\n  \"webdav\": {{\"url\": \"\", \"username\": \"\", \"password\": \"\", \"remote_dir\": \"rime-sync\"}},\n  \"password_ref\": \"none\",\n  \"proxy\": {{\"mode\": \"disabled\"}},\n  \"rime_dir\": {missing:?},\n  \"device_id\": \"device-d1\",\n  \"transport_interval_minutes\": 10,\n  \"sync_kinds\": {{\"userdict\": true, \"custom_yaml\": true, \"dict_sources\": true}},\n  \"exclude_globs\": [],\n  \"stop_fcitx5_during_apply\": true,\n  \"schema_version\": 3\n}}\n"
        );
        fs::write(self.config_path(), text).expect("write config");
    }

    /// 不带 --rime-dir，让命令走配置里的路径。
    fn run_without_rime_dir(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"))
            .arg("--config")
            .arg(self.config_path())
            .args(args)
            .env("RIMESYNC_PREBUILT", "0")
            .output()
            .expect("run rime-sync-cli")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 配置里的目录不存在 → 报错、非零退出、且点名那个目录。
#[test]
fn cli_refuses_a_missing_configured_rime_dir() {
    let fixture = Fixture::new("missing");
    let missing = fixture.root.join("no-such-rime-dir");
    fixture.write_config_with_missing_dir(&missing.display().to_string());

    let output = fixture.run_without_rime_dir(&["check-ime", "--json"]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "目录填错时必须失败退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("no-such-rime-dir") || stderr.contains("no-such-rime-dir"),
        "报错必须点名用户填的目录：stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("Rime") || stderr.contains("Rime"),
        "报错要说清是 Rime 目录的问题：stdout={stdout} stderr={stderr}"
    );
}

/// 命令失败时不许悄悄改写配置（用户填的值原样留在盘上）。
#[test]
fn cli_does_not_rewrite_the_config_when_the_dir_is_missing() {
    let fixture = Fixture::new("no-rewrite");
    let missing = fixture.root.join("no-such-rime-dir");
    fixture.write_config_with_missing_dir(&missing.display().to_string());
    let before = fs::read(fixture.config_path()).expect("read config");

    let _ = fixture.run_without_rime_dir(&["check-ime", "--json"]);

    let after = fs::read(fixture.config_path()).expect("read config again");
    assert_eq!(before, after, "失败时不许改写 config.json");
    let text = String::from_utf8_lossy(&after);
    assert!(
        text.contains("no-such-rime-dir"),
        "用户填的路径必须原样保留：{text}"
    );
}
