//! 快照目录名必须来自 <rime_dir>/installation.yaml 的 installation_id，而不是 config.device_id。
//!
//! 小狼毫把快照写在 sync/<installation_id>/；以前只扫 sync/<device_id>/ 所以永远推不上去。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::Engine;
use rime_sync_core::platform::FakeTrigger;

const REMOTE_PREFIX: &str = "/dav/rime-sync";
const LOCAL_ID: &str = "5d0d8b1e-0f7a-4c2a-9a3f-local";
const OTHER_ID: &str = "11111111-2222-3333-4444-555555555555";
const DEVICE: &str = "debian-528996";
const USERDB: &str = "luna_pinyin.userdb.txt";
const SNAPSHOT: &[u8] = b"ci\t1\n";
const REMOTE_SNAPSHOT: &[u8] = b"ci\t2\nxin\t3\n";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-inst-{}-{}-{}",
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

fn seed_rime(tag: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    fs::write(
        rime.join("installation.yaml"),
        format!("distribution_code_name: fcitx-rime\ninstallation_id: \"{LOCAL_ID}\"\n"),
    )
    .expect("installation.yaml");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    (rime, state)
}

fn config_for(mock: &MockServer, policy: FirstSyncPolicy) -> Config {
    let mut config = Config::default();
    config.webdav.url = mock.url();
    config.webdav.username = "u".into();
    config.device_id = DEVICE.into();
    config.first_sync_policy = policy;
    config
}

#[derive(Debug, Default)]
struct Remote {
    files: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
}

impl Remote {
    fn new(dirs: &[&str], files: &[(&str, &[u8])]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
                .collect(),
            dirs: dirs.iter().map(|path| path.to_string()).collect(),
        }
    }
}

struct MockServer {
    addr: SocketAddr,
    remote: Arc<Mutex<Remote>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockServer {
    fn start(remote: Remote) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let remote = Arc::new(Mutex::new(remote));
        let stop = Arc::new(AtomicBool::new(false));
        let loop_remote = Arc::clone(&remote);
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve(stream, &loop_remote);
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
            remote,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/dav", self.addr)
    }

    fn file(&self, rel: &str) -> Option<Vec<u8>> {
        self.remote.lock().expect("lock").files.get(rel).cloned()
    }

