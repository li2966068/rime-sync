//! 第三方词库清洗：剔除 code 里含「非法单音节」的词条。
//!
//! 背景（227 实测）：搜狗一类的第三方词库混着英文词，code 被逐字母拆开
//! （`web` → `w e b`）。这些单音节会挤进 librime 的音节表，把
//! `speller/algebra` 的首字母缩写索引顶掉 ⇒ 用户敲 `bhzt` 查不出「渤海钻探」。
//!
//! 覆盖：碎音节识别；白名单从**基准词库**推导（官方 pinyin_simp 的 `m`/`n`
//! 不许被误删）；官方词库本身不清洗；用户目录里的官方副本算基准不算第三方；
//! 幂等；BOM 与混合行尾保持原样；没脏词条时一个字节都不写；备份钩子被调用；
//! 合并词典检测路径。
//!
//! 不依赖平台、不碰真实 Rime 目录：全部在临时目录里搭最小场景。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_core::dict_sanitize::{
    base_single_syllables, collect_syllables, entry_has_broken_syllable, resolve_imported_dicts,
    sanitize_bytes, sanitize_imported_dicts, sanitize_in_place,
};

const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-dict-sanitize-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir parent");
    }
    fs::write(path, content.as_bytes()).expect("write");
}

/// 合并词典包装器（带 import_tables）。
fn merged_dict_yaml(name: &str, tables: &[&str]) -> String {
    let mut text = String::from("# 合并词典\n---\n");
    text.push_str(&format!("name: {name}\n"));
    text.push_str("version: \"2026.11.11\"\n");
    text.push_str("import_tables:\n");
    for table in tables {
        text.push_str(&format!("    - {table}\n"));
    }
    text.push_str("...\n");
    text
}

/// 一份第三方词库：合法中文 + 逐字母拆开的英文。
fn third_party_dict() -> String {
    let mut text = String::from("---\nname: extra_sogou\nversion: \"2026.11.11\"\n...\n");
    text.push_str("渤海\tdeng hai\t100\n");
    text.push_str("中文\tdong wen\t90\n");
    text.push_str("web\tw e b\t34\n");
    text.push_str("docker\td o c k e r\t43\n");
    text.push_str("admin\ta d m i n\t44\n");
    text
}

/// 官方 pinyin_simp 里合法存在的单音节词条（`m`/`n`）。
fn official_pinyin_simp_dict() -> String {
    let mut text = String::from("---\nname: pinyin_simp\nversion: \"2026.11.11\"\n...\n");
    text.push_str("呣\tm\t100\n");
    text.push_str("唔\tn\t90\n");
    text.push_str("嗯\tn\t80\n");
    text.push_str("中文\tdong wen\t70\n");
    text
}

/// 官方 luna_pinyin：只有 a/o/e 三个单音节。
fn official_luna_dict() -> String {
    let mut text = String::from("---\nname: luna_pinyin\nversion: \"2026.11.11\"\n...\n");
    text.push_str("啊\ta\t100\n");
    text.push_str("哦\to\t90\n");
    text.push_str("饿\te\t80\n");
    text.push_str("中文\tdong wen\t70\n");
    text
}

#[test]
fn detects_broken_syllable_rows() {
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    assert!(entry_has_broken_syllable(b"web\tw e b\t34", &allowed));
    assert!(entry_has_broken_syllable(b"admin\ta d m i n\t44", &allowed));
    assert!(!entry_has_broken_syllable(
        "渤海\tdeng hai\t100".as_bytes(),
        &allowed
    ));
    assert!(!entry_has_broken_syllable(
        "啊\ta\t100".as_bytes(),
        &allowed
    ));
    // 没有 code 列的行不算。
    assert!(!entry_has_broken_syllable("只有一列".as_bytes(), &allowed));
}

