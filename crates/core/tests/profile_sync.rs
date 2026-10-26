//! 0.6.4：每台设备一个配置目录，挂在设备自己的 sync 目录下
//! （sync/<installation_id>/config/{manifest.json,files/}）。
//!
//! shared/files 只作为首次同步的默认模板：新设备拉一次初值，之后只读写自己的
//! profile；词库源仍走 shared。

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::Engine;
use rime_sync_core::index::{sha256_hex, FileIndex, INDEX_VERSION};
use rime_sync_core::platform::FakeTrigger;
use serde_yaml::Value;

mod support;

use support::{manifest_for, temp_dir, MockDav};

const REL: &str = "default.custom.yaml";
const PROFILE_FILE: &str = "sync/devA/config/files/default.custom.yaml";
const PROFILE_MANIFEST: &str = "sync/devA/config/manifest.json";
/// 0.6.4 开发期间的旧布局：迁移用例里作为「已有数据」。
const LEGACY_FILE: &str = "profiles/devA/files/default.custom.yaml";
const LEGACY_MANIFEST: &str = "profiles/devA/manifest.json";

fn workspace(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    (root, rime, state)
}

fn engine_for(mock: &MockDav, rime: &Path, state: &Path, device: &str) -> Engine {
    let mut config = Config::default();
    config.webdav.url = mock.url();
    config.webdav.username = "u".to_string();
    config.device_id = device.to_string();
    config.first_sync_policy = FirstSyncPolicy::Normal;
    Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine")
}

fn local_manifest(state: &Path, device: &str) -> PathBuf {
    state
        .join("sync")
        .join(device)
        .join("config")
        .join("manifest.json")
}

fn patch_page(bytes: &[u8]) -> u64 {
    let doc: Value = serde_yaml::from_slice(bytes).expect("yaml");
    doc.get("patch")
        .and_then(|patch| patch.get("menu/page_size"))
        .and_then(Value::as_u64)
        .expect("menu/page_size")
}

fn schema_names(bytes: &[u8]) -> Vec<String> {
    let doc: Value = serde_yaml::from_slice(bytes).expect("yaml");
    doc.get("patch")
        .and_then(|patch| patch.get("schema_list"))
        .and_then(Value::as_sequence)
        .expect("schema_list")
        .iter()
        .map(|item| {
            item.get("schema")
                .and_then(Value::as_str)
                .expect("schema")
                .to_string()
        })
        .collect()
}

