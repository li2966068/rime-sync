//! 缺陷三（小键盘）：**常驻**只读判定 + ensure 的结论降级。
//!
//! 背景：用户目录里那份 `<id>.custom.yaml` 会把方案的 `engine/processors`
//! 整段顶掉（librime 的 patch 对列表是替换）。旧版 rime-sync 生成的模板里
//! 没有 `numpad`，于是小键盘（小键盘归一化处理器）一直不生效。
//!
//! B1 已经做到「ensure 会把缺 numpad 的 custom.yaml 修好」，但那只是
//! 「本轮修过谁就查谁」——修完之后引擎一直没重编译的话，后续轮次就又不报了，
//! 于是又回到「报告说 ok、小键盘其实用不了」的静默失败（缺陷三当初就是这么
//! 悄悄发布出去的）。
//!
//! 这里测的是**常驻判定**（core::ime_state::assess_numpad）：
//! * 每轮都重新判，与「本轮是否修过文件」无关；
//! * 只读：不写任何文件（字节与 mtime 都不许变）；
//! * 两类缺口分得清：source-stale（来源侧就坏，可修）/ artifact-stale（没重编译）；
//! * 健康机器 / 用户自己的文件 / 用户关掉定制 → 一律不报（防误报）。
//!
//! 全部用临时目录，不碰真实系统：不装输入法、不部署、不打 data 补丁。

use std::fs;
use std::path::{Path, PathBuf};

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

/// 用户自己写的 custom.yaml：没有 rime-sync 标记 → 谁都不许碰、也不许报缺口。
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

/// rime-sync 管理的、来源侧已经修好的 custom.yaml（processors 里有 numpad）。
const HEALTHY_CUSTOM: &str = "\
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
    - numpad
    - key_binder
    - speller
    - punctuator
    - selector
  engine/translators:
    - rimesync_vnum
    - punct_translator
";

/// rime-sync 管理的、但**没有定义** engine/processors 列表（处理器列表来自共享方案）。
/// 这种文件不许判成 source-stale（共享方案侧是 __include 的父方案，读不到 → 误判）。
const CUSTOM_NO_PROCESSORS: &str = "\
# 由 rime-sync 生成：只改翻译器，不动处理器列表
patch:
  engine/translators:
    - rimesync_vnum
    - punct_translator
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
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("numpad-assess-{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("rime/build")).expect("create rime dir");
        fs::create_dir_all(root.join("data")).expect("create data dir");
        Self { root }
    }

    fn rime(&self) -> PathBuf {
        self.root.join("rime")
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
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
        .with_auto_data_patch(false)
        .with_patch_data(false)
        .with_deploy(false)
        .with_prune(false)
        .with_data_resolver(Vec::new)
}

fn run(fixture: &Fixture) -> Outcome {
    ime_state::ensure(&fixture.rime(), None, &[], &options())
}

fn action<'a>(outcome: &'a Outcome, kind: &str) -> Option<&'a rime_sync_core::ime_state::Action> {
    outcome.actions.iter().find(|item| item.kind == kind)
}

fn state_of<'a>(status: &'a ime_state::NumpadStatus, id: &str) -> Option<&'a str> {
    status.state_of(id)
}

/// numpad 在 processors 列表里的行号（找不到返回 None）。
fn numpad_line(text: &str) -> Option<usize> {
    text.lines().position(|line| line.trim() == "- numpad")
}

fn vselect_line(text: &str) -> Option<usize> {
    text.lines()
        .position(|line| line.trim() == "- rimesync_vselect")
}

fn count_numpad(text: &str) -> usize {
    text.lines()
        .filter(|line| line.trim() == "- numpad")
        .count()
}

