//! 迭代 1：同步预览（dry-run）。
//!
//! 两条硬约束：
//! 1) 预览必须零副作用（不发 PUT/DELETE/MKCOL、不写本地、不动 index.json）；
//! 2) 预览说要做的事，就是随后真实 transport 实际做的事（动作一致）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::{Engine, PlanKind, PlanReport, Report};
use rime_sync_core::platform::FakeTrigger;

mod support;

use support::{manifest_for, temp_dir, MockDav};

const LOCAL_DEV: &str = "sync/devA/main.userdb.txt";
const REMOTE_DEV: &str = "sync/other-dev/main.userdb.txt";
const REMOTE_EXTRA: &str = "sync/other-dev/extra.userdb.txt";
const NOTES: &str = "sync/devA/notes.txt";
const SHARED: &str = "default.custom.yaml";
const CONFLICT: &str = "conflict.custom.yaml";
const DICT: &str = "custom.dict.yaml";
/// 0.6.4：配置进本机 profile，词库源仍走 shared。
const PROFILE_MANIFEST: &str = "sync/devA/config/manifest.json";
const PROFILE_SHARED: &str = "sync/devA/config/files/default.custom.yaml";
const PROFILE_CONFLICT: &str = "sync/devA/config/files/conflict.custom.yaml";

const YAML_BASE: &[u8] = b"patch:\n  a: 1\n";
const YAML_LOCAL: &[u8] = b"patch:\n  a: 2\n";
/// patch 不是映射：段落级合并不可用，只能回落文件级冲突。
const CONFLICT_LOCAL: &[u8] = b"patch: 1\n";
const CONFLICT_REMOTE: &[u8] = b"patch: 2\n";
const DICT_REMOTE: &[u8] = b"# only remote\n";
const SNAPSHOT_LOCAL: &[u8] = b"local\t1\n";
const SNAPSHOT_LOCAL_NEW: &[u8] = b"local\t2\n";
const SNAPSHOT_REMOTE: &[u8] = b"remote\t1\n";
const SNAPSHOT_EXTRA: &[u8] = b"extra\t1\n";

struct Scene {
    mock: MockDav,
    root: std::path::PathBuf,
    rime: std::path::PathBuf,
    state: std::path::PathBuf,
}

fn plan_engine_with(mock: &MockDav, rime: &Path, state: &Path, policy: FirstSyncPolicy) -> Engine {
    let mut config = Config::default();
    config.webdav.url = mock.url();
    config.webdav.username = "u".to_string();
    config.device_id = "devA".to_string();
    // 关闭词表同步：既能看到 push/pull，也能看到「因策略跳过」。
    config.first_sync_policy = policy;
    config.sync_kinds.dict_sources = false;
    Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine")
}

fn plan_engine(mock: &MockDav, rime: &Path, state: &Path) -> Engine {
    plan_engine_with(mock, rime, state, FirstSyncPolicy::Normal)
}

