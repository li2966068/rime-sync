//! SPEC-4 §A1 收尾：policy != normal 时永远只读（每一轮），且只读轮次也要把 shared 拉到本地。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::{Engine, Report};
use rime_sync_core::index::{FileIndex, FileMeta, INDEX_VERSION};
use rime_sync_core::platform::FakeTrigger;

const SNAPSHOT: &[u8] = b"a\t1\nb\t2\n";
const SHARED_REL: &str = "default.custom.yaml";
const SHARED_BODY: &[u8] = b"patch:\n  a: 1\n";
const LOCAL_SHARED_BODY: &[u8] = b"patch:\n  a: 2\n";
const REMOTE_PREFIX: &str = "/dav/rime-sync";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-read-only-{}-{}-{}",
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

fn manifest_for(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut manifest = FileIndex {
        version: INDEX_VERSION,
        ..FileIndex::default()
    };
    for (rel, bytes) in entries {
        manifest
            .files
            .insert((*rel).to_string(), FileMeta::local(bytes));
    }
    serde_json::to_vec_pretty(&manifest).expect("manifest json")
}

fn etag_for(bytes: &[u8]) -> String {
    format!("\"{}\"", rime_sync_core::index::sha256_hex(bytes))
}

#[derive(Default)]
struct RemoteState {
    files: BTreeMap<String, Vec<u8>>,
    requests: Vec<(String, String)>,
}

struct MockDav {
    addr: SocketAddr,
    state: Arc<Mutex<RemoteState>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockDav {
    fn start(files: Vec<(&str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(RemoteState::default()));
        for (rel, bytes) in files {
            state
                .lock()
                .expect("lock")
                .files
                .insert(rel.to_string(), bytes);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let loop_state = Arc::clone(&state);
        let loop_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).expect("nonblocking");
            while !loop_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve(stream, &loop_state);
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
            state,
            stop,
            handle: Some(handle),
        }
    }

    fn methods(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("lock")
            .requests
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        self.state.lock().expect("lock").files.clone()
    }
}

