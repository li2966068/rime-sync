//! 缺陷三（Windows 小键盘失效）接进真实流程的回归测试。
//!
//! 背景：librime 的 `patch:` 对**列表**是整段替换，用户目录里那份
//! `<id>.custom.yaml` 会把方案自带的 `engine/processors` 整份顶掉。
//! rime-sync v0.6.18 之前生成的模板没有 `numpad`，于是老用户目录里的
//! custom.yaml 把小键盘顶没了 —— 227 实机就是这个状态。
//!
//! 这里测的是「接进 ensure 之后」的行为：
//! * 227 那种旧模板（带 rime-sync 标记、没有 numpad）→ ensure 修好它；
//! * 用户自己的 / 用户主动关掉定制的 custom.yaml → 一个字节都不许动；
//! * 幂等：第二次 ensure 不许再写盘；
//! * 修过的方案，编译产物里没有 numpad → 结论必须降级（不许报 ok 骗人）。
//!
//! 全部用临时目录，不碰真实系统：不装输入法、不打 data 补丁、不部署。

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use rime_sync_core::ime_state::{self, Mode, Options, Outcome, OutcomeResult};

/// 227 那台机器上的旧模板：带 rime-sync 标记、processors 里有 rimesync_vselect，
/// 但**没有 numpad**（小键盘就是被它顶掉的）。
const OLD_CUSTOM: &str = "\
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_punct
    - rimesync_vselect
    - key_binder
    - speller
    - punctuator
    - selector
  engine/translators:
    - rimesync_vnum
    - punct_translator
";

/// 用户自己写的 custom.yaml：没有 rime-sync 标记 → 谁都不许碰。
const USER_CUSTOM: &str = "\
# 用户自己写的配置
patch:
  engine/processors:
    - ascii_composer
    - recognizer
";

/// 用户主动关掉了 rime-sync 的引擎定制（还留着标记，但三个 rimesync_* 名字都没了）。
const DISABLED_CUSTOM: &str = "\
# 由 rime-sync 生成：用户已关闭 rime-sync 引擎定制
patch:
  engine/processors:
    - ascii_composer
    - recognizer
";

/// 编译产物（含 numpad = 引擎已经重编译过）。
const BUILD_WITH_NUMPAD: &str = "\
engine:
  processors:
    - ascii_composer
    - rimesync_vnum
    - rimesync_punct
    - rimesync_vselect
    - numpad
";

/// 编译产物（不含 numpad = 引擎还没重编译，227 的原始状态）。
const BUILD_WITHOUT_NUMPAD: &str = "\
engine:
  processors:
    - ascii_composer
    - rimesync_vnum
    - rimesync_punct
    - rimesync_vselect
";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ime-custom-repair-{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("rime/build")).expect("create rime dir");
        Self { root }
    }

    fn rime(&self) -> PathBuf {
        self.root.join("rime")
    }

    fn custom(&self, id: &str) -> PathBuf {
        self.rime().join(format!("{id}.custom.yaml"))
    }

    fn write_custom(&self, id: &str, text: &str) {
        fs::write(self.custom(id), text).expect("write custom");
    }

    fn write_build(&self, text: &str) {
        fs::write(self.rime().join("build/wubi86.schema.yaml"), text).expect("write build");
        fs::write(self.rime().join("build/wubi86.prism.bin"), b"prism").expect("write bin");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 完全不碰真实系统的 ensure 参数。
fn options() -> Options {
    Options::new(Mode::Fix)
        // 不装输入法本体、不播模板、不裁剪、不铺 config：只跑状态机本身。
        .with_auto_data_patch(false)
        .with_patch_data(false)
        .with_deploy(false)
        .with_prune(false)
        // data 目录解析器注入空集：绝不现场探测真机的 /usr/share/rime-data。
        .with_data_resolver(Vec::new)
}

fn run(fixture: &Fixture) -> Outcome {
    ime_state::ensure(&fixture.rime(), None, &[], &options())
}

fn action<'a>(outcome: &'a Outcome, kind: &str) -> Option<&'a rime_sync_core::ime_state::Action> {
    outcome.actions.iter().find(|action| action.kind == kind)
}

