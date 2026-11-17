//! transport 覆盖本地文件前必须留备份（B2）+ 失败退避如实报告（B1）。
//!
//! 覆盖本地文件的两条 transport 路径（远端快照拉取、配置树合并）都走同一道
//! 闸门：内容不同 ⇒ 先把旧内容存进本轮备份会话；备份失败 ⇒ 一个字都不写；
//! 内容完全相同 ⇒ 不碰文件（幂等性）。

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rime_sync_core::backup::BackupManager;
use rime_sync_core::config::Config;
use rime_sync_core::engine::Engine;
use rime_sync_core::platform::FakeTrigger;

/// 远端设备目录里的快照内容（与本地不同 ⇒ 触发覆盖）。
const REMOTE_SNAPSHOT: &[u8] = b"remote\t1\nlocal\t2\n";
/// 本地已有的旧内容（覆盖后应该能从备份里找回来）。
const LOCAL_OLD: &[u8] = b"local only\n";
const DEVICE: &str = "devB";
const REMOTE_PREFIX: &str = "/dav/rime-sync";

const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-transport-backup-{tag}-{}-{}",
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

/// 极简 WebDAV mock：只提供另一个设备目录下的一个快照。
struct MockServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve(stream);
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
            stop,
            handle: Some(handle),
        }
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

