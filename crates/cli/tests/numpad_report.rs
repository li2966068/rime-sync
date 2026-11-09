//! CLI 侧的「常驻小键盘判定」回归：check-ime / ensure-ime 的结论一致 + 只读。
//!
//! check-ime 是只读命令（NSIS 自检用的就是它），所以它必须：
//! * 报出小键盘缺口（不许报 ok 骗人），并以非零退出码结束；
//! * 与 ensure-ime 给出**同一个**结论（e2e 用例 e 的精神：只读判据 = 写路径的结论）；
//! * 一个字节都不许改用户目录（字节与 mtime 都不变）。
//!
//! 全部用临时目录：不装输入法、不部署、不打 data 补丁。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 227 形状的用户 custom.yaml：带 rime-sync 标记、有 rimesync_vselect、没有 numpad。
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

/// 来源侧已经修好的 custom.yaml（processors 里有 numpad）。
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

/// 编译产物：有 rime-sync 定制，但**没有** numpad（= 引擎还没重编译）。
const ARTIFACT_STALE: &str = "\
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
engine:
  processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
";

/// 编译产物：含 numpad（小键盘已经编译进去了）。
const ARTIFACT_OK: &str = "\
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
engine:
  processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
    - numpad
";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-cli-numpad-{}-{seq}-{tag}",
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

    fn custom(&self) -> PathBuf {
        self.rime().join("wubi86.custom.yaml")
    }

    fn write_custom(&self, text: &str) {
        fs::write(self.custom(), text).expect("write custom");
    }

    /// 产物（用户目录与 payload 各一份，内容相同）。
    fn write_artifact(&self, text: &str) {
        for dir in [self.rime().join("build"), self.payload().join("build")] {
            fs::write(dir.join("wubi86.schema.yaml"), text).expect("write schema");
            fs::write(dir.join("wubi86.prism.bin"), b"prism").expect("write bin");
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

/// check-ime：227 形状的来源侧（没 numpad）→ 报缺口、非零退出、且只读。
#[test]
fn check_ime_reports_the_gap_and_stays_read_only() {
    let fixture = Fixture::new("gap");
    fixture.write_custom(OLD_CUSTOM);
    fixture.write_artifact(ARTIFACT_STALE);

    let path = fixture.custom();
    let before_bytes = fs::read(&path).expect("read custom");
    let before_mtime = fs::metadata(&path)
        .expect("stat")
        .modified()
        .expect("mtime");

    let output = fixture.run(&["check-ime", "--json"]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "有缺口就必须非零退出：stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report = json_of(&output);
    assert_eq!(report["result"], "partial", "报告：{report}");
    assert_eq!(report["numpad"]["ok"], false, "报告：{report}");
    assert_eq!(
        report["numpad"]["missing"][0], "wubi86=source-stale",
        "报告：{report}"
    );
    assert!(
        report["numpad"]["hint"]
            .as_str()
            .expect("hint")
            .contains("修复输入法"),
        "hint 必须给出能执行的下一步：{report}"
    );

    // 只读：字节与 mtime 都不许变。
    assert_eq!(
        fs::read(&path).expect("read custom"),
        before_bytes,
        "check-ime 不许改字节"
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("stat")
            .modified()
            .expect("mtime"),
        before_mtime,
        "check-ime 不许改 mtime"
    );
}

/// check-ime 与 ensure-ime 必须给出同一个结论（只读判据 = 写路径的结论）。
#[test]
fn check_ime_and_ensure_ime_agree() {
    let fixture = Fixture::new("agree");
    fixture.write_custom(HEALTHY_CUSTOM);
    fixture.write_artifact(ARTIFACT_STALE);

    let check = json_of(&fixture.run(&["check-ime", "--json"]));
    assert_eq!(check["result"], "partial", "check-ime：{check}");
    assert_eq!(
        check["numpad"]["missing"][0], "wubi86=artifact-stale",
        "check-ime：{check}"
    );

    let ensure = fixture.run(&["ensure-ime", "--no-data-patch", "--json"]);
    let report = json_of(&ensure);
    assert_eq!(report["result"], "partial", "ensure-ime：{report}");
    assert_eq!(
        report["numpad"]["missing"][0], "wubi86=artifact-stale",
        "ensure-ime：{report}"
    );
    assert_eq!(
        report["numpad"]["missing"], check["numpad"]["missing"],
        "check-ime 与 ensure-ime 必须一致：{report} vs {check}"
    );
    assert_ne!(ensure.status.code(), Some(0), "partial 必须非零退出");
}

/// 产物里已经有 numpad → 不许误报。
#[test]
fn check_ime_is_ok_when_the_artifact_has_numpad() {
    let fixture = Fixture::new("healthy");
    fixture.write_custom(HEALTHY_CUSTOM);
    fixture.write_artifact(ARTIFACT_OK);

    let output = fixture.run(&["check-ime", "--json"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "健康机器必须退出 0：stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report = json_of(&output);
    assert_eq!(report["result"], "ok", "报告：{report}");
    assert_eq!(report["numpad"]["ok"], true, "报告：{report}");
    assert_eq!(
        report["numpad"]["missing"].as_array().expect("array").len(),
        0
    );
}

/// 文本模式也要有一行人话。
#[test]
fn text_mode_prints_a_human_readable_numpad_line() {
    let fixture = Fixture::new("text");
    fixture.write_custom(HEALTHY_CUSTOM);
    fixture.write_artifact(ARTIFACT_STALE);

    let output = fixture.run(&["check-ime"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("numpad=问题"),
        "文本模式必须有一行人话：{stdout}"
    );
    assert!(
        stdout.contains("wubi86=artifact-stale"),
        "文本模式必须点名方案与档位：{stdout}"
    );
}

/// ensure-ime 的 JSON 里也必须有 numpad 段。
#[test]
fn ensure_ime_json_carries_the_numpad_section() {
    let fixture = Fixture::new("ensure-json");
    fixture.write_custom(HEALTHY_CUSTOM);
    fixture.write_artifact(ARTIFACT_OK);

    let output = fixture.run(&["ensure-ime", "--no-data-patch", "--json"]);
    let report = json_of(&output);

    assert!(report["numpad"].is_object(), "报告：{report}");
    assert!(report["numpad"]["ok"].is_boolean(), "报告：{report}");
    assert!(report["numpad"]["missing"].is_array(), "报告：{report}");
    assert!(report["numpad"]["detail"].is_array(), "报告：{report}");
    assert!(report["numpad"]["hint"].is_string(), "报告：{report}");
}

/// rime 目录不存在时的 check-ime 也不能崩。
#[test]
fn check_ime_on_empty_dir_does_not_panic() {
    let fixture = Fixture::new("empty");
    let output = fixture.run(&["check-ime", "--json"]);
    assert!(output.status.code().is_some(), "必须有退出码");
    let report = json_of(&output);
    assert_eq!(report["result"], "failed", "报告：{report}");
    assert!(report["numpad"].is_object(), "报告：{report}");
}

/// 用户目录里的文件一个字节都不许被 check-ime 动过（目录级快照）。
#[test]
fn check_ime_touches_nothing_in_the_user_dir() {
    let fixture = Fixture::new("snapshot");
    fixture.write_custom(OLD_CUSTOM);
    fixture.write_artifact(ARTIFACT_STALE);
    fs::write(
        fixture.rime().join("default.custom.yaml"),
        "patch:\n  schema_list: []\n",
    )
    .expect("write default");

    let snapshot = |dir: &Path| -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            for entry in fs::read_dir(&current).expect("read dir").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let meta = fs::metadata(&path).expect("stat");
                    out.push((
                        path.clone(),
                        fs::read(&path).expect("read"),
                        meta.modified().expect("mtime"),
                    ));
                }
            }
        }
        out.sort();
        out
    };

    let before = snapshot(&fixture.rime());
    let _ = fixture.run(&["check-ime", "--json"]);
    let after = snapshot(&fixture.rime());

    assert_eq!(before.len(), after.len(), "文件数不许变");
    for (before_item, after_item) in before.iter().zip(after.iter()) {
        assert_eq!(before_item.0, after_item.0);
        assert_eq!(
            before_item.1, after_item.1,
            "字节不许变：{:?}",
            before_item.0
        );
        assert_eq!(
            before_item.2, after_item.2,
            "mtime 不许变：{:?}",
            before_item.0
        );
    }
}
