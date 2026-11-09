//! 组合不变量测试：ime_state::ensure 是唯一状态机。
//!
//! 枚举 build 状态 × 权限 × 来源 × payload，断言四条不变量：
//! 1) 返回 Ok ⇒ 之后 assess().build_ok == true；
//! 2) 返回 Noop ⇒ 前后目录逐字节一致（日志文件除外）；
//! 3) 返回 Partial/Failed ⇒ detail 非空，且结论与 build_ok 不矛盾；
//! 4) 任何情况下都不得出现「result=Ok 但 assess 不通过」。
//!
//! 另加一条契约：result == Ok 时 actions 里必须有 changed=true 的一步。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rime_sync_core::ime_state::{self, Mode, Options, Outcome, OutcomeResult};

const SCHEMA: &str = "luna_pinyin_simp";
// 随包产物的真实形态：engine/processors 里带 numpad（小键盘归一化处理器）。
// 少了 numpad 的假产物会被「常驻小键盘判定」当成「还没重编译」→ 误报成 partial，
// 所以假数据必须与真实随包产物一致（让假数据更真，而不是放宽断言）。
const MARKED: &str = "patch:\n  rimesync_vnum: '['\n  rimesync_punct: ']'\nengine:\n  processors:\n    - ascii_composer\n    - rimesync_punct\n    - rimesync_vselect\n    - numpad\n    - selector\n";
const PLAIN: &str = "name: luna_pinyin_simp\nversion: '1.0'\n";
const DATA_SAMPLE: &str = "\
engine:
  processors:
    - ascii_composer
    - key_binder
    - speller
    - punctuator
    - selector
  translators:
    - script_translator
punctuator:
  import_preset: symbols
recognizer:
  import_preset: default
";

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 目录快照：相对路径 → 内容。日志文件不参与比较（Noop 也允许写日志）。
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if name.contains(".rimesync-prebuilt.log") {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(bytes) = fs::read(&path) {
                out.insert(relative, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-ime-matrix-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["payload/build", "payload/config", "rime/build", "data"] {
            fs::create_dir_all(root.join(dir)).expect("create fixture dir");
        }
        fs::write(
            root.join("payload/config/default.custom.yaml"),
            "patch:\n  schema_list: []\n",
        )
        .expect("write config");
        // data 目录里的方案源文件：可写时会被打补丁（本次改过 → Ok），
        // 只读时写入失败（→ Partial）；第二遍变成 unchanged（→ Noop）。
        fs::write(root.join("data/luna_pinyin.schema.yaml"), DATA_SAMPLE)
            .expect("write data schema");
        // wubi86 也必须在 data 里：config 门禁（default.custom.yaml 的
        // schema_list）要求 data 里有 wubi86，否则 default.custom.yaml 会被跳过，
        // 结果降级为 Partial（B-2 语义），可写场景就再也测不到 Ok。
        fs::write(root.join("data/wubi86.schema.yaml"), DATA_SAMPLE).expect("write wubi86 schema");
        Self { root }
    }

    fn rime(&self) -> PathBuf {
        self.root.join("rime")
    }

    fn payload(&self) -> PathBuf {
        self.root.join("payload")
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, text).expect("write fixture file");
    }

    fn chmod(&self, path: &Path, readonly: bool) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if readonly { 0o555 } else { 0o755 };
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
        }
        #[cfg(not(unix))]
        {
            let _ = (path, readonly);
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // 只读目录要先恢复权限才能删。
        self.chmod(&self.rime(), false);
        self.chmod(&self.data(), false);
        self.chmod(&self.root, false);
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 读权限实际生效吗（root 下 chmod 555 仍然可写，此时跳过依赖权限的断言）。
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Copy)]
enum BuildState {
    NoSchemaYaml,
    NoBin,
    StaleMarker,
    AllGood,
}

impl BuildState {
    /// 把用户目录摆成该 build 状态（payload 由 payload_full 决定）。
    fn apply(self, fixture: &Fixture) {
        match self {
            Self::NoSchemaYaml => {}
            Self::NoBin => fixture.write(&format!("rime/build/{SCHEMA}.schema.yaml"), MARKED),
            Self::StaleMarker => {
                fixture.write(&format!("rime/build/{SCHEMA}.schema.yaml"), PLAIN);
                fixture.write(&format!("rime/build/{SCHEMA}.prism.bin"), "prism");
            }
            Self::AllGood => {
                fixture.write(&format!("rime/build/{SCHEMA}.schema.yaml"), MARKED);
                fixture.write(&format!("rime/build/{SCHEMA}.prism.bin"), "prism");
            }
        }
    }
}

