//! CLI 侧的 D6 回归：check-ime 必须报出**标点直出**（rimesync_punct）的状态。
//!
//! 改前：check-ime 只看小键盘（numpad），从不看标点直出 ⇒ 用户的中文标点直出
//! 明明是关的，报告却写着 ok。
//!
//! 现在：check-ime 的 JSON 里有 `punct` 段（与 ensure-ime 同一份判据），
//! 并且能区分两种问题：
//! * config-missing    —— 配置里少写了 rimesync_punct，补配置就能修；
//! * engine-unsupported —— 本机输入法引擎压根不认识这个组件，必须换输入法本体。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 受管但**没有** rimesync_punct 的用户 custom.yaml（= 补配置就能修）。
const CUSTOM_WITHOUT_PUNCT: &str = "\
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - rimesync_vselect
    - key_binder
    - speller
    - punctuator
    - selector
  engine/translators:
    - rimesync_vnum
";

/// 受管且**带** rimesync_punct 的用户 custom.yaml。
const CUSTOM_WITH_PUNCT: &str = "\
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
    - key_binder
    - speller
    - punctuator
    - selector
  engine/translators:
    - rimesync_vnum
";

/// 受管、小键盘齐备、但**缺** rimesync_punct 的用户 custom.yaml。
///
/// 与 CUSTOM_WITHOUT_PUNCT 的区别：这里带 numpad ⇒ 小键盘判定是 ok 的，
/// 这样「结果降级」就只可能由标点直出引起，测试才断得准。
const CUSTOM_WITH_NUMPAD_NO_PUNCT: &str = "\
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - rimesync_vselect
    - numpad
    - key_binder
    - speller
    - punctuator
    - selector
  engine/translators:
    - rimesync_vnum
";

/// 用户自己写的 custom.yaml（没有 rime-sync 的受管标记，也没有增强组件）。
///
/// 这种情况标点直出判 skipped —— 不是缺陷，不该影响结论与退出码。
const USER_OWNED_CUSTOM: &str = "\
# 我自己改的
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - punctuator
    - selector
";

/// 产物：内容带随包插件标记（rimesync_vnum），且小键盘与标点都编进去了。
const ARTIFACT_COMPLETE: &str = "\
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
engine:
  processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
    - numpad
    - selector