#[test]
fn whitelist_comes_from_base_dicts() {
    let dir = temp_dir("whitelist");
    let base = dir.join("pinyin_simp.dict.yaml");
    write(&base, &official_pinyin_simp_dict());
    let allowed = base_single_syllables(&[&base]);
    // 官方词库里的合法单音节 m/n 必须在白名单里。
    assert!(allowed.contains("m"));
    assert!(allowed.contains("n"));
    // 兜底三音节也必须在。
    assert!(allowed.contains("a"));
    assert!(allowed.contains("e"));
    assert!(allowed.contains("o"));
    // 第三方词库里的 w/e/b 不在白名单（e 在，w/b 不在）。
    assert!(!allowed.contains("w"));
    assert!(!allowed.contains("b"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn official_single_letters_are_not_deleted() {
    // 官方 pinyin_simp 自己的 `呣 m` / `唔 n` 不许被当成垃圾。
    let dir = temp_dir("official-single");
    let base = dir.join("pinyin_simp.dict.yaml");
    write(&base, &official_pinyin_simp_dict());
    let allowed = base_single_syllables(&[&base]);
    assert!(!entry_has_broken_syllable(
        "呣\tm\t100".as_bytes(),
        &allowed
    ));
    assert!(!entry_has_broken_syllable("唔\tn\t90".as_bytes(), &allowed));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sanitize_bytes_keeps_header_and_drops_broken_rows() {
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    let (cleaned, removed) = sanitize_bytes(third_party_dict().as_bytes(), &allowed).expect("dict");
    assert_eq!(removed, 3);
    let text = String::from_utf8(cleaned).expect("utf8");
    // 头部原样保留。
    assert!(text.contains("name: extra_sogou"));
    assert!(text.contains("version: \"2026.11.11\""));
    // 合法词条保留，坏词条删除。
    assert!(text.contains("渤海\tdeng hai\t100"));
    assert!(!text.contains("w e b"));
    assert!(!text.contains("d o c k e r"));
    assert!(!text.contains("a d m i n"));
}

#[test]
fn sanitize_bytes_preserves_bom_and_mixed_line_endings() {
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    let mut raw: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    raw.extend_from_slice(b"---\r\nname: extra\r\n...\r\n");
    raw.extend_from_slice("渤海\tdeng hai\t100\r\n".as_bytes());
    raw.extend_from_slice("web\tw e b\t34\r\n".as_bytes());
    raw.extend_from_slice("中文\tdong wen\t90\n".as_bytes());
    let (cleaned, removed) = sanitize_bytes(&raw, &allowed).expect("dict");
    assert_eq!(removed, 1);
    assert_eq!(&cleaned[..3], &[0xEF, 0xBB, 0xBF]);
    let text = String::from_utf8(cleaned).expect("utf8");
    assert!(text.contains("\r\n"));
    assert!(text.ends_with("中文\tdong wen\t90\n"));
}

#[test]
fn non_dict_file_is_untouched() {
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    // 没有 `...` 标记 ⇒ 不是词典文件，不该动。
    assert!(sanitize_bytes(b"---\nname: x\n", &allowed).is_none());
}

#[test]
fn clean_file_is_not_rewritten() {
    let dir = temp_dir("no-rewrite");
    let path = dir.join("extra_sogou.dict.yaml");
    let original = "---\nname: extra_sogou\n...\n渤海\tdeng hai\t100\n";
    write(&path, original);
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    let mut backup_calls = 0usize;
    let outcome = sanitize_in_place(&path, &allowed, &mut |_p| {
        backup_calls += 1;
        Ok(())
    })
    .expect("sanitize");
    assert!(!outcome.changed);
    assert_eq!(outcome.removed, 0);
    // 没脏词条 ⇒ 不写、不备份。
    assert_eq!(backup_calls, 0);
    assert_eq!(fs::read_to_string(&path).expect("read"), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backup_hook_runs_before_write_and_orig_is_kept() {
    let dir = temp_dir("backup");
    let path = dir.join("extra_sogou.dict.yaml");
    write(&path, &third_party_dict());
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    let mut seen: Vec<PathBuf> = Vec::new();
    let outcome = sanitize_in_place(&path, &allowed, &mut |p| {
        seen.push(p.to_path_buf());
        Ok(())
    })
    .expect("sanitize");
    assert!(outcome.changed);
    assert_eq!(outcome.removed, 3);
    assert_eq!(seen, vec![path.clone()]);
    // `.rimesync-orig` 里是**原始**内容。
    let orig = path.with_file_name("extra_sogou.dict.yaml.rimesync-orig");
    assert!(orig.is_file());
    let orig_text = fs::read_to_string(&orig).expect("read orig");
    assert!(orig_text.contains("w e b"));
    // 目标文件已是干净的。
    let text = fs::read_to_string(&path).expect("read");
    assert!(!text.contains("w e b"));
    assert!(text.contains("渤海"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backup_failure_leaves_file_untouched() {
    let dir = temp_dir("backup-fail");
    let path = dir.join("extra_sogou.dict.yaml");
    let original = third_party_dict();
    write(&path, &original);
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    let result = sanitize_in_place(&path, &allowed, &mut |_p| Err(anyhow::anyhow!("boom")));
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).expect("read"), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sanitize_is_idempotent() {
    let dir = temp_dir("idempotent");
    let path = dir.join("extra_sogou.dict.yaml");
    write(&path, &third_party_dict());
    let allowed: BTreeSet<String> = ["a", "e", "o"].iter().map(|s| (*s).to_string()).collect();
    let first = sanitize_in_place(&path, &allowed, &mut |_p| Ok(())).expect("first");
    assert!(first.changed);
    let after_first = fs::read_to_string(&path).expect("read");
    let second = sanitize_in_place(&path, &allowed, &mut |_p| Ok(())).expect("second");
    assert!(!second.changed);
    assert_eq!(second.removed, 0);
    assert_eq!(fs::read_to_string(&path).expect("read"), after_first);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collect_syllables_sees_single_letters() {
    let dir = temp_dir("syllables");
    let path = dir.join("extra_sogou.dict.yaml");
    write(&path, &third_party_dict());
    let bytes = fs::read(&path).expect("read");
    let syllables = collect_syllables(&bytes);
    assert!(syllables.contains("w"));
    assert!(syllables.contains("hai"));
    assert!(syllables.contains("deng"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolve_marks_third_party_and_base_correctly() {
    let dir = temp_dir("resolve");
    let rime_dir = dir.join("user");
    let data_dir = dir.join("data");
    fs::create_dir_all(&rime_dir).expect("mkdir");
    fs::create_dir_all(&data_dir).expect("mkdir");
    write(
        &data_dir.join("luna_pinyin.dict.yaml"),
        &official_luna_dict(),
    );
    write(
        &data_dir.join("pinyin_simp.dict.yaml"),
        &official_pinyin_simp_dict(),
    );
    // 用户目录里：一份第三方词库 + 一份官方词库的副本。
    write(&rime_dir.join("extra_sogou.dict.yaml"), &third_party_dict());
    write(
        &rime_dir.join("pinyin_simp.dict.yaml"),
        &official_pinyin_simp_dict(),
    );
    let data_dirs = vec![data_dir.clone(), rime_dir.clone()];
    let refs = resolve_imported_dicts(
        &rime_dir,
        &data_dirs,
        &[
            "luna_pinyin".to_string(),
            "pinyin_simp".to_string(),
            "extra_sogou".to_string(),
        ],
    );
    let luna = refs.iter().find(|r| r.name == "luna_pinyin").expect("luna");
    assert!(!luna.user_owned);
    assert!(luna.base_path.is_some());
    // 用户目录里的官方副本 ⇒ 算基准，不算第三方（否则 m/n 会被误删）。
    let simp = refs.iter().find(|r| r.name == "pinyin_simp").expect("simp");
    assert!(!simp.user_owned);
    assert!(simp.base_path.is_some());
    // 纯第三方 ⇒ 要清洗。
    let sogou = refs
        .iter()
        .find(|r| r.name == "extra_sogou")
        .expect("sogou");
    assert!(sogou.user_owned);
    assert!(sogou.base_path.is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sanitize_imported_dicts_cleans_only_third_party() {
    let dir = temp_dir("imported");
    let rime_dir = dir.join("user");
    let data_dir = dir.join("data");
    fs::create_dir_all(&rime_dir).expect("mkdir");
    fs::create_dir_all(&data_dir).expect("mkdir");
    write(
        &data_dir.join("luna_pinyin.dict.yaml"),
        &official_luna_dict(),
    );
    write(
        &data_dir.join("pinyin_simp.dict.yaml"),
        &official_pinyin_simp_dict(),
    );
    write(&rime_dir.join("extra_sogou.dict.yaml"), &third_party_dict());
    write(
        &rime_dir.join("pinyin_simp.dict.yaml"),
        &official_pinyin_simp_dict(),
    );
    // 合并词典包装器：把官方词库和第三方词库合并进来。
    write(
        &rime_dir.join("merged.dict.yaml"),
        &merged_dict_yaml("merged", &["luna_pinyin", "pinyin_simp", "extra_sogou"]),
    );
    let data_dirs = vec![data_dir.clone(), rime_dir.clone()];
    let outcomes =
        sanitize_imported_dicts(&rime_dir, &data_dirs, &mut |_p| Ok(())).expect("sanitize");
    let changed: Vec<&str> = outcomes
        .iter()
        .filter(|outcome| outcome.changed)
        .map(|outcome| outcome.dict.as_str())
        .collect();
    assert_eq!(changed, vec!["extra_sogou"]);
    // 官方词库（含用户目录副本）一个字节都没动。
    let simp_text = fs::read_to_string(rime_dir.join("pinyin_simp.dict.yaml")).expect("read");
    assert!(simp_text.contains("呣\tm"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn no_merged_dict_nothing_happens() {
    let dir = temp_dir("no-merged");
    let rime_dir = dir.join("user");
    fs::create_dir_all(&rime_dir).expect("mkdir");
    write(&rime_dir.join("extra_sogou.dict.yaml"), &third_party_dict());
    let data_dirs = vec![rime_dir.clone()];
    let outcomes =
        sanitize_imported_dicts(&rime_dir, &data_dirs, &mut |_p| Ok(())).expect("sanitize");
    assert!(outcomes.is_empty());
    // 没有合并词典 ⇒ 不该动任何词库文件。
    let text = fs::read_to_string(rime_dir.join("extra_sogou.dict.yaml")).expect("read");
    assert!(text.contains("w e b"));
    let _ = fs::remove_dir_all(&dir);
}