fn labels() -> Vec<(BuildState, &'static str)> {
    vec![
        (BuildState::NoSchemaYaml, "no-schema-yaml"),
        (BuildState::NoBin, "no-bin"),
        (BuildState::StaleMarker, "stale-marker"),
        (BuildState::AllGood, "all-good"),
    ]
}

fn fill_payload(fixture: &Fixture, full: bool) {
    fixture.write(&format!("payload/build/{SCHEMA}.schema.yaml"), MARKED);
    if full {
        fixture.write(&format!("payload/build/{SCHEMA}.prism.bin"), "prism");
    }
}

fn assess_now(fixture: &Fixture) -> ime_state::Assessment {
    let keep = ime_state::KeepSet::resolve(
        &fixture.rime(),
        &[fixture.data()],
        Some(fixture.payload().as_path()),
    );
    ime_state::assess(&fixture.rime(), &[fixture.data()], &keep)
}

/// 四条不变量 + Ok/actions 契约，返回结论供调用方统计覆盖。
fn check(
    label: &str,
    outcome: &Outcome,
    fixture: &Fixture,
    snapshot_before: &BTreeMap<String, Vec<u8>>,
) -> OutcomeResult {
    let assessment = assess_now(fixture);
    let result = outcome.result;

    // ① Ok ⇒ 之后 assess 通过。
    if result == OutcomeResult::Ok {
        assert!(
            assessment.build_ok,
            "[{label}] result=ok 但 assess 不通过：{:?} detail={}",
            assessment.schemas, outcome.detail
        );
    }
    // ② Noop ⇒ 前后逐字节一致。
    if result == OutcomeResult::Noop {
        let after = snapshot(&fixture.root);
        assert_eq!(
            &after, snapshot_before,
            "[{label}] result=noop 却改动了文件"
        );
    }
    // ③ Partial/Failed ⇒ detail 非空，且与 build_ok 不矛盾。
    if matches!(result, OutcomeResult::Partial | OutcomeResult::Failed) {
        assert!(
            !outcome.detail.trim().is_empty(),
            "[{label}] {result:?} 没有 detail"
        );
        if result == OutcomeResult::Partial {
            assert!(assessment.build_ok, "[{label}] partial 但 build 不可用");
        } else {
            assert!(!assessment.build_ok, "[{label}] failed 但 build 可用");
            assert!(!outcome.ok(), "[{label}] failed 却被判成功");
        }
    }
    // ④ 任何情况下都不得「Ok 但 assess 不通过」。
    assert!(
        !(result == OutcomeResult::Ok && !assessment.build_ok),
        "[{label}] 违反不变量 ④"
    );
    // 契约：Ok 必须真的做过事。
    if result == OutcomeResult::Ok {
        assert!(
            !outcome.actions.is_empty(),
            "[{label}] result=ok 但 actions 为空"
        );
        assert!(
            outcome.actions.iter().any(|action| action.changed),
            "[{label}] result=ok 但没有任何 changed=true 的 action：{:?}",
            outcome.actions
        );
    }
    // report.build_ok 必须与最终 assess 一致（报告不能自己推结论）。
    assert_eq!(
        outcome.report.build_ok, assessment.build_ok,
        "[{label}] report.build_ok 与 assess 不一致"
    );
    result
}

fn options(mode: Mode) -> Options {
    Options::new(mode)
        // 只用传入的 data 目录：绝不碰真机上的 Weasel / /usr/share/rime-data。
        .with_auto_data_patch(false)
        .with_patch_data(true)
        .with_version(Some("0.0.0-matrix".to_string()))
}

