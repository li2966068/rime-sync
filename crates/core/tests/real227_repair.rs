//! 用 227 真机上真实损坏的方案文件做回归（不是合成数据）。
//!
//! 背景：227（Windows 10 / Weasel 0.17.4）上 data 目录的 luna_pinyin.schema.yaml
//! 在 0.6.19 时代被两条补丁路径各写了一遍 —— recognizer/patterns 下两份 vnum
//! （一份双引号、一份单引号）、engine/translators 下两份 rimesync_vnum、
//! engine/processors 下两份 rimesync_vselect 与两份 numpad —— serde_yaml 与
//! librime 都会拒绝这种重复键（duplicate entry with key "vnum"），方案加载失败。
//!
//! 夹具说明：
//! * real227_luna_pinyin.broken.yaml —— 从 227 拉回的损坏态（重复项位置与真机一致）。
//! * real227_luna_pinyin.orig.yaml  —— 同目录下的 *.rimesync-orig 备份（0.6.19 首次
//!   patch 之前的干净版，无 vnum、无 rimesync_vnum）。

use rime_sync_core::data_patch::{patch_file_detailed, PatchOutcome};
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const BROKEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real227_luna_pinyin.broken.yaml"
);
const ORIG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real227_luna_pinyin.orig.yaml"
);

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

fn case(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn real_227_broken_schema_with_backup_is_recovered() {
    let dir = case("real227_with_backup");
    let file = dir.join("luna_pinyin.schema.yaml");
    fs::write(&file, fs::read(BROKEN).unwrap()).unwrap();
    fs::write(
        dir.join("luna_pinyin.schema.yaml.rimesync-orig"),
        fs::read(ORIG).unwrap(),
    )
    .unwrap();

    let (outcome, _) = patch_file_detailed(&file, "0.6.25").unwrap();
    assert!(
        matches!(outcome, PatchOutcome::Recovered),
        "有备份时应当自动恢复，实际 {:?}",
        outcome
    );

    // 恢复后必须合法，且各处补丁都只剩一份。
    let text = fs::read_to_string(&file).unwrap();
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

    // 损坏现场留一份，且与损坏文件逐字节一致。
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

#[test]
fn real_227_broken_schema_without_backup_is_reported_and_untouched() {
    let dir = case("real227_no_backup");
    let file = dir.join("luna_pinyin.schema.yaml");
    let original = fs::read(BROKEN).unwrap();
    fs::write(&file, &original).unwrap();

    let err = patch_file_detailed(&file, "0.6.25").expect_err("没有备份时应当报错");
    let msg = format!("{}", err);
    assert!(msg.contains("vnum"), "错误信息要点名重复的键：{}", msg);
    assert!(
        msg.contains("luna_pinyin.schema.yaml"),
        "错误信息要点名文件：{}",
        msg
    );
    assert_eq!(fs::read(&file).unwrap(), original, "原文件必须一个字节不动");
    assert!(names_with(&dir, ".rimesync-broken").is_empty(), "不该造现场副本");
    assert!(names_with(&dir, ".rimesync-orig").is_empty(), "不该造备份");
    // 备份目录里也不能凭空多出文件。
    assert!(fs::read_dir(FIXTURES).unwrap().count() >= 2);
}
