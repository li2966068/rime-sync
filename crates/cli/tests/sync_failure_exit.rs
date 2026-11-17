//! CLI 回归：命令失败必须以**非零退出码**收场，JSON 报告里的 ok 必须如实为 false。
//!
//! 改前（缺陷 1）：transport/apply 失败时引擎返回的是 `Report{ok:false}` 而不是 `Err`，
//! CLI 照常返回 Ok(()) ⇒ 进程退出码 0。脚本（cron、E2E）看到 0 就以为同步成功了。
//!
//! 改前（缺陷 6）：repair-customs / repair-schema-list 的 JSON 里 `ok` 硬编码为 true，
//! 于是同一份报告里同时出现 `failed` 非空与 `ok=true`，自相矛盾。
//!
//! 现在：
//! * 失败 ⇒ 退出码非 0；
//! * `--json` 模式下 stdout 里仍然**只有一份** JSON（不额外追加一行 {"ok":false,...}），
//!   且其中 `ok=false`、`error`/`failed` 如实反映失败。

use std::fs;
use std::net::TcpListener;
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
            "rime-sync-cli-fail-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("rime")).expect("create rime dir");
        fs::create_dir_all(root.join("state")).expect("create state dir");
        Self { root }
    }

    fn rime_dir(&self) -> PathBuf {
        self.root.join("rime")
    }

    fn config_path(&self) -> PathBuf {
        self.root.join("state/config.json")
    }

    /// 最小可用配置：地址留空（测试用 --dav-url 覆盖），密码不落盘。
    fn write_config(&self) {
        let rime_dir = self.rime_dir();
        let text = format!(
            "{{\n  \"webdav\": {{\"url\": \"\", \"username\": \"\", \"password\": \"\", \"remote_dir\": \"rime-sync\"}},\n  \"password_ref\": \"none\",\n  \"proxy\": {{\"mode\": \"disabled\"}},\n  \"rime_dir\": {},\n  \"device_id\": \"dev-fail\",\n  \"transport_interval_minutes\": 10,\n  \"sync_kinds\": {{\"userdict\": true, \"custom_yaml\": true, \"dict_sources\": true}},\n  \"exclude_globs\": [],\n  \"stop_fcitx5_during_apply\": true,\n  \"schema_version\": 2\n}}\n",
            serde_json::to_string(&rime_dir.to_string_lossy()).expect("json")
        );
        fs::write(self.config_path(), text).expect("write config");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
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

/// 借一个端口再立刻放开：连上去会被拒绝（连接不可达），正是「服务器连不上」的形态。
fn unreachable_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/dav")
}

/// stdout 必须能被当成**一份** JSON 解析（多打一行 JSON 会让脚本解析到错误的那份）。
fn parse_single_json(stdout: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stdout);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|error| panic!("stdout 必须是且仅是一份 JSON（{error}）：{text}"))
}

/// 缺陷 1：同步失败（连不上服务器）必须退出码非 0，JSON 里 ok=false。
#[test]
fn transport_failure_exits_non_zero_and_reports_ok_false() {
    let fixture = Fixture::new("transport");
    fixture.write_config();
    let url = unreachable_url();

    let output = fixture.run(&["--json", "--dav-url", url.as_str(), "transport"]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "同步失败必须非零退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "JSON 里的 ok 必须如实为 false：{value}"
    );
    let error = value
        .get("error")
        .and_then(|item| item.as_str())
        .unwrap_or_default();
    assert!(!error.is_empty(), "失败必须带上原因：{value}");
}

/// 缺陷 1 的另一半：apply 失败同样不得退出 0。
#[test]
fn apply_failure_exits_non_zero_and_reports_ok_false() {
    let fixture = Fixture::new("apply");
    fixture.write_config();
    let url = unreachable_url();

    let output = fixture.run(&["--json", "--dav-url", url.as_str(), "apply"]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "apply 失败必须非零退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "JSON 里的 ok 必须如实为 false：{value}"
    );
}

/// 缺陷 1：非 JSON 模式下失败信息必须写到 stderr（人看得到），而不是悄悄退出 0。
#[test]
fn transport_failure_prints_the_reason_to_stderr() {
    let fixture = Fixture::new("stderr");
    fixture.write_config();
    let url = unreachable_url();

    let output = fixture.run(&["--dav-url", url.as_str(), "transport"]);

    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error:"),
        "人读模式必须把失败原因写到 stderr：{stderr}"
    );
}

/// 缺陷 6：repair-schema-list 修补失败时 JSON 的 ok 必须是 false（之前硬编码 true）。
#[test]
fn repair_schema_list_failure_reports_ok_false() {
    let fixture = Fixture::new("repair-schema-list");
    // 非 UTF-8 的 default.custom.yaml：修补必然读失败。
    fs::write(
        fixture.rime_dir().join("default.custom.yaml"),
        [0xffu8, 0xfe, b'x'],
    )
    .expect("write broken custom yaml");
    let user_dir = fixture.rime_dir();
    let user_dir = user_dir.to_string_lossy().to_string();

    let output = fixture.run(&[
        "--json",
        "repair-schema-list",
        "--user-dir",
        user_dir.as_str(),
    ]);

    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "修补失败时 ok 必须是 false：{value}"
    );
    let failed = value
        .get("failed")
        .and_then(|item| item.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    assert!(failed > 0, "failed 必须记录失败项：{value}");
    assert_ne!(
        output.status.code(),
        Some(0),
        "修补失败必须非零退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 缺陷 6：repair-customs 修补失败时 JSON 的 ok 必须是 false（之前硬编码 true）。
///
/// 触发方式：被修补的 <schema>.custom.yaml 不是 UTF-8 —— 修补必然读失败。
#[test]
fn repair_customs_failure_reports_ok_false() {
    let fixture = Fixture::new("repair-customs");
    let rime_dir = fixture.rime_dir();
    fs::write(
        rime_dir.join("luna_pinyin_simp.custom.yaml"),
        [0xffu8, 0xfe, b'x'],
    )
    .expect("write broken custom yaml");

    let rime_dir_arg = rime_dir.to_string_lossy().to_string();
    let output = fixture.run(&[
        "--json",
        "--rime-dir",
        rime_dir_arg.as_str(),
        "repair-customs",
    ]);

    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "修补失败时 ok 必须是 false：{value}"
    );
    assert_ne!(
        output.status.code(),
        Some(0),
        "修补失败必须非零退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