impl Drop for MockDav {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn rel_from_path(path: &str) -> String {
    let raw = path.split('?').next().unwrap_or(path);
    raw.strip_prefix(REMOTE_PREFIX)
        .unwrap_or(raw)
        .trim_matches('/')
        .to_string()
}

fn serve(mut stream: TcpStream, state: &Mutex<RemoteState>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let path = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    let mut if_match: Option<String> = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                "if-match" => if_match = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let rel = rel_from_path(&path);
    let mut guard = state.lock().expect("lock");
    guard.requests.push((method.clone(), rel.clone()));

    match method.as_str() {
        "PROPFIND" => {
            let (status, xml) = propfind(&guard, &rel);
            drop(guard);
            respond(&mut stream, status, &xml, true, None)
        }
        "GET" => {
            let bytes = guard.files.get(&rel).cloned();
            drop(guard);
            match bytes {
                Some(bytes) => {
                    let etag = etag_for(&bytes);
                    respond(&mut stream, "200 OK", &bytes, true, Some(&etag))
                }
                None => respond(&mut stream, "404 Not Found", b"", true, None),
            }
        }
        "HEAD" => {
            let bytes = guard.files.get(&rel).cloned();
            drop(guard);
            match bytes {
                Some(bytes) => {
                    let etag = etag_for(&bytes);
                    respond(&mut stream, "200 OK", &bytes, false, Some(&etag))
                }
                None => respond(&mut stream, "404 Not Found", b"", false, None),
            }
        }
        "PUT" => {
            let allowed = match if_match.as_deref() {
                Some(expected) => guard
                    .files
                    .get(&rel)
                    .is_some_and(|current| etag_for(current) == expected),
                None => true,
            };
            if !allowed {
                drop(guard);
                return respond(&mut stream, "412 Precondition Failed", b"", true, None);
            }
            guard.files.insert(rel, body.clone());
            let etag = etag_for(&body);
            drop(guard);
            respond(&mut stream, "201 Created", b"", true, Some(&etag))
        }
        "MKCOL" => {
            drop(guard);
            respond(&mut stream, "201 Created", b"", true, None)
        }
        "DELETE" => {
            guard.files.remove(&rel);
            drop(guard);
            respond(&mut stream, "204 No Content", b"", true, None)
        }
        _ => {
            drop(guard);
            respond(&mut stream, "405 Method Not Allowed", b"", true, None)
        }
    }
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
    send_body: bool,
    etag: Option<&str>,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(etag) = etag {
        head.push_str(&format!("ETag: {etag}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if send_body {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn propfind(state: &RemoteState, rel: &str) -> (&'static str, Vec<u8>) {
    let prefix = if rel.is_empty() {
        String::new()
    } else {
        format!("{rel}/")
    };
    let is_file = state.files.contains_key(rel);
    let mut children: BTreeSet<(String, bool)> = BTreeSet::new();
    for key in state.files.keys() {
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (name, is_dir) = match rest.split_once('/') {
            Some((name, _)) => (name.to_string(), true),
            None => (rest.to_string(), false),
        };
        children.insert((name, is_dir));
    }
    if !is_file && children.is_empty() && !rel.is_empty() {
        return ("404 Not Found", Vec::new());
    }

    let mut xml = String::from("<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">");
    if is_file {
        push_file_entry(&mut xml, rel, &state.files[rel]);
    } else {
        if !rel.is_empty() {
            push_dir_entry(&mut xml, rel);
        }
        for (name, is_dir) in children {
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if is_dir {
                push_dir_entry(&mut xml, &child_rel);
            } else if let Some(bytes) = state.files.get(&child_rel) {
                push_file_entry(&mut xml, &child_rel, bytes);
            }
        }
    }
    xml.push_str("</D:multistatus>");
    ("207 Multi-Status", xml.into_bytes())
}

fn push_dir_entry(xml: &mut String, rel: &str) {
    xml.push_str(&format!(
        "<D:response><D:href>{REMOTE_PREFIX}/{rel}/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
    ));
}

fn push_file_entry(xml: &mut String, rel: &str, bytes: &[u8]) {
    let etag = etag_for(bytes);
    xml.push_str(&format!(
        "<D:response><D:href>{REMOTE_PREFIX}/{rel}</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>{}</D:getcontentlength><D:getetag>{etag}</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        bytes.len()
    ));
}

fn engine_for(mock: &MockDav, rime: &Path, state: &Path, policy: FirstSyncPolicy) -> Engine {
    let mut config = Config::default();
    config.webdav.url = format!("http://{}/dav", mock.addr);
    config.webdav.username = "u".to_string();
    config.device_id = "devA".to_string();
    config.first_sync_policy = policy;
    Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine")
}

fn write_methods(methods: &[String]) -> Vec<String> {
    methods
        .iter()
        .filter(|method| !matches!(method.as_str(), "PROPFIND" | "GET" | "HEAD"))
        .cloned()
        .collect()
}

fn assert_nothing_pushed(report: &Report) {
    assert_eq!(report.pushed, 0, "read-only must not push: {report:?}");
}

fn read_only_twice(policy: FirstSyncPolicy, expected_mode: &str, tag: &str) {
    let manifest = manifest_for(&[(SHARED_REL, SHARED_BODY)]);
    let mock = MockDav::start(vec![
        ("sync/other-dev/main.userdb.txt", SNAPSHOT.to_vec()),
        ("shared/manifest.json", manifest),
        ("shared/files/default.custom.yaml", SHARED_BODY.to_vec()),
    ]);
    let before = mock.snapshot();
    let root = temp_dir(tag);
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, &rime, &state, policy);
    let first = engine.transport();
    let second = engine.transport();
    assert!(first.ok, "first transport failed: {first:?}");
    assert!(second.ok, "second transport failed: {second:?}");

    // 第一轮：首次接入；第二轮：同一策略下仍然只读，绝不滑进双向分支。
    assert!(first.first_sync, "first run must be detected as first sync");
    assert!(!second.first_sync, "later runs are no longer first sync");
    assert_eq!(first.mode, expected_mode);
    assert_eq!(second.mode, expected_mode);
    assert_ne!(second.mode, "normal");
    assert_nothing_pushed(&first);
    assert_nothing_pushed(&second);

    // 只读轮次要完整地拉：远端快照 + shared 文件都落到本地。
    assert_eq!(
        fs::read(rime.join("sync/other-dev/main.userdb.txt")).expect("snapshot"),
        SNAPSHOT
    );
    assert_eq!(
        fs::read(rime.join(SHARED_REL)).expect("shared file"),
        SHARED_BODY
    );
    assert!(
        !rime.join("sync/devA").exists(),
        "read-only must not create the local device dir"
    );

    // 两次都只有读方法；远端文件集合一个字节都没变。
    let methods = mock.methods();
    assert!(!methods.is_empty(), "expected remote probes");
    let writes = write_methods(&methods);
    assert!(
        writes.is_empty(),
        "read-only must issue no write methods, saw: {writes:?} (all: {methods:?})"
    );
    assert_eq!(mock.snapshot(), before, "remote file set must not change");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn preview_policy_is_read_only_on_every_transport() {
    read_only_twice(FirstSyncPolicy::Preview, "preview", "preview");
}

#[test]
fn pull_only_policy_is_read_only_on_every_transport() {
    read_only_twice(FirstSyncPolicy::PullOnly, "pull-only", "pull-only");
}

#[test]
fn read_only_pulls_shared_files() {
    let manifest = manifest_for(&[(SHARED_REL, SHARED_BODY)]);
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest),
        ("shared/files/default.custom.yaml", SHARED_BODY.to_vec()),
    ]);
    let before = mock.snapshot();
    let root = temp_dir("shared-pull");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Preview);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.first_sync, "shared data must count as first sync");
    assert_eq!(report.mode, "preview");
    assert_eq!(report.pushed, 0);
    assert!(report.pulled >= 1, "shared file must be pulled: {report:?}");
    assert_eq!(
        fs::read(rime.join(SHARED_REL)).expect("shared file"),
        SHARED_BODY
    );
    assert!(!rime.join("sync/devA").exists());
    assert!(write_methods(&mock.methods()).is_empty());
    assert_eq!(mock.snapshot(), before);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_only_stores_conflict_copy_without_writing_remote() {
    let manifest = manifest_for(&[(SHARED_REL, SHARED_BODY)]);
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest),
        ("shared/files/default.custom.yaml", SHARED_BODY.to_vec()),
    ]);
    let before = mock.snapshot();
    let root = temp_dir("conflict");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    fs::write(rime.join(SHARED_REL), LOCAL_SHARED_BODY).expect("local shared");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Preview);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert_eq!(report.mode, "preview");
    assert_eq!(report.pushed, 0);
    assert!(
        report.conflicts.iter().any(|item| item == SHARED_REL),
        "conflict must be reported: {report:?}"
    );

    // 本地原文件保留，另存 conflict 副本（既有规则）；远端一个字节都没写。
    assert_eq!(
        fs::read(rime.join(SHARED_REL)).expect("local shared"),
        LOCAL_SHARED_BODY
    );
    let conflict_copy = fs::read_dir(&rime)
        .expect("read rime")
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("default.custom.yaml.conflict-devA-")
        })
        .expect("conflict copy");
    assert_eq!(
        fs::read(conflict_copy.path()).expect("conflict copy"),
        LOCAL_SHARED_BODY
    );
    assert!(write_methods(&mock.methods()).is_empty());
    assert_eq!(mock.snapshot(), before);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn normal_policy_still_pushes_bidirectionally() {
    let mock = MockDav::start(Vec::new());
    let root = temp_dir("normal");
    let rime = root.join("rime");
    fs::create_dir_all(rime.join("sync/devA")).expect("sync dir");
    fs::write(rime.join("sync/devA/main.userdb.txt"), SNAPSHOT).expect("snapshot");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert_eq!(report.mode, "normal");
    assert!(!report.first_sync, "empty remote is not a first sync");
    assert!(report.pushed >= 1, "normal policy must push: {report:?}");

    let files = mock.snapshot();
    assert_eq!(
        files.get("sync/devA/main.userdb.txt").map(Vec::as_slice),
        Some(SNAPSHOT),
        "snapshot must be uploaded"
    );
    assert!(
        files.contains_key("sync/devA/owner.json"),
        "normal policy writes owner.json: {:?}",
        files.keys().collect::<Vec<_>>()
    );
    let methods = mock.methods();
    assert!(
        methods.iter().any(|method| method == "MKCOL"),
        "{methods:?}"
    );
    assert!(methods.iter().any(|method| method == "PUT"), "{methods:?}");

    let _ = fs::remove_dir_all(&root);
}