";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-cli-punct-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["payload/build", "rime/build", "state", "data"] {
            fs::create_dir_all(root.join(dir)).expect("create dir");
        }
        Self { root }
    }

    fn rime(&self) -> PathBuf {
        self.root.join("rime")
    }

    fn payload(&self) -> PathBuf {
        self.root.join("payload")
    }

    fn write_custom(&self, text: &str) {
        fs::write(self.rime().join("wubi86.custom.yaml"), text).expect("write custom");
    }

    /// 产物：编译出来的 schema.yaml（有没有 rimesync_punct 由内容决定）。
    fn write_artifact(&self, text: &str) {
        for dir in [self.rime().join("build"), self.payload().join("build")] {
            fs::write(dir.join("wubi86.schema.yaml"), text).expect("write schema");
            fs::write(dir.join("wubi86.prism.bin"), b"prism").expect("write bin");
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"))
            .arg("--config")
            .arg(self.root.join("state/config.json"))
            .arg("--rime-dir")
            .arg(self.rime())
            .args(args)
            .env("RIMESYNC_PREBUILT_DIR", self.payload())
            .env("RIMESYNC_INSTALL_IME", "0")
            .env("RIMESYNC_PRUNE_SCHEMAS", "0")
            .output()
            .expect("run rime-sync-cli")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn json_of(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("parse json")
}

/// check-ime 的 JSON 必须带 punct 段（以前完全没有这一段）。
#[test]
fn check_ime_json_carries_the_punct_section() {
    let fixture = Fixture::new("json");
    fixture.write_custom(CUSTOM_WITH_PUNCT);
    fixture.write_artifact(
        "engine:\n  processors:\n    - ascii_composer\n    - rimesync_punct\n    - selector\n",
    );

    let report = json_of(&fixture.run(&["check-ime", "--json"]));

    assert!(report["punct"].is_object(), "报告：{report}");
    assert!(report["punct"]["ok"].is_boolean(), "报告：{report}");
    assert!(report["punct"]["missing"].is_array(), "报告：{report}");
    assert!(report["punct"]["detail"].is_array(), "报告：{report}");
    assert!(report["punct"]["hint"].is_string(), "报告：{report}");
    assert!(
        report["punct"]["engine_support"].is_string(),
        "必须报出引擎支不支持：{report}"
    );
}

/// 配置里少了 rimesync_punct ⇒ 必须报成 config-missing（补配置就能修）。
#[test]
fn missing_punct_in_the_config_is_reported_as_config_missing() {
    let fixture = Fixture::new("config-missing");
    fixture.write_custom(CUSTOM_WITHOUT_PUNCT);
    fixture.write_artifact(
        "engine:\n  processors:\n    - ascii_composer\n    - rimesync_punct\n    - selector\n",
    );

    let report = json_of(&fixture.run(&["check-ime", "--json"]));

    assert_eq!(report["punct"]["ok"], false, "报告：{report}");
    let missing = report["punct"]["missing"].as_array().expect("array");
    let text = missing
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("wubi86=config-missing"),
        "必须报成 config-missing（补配置就能修）：{report}"
    );
    assert!(
        !text.contains("engine-unsupported"),
        "引擎支持时不该报成 engine-unsupported：{report}"
    );
    let hint = report["punct"]["hint"].as_str().expect("hint");
    assert!(
        hint.contains("修复输入法"),
        "补配置的下一步必须说清：{hint}"
    );
}

/// 配置里有 punct 但产物里没有 ⇒ 报成 artifact-stale（还没重编译）。
#[test]
fn stale_artifact_is_reported_as_artifact_stale() {
    let fixture = Fixture::new("artifact-stale");
    fixture.write_custom(CUSTOM_WITH_PUNCT);
    fixture.write_artifact(
        "engine:\n  processors:\n    - ascii_composer\n    - rimesync_vselect\n    - selector\n",
    );

    let report = json_of(&fixture.run(&["check-ime", "--json"]));

    let missing = report["punct"]["missing"].as_array().expect("array");
    let text = missing
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    // 产物缺 punct 时可能是「还没重编译」，也可能是「引擎本体不认识」——
    // 两种都算缺口，但都不许悄悄报 ok。
    assert!(
        text.contains("wubi86=artifact-stale") || text.contains("wubi86=engine-unsupported"),
        "产物缺 punct 必须报出缺口：{report}"
    );
    assert_eq!(report["punct"]["ok"], false, "报告：{report}");
}

/// 文本模式也要有一行人话。
#[test]
fn text_mode_prints_a_human_readable_punct_line() {
    let fixture = Fixture::new("text");
    fixture.write_custom(CUSTOM_WITHOUT_PUNCT);
    fixture.write_artifact(
        "engine:\n  processors:\n    - ascii_composer\n    - rimesync_punct\n    - selector\n",
    );

    let output = fixture.run(&["check-ime"]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("punct="),
        "文本模式必须有一行 punct：{stdout}"
    );
    assert!(
        stdout.contains("wubi86=config-missing"),
        "文本模式必须点名方案与档位：{stdout}"
    );
}

/// ensure-ime 的 JSON 里也必须有 punct 段（两端同一份判据）。
#[test]
fn ensure_ime_json_carries_the_punct_section() {
    let fixture = Fixture::new("ensure-json");
    fixture.write_custom(CUSTOM_WITH_PUNCT);
    fixture.write_artifact(
        "engine:\n  processors:\n    - ascii_composer\n    - rimesync_punct\n    - selector\n",
    );

    let report = json_of(&fixture.run(&["ensure-ime", "--no-data-patch", "--json"]));

    assert!(report["punct"].is_object(), "报告：{report}");
    assert!(report["punct"]["ok"].is_boolean(), "报告：{report}");
    assert!(report["punct"]["missing"].is_array(), "报告：{report}");
}

/// 标点直出有缺口时，check-ime 必须降级成 partial 并非零退出。
///
/// 退出码 0 的含义是「输入法现在就绪」：标点直出关着/产物没重编译/引擎根本
/// 不支持时都不是就绪状态，报 ok 就是假 ok（审计原文：check-ime 必须不报假 ok）。
///
/// 夹具刻意把小键盘补齐（numpad.ok=true）并让产物可用（build_ok=true），
/// 这样「降级」只可能由标点直出引起。
#[test]
fn punct_gap_downgrades_check_ime_to_partial_and_non_zero_exit() {
    let fixture = Fixture::new("punct-partial");
    fixture.write_custom(CUSTOM_WITH_NUMPAD_NO_PUNCT);
    fixture.write_artifact(ARTIFACT_COMPLETE);

    let output = fixture.run(&["check-ime", "--json"]);
    let report = json_of(&output);

    // 前置条件：产物可用、小键盘没问题 —— 只有标点直出缺了。
    assert_eq!(report["build_ok"], true, "报告：{report}");
    assert_eq!(report["numpad"]["ok"], true, "报告：{report}");
    assert_eq!(report["punct"]["ok"], false, "报告：{report}");

    assert_eq!(report["result"], "partial", "报告：{report}");
    assert!(
        !output.status.success(),
        "标点直出有缺口时退出码必须非零（实际 {:?}）",
        output.status.code()
    );
    let detail = report["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("punct_missing=wubi86=config-missing"),
        "detail 必须点名缺口：{detail}"
    );
}

/// 用户自己写的 / 用户主动关掉定制的 custom.yaml ⇒ skipped，不是缺陷。
///
/// skipped 不进 punct.missing ⇒ 既不改结论也不改退出码。
#[test]
fn skipped_punct_does_not_change_the_result() {
    let fixture = Fixture::new("punct-skipped");
    fixture.write_custom(USER_OWNED_CUSTOM);
    fixture.write_artifact(ARTIFACT_COMPLETE);

    let output = fixture.run(&["check-ime", "--json"]);
    let report = json_of(&output);

    assert_eq!(report["build_ok"], true, "报告：{report}");
    assert_eq!(report["numpad"]["ok"], true, "报告：{report}");
    assert_eq!(report["punct"]["ok"], true, "报告：{report}");
    let detail = report["punct"]["detail"].as_array().expect("array");
    let text = detail
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("wubi86=skipped"),
        "用户自己的文件必须判 skipped：{report}"
    );
    assert_eq!(report["result"], "ok", "报告：{report}");
    assert!(
        output.status.success(),
        "skipped 不是缺口，退出码必须是 0（实际 {:?}）",
        output.status.code()
    );
}
