//! 缺陷 A 的恢复路径回归：227 真机上真实损坏的方案文件。
//!
//! 背景：227（Windows 10 / Weasel 0.17.4）上 data 目录的 luna_pinyin.schema.yaml
//! 在 0.6.19 时代被两条补丁路径各写了一遍 —— recognizer/patterns 下两份 vnum
//! （一份双引号、一份单引号），serde_yaml 与 librime 都会拒绝这种重复键
//! （duplicate entry with key "vnum"），方案加载失败。
//!
//! 夹具：
//! * real227_luna_pinyin.broken.yaml —— 从 227 拉回的损坏态（重复项位置与真机一致）。
//! * real227_luna_pinyin.orig.yaml  —— 同目录的 *.rimesync-orig 备份（0.6.19 首次
//!   patch 之前的干净版，无 vnum、无 rimesync_vnum）。
//!
//! 恢复顺序（零损优先）：
//! ① 文本级去重 —— 只删掉多余的那一行，其余内容全部保留，不需要备份；
//! ② 用 *.rimesync-orig 备份覆盖回来重打补丁（去重不适用时兜底）；
//! ③ 两条都不行才报错，且原文件一个字节不动。

use rime_sync_core::data_patch::{patch_file_detailed, PatchOutcome};
use std::fs;
use std::path::{Path, PathBuf};

const BROKEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real227_luna_pinyin.broken.yaml"
);
const ORIG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real227_luna_pinyin.orig.yaml"
);

/// 用户手工加的一行：任何恢复方式都不该把它弄丢。
const USER_LINE: &str = "# 用户自己加的一行：我的分词偏好";

fn names_with(dir: &Path, needle: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(needle))
        .collect();
    v.sort();
    v
}

fn count_seq(v: &serde_yaml::Value, path: &[&str], want: &str) -> usize {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p).unwrap_or(&serde_yaml::Value::Null);
    }
    cur.as_sequence()
        .map(|s| s.iter().filter(|x| x.as_str() == Some(want)).count())
        .unwrap_or(0)
}

/// 把损坏文件写进临时目录，并按需放一份 *.rimesync-orig 备份。
/// user_line 非空时，在文件里插一行用户自己的内容（模拟手工改动）。
fn case(name: &str, with_backup: bool, user_line: &str) -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("luna_pinyin.schema.yaml");
    let mut text = fs::read_to_string(BROKEN).unwrap();
    if !user_line.is_empty() {
        text = format!("{user_line}\n{text}");
    }
    fs::write(&file, text).unwrap();
    if with_backup {
        fs::write(
            dir.join("luna_pinyin.schema.yaml.rimesync-orig"),
            fs::read(ORIG).unwrap(),
        )
        .unwrap();
    }
    (dir, file)
}

/// 恢复后必须满足：YAML 合法、我们负责的四处补丁各只剩一份、符号直出正确。
fn assert_healthy(file: &Path) {
    let text = fs::read_to_string(file).unwrap();
    let v: serde_yaml::Value = serde_yaml::from_str(&text).expect("恢复后必须是合法 YAML");
    let vnum = v
        .get("recognizer")
        .and_then(|r| r.get("patterns"))
        .and_then(|p| p.as_mapping())
        .map(|m| m.iter().filter(|(k, _)| k.as_str() == Some("vnum")).count())
        .unwrap_or(0);
    assert_eq!(vnum, 1, "recognizer.patterns 下只能有一份 vnum");
    assert_eq!(count_seq(&v, &["engine", "translators"], "rimesync_vnum"), 1);
    assert_eq!(count_seq(&v, &["engine", "processors"], "rimesync_vselect"), 1);
    assert_eq!(count_seq(&v, &["engine", "processors"], "numpad"), 1);
    let half = v
        .get("punctuator")
        .and_then(|p| p.get("half_shape"))
        .and_then(|h| h.as_mapping());
    assert!(half.is_some(), "punctuator.half_shape 应该在");
    assert_eq!(
        half.unwrap().get("\\").and_then(|x| x.as_str()),
        Some("\u{3001}"),
        "反斜杠应映射成顿号"
    );
}