fn processors(text: &str) -> Vec<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(text).expect("custom 必须是合法 YAML");
    value
        .get("patch")
        .and_then(|patch| patch.get("engine/processors"))
        .and_then(serde_yaml::Value::as_sequence)
        .map(|list| {
            list.iter()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .expect("custom 里必须有 engine/processors")
}

fn mtime(path: &Path) -> std::time::SystemTime {
    fs::metadata(path)
        .expect("metadata")
        .modified()
        .expect("modified")
}

/// 227 场景：旧模板被修好、备份生成、只多出 numpad 一项、其它文件一字不动。
#[test]
fn ensure_repairs_227_style_custom_yaml() {
    let fixture = Fixture::new("227");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_custom("mine", USER_CUSTOM);
    fixture.write_custom("disabled", DISABLED_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    let user_before = fs::read(fixture.custom("mine")).expect("read user custom");
    let disabled_before = fs::read(fixture.custom("disabled")).expect("read disabled custom");

    let outcome = run(&fixture);

    // ① 动作里必须有 custom-repair，且 changed=true（改了盘）。
    let repair = action(&outcome, "custom-repair").expect("必须有 custom-repair 动作");
    assert!(
        repair.changed,
        "custom-repair 必须 changed=true：{}",
        repair.detail
    );
    assert!(
        repair.detail.contains("wubi86=repaired"),
        "custom-repair 明细里必须有 wubi86=repaired：{}",
        repair.detail
    );

    // ② 文件真的修好了：numpad 紧跟 rimesync_vselect，只有一份。
    let after = fs::read_to_string(fixture.custom("wubi86")).expect("read repaired custom");
    let names = processors(&after);
    let vselect = names
        .iter()
        .position(|name| name == "rimesync_vselect")
        .expect("rimesync_vselect 必须在");
    assert_eq!(
        names.get(vselect + 1).map(String::as_str),
        Some("numpad"),
        "numpad 必须紧跟 rimesync_vselect：{names:?}"
    );
    assert_eq!(
        names.iter().filter(|name| *name == "numpad").count(),
        1,
        "numpad 只能有一份：{names:?}"
    );

    // ③ 内容只多出 numpad 一项，其余字节不变。
    let mut expected = OLD_CUSTOM.to_string();
    let anchor = "    - rimesync_vselect\n";
    let at = expected.find(anchor).expect("模板里有 rimesync_vselect") + anchor.len();
    expected.insert_str(at, "    - numpad\n");
    assert_eq!(after, expected, "除了多出 numpad，其余必须逐字节不变");

    // ④ 备份生成，内容 = 修复前的字节。
    let backup = fixture.rime().join("wubi86.custom.yaml.rimesync-orig");
    assert!(backup.is_file(), "必须生成 .rimesync-orig 备份");
    assert_eq!(
        fs::read(&backup).expect("read backup"),
        OLD_CUSTOM.as_bytes(),
        "备份必须是修复前的原始字节"
    );

    // ⑤ 别人的文件一个字都没动。
    assert_eq!(
        fs::read(fixture.custom("mine")).expect("read user custom"),
        user_before,
        "非 rime-sync 管理的 custom.yaml 必须逐字节不变"
    );
    assert_eq!(
        fs::read(fixture.custom("disabled")).expect("read disabled custom"),
        disabled_before,
        "用户关掉定制的 custom.yaml 必须逐字节不变"
    );

    // ⑥ 产物里还没有 numpad → 结论必须降级（不许报 ok 骗人）。
    let check = action(&outcome, "numpad-check").expect("必须有 numpad-check 动作");
    assert!(
        check.detail.contains("wubi86=no"),
        "numpad-check 必须报出 wubi86=no：{}",
        check.detail
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Partial,
        "产物里没有 numpad 时结论必须降级为 partial"
    );
    assert!(
        outcome.detail.contains("numpad_missing=wubi86"),
        "detail 里必须说明产物缺 numpad：{}",
        outcome.detail
    );
}

/// 非 rime-sync 管理的 custom.yaml：ensure 之后逐字节不变。
#[test]
fn ensure_leaves_unmanaged_custom_yaml_untouched() {
    let fixture = Fixture::new("unmanaged");
    fixture.write_custom("mine", USER_CUSTOM);
    fixture.write_build(BUILD_WITH_NUMPAD);
    let before = fs::read(fixture.custom("mine")).expect("read");

    let outcome = run(&fixture);

    assert_eq!(
        fs::read(fixture.custom("mine")).expect("read"),
        before,
        "非 rime-sync 管理的 custom.yaml 必须逐字节不变"
    );
    let repair = action(&outcome, "custom-repair").expect("custom-repair 动作");
    assert!(
        repair.detail.contains("mine=skipped(not-managed)"),
        "明细里必须写明跳过原因：{}",
        repair.detail
    );
    assert!(!repair.changed, "没有修任何文件时 changed 必须为 false");
}

/// 用户自己关掉了 rime-sync 定制：ensure 之后逐字节不变。
#[test]
fn ensure_leaves_disabled_custom_yaml_untouched() {
    let fixture = Fixture::new("disabled");
    fixture.write_custom("disabled", DISABLED_CUSTOM);
    fixture.write_build(BUILD_WITH_NUMPAD);
    let before = fs::read(fixture.custom("disabled")).expect("read");

    let outcome = run(&fixture);

    assert_eq!(
        fs::read(fixture.custom("disabled")).expect("read"),
        before,
        "用户关掉定制的 custom.yaml 必须逐字节不变"
    );
    let repair = action(&outcome, "custom-repair").expect("custom-repair 动作");
    assert!(
        repair
            .detail
            .contains("disabled=skipped(rimesync-disabled)"),
        "明细里必须写明跳过原因：{}",
        repair.detail
    );
    assert!(!repair.changed, "没有修任何文件时 changed 必须为 false");
}

/// 幂等：第二次 ensure 不许再写盘（mtime 与内容都不变），changed 必须为 false。
#[test]
fn ensure_is_idempotent_for_custom_repair() {
    let fixture = Fixture::new("idempotent");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    let first = run(&fixture);
    let repair_first = action(&first, "custom-repair").expect("custom-repair 动作");
    assert!(repair_first.changed, "第一次必须 changed=true");

    let content_after_first = fs::read(fixture.custom("wubi86")).expect("read");
    let mtime_after_first = mtime(&fixture.custom("wubi86"));

    // 隔一秒再跑：万一第二次真的写了盘，mtime 一定会变（秒级时间戳也够）。
    sleep(Duration::from_millis(1100));

    let second = run(&fixture);
    let repair_second = action(&second, "custom-repair").expect("custom-repair 动作");
    assert!(
        !repair_second.changed,
        "第二次不该再改盘：{}",
        repair_second.detail
    );
    assert!(
        repair_second.detail.contains("wubi86=unchanged"),
        "第二次必须是 unchanged：{}",
        repair_second.detail
    );
    assert_eq!(
        fs::read(fixture.custom("wubi86")).expect("read"),
        content_after_first,
        "第二次 ensure 之后内容必须不变"
    );
    assert_eq!(
        mtime(&fixture.custom("wubi86")),
        mtime_after_first,
        "第二次 ensure 之后 mtime 必须不变（不许再写盘）"
    );
}

/// 修文件必须计入 changed：否则 deploy 分支会被 no-changes 跳过，重编译不会发生。
#[test]
fn custom_repair_counts_as_changed_so_deploy_is_not_skipped() {
    let fixture = Fixture::new("changed");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    // 产物里已经有 numpad（模拟引擎已经重编译过）→ 结论不该被降级。
    fixture.write_build(BUILD_WITH_NUMPAD);

    let outcome = run(&fixture);

    let repair = action(&outcome, "custom-repair").expect("custom-repair 动作");
    assert!(
        repair.changed,
        "修了文件就必须 changed=true（changed 参与 deploy 分支的判断）"
    );
    // result=Ok 只有在 changed==true 时才可能（否则是 Noop）。
    assert_eq!(
        outcome.result,
        OutcomeResult::Ok,
        "产物已含 numpad 时结论应为 ok（detail={}）",
        outcome.detail
    );
    assert!(
        outcome.actions.iter().any(|item| item.changed),
        "必须有 changed=true 的动作"
    );
    let check = action(&outcome, "numpad-check").expect("numpad-check 动作");
    assert!(
        check.detail.contains("wubi86=yes"),
        "numpad-check 必须报出 wubi86=yes：{}",
        check.detail
    );
}