    fn paths(&self) -> Vec<String> {
        self.remote
            .lock()
            .expect("lock")
            .files
            .keys()
            .cloned()
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

fn relative_path(full: &str) -> Option<String> {
    let trimmed = full.trim_end_matches('/');
    if trimmed == REMOTE_PREFIX {
        return Some(String::new());
    }
    trimmed
        .strip_prefix(&format!("{REMOTE_PREFIX}/"))
        .map(|value| value.to_string())
}

fn serve(mut stream: TcpStream, remote: &Mutex<Remote>) -> std::io::Result<()> {
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
    let mut body = Vec::new();
    if content_length > 0 {
        body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
    }

    let rel = relative_path(&path);
    let (status, payload) = respond(&method, rel.as_deref(), &body, remote);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()
}

fn respond(
    method: &str,
    rel: Option<&str>,
    body: &[u8],
    remote: &Mutex<Remote>,
) -> (&'static str, Vec<u8>) {
    let Some(rel) = rel else {
        return ("404 Not Found", Vec::new());
    };
    let mut state = remote.lock().expect("lock");
    match method {
        "PROPFIND" => {
            if !rel.is_empty() && !state.dirs.contains(rel) && !state.files.contains_key(rel) {
                return ("404 Not Found", Vec::new());
            }
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            let mut dir_children: Vec<String> = state
                .dirs
                .iter()
                .filter_map(|dir| {
                    let name = dir.strip_prefix(&prefix)?;
                    (!name.is_empty() && !name.contains('/')).then(|| name.to_string())
                })
                .collect();
            dir_children.sort();
            dir_children.dedup();
            let mut file_children: Vec<String> = state
                .files
                .keys()
                .filter_map(|file| {
                    let name = file.strip_prefix(&prefix)?;
                    (!name.is_empty() && !name.contains('/')).then(|| name.to_string())
                })
                .collect();
            file_children.sort();
            file_children.dedup();
            let mut xml = String::from("<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">");
            for name in &dir_children {
                xml.push_str(&format!(
                    "<D:response><D:href>{REMOTE_PREFIX}/{prefix}{name}/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
                ));
            }
            for name in &file_children {
                let size = state
                    .files
                    .get(&format!("{prefix}{name}"))
                    .map(|bytes| bytes.len())
                    .unwrap_or(0);
                xml.push_str(&format!(
                    "<D:response><D:href>{REMOTE_PREFIX}/{prefix}{name}</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>{size}</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
                ));
            }
            xml.push_str("</D:multistatus>");
            ("207 Multi-Status", xml.into_bytes())
        }
        "MKCOL" => {
            if rel.is_empty() || state.dirs.contains(rel) {
                ("405 Method Not Allowed", Vec::new())
            } else {
                state.dirs.insert(rel.to_string());
                ("201 Created", Vec::new())
            }
        }
        "PUT" => {
            state.files.insert(rel.to_string(), body.to_vec());
            ("201 Created", Vec::new())
        }
        "GET" => match state.files.get(rel) {
            Some(bytes) => ("200 OK", bytes.clone()),
            None => ("404 Not Found", Vec::new()),
        },
        "HEAD" => match state.files.get(rel) {
            Some(_) => ("200 OK", Vec::new()),
            None => ("404 Not Found", Vec::new()),
        },
        _ => ("500 Internal Server Error", Vec::new()),
    }
}

#[test]
fn pull_reads_remote_installation_id_directory_and_marks_pending_merge() {
    let mock = MockServer::start(Remote::new(
        &["", "sync", &format!("sync/{OTHER_ID}")],
        &[(&format!("sync/{OTHER_ID}/{USERDB}"), REMOTE_SNAPSHOT)],
    ));
    let (rime, state) = seed_rime("pull");

    let mut engine = Engine::new(
        config_for(&mock, FirstSyncPolicy::Preview),
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert_eq!(
        report.pulled, 1,
        "must pull the remote installation snapshot"
    );
    assert_eq!(report.pushed, 0, "preview policy must not push");
    assert!(
        report.pending_merge,
        "a pulled *.userdb.txt still needs apply to reach the local lexicon"
    );

    let pulled = rime.join("sync").join(OTHER_ID).join(USERDB);
    assert_eq!(fs::read(&pulled).expect("pulled snapshot"), REMOTE_SNAPSHOT);

    // 自己的 device_id 目录不该出现（快照目录名是 installation_id）。
    assert!(!rime.join("sync").join(DEVICE).exists());
    assert!(!rime.join("sync").join(LOCAL_ID).exists());

    let _ = fs::remove_dir_all(rime.parent().expect("root"));
}

#[test]
fn push_writes_to_sync_installation_id_not_device_id() {
    let mock = MockServer::start(Remote::new(&["", "sync"], &[]));
    let (rime, state) = seed_rime("push");
    let local_dir = rime.join("sync").join(LOCAL_ID);
    fs::create_dir_all(&local_dir).expect("local snapshot dir");
    fs::write(local_dir.join(USERDB), SNAPSHOT).expect("local snapshot");

    let mut engine = Engine::new(
        config_for(&mock, FirstSyncPolicy::Normal),
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert_eq!(
        report.pushed, 1,
        "exactly one local snapshot must be uploaded"
    );
    assert!(!report.pending_merge, "nothing was pulled");

    let remote_snapshot = format!("sync/{LOCAL_ID}/{USERDB}");
    assert_eq!(
        mock.file(&remote_snapshot).expect("remote snapshot"),
        SNAPSHOT,
        "snapshot must land under sync/<installation_id>/"
    );
    assert!(
        mock.paths().iter().all(|path| !path.contains(DEVICE)),
        "device_id must not be used as a remote directory: {:?}",
        mock.paths()
    );

    let _ = fs::remove_dir_all(rime.parent().expect("root"));
}

#[test]
fn apply_stops_ime_syncs_deploys_and_restarts() {
    let mock = MockServer::start(Remote::new(&["", "sync"], &[]));
    let (rime, state) = seed_rime("apply");
    let trigger = FakeTrigger::new().with_userdb_entries(&["词条一", "词条二"]);
    let calls = trigger.calls_shared();

    let mut engine = Engine::new(
        config_for(&mock, FirstSyncPolicy::Normal),
        rime.clone(),
        state.clone(),
        Box::new(trigger),
    )
    .expect("engine");
    let report = engine.apply();
    assert!(report.ok, "apply failed: {:?}", report.error);
    assert_eq!(
        report.merged_entries,
        Some(2),
        "apply must report the post-merge entry count"
    );
    let calls = calls.lock().map(|guard| guard.clone()).unwrap_or_default();
    assert_eq!(
        calls,
        vec![
            "stop_ime",
            "export_userdb",
            "sync_userdb",
            "export_userdb",
            "deploy",
            "start_ime",
        ],
        "apply order: stop fcitx5 -> sync -> deploy -> start fcitx5, with exports around the merge"
    );
    assert!(
        !report.pending_merge,
        "apply cleared the pending merge debt"
    );

    let tmp = state.join("tmp");
    let leftovers: Vec<String> = fs::read_dir(&tmp)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "temp exports must be cleaned: {leftovers:?}"
    );

    let _ = fs::remove_dir_all(rime.parent().expect("root"));
}
