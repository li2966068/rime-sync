//! 0.6.3：*.custom.yaml 按 key 的设备本地作用域。
//!
//! 共享 menu/page_size 这类键，但 schema_list 每台机器各自独立：比较/合并前剔除，
//! 写回本机时放回原值，上传的字节里永远没有它。

use std::fs;
use std::path::Path;

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::Engine;
use rime_sync_core::index::{sha256_hex, FileIndex, FileMeta, INDEX_VERSION};
use rime_sync_core::platform::FakeTrigger;
use serde_yaml::Value;

mod support;

use support::{manifest_for, temp_dir, write_methods, MockDav};

const REL: &str = "default.custom.yaml";
const REMOTE_PATH: &str = "sync/devA/config/files/default.custom.yaml";
const PROFILE_MANIFEST: &str = "sync/devA/config/manifest.json";

const LOCAL: &[u8] = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n  menu/page_size: 8\n";
/// 旧版本上传的远端文件：还带着 schema_list。
const REMOTE_OLD: &[u8] =
    b"patch:\n  schema_list:\n    - schema: luna_pinyin\n  menu/page_size: 7\n";
/// 上一轮同步的基线（新格式：设备本地键从不落进远端）。
const BASE: &[u8] = b"patch:\n  menu/page_size: 8\n";

fn engine(mock: &MockDav, rime: &Path, state: &Path, device_local: &[&str]) -> Engine {
    let mut config = Config::default();
    config.webdav.url = mock.url();
    config.webdav.username = "u".to_string();
    config.device_id = "devA".to_string();
    config.first_sync_policy = FirstSyncPolicy::Normal;
    config.device_local_keys = device_local.iter().map(|key| key.to_string()).collect();
    Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine")
}

fn workspace(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = temp_dir(tag);
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    (root, rime, state)
}

/// 写入「上一轮同步」的基线：本地 profile manifest + base 缓存（base_dir/<sha256>）。
fn seed_base(state: &Path, base: &[u8]) {
    let sha = sha256_hex(base);
    let mut index = FileIndex {
        version: INDEX_VERSION,
        ..FileIndex::default()
    };
    index.files.insert(
        REL.to_string(),
        FileMeta {
            sha256: sha.clone(),
            size: base.len() as u64,
            mtime: String::new(),
            etag: None,
        },
    );
    let manifest_path = state
        .join("sync")
        .join("devA")
        .join("config")
        .join("manifest.json");
    fs::create_dir_all(manifest_path.parent().expect("parent")).expect("profile dir");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&index).expect("index json"),
    )
    .expect("profile manifest");
    fs::create_dir_all(state.join("base")).expect("base dir");
    fs::write(state.join("base").join(&sha), base).expect("base cache");
}

fn patch(text: &str) -> Value {
    let doc: Value = serde_yaml::from_str(text).expect("yaml");
    doc.get("patch").cloned().expect("patch")
}

fn schema_names(text: &str) -> Vec<String> {
    patch(text)
        .get("schema_list")
        .and_then(Value::as_sequence)
        .expect("schema_list")
        .iter()
        .map(|item| {
            item.get("schema")
                .and_then(Value::as_str)
                .expect("schema name")
                .to_string()
        })
        .collect()
}

fn page_size(text: &str) -> u64 {
    patch(text)
        .get("menu/page_size")
        .and_then(Value::as_u64)
        .expect("menu/page_size")
}

fn remote_body(mock: &MockDav, rel: &str) -> String {
    let remote = mock.snapshot();
    String::from_utf8(remote.get(rel).expect("remote file").clone()).expect("utf8")
}