/// a) 227 形状：修之前是 source-stale；ensure 修好来源后变成 artifact-stale，
///    结论降级 partial 且 detail 指名 artifact-stale。
#[test]
fn old_custom_is_repaired_and_still_reported_as_stale_artifact() {
    let fixture = Fixture::new("old");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    // 修之前：来源侧就坏（自己定义的 processors 列表里没有 numpad）。
    let before = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert!(!before.ok, "修之前必须报缺口：{:?}", before.detail);
    assert_eq!(
        state_of(&before, "wubi86"),
        Some("source-stale"),
        "修之前来源侧就该判成 source-stale：{:?}",
        before.detail
    );
    assert!(
        before
            .missing
            .iter()
            .any(|item| item == "wubi86=source-stale"),
        "missing 必须点名方案与档位：{:?}",
        before.missing
    );

    let outcome = run(&fixture);

    // 文件被修好：numpad 紧跟 rimesync_vselect，而且只有一份。
    let text = fs::read_to_string(fixture.custom("wubi86")).expect("read custom");
    assert_eq!(count_numpad(&text), 1, "numpad 必须只有一份：{text}");
    assert_eq!(
        numpad_line(&text),
        vselect_line(&text).map(|line| line + 1),
        "numpad 必须紧跟 rimesync_vselect：{text}"
    );

    // 修完之后：来源侧好了，产物还是旧的 → artifact-stale，结论 partial。
    let after = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert_eq!(
        state_of(&after, "wubi86"),
        Some("artifact-stale"),
        "修好来源后应判成 artifact-stale：{:?}",
        after.detail
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Partial,
        "产物还没重编译 → 结论必须降级 partial（不许报 ok 骗人），detail={}",
        outcome.detail
    );
    assert!(
        outcome.detail.contains("numpad_missing=wubi86"),
        "detail 必须点名缺口方案：{}",
        outcome.detail
    );
    assert!(
        outcome.detail.contains("artifact-stale"),
        "detail 必须点名档位：{}",
        outcome.detail
    );
    assert!(!outcome.numpad.ok, "Outcome 上必须带着判定结果");
}

/// b) 常驻判定：修好之后**再次** ensure，来源不变（unchanged），结论仍然 partial。
#[test]
fn second_ensure_still_reports_the_gap() {
    let fixture = Fixture::new("second");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    let first = run(&fixture);
    assert_eq!(
        first.result,
        OutcomeResult::Partial,
        "第一次：detail={}",
        first.detail
    );

    let second = run(&fixture);
    let repair = action(&second, "custom-repair").expect("custom-repair 动作");
    assert!(
        repair.detail.contains("wubi86=unchanged"),
        "第二次不该再改文件：{}",
        repair.detail
    );
    assert_eq!(
        second.result,
        OutcomeResult::Partial,
        "常驻判定：产物没重编译就必须继续报 partial，detail={}",
        second.detail
    );
    assert!(
        second
            .numpad
            .missing
            .iter()
            .any(|item| item == "wubi86=artifact-stale"),
        "必须继续点名 artifact-stale：{:?}",
        second.numpad.missing
    );
}

/// c) 产物里已经有 numpad → 不误报。
#[test]
fn artifact_with_numpad_is_ok() {
    let fixture = Fixture::new("healthy");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_build(BUILD_WITH_NUMPAD);

    let outcome = run(&fixture);

    assert!(
        outcome.numpad.ok,
        "产物里已有 numpad → 不许报缺口：{:?}",
        outcome.numpad.detail
    );
    assert!(
        !outcome.detail.contains("numpad_missing"),
        "不该出现缺口字样：{}",
        outcome.detail
    );
    assert!(
        matches!(outcome.result, OutcomeResult::Ok | OutcomeResult::Noop),
        "健康机器不许被降级：{}",
        outcome.detail
    );
}

/// d) 用户主动关掉了定制 → 不报、不动。
#[test]
fn disabled_customization_is_neither_touched_nor_reported() {
    let fixture = Fixture::new("disabled");
    fixture.write_custom("wubi86", DISABLED_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);
    let before = fs::read(fixture.custom("wubi86")).expect("read custom");

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert!(status.ok, "定制已关 → 不许报缺口：{:?}", status.detail);
    assert_eq!(state_of(&status, "wubi86"), Some("skipped"));

    let outcome = run(&fixture);
    assert_eq!(
        fs::read(fixture.custom("wubi86")).expect("read custom"),
        before,
        "用户关掉定制的文件一个字节都不许动"
    );
    assert!(
        !matches!(outcome.result, OutcomeResult::Partial),
        "定制已关 → 不许降级：{}",
        outcome.detail
    );
}

/// e) 用户自己改过的文件 → 不动、不报。
#[test]
fn unmanaged_file_is_neither_touched_nor_reported() {
    let fixture = Fixture::new("unmanaged");
    fixture.write_custom("wubi86", USER_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);
    let before = fs::read(fixture.custom("wubi86")).expect("read custom");

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert!(
        status.ok,
        "用户自己的文件 → 不许报缺口：{:?}",
        status.detail
    );
    assert_eq!(state_of(&status, "wubi86"), Some("skipped"));

    let outcome = run(&fixture);
    assert_eq!(
        fs::read(fixture.custom("wubi86")).expect("read custom"),
        before,
        "用户自己的文件一个字节都不许动"
    );
    assert!(
        !matches!(outcome.result, OutcomeResult::Partial),
        "用户自己的文件 → 不许降级：{}",
        outcome.detail
    );
}

