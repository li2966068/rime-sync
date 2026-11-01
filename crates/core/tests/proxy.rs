//! 0.6.8 代理支持：真实 HTTP / SOCKS5 代理上的端到端验证。
//!
//! 测试里自带两个极小的代理服务端（HTTP absolute-form + CONNECT、SOCKS5 + RFC1929），
//! 不依赖 docker / 外部二进制，CI 里也能稳定跑。

mod support;

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rime_sync_core::config::{Config, FirstSyncPolicy, WebDavConfig};
use rime_sync_core::engine::Engine;
use rime_sync_core::platform::FakeTrigger;
use rime_sync_core::proxy::ProxyConfig;
use rime_sync_core::webdav::DavClient;

use support::{temp_dir, MockDav};

/* ---------- 极简 HTTP 代理 ---------- */

struct HttpProxy {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl HttpProxy {
    fn start(auth: Option<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let loop_requests = Arc::clone(&requests);
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let seen = Arc::clone(&loop_requests);
                        thread::spawn(move || {
                            let _ = serve_http(stream, auth, &seen);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for HttpProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_http(
    mut client: TcpStream,
    auth: Option<(&'static str, &'static str)>,
    seen: &AtomicUsize,
) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    client.set_write_timeout(Some(Duration::from_secs(5)))?;
    let head = read_head(&mut client)?;
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let headers: Vec<String> = lines
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    if let Some((user, pass)) = auth {
        let expected = basic_header(user, pass);
        let got = header_value(&headers, "proxy-authorization");
        if got.as_deref() != Some(expected.as_str()) {
            let body = b"proxy authentication required";
            let response = format!(
                "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"rime-sync-test\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            client.write_all(response.as_bytes())?;
            client.write_all(body)?;
            client.flush()?;
            return Ok(());
        }
    }
    seen.fetch_add(1, Ordering::SeqCst);

    if method.eq_ignore_ascii_case("CONNECT") {
        let mut upstream = TcpStream::connect(target.as_str())?;
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        client.flush()?;
        return tunnel(client, &mut upstream);
    }

    // reqwest 对 http:// 目标使用 absolute-form：这里是唯一需要改写的地方。
    let (host_port, path) = split_absolute(&target).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported proxy target {target}"),
        )
    })?;
    let mut upstream = TcpStream::connect(host_port)?;
    let mut rewritten = format!("{method} {path} {version}\r\n");
    for header in &headers {
        let name = header
            .split(':')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if name == "proxy-authorization" || name == "proxy-connection" {
            continue;
        }
        rewritten.push_str(header);
        rewritten.push_str("\r\n");
    }
    rewritten.push_str("\r\n");
    upstream.write_all(rewritten.as_bytes())?;
    upstream.flush()?;
    tunnel(client, &mut upstream)
}

fn split_absolute(target: &str) -> Option<(&str, &str)> {
    let rest = target.strip_prefix("http://")?;
    match rest.find('/') {
        Some(index) => Some((&rest[..index], &rest[index..])),
        None => Some((rest, "/")),
    }
}

fn tunnel(mut client: TcpStream, upstream: &mut TcpStream) -> io::Result<()> {
    let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = upstream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let handle = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    let _ = io::copy(upstream, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = handle.join();
    Ok(())
}

fn read_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
        if head.len() > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head too large"));
        }
    }
    Ok(head)
}