fn serve(mut stream: TcpStream) -> std::io::Result<()> {
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

    let trimmed = path.trim_end_matches('/');
    let sync_path = format!("{REMOTE_PREFIX}/sync");
    let device_path = format!("{REMOTE_PREFIX}/sync/{DEVICE}");
    let snapshot_path = format!("{device_path}/main.userdb.txt");
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
                REMOTE_SNAPSHOT.len()
            )
            .into_bytes(),
        )
    } else if method == "PROPFIND" {
        (
            "207 Multi-Status",
            b"<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\"></D:multistatus>".to_vec(),
        )
    } else if method == "GET" && trimmed == snapshot_path {
        ("200 OK", REMOTE_SNAPSHOT.to_vec())
    } else {
        ("404 Not Found", Vec::new())
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

/// 造一个「本地已有旧快照」的 Rime 目录 + 状态目录。
fn seed_local_snapshot(root: &Path, content: &[u8]) -> PathBuf {
    let rime = root.join("rime");
    fs::create_dir_all(rime.join("sync").join(DEVICE)).expect("sync dir");
    fs::write(
        rime.join("sync").join(DEVICE).join("main.userdb.txt"),
        content,
    )
    .expect("seed");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state dir");
    rime
}

fn engine_for(mock: &MockServer, rime: PathBuf, state: PathBuf) -> Engine {
    let mut config = Config::default();
    config.webdav.url = format!("http://{}/dav", mock.addr);
    config.webdav.username = "u".into();
    config.device_id = "devA".into();
    Engine::new(config, rime, state, Box::new(FakeTrigger::new())).expect("engine")
}

#[test]
fn transport_overwrite_keeps_old_content_in_backup() {
    let mock = MockServer::start();
    let root = temp_dir("backup");
    let rime = seed_local_snapshot(&root, LOCAL_OLD);
    let state = root.join("state");

    let mut engine = engine_for(&mock, rime.clone(), state.clone());
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert_eq!(report.pulled, 1);

    // 本地被远端内容覆盖了。
    let local = rime.join("sync").join(DEVICE).join("main.userdb.txt");
    assert_eq!(fs::read(&local).expect("read local"), REMOTE_SNAPSHOT);

    // 备份里有旧内容，且能被既有的 restore 机制找回来。
    let manager = BackupManager::new(&rime, &state.join("backups"));
    let list = manager.list().expect("list");
    assert_eq!(list.len(), 1, "one backup expected: {list:?}");
    assert_eq!(list[0].reason, "pre-transport");
    let restored = manager.restore(&list[0].id).expect("restore");
    assert_eq!(restored.restored, 1);
    assert_eq!(fs::read(&local).expect("read local"), LOCAL_OLD);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn transport_leaves_identical_file_untouched() {
    let mock = MockServer::start();
    let root = temp_dir("identical");
    // 本地内容与远端完全相同 ⇒ 不该碰文件。
    let rime = seed_local_snapshot(&root, REMOTE_SNAPSHOT);
    let state = root.join("state");
    let local = rime.join("sync").join(DEVICE).join("main.userdb.txt");
    let before = fs::metadata(&local)
        .expect("meta")
        .modified()
        .expect("mtime");

    let mut engine = engine_for(&mock, rime.clone(), state.clone());
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert_eq!(report.pulled, 0, "identical content must not be counted");

    let after = fs::metadata(&local)
        .expect("meta")
        .modified()
        .expect("mtime");
    assert_eq!(before, after, "identical file must not be rewritten");
    assert_eq!(fs::read(&local).expect("read local"), REMOTE_SNAPSHOT);

    // 没有覆盖 ⇒ 不该留下备份。
    let manager = BackupManager::new(&rime, &state.join("backups"));
    assert!(
        manager.list().expect("list").is_empty(),
        "no backup expected when nothing was overwritten"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn transport_does_not_overwrite_when_backup_fails() {
    let mock = MockServer::start();
    let root = temp_dir("backup-fail");
    let rime = seed_local_snapshot(&root, LOCAL_OLD);
    let state = root.join("state");
    let local = rime.join("sync").join(DEVICE).join("main.userdb.txt");

    // 备份根指向一个普通文件 ⇒ 建目录必失败（备份失败）。
    let blocked = root.join("blocked-backups");
    fs::write(&blocked, b"not a directory").expect("blocked");

    let mut engine = engine_for(&mock, rime.clone(), state.clone()).with_backup_root(blocked);
    let report = engine.transport();
    assert!(!report.ok, "transport must fail when backup fails");
    let error = report.error.unwrap_or_default();
    assert!(
        error.contains("backup"),
        "error should mention backup: {error}"
    );

    // 一个字都没写：本地还是旧内容。
    assert_eq!(fs::read(&local).expect("read local"), LOCAL_OLD);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn transport_refuses_to_overwrite_an_unreadable_local_path() {
    let mock = MockServer::start();
    let root = temp_dir("unreadable");
    let rime = root.join("rime");
    // 这条本地路径存在，但内容读不出来（是个目录 ⇒ fs::read 报
    // IsADirectory；root 下也稳定复现，不依赖 chmod）。
    // 这种情况下绝不允许无备份覆盖。
    let blocked = rime.join("sync").join(DEVICE).join("main.userdb.txt");
    fs::create_dir_all(&blocked).expect("dir");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, rime.clone(), state.clone());
    let report = engine.transport();
    assert!(!report.ok, "must not overwrite an unreadable path");
    assert!(blocked.is_dir(), "the local path must be untouched");

    let manager = BackupManager::new(&rime, &state.join("backups"));
    assert!(
        manager.list().expect("list").is_empty(),
        "an unreadable path cannot be backed up, so no backup may be claimed"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn corrupted_retry_state_is_reported_not_silently_reset() {
    let mock = MockServer::start();
    let root = temp_dir("retry-corrupt");
    let rime = seed_local_snapshot(&root, REMOTE_SNAPSHOT);
    let state = root.join("state");
    fs::write(state.join("retry.json"), b"{ this is not json").expect("corrupt");

    let mut engine = engine_for(&mock, rime.clone(), state.clone());
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("退避状态读取失败")),
        "corrupted retry.json must be reported: {:?}",
        report.notes
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_retry_state_is_not_reported_as_error() {
    let mock = MockServer::start();
    let root = temp_dir("retry-missing");
    let rime = seed_local_snapshot(&root, REMOTE_SNAPSHOT);
    let state = root.join("state");
    assert!(
        !state.join("retry.json").exists(),
        "first run has no retry.json"
    );

    let mut engine = engine_for(&mock, rime.clone(), state.clone());
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert!(
        !report
            .notes
            .iter()
            .any(|note| note.contains("退避状态读取失败")),
        "first run must not be reported as an error: {:?}",
        report.notes
    );

    let _ = fs::remove_dir_all(&root);
}