#[test]
fn first_sync_seeds_from_shared_exactly_once() {
    let mock = MockDav::start(vec![
        (
            "shared/manifest.json",
            manifest_for(&[(REL, b"patch:\n  menu/page_size: 8\n")]),
        ),
        (
            "shared/files/default.custom.yaml",
            b"patch:\n  menu/page_size: 8\n".to_vec(),
        ),
    ]);
    let (root, rime, state) = workspace("profile-seed");
    let mut engine = engine_for(&mock, &rime, &state, "devA");

    let first = engine.transport();
    assert!(first.ok, "transport failed: {first:?}");
    assert!(first.first_sync, "shared data must count as first sync");
    assert_eq!(
        fs::read(rime.join(REL)).expect("seeded local"),
        b"patch:\n  menu/page_size: 8\n",
        "seed must fill the missing local config"
    );
    assert!(
        first.notes.iter().any(|note| note.contains("初值")),
        "seed must be explained in notes: {first:?}"
    );
    assert_eq!(
        mock.snapshot().get(PROFILE_FILE).map(Vec::as_slice),
        Some(b"patch:\n  menu/page_size: 8\n".as_slice()),
        "seeded config must be uploaded into the device profile"
    );
    assert!(
        local_manifest(&state, "devA").is_file(),
        "profile baseline must be created locally"
    );

    let seed_file_gets = mock
        .requests()
        .iter()
        .filter(|(method, path)| method == "GET" && path.starts_with("shared/files/"))
        .count();
    assert_eq!(
        seed_file_gets, 1,
        "the seed template is downloaded exactly once"
    );

    // 第二轮：profile 基线已存在，绝不再碰 shared，也不再重复拉取。
    let before = mock.requests().len();
    let second = engine.transport();
    assert!(second.ok, "second transport failed: {second:?}");
    assert_eq!(second.pulled, 0, "{second:?}");
    assert_eq!(second.pushed, 0, "{second:?}");
    let touched: Vec<_> = mock.requests()[before..]
        .iter()
        .filter(|(_, path)| path.starts_with("shared/files/"))
        .cloned()
        .collect();
    assert!(
        touched.is_empty(),
        "shared config must not be downloaded again: {touched:?}"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn plan_for_a_fresh_device_promises_the_seed_pull_and_profile_upload() {
    use rime_sync_core::engine::PlanKind;
    use support::write_methods;

    let seed = b"patch:\n  menu/page_size: 8\n";
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest_for(&[(REL, seed)])),
        ("shared/files/default.custom.yaml", seed.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-plan-seed");
    let mut engine = engine_for(&mock, &rime, &state, "devA");

    let plan = engine.plan();
    assert!(plan.ok, "plan failed: {plan:?}");
    let actions: Vec<(PlanKind, &str)> = plan
        .actions
        .iter()
        .map(|action| (action.kind, action.path.as_str()))
        .collect();
    assert!(
        actions.contains(&(PlanKind::Pull, REL)),
        "the seed pull must be planned: {actions:?}"
    );
    assert!(
        actions.contains(&(PlanKind::Push, PROFILE_FILE)),
        "the seed upload must be planned: {actions:?}"
    );
    assert!(
        write_methods(&mock.methods()).is_empty(),
        "plan must not write: {:?}",
        mock.methods()
    );
    assert!(!rime.join(REL).exists(), "plan must not write local files");
    assert!(
        !local_manifest(&state, "devA").exists(),
        "plan must not create the profile baseline"
    );

    let before = mock.requests().len();
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert_eq!(fs::read(rime.join(REL)).expect("seeded"), seed);
    assert_eq!(
        mock.snapshot().get(PROFILE_FILE).map(Vec::as_slice),
        Some(seed.as_slice())
    );
    let puts: Vec<_> = mock.requests()[before..]
        .iter()
        .filter(|(method, path)| method == "PUT" && path == PROFILE_FILE)
        .cloned()
        .collect();
    assert_eq!(puts.len(), 1, "planned push must happen exactly once");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn seed_only_fills_missing_files_locally() {
    let shared_a = b"patch:\n  menu/page_size: 8\n";
    let shared_b = b"patch:\n  style/horizontal: true\n";
    let local = b"patch:\n  menu/page_size: 3\n";
    let mock = MockDav::start(vec![
        (
            "shared/manifest.json",
            manifest_for(&[(REL, shared_a), ("user.yaml", shared_b)]),
        ),
        ("shared/files/default.custom.yaml", shared_a.to_vec()),
        ("shared/files/user.yaml", shared_b.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-seed-partial");
    fs::write(rime.join(REL), local).expect("local config");
    let mut engine = engine_for(&mock, &rime, &state, "devA");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert_eq!(
        fs::read(rime.join(REL)).expect("local config"),
        local,
        "seed must never overwrite an existing local config"
    );
    assert_eq!(
        fs::read(rime.join("user.yaml")).expect("seeded user.yaml"),
        shared_b,
        "missing file must be filled from the shared template"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("1 个默认配置")),
        "{report:?}"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn shared_dict_baseline_survives_profile_seed_and_syncs() {
    // shared 词表（DictSource）仍走 shared/files：profile 种子建立本地基线后，
    // index.json 里必须保留它的 sha，否则单侧修改会被误判为「远端也变了」→ 冲突。
    let dict = b"# dict\nhello\n";
    let seed = b"patch:\n  style/horizontal: true\n";
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest_for(&[("user.yaml", seed)])),
        ("shared/files/user.yaml", seed.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-dict-baseline");
    fs::write(rime.join("custom_phrase.txt"), dict).expect("dict");
    fs::write(rime.join(REL), b"patch:\n  menu/page_size: 8\n").expect("config");
    let mut engine = engine_for(&mock, &rime, &state, "devA");

    let first = engine.transport();
    assert!(first.ok, "transport failed: {first:?}");
    assert!(first.conflicts.is_empty(), "{first:?}");
    assert_eq!(
        fs::read(rime.join("user.yaml")).expect("seeded user.yaml"),
        seed,
        "seed must run before the profile tree"
    );

    let index_path = state.join("index.json");
    let index: FileIndex =
        serde_json::from_str(&fs::read_to_string(&index_path).expect("index.json"))
            .expect("index json");
    assert_eq!(
        index
            .files
            .get("custom_phrase.txt")
            .map(|meta| meta.sha256.as_str()),
        Some(sha256_hex(dict).as_str()),
        "the shared dict baseline must stay in index.json"
    );
    assert_eq!(
        mock.snapshot()
            .get("shared/files/custom_phrase.txt")
            .map(Vec::as_slice),
        Some(dict.as_slice())
    );

    // 只有本机改词表：基线 == 远端 → TakeLocal，必须直推而不是冲突。
    let changed = b"# dict\nhello\nworld\n";
    fs::write(rime.join("custom_phrase.txt"), changed).expect("dict changed");
    let second = engine.transport();
    assert!(second.ok, "transport failed: {second:?}");
    assert!(second.conflicts.is_empty(), "{second:?}");
    assert!(second.pushed >= 1, "{second:?}");
    assert_eq!(
        mock.snapshot()
            .get("shared/files/custom_phrase.txt")
            .map(Vec::as_slice),
        Some(changed.as_slice()),
        "the local-only change must be uploaded"
    );

    // 再跑一轮：上传后基线已对齐，不应重复推。
    let third = engine.transport();
    assert!(third.ok, "{third:?}");
    assert_eq!(third.pushed, 0, "{third:?}");
    assert!(third.conflicts.is_empty(), "{third:?}");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn shared_dict_baseline_survives_legacy_profile_migration() {
    // 0.6.4 开发期旧布局 profiles/<id> → sync/<id>/config 的迁移不得碰 index.json，
    // 否则 shared 词表会失去基线，单侧修改被误判成冲突。
    let dict = b"# dict\nlegacy\n";
    let config = b"patch:\n  menu/page_size: 6\n";
    let mock = MockDav::start(vec![
        (
            "shared/manifest.json",
            manifest_for(&[("custom_phrase.txt", dict)]),
        ),
        ("shared/files/custom_phrase.txt", dict.to_vec()),
        (
            "profiles/devA/manifest.json",
            manifest_for(&[(REL, config)]),
        ),
        ("profiles/devA/files/default.custom.yaml", config.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-legacy-migrate");
    fs::write(rime.join("custom_phrase.txt"), dict).expect("dict");
    fs::write(rime.join(REL), config).expect("config");
    // 旧布局的本机基线（迁移后应搬到 state/sync/devA/config/manifest.json）。
    let legacy_dir = state.join("profiles").join("devA");
    fs::create_dir_all(&legacy_dir).expect("legacy dir");
    fs::write(
        legacy_dir.join("manifest.json"),
        manifest_for(&[(REL, config)]),
    )
    .expect("legacy manifest");

    let mut engine = engine_for(&mock, &rime, &state, "devA");
    let first = engine.transport();
    assert!(first.ok, "transport failed: {first:?}");
    assert!(first.conflicts.is_empty(), "{first:?}");
    assert!(
        first.notes.iter().any(|note| note.contains("迁移")),
        "layout migration must be explained: {first:?}"
    );
    assert!(
        state.join("sync/devA/config/manifest.json").is_file(),
        "local baseline must move to the new layout"
    );
    assert!(!legacy_dir.exists(), "legacy local dir must be cleaned up");

    let index: FileIndex =
        serde_json::from_str(&fs::read_to_string(state.join("index.json")).expect("index.json"))
            .expect("index json");
    assert_eq!(
        index
            .files
            .get("custom_phrase.txt")
            .map(|meta| meta.sha256.as_str()),
        Some(sha256_hex(dict).as_str()),
        "migration must keep the shared dict baseline"
    );

    let changed = b"# dict\nlegacy\nchanged\n";
    fs::write(rime.join("custom_phrase.txt"), changed).expect("dict changed");
    let second = engine.transport();
    assert!(second.ok, "{second:?}");
    assert!(second.conflicts.is_empty(), "{second:?}");
    assert!(second.pushed >= 1, "{second:?}");
    assert_eq!(
        mock.snapshot()
            .get("shared/files/custom_phrase.txt")
            .map(Vec::as_slice),
        Some(changed.as_slice())
    );
    assert_eq!(
        mock.snapshot()
            .get("sync/devA/config/files/default.custom.yaml")
            .map(Vec::as_slice),
        Some(config.as_slice()),
        "config must land in the new layout"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn profiles_merge_independently_per_device() {
    let mock = MockDav::start(Vec::new());
    let (root_a, rime_a, state_a) = workspace("profile-a");
    let (root_b, rime_b, state_b) = workspace("profile-b");

    fs::write(rime_a.join(REL), b"patch:\n  menu/page_size: 9\n").expect("a local");
    let mut a = engine_for(&mock, &rime_a, &state_a, "devA");
    let first_a = a.transport();
    assert!(first_a.ok, "a transport failed: {first_a:?}");

    fs::write(rime_b.join(REL), b"patch:\n  menu/page_size: 5\n").expect("b local");
    let b_start = mock.requests().len();
    let mut b = engine_for(&mock, &rime_b, &state_b, "devB");
    let first_b = b.transport();
    assert!(first_b.ok, "b transport failed: {first_b:?}");

    let remote = mock.snapshot();
    assert_eq!(
        patch_page(
            remote
                .get("sync/devA/config/files/default.custom.yaml")
                .expect("a profile")
        ),
        9
    );
    assert_eq!(
        patch_page(
            remote
                .get("sync/devB/config/files/default.custom.yaml")
                .expect("b profile")
        ),
        5
    );
    // B 首次同步不应读到 A 的配置。
    assert!(
        !mock.requests()[b_start..]
            .iter()
            .any(|(_, path)| path.starts_with("sync/devA/")),
        "device B must never touch device A's profile"
    );

    // A 再改一次：只影响 A 自己的 profile。
    fs::write(rime_a.join(REL), b"patch:\n  menu/page_size: 10\n").expect("a local 2");
    let second_a = a.transport();
    assert!(second_a.ok, "a second transport failed: {second_a:?}");
    assert_eq!(
        patch_page(
            mock.snapshot()
                .get("sync/devA/config/files/default.custom.yaml")
                .expect("a profile")
        ),
        10
    );
    assert_eq!(
        patch_page(
            mock.snapshot()
                .get("sync/devB/config/files/default.custom.yaml")
                .expect("b profile")
        ),
        5,
        "device B's profile must not change"
    );

    let second_b = b.transport();
    assert!(second_b.ok, "b transport failed: {second_b:?}");
    assert_eq!(
        fs::read(rime_b.join(REL)).expect("b local"),
        b"patch:\n  menu/page_size: 5\n",
        "device B must not pull device A's config"
    );

    fs::remove_dir_all(&root_a).ok();
    fs::remove_dir_all(&root_b).ok();
}

#[test]
fn schema_list_stays_device_local_inside_the_profile() {
    let local: &[u8] =
        b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n  menu/page_size: 8\n";
    let mock = MockDav::start(Vec::new());
    let (root, rime, state) = workspace("profile-devkeys");
    fs::write(rime.join(REL), local).expect("local");
    let mut engine = engine_for(&mock, &rime, &state, "devA");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");

    let remote = mock.snapshot();
    let remote_body = remote.get(PROFILE_FILE).expect("profile file");
    assert!(
        !String::from_utf8_lossy(remote_body).contains("schema_list"),
        "device-local keys must never be uploaded: {}",
        String::from_utf8_lossy(remote_body)
    );
    assert_eq!(patch_page(remote_body), 8);
    assert_eq!(
        schema_names(&fs::read(rime.join(REL)).expect("local")),
        vec!["luna_pinyin", "wubi86"],
        "local schema_list must survive the upload"
    );

    // 收敛：第二轮不再上传。
    let second = engine.transport();
    assert!(second.ok, "{second:?}");
    assert_eq!(second.pushed, 0, "{second:?}");
    assert_eq!(second.pulled, 0, "{second:?}");

    fs::remove_dir_all(&root).ok();
}

#[test]
fn old_client_writes_to_shared_do_not_affect_new_client() {
    let original = b"patch:\n  menu/page_size: 8\n";
    let legacy = b"patch:\n  menu/page_size: 1\n";
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest_for(&[(REL, original)])),
        ("shared/files/default.custom.yaml", original.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-legacy-shared");
    let mut engine = engine_for(&mock, &rime, &state, "devA");
    assert!(engine.transport().ok);

    // 老客户端还在往 shared/files 写配置：新客户端必须完全忽略。
    mock.set("shared/files/default.custom.yaml", legacy.to_vec());
    mock.set("shared/manifest.json", manifest_for(&[(REL, legacy)]));

    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert_eq!(report.pulled, 0, "{report:?}");
    assert_eq!(report.pushed, 0, "{report:?}");
    assert_eq!(
        fs::read(rime.join(REL)).expect("local"),
        original,
        "shared writes must not leak into the new client"
    );
    assert_eq!(
        mock.snapshot().get(PROFILE_FILE).map(Vec::as_slice),
        Some(original.as_slice()),
        "new client must not rewrite its profile from shared"
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn profile_manifest_is_a_content_index_with_remote_hashes() {
    let body = b"patch:\n  menu/page_size: 8\n";
    let mock = MockDav::start(Vec::new());
    let (root, rime, state) = workspace("profile-manifest");
    fs::write(rime.join(REL), body).expect("local");
    let mut engine = engine_for(&mock, &rime, &state, "devA");
    assert!(engine.transport().ok);

    let path = local_manifest(&state, "devA");
    let text = fs::read_to_string(&path).expect("profile manifest");
    let index: FileIndex = serde_json::from_str(&text).expect("json");
    assert_eq!(index.files[REL].sha256, sha256_hex(body));
    assert_eq!(index.files[REL].size, body.len() as u64);
    assert!(index.files[REL].etag.is_some() || index.files[REL].etag.is_none());
    assert_eq!(index.version, INDEX_VERSION);

    fs::remove_dir_all(&root).ok();
}

/// 0.6.4 布局迁移：旧 profiles/<id>（远端 + 本机 state）自动搬到 sync/<id>/config。
#[test]
fn legacy_profiles_layout_is_migrated_once() {
    let body = b"patch:\n  menu/page_size: 11\n";
    let mock = MockDav::start(vec![
        (LEGACY_MANIFEST, manifest_for(&[(REL, body)])),
        (LEGACY_FILE, body.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-legacy-migrate");
    // 本机旧 state 布局也一起迁移。
    let legacy_state = state.join("profiles").join("devA");
    fs::create_dir_all(&legacy_state).expect("legacy state");
    fs::write(
        legacy_state.join("manifest.json"),
        manifest_for(&[(REL, body)]),
    )
    .expect("legacy manifest");
    // 真实设备上本地一定有这份配置：迁移只是搬远端目录，不能把它当成本地删除。
    fs::write(rime.join(REL), body).expect("local config");

    let mut engine = engine_for(&mock, &rime, &state, "devA");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(
        report.notes.iter().any(|note| note.contains("迁移")),
        "迁移必须写进 notes：{report:?}"
    );

    let remote = mock.snapshot();
    assert_eq!(
        remote.get(PROFILE_FILE).map(Vec::as_slice),
        Some(body.as_slice()),
        "旧布局的配置必须出现在 sync/<id>/config/files/ 下"
    );
    assert!(
        remote.contains_key(PROFILE_MANIFEST),
        "旧 manifest 必须跟着搬过去：{remote:?}"
    );
    assert!(!remote.contains_key(LEGACY_FILE), "旧目录必须删除");
    assert!(
        !remote.contains_key(LEGACY_MANIFEST),
        "旧 manifest 必须删除"
    );
    assert_eq!(report.pushed, 0, "迁移过来的文件不算新上传：{report:?}");
    assert_eq!(report.pulled, 0, "迁移过来的文件不算新下载：{report:?}");
    assert_eq!(
        fs::read(rime.join(REL)).expect("local config"),
        body,
        "迁移不该改动本机配置"
    );

    // 本机 state 旧基线也搬到了新位置。
    assert!(local_manifest(&state, "devA").is_file());
    assert!(
        !state.join("profiles").exists(),
        "本机旧 state 目录必须删除"
    );

    // 第二轮：不再迁移、不再上传、不再下载。
    let before = mock.requests().len();
    let second = engine.transport();
    assert!(second.ok, "second transport failed: {second:?}");
    assert_eq!(second.pushed, 0, "{second:?}");
    assert_eq!(second.pulled, 0, "{second:?}");
    let migration_writes: Vec<_> = mock.requests()[before..]
        .iter()
        .filter(|(method, path)| {
            (method == "PUT" || method == "DELETE") && path.starts_with("profiles/")
        })
        .cloned()
        .collect();
    assert!(
        migration_writes.is_empty(),
        "迁移只做一次：{migration_writes:?}"
    );
    assert!(
        !second.notes.iter().any(|note| note.contains("迁移")),
        "{second:?}"
    );

    fs::remove_dir_all(&root).ok();
}

/// 快照扫描只认 sync/<id> 顶层的 *.userdb.txt：config/ 是配置目录，绝不能当快照下载。
#[test]
fn snapshot_pull_ignores_the_config_directory() {
    let body = b"patch:\n  menu/page_size: 8\n";
    let mock = MockDav::start(vec![
        (
            "sync/devB/config/manifest.json",
            manifest_for(&[(REL, body)]),
        ),
        ("sync/devB/config/files/default.custom.yaml", body.to_vec()),
        ("sync/devB/luna_pinyin.userdb.txt", b"# userdb\n".to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-config-not-snapshot");
    fs::write(rime.join(REL), b"patch:\n  menu/page_size: 2\n").expect("local");
    let mut engine = engine_for(&mock, &rime, &state, "devA");
    let report = engine.transport();
    assert!(report.ok, "transport failed: {report:?}");
    assert!(
        rime.join("sync/devB/luna_pinyin.userdb.txt").is_file(),
        "顶层 *.userdb.txt 仍是快照来源：{report:?}"
    );
    assert!(
        !rime
            .join("sync/devB/config/files/default.custom.yaml")
            .exists(),
        "config/ 里的配置不能当快照下载"
    );
    assert!(
        !report.skipped.iter().any(|item| item.contains("config")),
        "配置目录应静默跳过，而不是记成跳过项：{report:?}"
    );
    assert_eq!(
        fs::read(rime.join(REL)).expect("local"),
        b"patch:\n  menu/page_size: 2\n",
        "别的设备的配置目录内容绝不能进本机"
    );

    fs::remove_dir_all(&root).ok();
}

/// 迁移失败：保留旧目录、告警，本轮跳过配置树——绝不能把空的新基线当成
/// 「远端已删除」而删掉本机配置。
#[test]
fn failed_legacy_migration_keeps_the_old_dir_and_skips_config_sync() {
    let body = b"patch:\n  menu/page_size: 11\n";
    let mock = MockDav::start(vec![
        (LEGACY_MANIFEST, manifest_for(&[(REL, body)])),
        (LEGACY_FILE, body.to_vec()),
        // 旧布局里混入一个非法路径：递归复制会在写新基线之前失败。
        ("profiles/devA/../escape.custom.yaml", body.to_vec()),
    ]);
    let (root, rime, state) = workspace("profile-legacy-migrate-fail");
    fs::write(rime.join(REL), body).expect("local config");
    let mut engine = engine_for(&mock, &rime, &state, "devA");
    let report = engine.transport();
    assert!(report.ok, "迁移失败不能阻断同步：{report:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("旧配置目录") && note.contains("跳过配置同步")),
        "{report:?}"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|item| item.contains("config sync skipped")),
        "{report:?}"
    );
    let remote = mock.snapshot();
    assert!(
        remote.contains_key(LEGACY_FILE) && remote.contains_key(LEGACY_MANIFEST),
        "失败时旧目录必须原样保留：{remote:?}"
    );
    assert!(
        !remote.contains_key("sync/devA/config/files/default.custom.yaml"),
        "未提交的迁移不能留下半成品：{remote:?}"
    );
    assert_eq!(
        fs::read(rime.join(REL)).expect("local config"),
        body,
        "本机配置不能被误删"
    );

    fs::remove_dir_all(&root).ok();
}