fn scene(tag: &str) -> Scene {
    let mock = MockDav::start(vec![
        (REMOTE_DEV, SNAPSHOT_REMOTE.to_vec()),
        ("shared/manifest.json", manifest_for(&[(DICT, DICT_REMOTE)])),
        ("shared/files/custom.dict.yaml", DICT_REMOTE.to_vec()),
        (PROFILE_MANIFEST, manifest_for(&[(SHARED, YAML_BASE)])),
        (PROFILE_SHARED, YAML_BASE.to_vec()),
    ]);
    let root = temp_dir(tag);
    let rime = root.join("rime");
    fs::create_dir_all(rime.join("sync/devA")).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    fs::write(rime.join(LOCAL_DEV), SNAPSHOT_LOCAL).expect("snapshot");
    fs::write(rime.join(SHARED), YAML_BASE).expect("shared");
    fs::write(rime.join(NOTES), b"keep\n").expect("notes");
    Scene {
        mock,
        root,
        rime,
        state,
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 先真实同步一次，得到一个「已经同步过」的基线（index.json 存在）。
fn baseline(scene: &Scene) -> Report {
    let report = plan_engine(&scene.mock, &scene.rime, &scene.state).transport();
    assert!(report.ok, "baseline transport failed: {report:?}");
    report
}

/// 制造待同步的改动：本地改两个文件、远端多一个快照和一个冲突文件。
fn prepare_changes(scene: &Scene) {
    fs::write(scene.rime.join(SHARED), YAML_LOCAL).expect("local shared");
    fs::write(scene.rime.join(LOCAL_DEV), SNAPSHOT_LOCAL_NEW).expect("local snapshot");
    fs::write(scene.rime.join(CONFLICT), CONFLICT_LOCAL).expect("local conflict");
    scene.mock.set(REMOTE_EXTRA, SNAPSHOT_EXTRA.to_vec());
    scene.mock.set(PROFILE_CONFLICT, CONFLICT_REMOTE.to_vec());
    scene.mock.set(
        PROFILE_MANIFEST,
        manifest_for(&[(SHARED, YAML_BASE), (CONFLICT, CONFLICT_REMOTE)]),
    );
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("prefix")
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            out.insert(format!("{rel}/"), Vec::new());
            walk(root, &path, out);
        } else {
            out.insert(rel, fs::read(&path).unwrap_or_default());
        }
    }
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn actions_of(plan: &PlanReport, kind: PlanKind) -> Vec<&str> {
    plan.actions
        .iter()
        .filter(|action| action.kind == kind)
        .map(|action| action.path.as_str())
        .collect()
}

#[test]
fn plan_has_zero_side_effects() {
    let scene = scene("plan-zero");
    baseline(&scene);
    prepare_changes(&scene);

    let before_local = tree(&scene.rime);
    let before_state = tree(&scene.state);
    let before_remote = scene.mock.snapshot();
    let before_requests = scene.mock.requests().len();

    let plan = plan_engine(&scene.mock, &scene.rime, &scene.state).plan();

    assert!(plan.ok, "plan failed: {plan:?}");
    assert!(plan.dry_run, "plan must advertise itself as dry-run");
    assert!(
        !plan.actions.is_empty(),
        "the scenario has pending work, the plan must not be empty"
    );

    // 远端零写：没有 PUT / DELETE / MKCOL，文件集合一个字节都没变。
    let requests = scene.mock.requests();
    let writes: Vec<(String, String)> = requests
        .iter()
        .skip(before_requests)
        .filter(|(method, _)| !matches!(method.as_str(), "PROPFIND" | "GET" | "HEAD"))
        .cloned()
        .collect();
    assert!(writes.is_empty(), "plan issued write requests: {writes:?}");
    assert_eq!(
        scene.mock.snapshot(),
        before_remote,
        "remote must not change"
    );

    // 本地零写：文件集合与内容一一相同（含冲突副本不能出现）。
    assert_eq!(
        tree(&scene.rime),
        before_local,
        "local files must not change"
    );

    // 状态目录零写：index.json / base 缓存 / history / retry 全都不动。
    assert_eq!(
        tree(&scene.state),
        before_state,
        "state dir must not change (index.json included)"
    );

    // 计划本身要能说明「会发生什么」。
    assert!(
        actions_of(&plan, PlanKind::Pull).contains(&REMOTE_EXTRA),
        "new remote snapshot must be planned as pull: {plan:?}"
    );
    assert!(
        actions_of(&plan, PlanKind::Push).contains(&LOCAL_DEV),
        "changed local snapshot must be planned as push: {plan:?}"
    );
    assert!(
        actions_of(&plan, PlanKind::Push).contains(&PROFILE_SHARED),
        "local-only shared change must be planned as push: {plan:?}"
    );
    assert!(
        actions_of(&plan, PlanKind::Conflict).contains(&CONFLICT),
        "both sides changed must be planned as conflict: {plan:?}"
    );
    assert!(
        actions_of(&plan, PlanKind::Skip).contains(&DICT),
        "disabled kind must be planned as skip: {plan:?}"
    );
    assert_eq!(plan.counts.pull, actions_of(&plan, PlanKind::Pull).len());
    assert_eq!(plan.counts.push, actions_of(&plan, PlanKind::Push).len());
    assert_eq!(plan.total, plan.actions.len());
}

/// SPEC-4 §A1：只读策略下预览也只能出现拉取/跳过，绝不能承诺推送。
#[test]
fn plan_respects_pull_only_policy() {
    let scene = scene("plan-pull-only");
    prepare_changes(&scene);

    let before_requests = scene.mock.requests().len();
    let plan = plan_engine_with(
        &scene.mock,
        &scene.rime,
        &scene.state,
        FirstSyncPolicy::Preview,
    )
    .plan();

    assert!(plan.ok, "plan failed: {plan:?}");
    assert_eq!(plan.mode, "preview");
    assert!(plan.first_sync, "remote data without index is a first sync");
    assert!(
        actions_of(&plan, PlanKind::Push).is_empty(),
        "read-only policy must never plan a push: {plan:?}"
    );
    assert!(
        actions_of(&plan, PlanKind::DeleteRemote).is_empty(),
        "read-only policy must never plan a remote delete: {plan:?}"
    );
    assert_eq!(plan.counts.push, 0);
    assert_eq!(plan.counts.delete_remote, 0);
    assert!(
        !actions_of(&plan, PlanKind::Pull).is_empty(),
        "read-only policy still pulls: {plan:?}"
    );

    let requests = scene.mock.requests();
    let writes: Vec<(String, String)> = requests
        .iter()
        .skip(before_requests)
        .filter(|(method, _)| !matches!(method.as_str(), "PROPFIND" | "GET" | "HEAD"))
        .cloned()
        .collect();
    assert!(writes.is_empty(), "plan issued write requests: {writes:?}");
}

#[test]
fn plan_actions_match_real_transport() {
    let scene = scene("plan-match");
    baseline(&scene);
    prepare_changes(&scene);

    let plan = plan_engine(&scene.mock, &scene.rime, &scene.state).plan();
    assert!(plan.ok, "plan failed: {plan:?}");

    let before_requests = scene.mock.requests().len();
    let report = plan_engine(&scene.mock, &scene.rime, &scene.state).transport();
    assert!(report.ok, "transport failed: {report:?}");

    let requests = scene.mock.requests();
    let puts: BTreeSet<String> = requests
        .iter()
        .skip(before_requests)
        .filter(|(method, _)| method == "PUT")
        .map(|(_, path)| path.clone())
        .collect();

    // 预览说 push 的，执行后确实是一次 PUT。
    let planned_pushes = actions_of(&plan, PlanKind::Push);
    assert!(
        !planned_pushes.is_empty(),
        "scenario must plan at least one push: {plan:?}"
    );
    for path in &planned_pushes {
        assert!(
            puts.contains(*path),
            "plan promised push {path}, actual PUTs: {puts:?}"
        );
    }

    // 预览说 pull 的，执行后本地确实有这份文件。
    let planned_pulls = actions_of(&plan, PlanKind::Pull);
    assert!(planned_pulls.contains(&REMOTE_EXTRA), "{plan:?}");
    for path in &planned_pulls {
        let local = scene.rime.join(path);
        assert!(
            local.is_file(),
            "plan promised pull {path}, but {} is missing",
            local.display()
        );
    }

    // 预览说跳过的，执行后依然没有被上传/落盘。
    for path in actions_of(&plan, PlanKind::Skip) {
        assert!(
            !puts.contains(path),
            "skipped path {path} must not be uploaded"
        );
    }
    assert!(
        !scene.rime.join(DICT).exists(),
        "disabled kind must not be downloaded"
    );

    // 预览说冲突的，执行后仍然是冲突：远端字节没被覆盖，本地原文件保留。
    assert!(
        report.conflicts.iter().any(|item| item == CONFLICT),
        "real transport must report the planned conflict: {report:?}"
    );
    assert_eq!(
        scene
            .mock
            .snapshot()
            .get(PROFILE_CONFLICT)
            .map(Vec::as_slice),
        Some(CONFLICT_REMOTE),
        "conflict must not be pushed over the remote copy"
    );
    assert_eq!(
        fs::read(scene.rime.join(CONFLICT)).expect("local conflict file"),
        CONFLICT_LOCAL,
        "local file must survive until the user resolves the conflict"
    );
}