#[test]
fn matrix_invariants_hold_for_every_combination() {
    let mut combos = 0;
    let mut seen_ok = 0;
    let mut seen_noop = 0;
    let mut seen_partial = 0;
    let mut seen_failed = 0;
    let mut readonly_effective = 0;

    for (state, state_label) in labels() {
        for full in [true, false] {
            for readonly in [false, true] {
                for mode in [Mode::Install, Mode::Fix, Mode::Bootstrap] {
                    let label = format!(
                        "state={state_label} payload={} {} mode={}",
                        if full { "full" } else { "missing-bin" },
                        if readonly { "readonly" } else { "writable" },
                        mode.as_str()
                    );
                    let fixture = Fixture::new(&format!(
                        "{}-{}-{}-{}",
                        state_label,
                        if full { "full" } else { "thin" },
                        if readonly { "ro" } else { "rw" },
                        mode.as_str()
                    ));
                    state.apply(&fixture);
                    fill_payload(&fixture, full);
                    if readonly {
                        fixture.chmod(&fixture.rime(), true);
                        fixture.chmod(&fixture.data(), true);
                        if !is_writable(&fixture.rime()) {
                            readonly_effective += 1;
                        }
                    }

                    let before = snapshot(&fixture.root);
                    let first = ime_state::ensure(
                        &fixture.rime(),
                        Some(fixture.payload().as_path()),
                        &[fixture.data()],
                        &options(mode),
                    );
                    let first_result = check(&label, &first, &fixture, &before);
                    combos += 1;

                    // 第二遍：验证幂等（Noop 或仍然是同一个结论）。
                    let before_second = snapshot(&fixture.root);
                    let second = ime_state::ensure(
                        &fixture.rime(),
                        Some(fixture.payload().as_path()),
                        &[fixture.data()],
                        &options(mode),
                    );
                    let second_result =
                        check(&format!("{label} (2nd)"), &second, &fixture, &before_second);
                    combos += 1;

                    for result in [first_result, second_result] {
                        match result {
                            OutcomeResult::Ok => seen_ok += 1,
                            OutcomeResult::Noop => seen_noop += 1,
                            OutcomeResult::Partial => seen_partial += 1,
                            OutcomeResult::Failed => seen_failed += 1,
                        }
                    }

                    if readonly && is_writable(&fixture.rime()) {
                        // root 下 chmod 555 不生效：只记录，不因此判失败。
                        continue;
                    }
                    if readonly
                        && matches!(state, BuildState::NoSchemaYaml | BuildState::NoBin)
                        && !full
                    {
                        // 只读 + payload 缺文件：一定补不齐。
                        assert!(
                            matches!(first_result, OutcomeResult::Failed),
                            "[{label}] 只读且 payload 缺文件时应当 Failed，实际 {first_result:?}"
                        );
                    }
                }
            }
        }
    }

    assert!(combos >= 48, "组合数不足：{combos}");
    println!(
        "matrix combos={combos} ok={seen_ok} noop={seen_noop} partial={seen_partial} failed={seen_failed} readonly_effective={readonly_effective}"
    );
    for (name, count) in [
        ("ok", seen_ok),
        ("noop", seen_noop),
        ("failed", seen_failed),
    ] {
        assert!(count > 0, "矩阵没有覆盖 {name} 结论（组合数 {combos}）");
    }
    if readonly_effective > 0 {
        assert!(
            seen_partial > 0,
            "只读目录生效时应该出现 partial（data 补丁写不进去），实际 0"
        );
    }
}

#[test]
fn ok_implies_at_least_one_changed_action() {
    let fixture = Fixture::new("contract");
    BuildState::NoSchemaYaml.apply(&fixture);
    fill_payload(&fixture, true);

    let before = snapshot(&fixture.root);
    let outcome = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install),
    );

    assert_eq!(
        outcome.result,
        OutcomeResult::Ok,
        "detail={}",
        outcome.detail
    );
    assert!(outcome.ok());
    assert!(!outcome.actions.is_empty(), "Ok 不许 actions 为空");
    assert!(
        outcome.actions.iter().any(|action| action.changed),
        "Ok 必须有 changed=true 的 action：{:?}",
        outcome.actions
    );
    assert!(outcome.report.build_ok);
    assert_eq!(outcome.repaired, vec![SCHEMA.to_string()]);
    assert_ne!(snapshot(&fixture.root), before, "Ok 必须真的改动了磁盘");
}

#[test]
fn noop_leaves_the_tree_byte_identical() {
    let fixture = Fixture::new("noop");
    BuildState::AllGood.apply(&fixture);
    fill_payload(&fixture, true);

    // 第一遍把 data 补丁写进去（本次改过 → Ok）。
    let first = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install),
    );
    assert_eq!(first.result, OutcomeResult::Ok, "detail={}", first.detail);

    let before = snapshot(&fixture.root);
    let second = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install),
    );
    assert_eq!(
        second.result,
        OutcomeResult::Noop,
        "detail={}",
        second.detail
    );
    assert!(!second.actions.iter().any(|action| action.changed));
    assert_eq!(snapshot(&fixture.root), before, "Noop 必须逐字节一致");
}