fn header_value(headers: &[String], wanted: &str) -> Option<String> {
    headers.iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case(wanted) {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

/// 期望值直接取 core 的实现：单元测试同时证明了它符合 RFC 7617。
fn basic_header(user: &str, pass: &str) -> String {
    ProxyConfig {
        username: user.to_string(),
        password: pass.to_string(),
        ..ProxyConfig::default()
    }
    .auth_header()
    .expect("auth header")
}

/* ---------- 极简 SOCKS5 代理 ---------- */

struct Socks5Proxy {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Socks5Proxy {
    fn start(auth: Option<(&'static str, &'static str)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let loop_requests = Arc::clone(&requests);
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let seen = Arc::clone(&loop_requests);
                        thread::spawn(move || {
                            let _ = serve_socks5(stream, auth, &seen);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self, scheme: &str) -> String {
        format!("{scheme}://{}", self.addr)
    }

    fn count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for Socks5Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_socks5(
    mut client: TcpStream,
    auth: Option<(&'static str, &'static str)>,
    seen: &AtomicUsize,
) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    client.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting)?;
    if greeting[0] != 5 {
        return Ok(());
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods)?;

    match auth {
        Some((user, pass)) => {
            if !methods.contains(&2) {
                client.write_all(&[5, 0xff])?;
                return Ok(());
            }
            client.write_all(&[5, 2])?;
            // RFC 1929 用户名/密码认证。
            let mut ver = [0u8; 2];
            client.read_exact(&mut ver)?;
            let mut given_user = vec![0u8; ver[1] as usize];
            client.read_exact(&mut given_user)?;
            let mut plen = [0u8; 1];
            client.read_exact(&mut plen)?;
            let mut given_pass = vec![0u8; plen[0] as usize];
            client.read_exact(&mut given_pass)?;
            let ok = ver[0] == 1 && given_user == user.as_bytes() && given_pass == pass.as_bytes();
            client.write_all(&[1, if ok { 0 } else { 1 }])?;
            if !ok {
                return Ok(());
            }
        }
        None => client.write_all(&[5, 0])?,
    }

    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    if request[0] != 5 || request[1] != 1 {
        return Ok(());
    }
    let host = match request[3] {
        1 => {
            let mut bytes = [0u8; 4];
            client.read_exact(&mut bytes)?;
            format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len)?;
            let mut host = vec![0u8; len[0] as usize];
            client.read_exact(&mut host)?;
            String::from_utf8_lossy(&host).to_string()
        }
        4 => {
            let mut bytes = [0u8; 16];
            client.read_exact(&mut bytes)?;
            let groups: Vec<String> = (0..8)
                .map(|index| {
                    format!(
                        "{:x}",
                        u16::from_be_bytes([bytes[index * 2], bytes[index * 2 + 1]])
                    )
                })
                .collect();
            format!("[{}]", groups.join(":"))
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);

    let mut upstream = TcpStream::connect((host.as_str(), port))?;
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;
    client.flush()?;
    seen.fetch_add(1, Ordering::SeqCst);
    tunnel(client, &mut upstream)
}

/* ---------- 场景 ---------- */

fn proxy_config(url: &str) -> ProxyConfig {
    ProxyConfig {
        enabled: true,
        url: url.to_string(),
        ..ProxyConfig::default()
    }
}

fn engine_with_proxy(mock: &MockDav, rime: &Path, state: &Path, proxy: ProxyConfig) -> Engine {
    let config = Config {
        webdav: WebDavConfig {
            url: mock.url(),
            username: "u".to_string(),
            ..WebDavConfig::default()
        },
        device_id: "devA".to_string(),
        first_sync_policy: FirstSyncPolicy::Normal,
        proxy,
        ..Config::default()
    };
    Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine")
}

/// 配置启用 HTTP 代理 → 整个 transport（PROPFIND/MKCOL/…）都从代理走。
#[test]
fn transport_goes_through_http_proxy() {
    let mock = MockDav::start(vec![]);
    let proxy = HttpProxy::start(None);
    let root = temp_dir("http-proxy");
    let rime = root.join("rime");
    let state = root.join("state");
    fs::create_dir_all(&rime).expect("rime");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_with_proxy(&mock, &rime, &state, proxy_config(&proxy.url()));
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert!(!mock.methods().is_empty(), "远端没有收到任何请求");
    assert!(proxy.count() > 0, "请求没有经过代理");
}

/// 代理要求认证：凭据正确 → 请求成功；凭据错误 → 明确说是代理 407。
#[test]
fn http_proxy_auth_success_and_failure() {
    let mock = MockDav::start(vec![]);
    let proxy = HttpProxy::start(Some(("alice", "s3cret")));

    let mut good = proxy_config(&proxy.url());
    good.username = "alice".to_string();
    good.password = "s3cret".to_string();
    let client =
        DavClient::with_proxy(&mock.url(), "u", "p", "rime-sync", Some(&good)).expect("client");
    let check = client.check_connection().expect("check");
    assert!(check.ok, "{}", check.message);
    assert!(proxy.count() > 0, "认证请求没有经过代理");

    let mut bad = proxy_config(&proxy.url());
    bad.username = "alice".to_string();
    bad.password = "wrong".to_string();
    let client = DavClient::with_proxy("http://example.invalid/dav", "u", "p", "", Some(&bad))
        .expect("client");
    let error = client.propfind("", 0).expect_err("must fail");
    let text = format!("{error:#}");
    assert!(text.contains("代理认证失败"), "{text}");
    assert!(text.contains("407"), "{text}");
    assert!(!text.contains("wrong"), "错误信息不能泄露密码: {text}");
}

/// socks5h（由代理端解析域名）+ 用户名密码认证。
#[test]
fn socks5h_with_auth_carries_requests() {
    let mock = MockDav::start(vec![]);
    let proxy = Socks5Proxy::start(Some(("alice", "s3cret")));
    let mut config = proxy_config(&proxy.url("socks5h"));
    config.username = "alice".to_string();
    config.password = "s3cret".to_string();

    let client =
        DavClient::with_proxy(&mock.url(), "u", "p", "rime-sync", Some(&config)).expect("client");
    let check = client.check_connection().expect("check");
    assert!(check.ok, "{}", check.message);
    assert!(proxy.count() > 0, "请求没有经过 socks5 代理");
}

/// socks5（无认证）+ 本地解析域名。
#[test]
fn socks5_without_auth_carries_requests() {
    let mock = MockDav::start(vec![]);
    let proxy = Socks5Proxy::start(None);
    let config = proxy_config(&proxy.url("socks5"));

    let client =
        DavClient::with_proxy(&mock.url(), "u", "p", "rime-sync", Some(&config)).expect("client");
    let check = client.check_connection().expect("check");
    assert!(check.ok, "{}", check.message);
    assert!(proxy.count() > 0, "请求没有经过 socks5 代理");
}

/// 配置里写了代理但 enabled=false → 直连，代理一次都不该被访问。
#[test]
fn disabled_proxy_is_not_used() {
    let mock = MockDav::start(vec![]);
    let proxy = HttpProxy::start(None);
    let mut config = proxy_config(&proxy.url());
    config.enabled = false;

    let client =
        DavClient::with_proxy(&mock.url(), "u", "p", "rime-sync", Some(&config)).expect("client");
    let check = client.check_connection().expect("check");
    assert!(check.ok, "{}", check.message);
    assert_eq!(proxy.count(), 0, "禁用时请求绝不能经过代理");
}

/// 代理连不上：错误信息必须指向代理，而不是让人去查 WebDAV 服务器。
#[test]
fn unreachable_proxy_error_points_at_proxy() {
    let config = proxy_config("http://127.0.0.1:1");
    let client = DavClient::with_proxy("http://127.0.0.1:9/dav", "u", "p", "", Some(&config))
        .expect("client");
    let error = client.propfind("", 0).expect_err("must fail");
    let text = format!("{error:#}");
    assert!(text.contains("已启用代理"), "{text}");
    assert!(text.contains("127.0.0.1:1"), "{text}");
}