#[test]
fn device_local_keys_stay_local_and_are_never_uploaded() {
    let mock = MockDav::start(vec![
        (PROFILE_MANIFEST, manifest_for(&[(REL, REMOTE_OLD)])),
        (REMOTE_PATH, REMOTE_OLD.to_vec()),
    ]);
    let (root, rime, state) = workspace("devkeys-shared");
    fs::write(rime.join(REL), LOCAL).expect("local");
    seed_base(&state, BASE);

    let mut engine = engine(&mock, &rime, &state, &["schema_list"]);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("stripped 1 device-local key")),
        "merge must explain the strip: {report:?}"
    );

    // 本机：schema_list 原样保留，共享键 page_size 跟随远端。
    let local = fs::read_to_string(rime.join(REL)).expect("local");
    assert_eq!(schema_names(&local), vec!["luna_pinyin", "wubi86"]);
    assert_eq!(page_size(&local), 7);

    // 远端：确实 PUT 过，且上传内容不含 schema_list。
    assert!(
        mock.requests()
            .iter()
            .any(|(method, path)| method == "PUT" && path == REMOTE_PATH),
        "expected a PUT of {REMOTE_PATH}: {:?}",
        mock.requests()
    );
    let remote = remote_body(&mock, REMOTE_PATH);
    assert!(!remote.contains("schema_list"), "{remote}");
    assert_eq!(page_size(&remote), 7);

    // 第二轮收敛：本地共享视图已等于远端文件，不再 GET/PUT 这个文件。
    let start = mock.requests().len();
    let second = engine.transport();
    assert!(second.ok, "second transport failed: {second:?}");
    assert_eq!(second.pulled, 0, "{second:?}");
    assert_eq!(second.pushed, 0, "{second:?}");
    assert!(second.conflicts.is_empty(), "{second:?}");
    assert_eq!(fs::read_to_string(rime.join(REL)).expect("local"), local);
    let touched: Vec<_> = mock.requests()[start..]
        .iter()
        .filter(|(_, path)| path == REMOTE_PATH)
        .cloned()
        .collect();
    assert!(touched.is_empty(), "{touched:?}");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn pulling_an_old_remote_keeps_local_schema_list() {
    // 只读策略（preview）：从旧格式远端拉取，本机 schema_list 不能被覆盖，
    // 也不能在只读轮次里改写远端。
    let same_page = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n  menu/page_size: 8\n";
    let mock = MockDav::start(vec![
        (PROFILE_MANIFEST, manifest_for(&[(REL, same_page)])),
        (REMOTE_PATH, same_page.to_vec()),
    ]);
    let (root, rime, state) = workspace("devkeys-pull");
    fs::write(rime.join(REL), LOCAL).expect("local");

    let mut config = Config::default();
    config.webdav.url = mock.url();
    config.webdav.username = "u".to_string();
    config.device_id = "devA".to_string();
    config.first_sync_policy = FirstSyncPolicy::Preview;
    config.device_local_keys = vec!["schema_list".to_string()];
    let mut engine = Engine::new(
        config,
        rime.clone(),
        state.clone(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine");

    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert_eq!(report.pushed, 0, "read-only policy must not push");
    assert!(
        write_methods(&mock.methods()).is_empty(),
        "{:?}",
        mock.methods()
    );

    // 本机的 schema_list 仍是自己的两个方案（远端那份不能覆盖它）。
    let local = fs::read_to_string(rime.join(REL)).expect("local");
    assert_eq!(schema_names(&local), vec!["luna_pinyin", "wubi86"]);
    assert_eq!(page_size(&local), 8);
    // 远端保持原样（只读）。
    assert_eq!(remote_body(&mock, REMOTE_PATH).as_bytes(), same_page);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn patch_merge_restores_local_keys_and_uploads_the_shared_view() {
    // 没有基线（升级后首轮）→ 段落级合并：共享键按 key 合并，设备私有键既不
    // 参与比较（不报冲突），也不会出现在上传内容里。
    let remote = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n  style/horizontal: true\n";
    let mock = MockDav::start(vec![
        (PROFILE_MANIFEST, manifest_for(&[(REL, remote)])),
        (REMOTE_PATH, remote.to_vec()),
    ]);
    let (root, rime, state) = workspace("devkeys-merge");
    fs::write(rime.join(REL), LOCAL).expect("local");

    let mut engine = engine(&mock, &rime, &state, &["schema_list"]);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("stripped 1 device-local key")),
        "{report:?}"
    );

    let local = fs::read_to_string(rime.join(REL)).expect("local");
    assert_eq!(schema_names(&local), vec!["luna_pinyin", "wubi86"]);
    assert_eq!(page_size(&local), 8);
    assert_eq!(
        patch(&local)
            .get("style/horizontal")
            .and_then(Value::as_bool),
        Some(true),
        "disjoint shared key from the remote must merge: {local}"
    );

    let remote = remote_body(&mock, REMOTE_PATH);
    assert!(!remote.contains("schema_list"), "{remote}");
    assert_eq!(page_size(&remote), 8);
    assert_eq!(
        patch(&remote)
            .get("style/horizontal")
            .and_then(Value::as_bool),
        Some(true),
        "{remote}"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn local_only_change_uploads_without_device_keys() {
    // 只有本机改了共享键（TakeLocal）：上传的是共享视图，本机文件保留 schema_list。
    let base = b"patch:\n  menu/page_size: 8\n";
    let remote = b"patch:\n  menu/page_size: 8\n";
    let local = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n  menu/page_size: 9\n";
    let mock = MockDav::start(vec![
        (PROFILE_MANIFEST, manifest_for(&[(REL, remote)])),
        (REMOTE_PATH, remote.to_vec()),
    ]);
    let (root, rime, state) = workspace("devkeys-push");
    fs::write(rime.join(REL), local).expect("local");
    seed_base(&state, base);

    let mut engine = engine(&mock, &rime, &state, &["schema_list"]);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert!(report.pushed >= 1, "{report:?}");

    let local = fs::read_to_string(rime.join(REL)).expect("local");
    assert_eq!(schema_names(&local), vec!["luna_pinyin", "wubi86"]);
    assert_eq!(page_size(&local), 9);

    let remote = remote_body(&mock, REMOTE_PATH);
    assert!(!remote.contains("schema_list"), "{remote}");
    assert_eq!(page_size(&remote), 9);

    fs::remove_dir_all(&root).ok();
}

#[test]
fn empty_device_local_keys_behaves_exactly_like_before() {
    // 回归：device_local_keys=[] 时 schema_list 仍按并集合并，不做任何剥离。
    let remote = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n  menu/page_size: 8\n";
    let mock = MockDav::start(vec![
        (PROFILE_MANIFEST, manifest_for(&[(REL, remote)])),
        (REMOTE_PATH, remote.to_vec()),
    ]);
    let (root, rime, state) = workspace("devkeys-empty");
    fs::write(rime.join(REL), LOCAL).expect("local");

    let mut engine = engine(&mock, &rime, &state, &[]);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert!(
        report.notes.iter().any(|note| note.contains("set union")),
        "empty scope must keep the schema_list union: {report:?}"
    );
    assert!(
        !report
            .notes
            .iter()
            .any(|note| note.contains("device-local")),
        "{report:?}"
    );

    let local = fs::read_to_string(rime.join(REL)).expect("local");
    assert_eq!(schema_names(&local), vec!["luna_pinyin", "wubi86"]);
    let remote = remote_body(&mock, REMOTE_PATH);
    assert!(remote.contains("schema_list"), "{remote}");
    assert_eq!(
        schema_names(&remote),
        vec!["luna_pinyin", "wubi86"],
        "{remote}"
    );

    fs::remove_dir_all(&root).ok();
}