/// 序列里出现同名项是合法 YAML，重复键问题只出在映射里 —— 补丁流程负责归一。
#[test]
fn broken_schema_without_backup_is_recovered_by_text_dedupe() {
    let (dir, file) = case("dedupe_no_backup", false, "");

    let (outcome, _) = patch_file_detailed(&file, "0.6.25").unwrap();
    assert!(
        matches!(outcome, PatchOutcome::Recovered),
        "没有备份也应当能自愈，实际 {:?}",
        outcome
    );
    assert_healthy(&file);
    // 现场留一份，且与损坏文件逐字节一致。
    let copies = names_with(&dir, ".rimesync-broken");
    assert_eq!(copies.len(), 1, "应恰好留一份现场副本");
    assert_eq!(
        fs::read(dir.join(&copies[0])).unwrap(),
        fs::read(BROKEN).unwrap()
    );
    // 幂等：再跑一次不动文件，也不攒第二份现场副本。
    let before = fs::read_to_string(&file).unwrap();
    let (outcome2, _) = patch_file_detailed(&file, "0.6.25").unwrap();
    assert!(
        matches!(outcome2, PatchOutcome::Unchanged),
        "第二次应当无改动，实际 {:?}",
        outcome2
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), before);
    assert_eq!(names_with(&dir, ".rimesync-broken").len(), 1);
}

/// 去重要比「用备份覆盖」优先：用户手工加的内容不能被备份覆盖掉。
#[test]
fn text_dedupe_keeps_user_edits_even_when_a_backup_exists() {
    let (dir, file) = case("dedupe_with_backup", true, USER_LINE);

    let (outcome, _) = patch_file_detailed(&file, "0.6.25").unwrap();
    assert!(
        matches!(outcome, PatchOutcome::Recovered),
        "应当恢复，实际 {:?}",
        outcome
    );
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(USER_LINE),
        "用户自己加的那一行不该被丢掉（备份覆盖会丢它）"
    );
    assert_healthy(&file);
    assert_eq!(names_with(&dir, ".rimesync-broken").len(), 1);
}

/// 去重不适用时（重复键写在同一行的 flow 风格里），退回备份那条路；
/// 没有备份就只报错，原文件一个字节不动。
#[test]
fn flow_style_duplicate_key_is_reported_and_untouched() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dedupe_flow_style");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("flow.schema.yaml");
    let original = "recognizer: {patterns: {vnum: \"a\", vnum: \"b\"}}\n";
    fs::write(&file, original).unwrap();

    let err = patch_file_detailed(&file, "0.6.25").expect_err("flow 风格应当报错");
    let msg = format!("{}", err);
    assert!(msg.contains("vnum"), "错误信息要点名重复的键：{}", msg);
    assert!(msg.contains("flow.schema.yaml"), "错误信息要点名文件：{}", msg);
    assert_eq!(fs::read_to_string(&file).unwrap(), original, "原文件必须不动");
    assert!(names_with(&dir, ".rimesync-broken").is_empty(), "不该造现场副本");
    assert!(names_with(&dir, ".rimesync-orig").is_empty(), "不该造备份");
}

/// 备份本身非法时也不能拿它覆盖用户的好文件。
#[test]
fn broken_backup_is_not_used_to_overwrite_user_file() {
    let (dir, file) = case("dedupe_bad_backup", false, "");
    // 一份本身就不合法的备份：内容也是重复键。
    fs::write(
        dir.join("luna_pinyin.schema.yaml.rimesync-orig"),
        fs::read(BROKEN).unwrap(),
    )
    .unwrap();

    // 去重能救 → 直接修好（备份不合法也不影响）。
    let (outcome, _) = patch_file_detailed(&file, "0.6.25").unwrap();
    assert!(
        matches!(outcome, PatchOutcome::Recovered),
        "去重路径应当能修好，实际 {:?}",
        outcome
    );
    assert_healthy(&file);
}