/// f) 只读：判定函数跑完，用户目录里文件的**字节与 mtime**都不许变。
#[test]
fn assessment_is_read_only() {
    let fixture = Fixture::new("readonly");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    let path = fixture.custom("wubi86");
    let before_bytes = fs::read(&path).expect("read custom");
    let before_mtime = fs::metadata(&path)
        .expect("stat")
        .modified()
        .expect("mtime");

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert!(!status.ok, "这一档必须有缺口（否则这个用例没意义）");
    // 再判一次：常驻判定也必须是纯读。
    let _ = ime_state::assess_numpad(&fixture.rime(), &[]);

    assert_eq!(
        fs::read(&path).expect("read custom"),
        before_bytes,
        "只读判定不许改字节"
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("stat")
            .modified()
            .expect("mtime"),
        before_mtime,
        "只读判定不许改 mtime"
    );
}

/// g) 用户 custom.yaml 没有定义 engine/processors 列表 → 不许判成 source-stale。
#[test]
fn custom_without_processors_list_is_not_source_stale() {
    let fixture = Fixture::new("no-processors");
    fixture.write_custom("wubi86", CUSTOM_NO_PROCESSORS);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert_ne!(
        state_of(&status, "wubi86"),
        Some("source-stale"),
        "没定义 processors 列表 → 不许判成 source-stale：{:?}",
        status.detail
    );
    assert_eq!(
        state_of(&status, "wubi86"),
        Some("artifact-stale"),
        "这一档只能来自产物侧：{:?}",
        status.detail
    );
}

/// 产物压根不存在 = 还没编译过 → 不算缺口。
#[test]
fn missing_artifact_is_not_a_gap() {
    let fixture = Fixture::new("no-artifact");
    fixture.write_custom("wubi86", HEALTHY_CUSTOM);

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert!(status.ok, "没有产物不许算缺口：{:?}", status.detail);
    assert_eq!(state_of(&status, "wubi86"), Some("skipped"));
}

/// 依赖方案（luna_pinyin / pinyin_simp / stroke）不参与判定：
/// 随包产物的依赖方案本来就没有 numpad，报了就是误报。
#[test]
fn only_retained_schemas_are_judged() {
    let fixture = Fixture::new("retained");
    // 一个依赖方案形状的产物：有 rimesync 定制，但**没有** numpad。
    fs::write(
        fixture.rime().join("build/luna_pinyin.schema.yaml"),
        BUILD_WITHOUT_NUMPAD,
    )
    .expect("write build");

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    assert!(status.ok, "依赖方案不参与判定：{:?}", status.detail);
    assert_eq!(
        status.detail.len(),
        3,
        "只查保留的三个主方案：{:?}",
        status.detail
    );
}

/// data 目录里没有产物时的档位是 no-artifact（不是缺口）。
#[test]
fn artifact_absent_but_scheme_present_is_no_artifact() {
    let fixture = Fixture::new("no-artifact-state");
    fixture.write_custom("wubi86", HEALTHY_CUSTOM);
    fs::write(fixture.data().join("wubi86.schema.yaml"), "name: wubi86\n")
        .expect("write data schema");

    let status = ime_state::assess_numpad(&fixture.rime(), &[fixture.data()]);
    assert!(status.ok, "还没编译过 → 不算缺口：{:?}", status.detail);
    assert_eq!(state_of(&status, "wubi86"), Some("no-artifact"));
}

/// 判定结果必须能直接喂给 CLI/GUI：四个字段都在，serde 能序列化。
#[test]
fn status_is_serializable() {
    let fixture = Fixture::new("serde");
    fixture.write_custom("wubi86", OLD_CUSTOM);
    fixture.write_build(BUILD_WITHOUT_NUMPAD);

    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    let json = serde_json::to_value(&status).expect("serialize status");
    assert!(json.get("ok").is_some(), "JSON 里必须有 ok");
    assert!(json.get("missing").is_some(), "JSON 里必须有 missing");
    assert!(json.get("detail").is_some(), "JSON 里必须有 detail");
    assert!(json.get("hint").is_some(), "JSON 里必须有 hint");
    assert!(!status.hint.is_empty(), "有缺口就必须给出人话的下一步");
}

/// 保留清单里的方案名要跟判定输出的对得上。
#[test]
fn detail_covers_every_retained_schema() {
    let fixture = Fixture::new("detail");
    let status = ime_state::assess_numpad(&fixture.rime(), &[]);
    for id in ["luna_pinyin_simp", "wubi_pinyin", "wubi86"] {
        assert!(
            status
                .detail
                .iter()
                .any(|line| line.starts_with(&format!("{id}="))),
            "detail 必须逐方案一行：{:?}",
            status.detail
        );
    }
    assert!(Path::new(&fixture.rime()).is_dir());
}
