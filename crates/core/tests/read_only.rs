//! SPEC-4 §A1 收尾：policy != normal 时永远只读（每一轮），且只读轮次也要把 shared 拉到本地。
//!
//! MockDav 在 tests/support/mod.rs；预览测试 (tests/plan_preview.rs) 复用同一份。

use std::fs;
use std::path::Path;

use rime_sync_core::config::FirstSyncPolicy;
use rime_sync_core::engine::Report;

mod support;

use support::{
    engine_for, engine_for_with_device_local, manifest_for, temp_dir, write_methods, MockDav,
};

const SNAPSHOT: &[u8] = b"a\t1\nb\t2\n";
const SHARED_REL: &str = "default.custom.yaml";
const SHARED_BODY: &[u8] = b"patch:\n  a: 1\n";
const LOCAL_SHARED_BODY: &[u8] = b"patch:\n  a: 2\n";

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
        ("sync/devA/config/manifest.json", manifest),
        (
            "sync/devA/config/files/default.custom.yaml",
            SHARED_BODY.to_vec(),
        ),
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

// ---- *.custom.yaml 规范化比较：metadata 变化不再算冲突 ----

/// Weasel 改设置后重写的文件头（patch 完全一样，只有 customization 不同）。
const META_LOCAL: &[u8] = b"# Rime settings\npatch:\n  \"menu/page_size\": 9\ncustomization:\n  distribution_code_name: Weasel\n  distribution_version: \"0.16.3\"\n  modified_time: \"Tue Oct 21 10:00:00 2025\"\n";
const META_REMOTE: &[u8] = b"# Rime settings\npatch:\n  \"menu/page_size\": 9\ncustomization:\n  distribution_code_name: Weasel\n  distribution_version: \"0.17.0\"\n  modified_time: \"Wed Oct 22 11:11:11 2025\"\n";
const PATCH_LOCAL: &[u8] = b"patch:\n  menu/page_size: 9\n";
const PATCH_REMOTE: &[u8] = b"patch:\n  menu/page_size: 6\n";

fn conflict_copies(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".conflict-"))
        .collect()
}

