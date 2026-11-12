//! CLI 侧的「增强组件可用性」回归：check-ime / fix-ime 必须把
//! 「本机输入法引擎带没带 rime-sync 的增强组件」报出来。
//!
//! 背景：这四个组件（数字小键盘 / 中文标点直出 / v 转数字 / v 选择）是 rime-sync
//! 自研、编进 librime 本体的，发行版 librime 里没有。方案里一引用它们就报
//! `error creating processor: 'numpad'`，**整个方案直接不可用**。
//!
//! 所以：
//! * JSON：顶层字段 `extra_support`（supported / unsupported / unknown）；
//! * 人话：一行 `extra_support=<值>`，值不是 supported 时还要附上 core 写的建议原文。
//!
//! 全部用临时目录：不装输入法、不部署、不打 data 补丁。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use rime_sync_core::ime_state;

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 合法的三个取值（与 core 的 ExtraSupport::as_str 一致）。
const VALID: [&str; 3] = ["supported", "unsupported", "unknown"];

/// 带 rime-sync 标记的编译产物（core 判「确定支持」用的就是 rimesync 这个标记）。
const MARKED_ARTIFACT: &str = "patch:\n  rimesync_vnum: '['\n  rimesync_punct: ']'\n";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-cli-extra-support-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in [
            "payload/build",
            "payload/config",
            "rime/build",
            "state",
            "data",
        ] {
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

    /// 写一份带 rime-sync 标记的编译产物（用户目录 + payload 各一份）。
    fn write_marked_artifact(&self) {
        for dir in [self.rime().join("build"), self.payload().join("build")] {
            fs::write(dir.join("luna_pinyin_simp.schema.yaml"), MARKED_ARTIFACT)
                .expect("write schema");
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"));
        command
            .arg("--config")
            .arg(self.root.join("state/config.json"))
            .arg("--rime-dir")
            .arg(self.rime())
            .args(args)
            .env("RIMESYNC_PREBUILT_DIR", self.payload())
            .env("RIMESYNC_INSTALL_IME", "0")
            .env("RIMESYNC_PRUNE_SCHEMAS", "0");
        command.output().expect("run rime-sync-cli")
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

/// check-ime 的 JSON 必须带 extra_support，取值必须是三个合法值之一。
#[test]
fn check_ime_json_carries_extra_support() {
    let fixture = Fixture::new("check-json");
    fixture.write_marked_artifact();

    let output = fixture.run(&["check-ime", "--json"]);
    let report = json_of(&output);
    let value = report["extra_support"]
        .as_str()
        .expect("extra_support 必须是字符串字段")
        .to_string();
    assert!(
        VALID.contains(&value.as_str()),
        "extra_support 取值非法：{value}，报告：{report}"
    );
    // 产物里有 rime-sync 标记 → core 判定「确定支持」，CLI 必须照抄 core 的结论。
    assert_eq!(value, "supported", "报告：{report}");
}

/// CLI 的结论必须与 core 的判定一致（CLI 不许自己另判一套）。
#[test]
fn check_ime_agrees_with_core_assessment() {
    let fixture = Fixture::new("agree");
    fixture.write_marked_artifact();

    let report = json_of(&fixture.run(&["check-ime", "--json"]));
    let expected = ime_state::assess_extra_support(&fixture.rime());
    assert_eq!(
        report["extra_support"].as_str(),
        Some(expected.as_str()),
        "报告：{report}"
    );
}

/// 人话输出必须有一行 extra_support=<值>；不可用/判断不了时还要附上 core 的建议原文。
#[test]
fn check_ime_text_output_shows_extra_support_and_advice() {
    let fixture = Fixture::new("check-text");
    fixture.write_marked_artifact();

    let output = fixture.run(&["check-ime"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = ime_state::assess_extra_support(&fixture.rime());
    assert!(
        stdout.contains(&format!("extra_support={}", expected.as_str())),
        "人话输出必须有一行 extra_support=<值>：{stdout}"
    );
    if !expected.usable() {
        assert!(
            stdout.contains(expected.advice()),
            "不可用/判断不了时必须把 core 的建议原样打出来：{stdout}"
        );
    }
}

/// fix-ime 的 JSON 同样必须带 extra_support。
#[test]
fn fix_ime_json_carries_extra_support() {
    let fixture = Fixture::new("fix-json");
    fixture.write_marked_artifact();

    let output = fixture.run(&["fix-ime", "--json", "--no-data-patch"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("fix-ime 的 JSON 报告");
    let value = report["extra_support"]
        .as_str()
        .expect("extra_support 必须是字符串字段")
        .to_string();
    assert!(
        VALID.contains(&value.as_str()),
        "extra_support 取值非法：{value}，报告：{report}"
    );
}
