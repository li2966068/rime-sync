//! CLI 回归：只读检查类命令在「检查没通过」时必须以**非零退出码**收场。
//!
//! 缺陷（D2/D3/D4）：
//! * `prune-schemas` 有文件没能删掉（多半是权限）时仍退出 0；
//! * `status` 远端不可达（remote_error 有值）时仍退出 0；
//! * `test-conn` 连接检查失败（ok=false，如 HTTP 401/404/405）时仍退出 0。
//!
//! 脚本（cron、安装脚本、E2E）只看退出码：退出 0 就以为「检查通过了」，
//! 于是把「连不上服务器 / 清理没做完」当成成功。
//!
//! 现在：
//! * 检查没通过 ⇒ 退出码非 0；
//! * `--json` 模式下 stdout 里仍然**只有一份** JSON（不额外追加 {"ok":false,...}），
//!   且其中的 ok / remote_error / failures 如实反映失败。

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-cli-check-{}-{seq}-{tag}",
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
            "{{\n  \"webdav\": {{\"url\": \"\", \"username\": \"\", \"password\": \"\", \"remote_dir\": \"\"}},\n  \"password_ref\": \"none\",\n  \"proxy\": {{\"mode\": \"disabled\"}},\n  \"rime_dir\": {},\n  \"device_id\": \"dev-check\",\n  \"transport_interval_minutes\": 10,\n  \"sync_kinds\": {{\"userdict\": true, \"custom_yaml\": true, \"dict_sources\": true}},\n  \"exclude_globs\": [],\n  \"stop_fcitx5_during_apply\": true,\n  \"schema_version\": 2\n}}\n",
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

/// stdout 必须能被当成**一份** JSON 解析（多打一行 JSON 会让脚本解析到错误的那份）。
fn parse_single_json(stdout: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stdout);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|error| panic!("stdout 必须是且仅是一份 JSON（{error}）：{text}"))
}

/// 借一个端口再立刻放开：连上去会被拒绝（连接不可达）。
fn unreachable_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/dav")
}

const MULTISTATUS: &str =
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\"></D:multistatus>";

/// 起一个极简 HTTP 服务：任何请求都回固定状态行 + 一个空 multistatus。
///
/// 用来模拟「服务器能连上，但拒绝/不认这个地址」（HTTP 4xx），
/// 这是 test-conn 判定 ok=false 的典型形态。
fn spawn_http(status_line: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = serve_once(&mut stream, status_line);
        }
    });
    (format!("http://{addr}"), handle)
}