#[test]
fn metadata_only_custom_yaml_is_adopted_without_conflict() {
    let manifest = manifest_for(&[(SHARED_REL, META_REMOTE)]);
    let mock = MockDav::start(vec![
        ("sync/devA/config/manifest.json", manifest),
        (
            "sync/devA/config/files/default.custom.yaml",
            META_REMOTE.to_vec(),
        ),
    ]);
    let root = temp_dir("meta-only");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    fs::write(rime.join(SHARED_REL), META_LOCAL).expect("local custom");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let first = engine.transport();
    assert!(first.ok, "transport failed: {first:?}");
    assert!(
        first.conflicts.is_empty(),
        "metadata-only difference must not conflict: {first:?}"
    );
    assert!(
        conflict_copies(&rime).is_empty(),
        "no conflict copy may be produced"
    );
    assert!(
        first.config_changed,
        "adopting a custom yaml must ask for a redeploy: {first:?}"
    );
    // 本地被远端字节完全覆盖：下一轮整文件一致。
    assert_eq!(
        fs::read(rime.join(SHARED_REL)).expect("local custom"),
        META_REMOTE
    );

    let second = engine.transport();
    assert!(second.ok, "second transport failed: {second:?}");
    assert!(
        second.conflicts.is_empty(),
        "the bug was a conflict on every round: {second:?}"
    );
    assert!(
        !second.config_changed,
        "nothing new to deploy on the second round: {second:?}"
    );
    assert_eq!(
        fs::read(rime.join(SHARED_REL)).expect("local custom"),
        META_REMOTE
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn differing_custom_yaml_patch_conflicts_with_file_and_keys() {
    let manifest = manifest_for(&[(SHARED_REL, PATCH_REMOTE)]);
    let mock = MockDav::start(vec![
        ("sync/devA/config/manifest.json", manifest),
        (
            "sync/devA/config/files/default.custom.yaml",
            PATCH_REMOTE.to_vec(),
        ),
    ]);
    let root = temp_dir("patch-conflict");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    fs::write(rime.join(SHARED_REL), PATCH_LOCAL).expect("local custom");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    let entry = report
        .conflicts
        .iter()
        .find(|item| item.contains(SHARED_REL))
        .unwrap_or_else(|| panic!("conflict entry must name the file: {report:?}"));
    assert!(
        entry.contains("patch.menu/page_size"),
        "conflict entry must name the patch key: {entry}"
    );
    assert!(
        !conflict_copies(&rime).is_empty(),
        "local copy must be preserved as a conflict copy"
    );
    assert!(
        fs::read_to_string(rime.join(SHARED_REL))
            .expect("local custom")
            .contains("menu/page_size: 9"),
        "local patch value wins until the user resolves it"
    );

    let _ = fs::remove_dir_all(&root);
}

// ---- schema_list 三向合并取并集：两台机器各自装的方案都不能丢 ----

const SCHEMA_BASE: &[u8] = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
const SCHEMA_LOCAL: &[u8] =
    b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n";
const SCHEMA_REMOTE: &[u8] =
    b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: cangjie\n";

fn schema_names(bytes: &[u8]) -> Vec<String> {
    let doc: serde_yaml::Value = serde_yaml::from_slice(bytes).expect("yaml");
    let list = doc
        .get("patch")
        .and_then(|patch| patch.get("schema_list"))
        .and_then(|value| value.as_sequence())
        .expect("schema_list");
    list.iter()
        .map(|item| {
            item.get("schema")
                .and_then(|name| name.as_str())
                .expect("schema name")
                .to_string()
        })
        .collect()
}

#[test]
fn schema_list_union_is_written_locally_and_pushed_back() {
    let manifest = manifest_for(&[(SHARED_REL, SCHEMA_BASE)]);
    let mock = MockDav::start(vec![
        ("sync/devA/config/manifest.json", manifest),
        (
            "sync/devA/config/files/default.custom.yaml",
            SCHEMA_BASE.to_vec(),
        ),
    ]);
    let root = temp_dir("schema-union");
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");

    // 先同步一轮，拿到 base=[luna_pinyin]。
    // device_local_keys=[] 是 0.6.3 之前的映射：schema_list 仍按集合并集处理。
    let mut engine =
        engine_for_with_device_local(&mock, &rime, &state, FirstSyncPolicy::Normal, &[]);
    let first = engine.transport();
    assert!(first.ok, "baseline transport failed: {first:?}");
    assert_eq!(
        fs::read(rime.join(SHARED_REL)).expect("baseline local"),
        SCHEMA_BASE
    );

    // 两台机器各加一个方案：本地 [luna, wubi]，远端 [luna, cangjie]。
    fs::write(rime.join(SHARED_REL), SCHEMA_LOCAL).expect("local schema list");
    mock.set(
        "sync/devA/config/files/default.custom.yaml",
        SCHEMA_REMOTE.to_vec(),
    );
    mock.set(
        "sync/devA/config/manifest.json",
        manifest_for(&[(SHARED_REL, SCHEMA_REMOTE)]),
    );

    let report = engine.transport();
    assert!(report.ok, "union transport failed: {report:?}");
    assert!(
        report.conflicts.is_empty(),
        "schema_list union must not conflict: {report:?}"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains(SHARED_REL) && note.contains("set union")),
        "the union must be recorded in the report: {report:?}"
    );

    let expected = ["luna_pinyin", "wubi86", "cangjie"];
    assert_eq!(
        schema_names(&fs::read(rime.join(SHARED_REL)).expect("local")),
        expected,
        "local file must keep both machines' schemas"
    );
    let remote = mock.snapshot();
    assert_eq!(
        schema_names(
            remote
                .get("sync/devA/config/files/default.custom.yaml")
                .expect("remote custom yaml")
        ),
        expected,
        "the union must be pushed back to the remote"
    );
    assert!(report.pushed >= 1, "union must be pushed: {report:?}");
    assert!(
        report.config_changed,
        "a merged custom yaml needs a redeploy: {report:?}"
    );

    let _ = fs::remove_dir_all(&root);
}
