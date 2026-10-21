//! SPEC-4 §A1/A3：首次接入只拉不推（远端一个字节都不写），并落一条 history。

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rime_sync_core::config::Config;
use rime_sync_core::engine::Engine;
use rime_sync_core::history::HistoryEntry;
use rime_sync_core::platform::FakeTrigger;

const SNAPSHOT: &[u8] = b"a\t1\nb\t2\n";
const DEVICE: &str = "other-dev";
/// 客户端 base = <url>/<remote_dir>，即 http://<addr>/dav/rime-sync。
const REMOTE_PREFIX: &str = "/dav/rime-sync";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-first-sync-{}-{}-{}",
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

struct MockServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let loop_requests = Arc::clone(&requests);
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // 每个连接只处理一个请求；出错不影响测试主线程。
                        let _ = serve(stream, &loop_requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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

    fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("lock")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(mut stream: TcpStream, requests: &Mutex<Vec<(String, String)>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
    }
    requests
        .lock()
        .expect("lock")
        .push((method.clone(), path.clone()));

    let trimmed = path.trim_end_matches('/');
    let sync_path = format!("{REMOTE_PREFIX}/sync");
    let device_path = format!("{REMOTE_PREFIX}/sync/{DEVICE}");
    let snapshot_path = format!("{device_path}/main.userdb.txt");
    let shared_manifest_path = format!("{REMOTE_PREFIX}/shared/manifest.json");
    let (status, body): (&str, Vec<u8>) = if method == "PROPFIND" && trimmed == sync_path {
        (
            "207 Multi-Status",
            format!(
                "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>{REMOTE_PREFIX}/sync/{DEVICE}/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"
            )
            .into_bytes(),
        )
    } else if method == "PROPFIND" && trimmed == device_path {
        (
            "207 Multi-Status",
            format!(
                "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>{REMOTE_PREFIX}/sync/{DEVICE}/main.userdb.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>{}</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>",
                SNAPSHOT.len()
            )
            .into_bytes(),
        )
    } else if method == "PROPFIND" {
        (
            "207 Multi-Status",
            b"<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\"></D:multistatus>".to_vec(),
        )
    } else if method == "GET" && trimmed == snapshot_path {
        ("200 OK", SNAPSHOT.to_vec())
    } else if method == "GET" && trimmed == shared_manifest_path {
        // 远端没有 shared/manifest.json：只读轮次必须当作"没有共享数据"继续。
        ("404 Not Found", Vec::new())
    } else {
        (
            "500 Internal Server Error",
            format!("unexpected {method} {path}").into_bytes(),
        )
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

#[test]
fn first_sync_pull_only_writes_nothing_to_remote() {
    let mock = MockServer::start();
    let root = temp_dir("pull-only");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut config = Config::default();
    config.webdav.url = format!("http://{}/dav", mock.addr);
    config.webdav.username = "u".into();
    config.device_id = "devA".into();

    let mut engine = Engine::new(
        config,
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert!(report.first_sync, "must be detected as first sync");
    // 默认 policy=preview：mode 用策略 id，而不是固定的 pull-only。
    assert_eq!(report.mode, "preview");
    assert_eq!(report.pulled, 1);
    assert_eq!(report.pushed, 0);

    // 只拉：远端快照落到本地，本地自己的设备目录没有创建。
    let pulled = rime.join("sync").join(DEVICE).join("main.userdb.txt");
    assert_eq!(fs::read(&pulled).expect("pulled snapshot"), SNAPSHOT);
    assert!(!rime.join("sync/devA").exists());
    let mut landed: Vec<String> = fs::read_dir(rime.join("sync"))
        .expect("sync dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .into_string()
                .expect("utf8")
        })
        .collect();
    landed.sort();
    assert_eq!(
        landed,
        vec![DEVICE.to_string()],
        "only the remote device snapshot may land"
    );

    // 远端一个写操作都没有：没有 MKCOL / PUT / DELETE。
    let methods = mock.methods();
    assert!(!methods.is_empty(), "expected remote probes");
    assert!(
        methods
            .iter()
            .all(|method| matches!(method.as_str(), "PROPFIND" | "GET" | "HEAD")),
        "pull-only must not write to remote, saw: {methods:?}"
    );

    // 历史：一条 transport 记录，pull-only 也如实落盘。
    let history = fs::read_to_string(state.join("history.jsonl")).expect("history");
    let lines: Vec<&str> = history.lines().collect();
    assert_eq!(lines.len(), 1);
    let entry: HistoryEntry = serde_json::from_str(lines[0]).expect("entry");
    assert_eq!(entry.mode, "transport");
    assert!(entry.ok);
    assert_eq!(entry.pulled, 1);

    let _ = fs::remove_dir_all(&root);
}