#[test]
fn missing_payload_fails_only_when_build_is_unusable() {
    // payload 目录找不到（payload=None）：build 已可用 → Noop；不可用 → Failed（不可静默放过）。
    let ready = Fixture::new("no-payload-ready");
    BuildState::AllGood.apply(&ready);
    // 没有任何 data 方案要补：本次不该改动磁盘（Noop 的前提）。
    // data 里两个方案都要拿掉：留下任何一个都会被 data 补丁改写（→ Ok）。
    fs::remove_file(ready.data().join("luna_pinyin.schema.yaml")).expect("remove data schema");
    fs::remove_file(ready.data().join("wubi86.schema.yaml")).expect("remove wubi86 schema");
    let before = snapshot(&ready.root);
    let outcome = ime_state::ensure(
        &ready.rime(),
        None,
        &[ready.data()],
        &options(Mode::Install),
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Noop,
        "detail={}",
        outcome.detail
    );
    assert_eq!(snapshot(&ready.root), before, "Noop 不许改文件");

    let broken = Fixture::new("no-payload-broken");
    BuildState::NoBin.apply(&broken);
    let outcome = ime_state::ensure(
        &broken.rime(),
        None,
        &[broken.data()],
        &options(Mode::Install),
    );
    assert_eq!(outcome.result, OutcomeResult::Failed);
    assert!(!outcome.detail.is_empty(), "Failed 必须带 detail");
    assert!(!outcome.ok());
}

/// A-3 回归：build/ 已可用、但用户目录缺 default.custom.yaml 时，
/// ensure 必须报 Ok（真的改了磁盘），不能报 Noop。
///
/// 0.6.24 的 bug：always_config 分支铺完 config 却没把 changed 计入汇总 →
/// 「build 早就可用、config 刚补上」被报成 Noop（CLI / GUI / 日志都说没做事）。
#[test]
fn always_config_reports_ok_when_only_config_was_added() {
    let fixture = Fixture::new("always-config");
    BuildState::AllGood.apply(&fixture);
    fill_payload(&fixture, true);

    let before = snapshot(&fixture.root);
    let outcome = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install).with_always_config(true),
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Ok,
        "detail={}",
        outcome.detail
    );
    assert!(
        outcome.actions.iter().any(|action| action.changed),
        "报 Ok 但没有任何 changed 的 action：{:?}",
        outcome.actions
    );
    assert!(
        fixture.rime().join("default.custom.yaml").is_file(),
        "config 没铺进用户目录"
    );
    assert_ne!(snapshot(&fixture.root), before, "报 Ok 但磁盘一个字节没变");
}

/// 幂等：同样的输入跑第二遍必须 Noop（且逐字节一致）。
#[test]
fn second_pass_is_noop() {
    let fixture = Fixture::new("second-pass");
    BuildState::AllGood.apply(&fixture);
    fill_payload(&fixture, true);

    let first = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install).with_always_config(true),
    );
    assert_eq!(first.result, OutcomeResult::Ok, "detail={}", first.detail);

    let before = snapshot(&fixture.root);
    let second = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install).with_always_config(true),
    );
    assert_eq!(
        second.result,
        OutcomeResult::Noop,
        "第二遍应当是 Noop，detail={}",
        second.detail
    );
    assert!(
        !second.actions.iter().any(|action| action.changed),
        "Noop 不许有 changed 的 action：{:?}",
        second.actions
    );
    assert_eq!(snapshot(&fixture.root), before, "Noop 必须逐字节一致");
}

/// 模板播种：缺失才写，写了就是 changed（跨平台直接测 seed_templates）。
#[test]
fn seed_templates_writes_missing_templates_once() {
    let fixture = Fixture::new("seed");
    let payload_config = fixture.payload().join("config");
    let report = ime_state::seed_templates(
        &payload_config,
        &fixture.rime(),
        std::slice::from_ref(&fixture.data()),
    );
    assert!(report.changed(), "第一次播种应当有写入：{report:?}");
    assert!(
        fixture.rime().join("default.custom.yaml").is_file(),
        "default.custom.yaml 没播进用户目录"
    );

    let again = ime_state::seed_templates(
        &payload_config,
        &fixture.rime(),
        std::slice::from_ref(&fixture.data()),
    );
    assert!(!again.changed(), "第二遍播种不该再改：{again:?}");
}

/// ensure 层面的播种（Windows 才有 seed 行为：ensure 里 seed_templates 受
/// cfg!(windows) 约束，Linux 上这一步由 always_config 分支负责）。
#[cfg(windows)]
#[test]
fn ensure_seeds_templates_and_reports_ok() {
    let fixture = Fixture::new("ensure-seed");
    BuildState::AllGood.apply(&fixture);
    fill_payload(&fixture, true);

    let outcome = ime_state::ensure(
        &fixture.rime(),
        Some(fixture.payload().as_path()),
        &[fixture.data()],
        &options(Mode::Install)
            .with_always_config(false)
            .with_seed_templates(true),
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Ok,
        "detail={}",
        outcome.detail
    );
    assert!(
        outcome.actions.iter().any(|action| action.changed),
        "报 Ok 但没有任何 changed 的 action：{:?}",
        outcome.actions
    );
}