fn serve_once(stream: &mut TcpStream, status_line: &'static str) -> std::io::Result<()> {
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let body = MULTISTATUS;
    let response = format!(
        "HTTP/1.1 {status_line}\r\nDAV: 1,2\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

// ---------------------------------------------------------------- D4 test-conn

/// D4：连接检查失败（服务器回了 401）时必须非零退出，
/// 且 `--json` 模式下 stdout 仍然只有一份 JSON。
#[test]
fn test_conn_failure_exits_non_zero_and_reports_ok_false() {
    let fixture = Fixture::new("test-conn-fail");
    fixture.write_config();
    let (url, _server) = spawn_http("401 Unauthorized");

    let output = fixture.run(&["--json", "--dav-url", url.as_str(), "test-conn"]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "连接检查失败必须非零退出：stdout={} stderr={}",
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

/// D4：连接检查通过时必须仍然退出 0（别把正常路径也弄坏了）。
#[test]
fn test_conn_success_exits_zero() {
    let fixture = Fixture::new("test-conn-ok");
    fixture.write_config();
    let (url, _server) = spawn_http("207 Multi-Status");

    let output = fixture.run(&["--json", "--dav-url", url.as_str(), "test-conn"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "连接正常必须退出 0：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(true)),
        "连接正常时 ok 必须是 true：{value}"
    );
}

/// D4：非 JSON 模式下失败原因必须写到 stderr（人看得到）。
#[test]
fn test_conn_failure_prints_the_reason_to_stderr() {
    let fixture = Fixture::new("test-conn-stderr");
    fixture.write_config();
    let (url, _server) = spawn_http("401 Unauthorized");

    let output = fixture.run(&["--dav-url", url.as_str(), "test-conn"]);

    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error:"),
        "人读模式必须把失败原因写到 stderr：{stderr}"
    );
}

// ------------------------------------------------------------------ D3 status

/// D3：远端不可达时 status 必须非零退出（status 是只读命令，不做副作用）。
#[test]
fn status_with_unreachable_remote_exits_non_zero() {
    let fixture = Fixture::new("status-fail");
    fixture.write_config();
    let url = unreachable_url();

    let output = fixture.run(&["--json", "--dav-url", url.as_str(), "status"]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "远端不可达必须非零退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    let error = value
        .get("remote_error")
        .and_then(|item| item.as_str())
        .unwrap_or_default();
    assert!(
        !error.is_empty(),
        "远端错误必须如实写进 remote_error：{value}"
    );
}

/// D3：远端可达时 status 必须仍然退出 0。
#[test]
fn status_with_reachable_remote_exits_zero() {
    let fixture = Fixture::new("status-ok");
    fixture.write_config();
    let (url, _server) = spawn_http("207 Multi-Status");

    let output = fixture.run(&["--json", "--dav-url", url.as_str(), "status"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "远端可达必须退出 0：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("remote_error"),
        Some(&serde_json::Value::Null),
        "远端可达时 remote_error 必须是 null：{value}"
    );
}

// -------------------------------------------------------------- D2 prune-schemas

/// 让目录不可写：里面的文件删不掉（fs::rename 报 EACCES）⇒ prune 记 failure。
#[cfg(unix)]
fn make_readonly_dir(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).expect("chmod 555");
}

#[cfg(unix)]
fn restore_dir(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
}

/// D2：有文件删不掉（权限不足）时 prune-schemas 必须非零退出，
/// 且 failures / ok 必须如实反映失败。
#[cfg(unix)]
#[test]
fn prune_schemas_failure_exits_non_zero() {
    let fixture = Fixture::new("prune-fail");
    fixture.write_config();
    let data = fixture.root.join("data");
    let user = fixture.root.join("user");
    fs::create_dir_all(&data).expect("data dir");
    fs::create_dir_all(&user).expect("user dir");
    // 非保留方案：会被裁掉；目录不可写 ⇒ 删不掉 ⇒ 记 failure。
    fs::write(
        data.join("terra_pinyin.schema.yaml"),
        "schema:\n  schema_id: terra_pinyin\n",
    )
    .expect("write schema");
    make_readonly_dir(&data);

    // 以 root 跑测试时目录权限拦不住删除（rename 照样成功）⇒ 这条判据
    // 在 root 下不成立，直接跳过，别让 CI 的 root 容器假红。
    if fs::write(data.join(".root-probe"), "x").is_ok() {
        let _ = fs::remove_file(data.join(".root-probe"));
        restore_dir(&data);
        eprintln!("跳过 prune 失败用例：当前是 root，目录权限拦不住删除");
        return;
    }

    let output = fixture.run(&[
        "--json",
        "prune-schemas",
        "--data-dir",
        data.to_str().expect("data path"),
        "--user-dir",
        user.to_str().expect("user path"),
    ]);

    restore_dir(&data);

    assert_ne!(
        output.status.code(),
        Some(0),
        "清理失败必须非零退出：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(false)),
        "清理失败时 ok 必须是 false：{value}"
    );
    let failures = value
        .get("failures")
        .and_then(|item| item.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    assert!(failures > 0, "failures 必须记录失败项：{value}");
}

/// D2 的另一半：一切正常时 prune-schemas 必须仍然退出 0。
#[test]
fn prune_schemas_success_exits_zero() {
    let fixture = Fixture::new("prune-ok");
    fixture.write_config();
    let data = fixture.root.join("data");
    let user = fixture.root.join("user");
    fs::create_dir_all(&data).expect("data dir");
    fs::create_dir_all(&user).expect("user dir");
    fs::write(
        data.join("terra_pinyin.schema.yaml"),
        "schema:\n  schema_id: terra_pinyin\n",
    )
    .expect("write schema");

    let output = fixture.run(&[
        "--json",
        "prune-schemas",
        "--data-dir",
        data.to_str().expect("data path"),
        "--user-dir",
        user.to_str().expect("user path"),
    ]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "清理顺利必须退出 0：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = parse_single_json(&output.stdout);
    assert_eq!(
        value.get("ok"),
        Some(&serde_json::Value::Bool(true)),
        "清理顺利时 ok 必须是 true：{value}"
    );
    assert!(
        !data.join("terra_pinyin.schema.yaml").exists(),
        "非保留方案应被裁掉：{value}"
    );
}
