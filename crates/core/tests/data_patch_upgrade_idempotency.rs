//! data 方案补丁的升级幂等回归测试（vnum 重复键缺陷）。
//!
//! 缺陷现场：真机 data/luna_pinyin.schema.yaml 变成非法 YAML，报 duplicate entry
//! with key "vnum" —— 同一个映射里出现了两个 vnum 键。
//!
//! 这里只用两条与版本无关的事实：
//! * vnum 只有落在 recognizer/patterns 下才生效（Rime 只认这个位置），recognizer
//!   的其它层级不生效；
//! * 文件里已经存在一份 vnum 时，补丁器如果只认 patterns 一个位置，就会再插一份，
//!   两个 vnum 落进同一个映射 → YAML 非法 → 方案加载失败。
//!
//! 覆盖：位置不对的那份被搬进 patterns（值不改）、同版本连跑两次字节一致、
//! 已经在 patterns 里的用户 vnum 不被改写、已经损坏的文件有备份时自动恢复回来、
//! 没有备份时给出可读错误而不是静默跳过。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rime_sync_core::data_patch::{self, PatchOutcome};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("rime-sync-upgrade-idem-{}-{seq}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// 与 third_party/rime-data 里的朙月拼音同构的最小方案。
const STOCK: &str = "\
# luna_pinyin.schema
schema:
  schema_id: luna_pinyin
  name: 朙月拼音
engine:
  processors:
    - ascii_composer
    - recognizer
    - key_binder
    - speller
    - punctuator
    - selector
    - navigator
    - express_editor
  segmentors:
    - ascii_segmentor
    - matcher
    - abc_segmentor
    - fallback_segmentor
  translators:
    - punct_translator
    - table_translator@custom_phrase
    - reverse_lookup_translator
    - script_translator
recognizer:
  import_preset: default
  patterns:
    punct: \"^/([0-9]0?|[A-Za-z]+)$\"
    reverse_lookup: \"`[a-z]*'?$\"
";

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, text).expect("write file");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

/// 文件里 vnum 键的个数（只看「键名是 vnum」的行，不含 rimesync_vnum 之类）。
fn vnum_key_count(text: &str) -> usize {
    text.lines()
        .filter(|line| line.trim_start().starts_with("vnum:"))
        .count()
}

/// vnum 键所在的所有行（排查重复键用）。
fn vnum_key_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.trim_start().starts_with("vnum:"))
        .map(|line| line.to_string())
        .collect()
}

/// vnum 被放在 recognizer 块首（patterns 之前）——Rime 不认这个位置。
fn with_vnum_at_recognizer_head(value: &str) -> String {
    let mut out = String::new();
    let mut in_recognizer = false;
    for line in value.lines() {
        if in_recognizer && line.starts_with("  patterns:") {
            out.push_str("  vnum: \"^[vV][0-9.]*$\"\n");
        }
        out.push_str(line);
        out.push('\n');
        if line.trim_end() == "recognizer:" {
            in_recognizer = true;
        }
    }
    out
}

/// vnum 落在 recognizer 块首的文件。
fn file_with_vnum_at_recognizer_head() -> String {
    with_vnum_at_recognizer_head(STOCK)
}

#[test]
fn misplaced_vnum_at_recognizer_head_does_not_produce_a_second_vnum() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    let original = file_with_vnum_at_recognizer_head();
    write(&schema, &original);
    assert_eq!(vnum_key_count(&original), 1, "起点就是一份 vnum");

    let outcome = data_patch::patch_file(&schema, "0.6.25").expect("patch");
    assert_ne!(outcome, PatchOutcome::Skipped, "recognizer 结构适用");

    let text = read(&schema);
    assert_eq!(
        vnum_key_count(&text),
        1,
        "不能出现第二个 vnum 键：{:?}",
        vnum_key_lines(&text)
    );
    // 文件必须还是合法 YAML（重复键会让 serde_yaml 直接报错）。
    let value: serde_yaml::Value = serde_yaml::from_str(&text).expect("patched yaml is valid");
    assert_eq!(
        value["recognizer"]["patterns"]["vnum"].as_str(),
        Some(data_patch::VNUM_PATTERN),
        "vnum 必须落在 recognizer/patterns 下（Rime 只认这个位置）"
    );
    assert!(
        value["recognizer"].get("vnum").is_none(),
        "recognizer 块首那份错位的 vnum 不能留着"
    );
}

#[test]
fn misplaced_vnum_is_moved_without_changing_its_value() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    // 用户自己写的正则（不是我们的默认值）：搬进 patterns 时不能被改写。
    let original = with_vnum_at_recognizer_head(STOCK)
        .replace("vnum: \"^[vV][0-9.]*$\"", "vnum: '^v[0-9]+$'");
    write(&schema, &original);

    data_patch::patch_file(&schema, "0.6.25").expect("patch");

    let text = read(&schema);
    assert_eq!(vnum_key_count(&text), 1, "仍然只有一份 vnum");
    assert!(
        text.contains("vnum: '^v[0-9]+$'"),
        "既有 vnum 的值不能被改写：{text}"
    );
    let value: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
    assert_eq!(
        value["recognizer"]["patterns"]["vnum"].as_str(),
        Some("^v[0-9]+$")
    );
}

#[test]
fn vnum_already_in_patterns_is_kept_and_not_duplicated() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    let original = STOCK.replace(
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n",
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n    vnum: '^v[0-9]+$'\n",
    );
    write(&schema, &original);

    data_patch::patch_file(&schema, "0.6.25").expect("patch");

    let text = read(&schema);
    assert_eq!(vnum_key_count(&text), 1, "已有的 vnum 不能再插一份");
    assert!(
        text.contains("vnum: '^v[0-9]+$'"),
        "用户自己的 vnum 覆盖不被改写：{text}"
    );
    serde_yaml::from_str::<serde_yaml::Value>(&text).expect("valid yaml");
}

#[test]
fn patching_twice_is_byte_identical_and_writes_no_second_backup() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, &file_with_vnum_at_recognizer_head());
    let backup = data_patch::backup_path(&schema);

    data_patch::patch_file(&schema, "0.6.25").expect("first patch");
    let first = read(&schema);
    let backup_bytes = fs::read(&backup).expect("backup exists");
    assert_eq!(backup_bytes, file_with_vnum_at_recognizer_head().as_bytes(), "备份是原始内容");

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.25").expect("second patch"),
        PatchOutcome::Unchanged,
        "重复执行不再写入"
    );
    assert_eq!(read(&schema), first, "幂等：一个字节都不变");
    assert_eq!(
        fs::read(&backup).expect("backup still readable"),
        backup_bytes,
        "备份不被第二次执行覆盖"
    );
}

#[test]
fn file_with_existing_misplaced_vnum_stays_legal_and_single() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    // 文件里已经有一份位置不对的 vnum（落在 recognizer 块首）——不管它是谁写的。
    write(&schema, &file_with_vnum_at_recognizer_head());
    // 再 patch 一次（版本号不同也不能产生第二份）。
    data_patch::patch_file(&schema, "0.6.25").expect("new patch");

    let text = read(&schema);
    assert_eq!(vnum_key_count(&text), 1, "不管文件是谁写的，都不能重复");
    serde_yaml::from_str::<serde_yaml::Value>(&text).expect("valid yaml");

    // 再来一次别的版本：仍然幂等。
    let before = read(&schema);
    data_patch::patch_file(&schema, "0.6.26").expect("third patch");
    assert_eq!(read(&schema), before, "换版本号也不会再动文件");
}

/// 真机现场：recognizer/patterns 里已经有两份 vnum（YAML 直接非法）。
///
/// 这类损坏现在应当能自愈，而且**不需要备份**：只删掉多余的那一行，其余内容
/// （含用户自己写的）全部保留。改不动的情形（不是重复键、或者重复键写在同一行）
/// 由 illegal_yaml_that_cannot_be_deduped_* 那两条护栏盯着。
#[test]
fn already_broken_file_is_recovered_by_text_dedupe() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("broken.schema.yaml");
    let broken = STOCK.replace(
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n",
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n    vnum: \"^[vV][0-9.]*$\"\n    vnum: '^[vV][0-9.]*$'\n",
    );
    let user_line = "# 用户自己加的一行：不能被自动修复弄丢";
    write(&schema, &format!("{user_line}\n{broken}"));

    data_patch::patch_file(&schema, "0.6.25").expect("应当自动去重恢复");

    let text = read(&schema);
    assert_eq!(vnum_key_count(&text), 1, "恢复后只能有一份 vnum");
    serde_yaml::from_str::<serde_yaml::Value>(&text).expect("valid yaml");
    assert!(
        text.contains(user_line),
        "自动修复只能删多余的那一行，用户自己写的内容要留着"
    );
    // 损坏现场留一份，且原样。
    let copies = broken_copies(&schema);
    assert_eq!(copies.len(), 1, "用户现场要留一份：{copies:?}");
    assert!(
        read(&copies[0]).contains("vnum: \"^[vV][0-9.]*$\"")
            && read(&copies[0]).contains("vnum: '^[vV][0-9.]*$'"),
        "现场副本就是坏文件原样"
    );
}

/// 重复键写在同一行里（flow 风格）：删行救不了，也不能瞎删 —— 只报错，
/// 原文件一个字节不动。
#[test]
fn illegal_yaml_that_cannot_be_deduped_is_reported_and_left_untouched() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("flow.schema.yaml");
    let broken = "recognizer: {patterns: {vnum: \"a\", vnum: \"b\"}}\n";
    write(&schema, broken);
    let bytes_before = fs::read(&schema).expect("read");

    let error = data_patch::patch_file(&schema, "0.6.25").expect_err("必须报错，不能静默跳过");
    let message = format!("{error:#}");
    assert!(
        message.contains("不是合法 YAML"),
        "错误要说清楚是 YAML 不合法：{message}"
    );
    assert!(
        message.contains("vnum"),
        "错误要点名重复的键：{message}"
    );
    assert!(
        message.contains("重复"),
        "错误要说清是重复键（而不是把 serde 的英文报错直接甩给用户）：{message}"
    );
    assert!(
        message.contains(&schema.display().to_string()),
        "错误要点名是哪个文件：{message}"
    );
    assert_eq!(
        fs::read(&schema).expect("read"),
        bytes_before,
        "改不动的坏文件不能被改写"
    );
    assert!(
        !data_patch::backup_path(&schema).exists(),
        "没有成功写入就不该留备份"
    );
    assert!(
        broken_copies(&schema).is_empty(),
        "没有成功写入就不该造现场副本"
    );
}

#[test]
fn safety_gate_rejects_rendered_text_with_duplicate_keys() {
    let duplicated = "recognizer:
  patterns:
    vnum: 'a'
    vnum: 'b'
";
    let error = data_patch::check_rendered_yaml(duplicated).expect_err("重复键必须被拦下");
    let message = format!("{error:#}");
    assert!(
        message.contains("vnum"),
        "安全闸要点名重复的键：{message}"
    );
    assert!(
        message.contains("重复"),
        "安全闸要说清是重复键：{message}"
    );
}

/// 缩进宽度不是 2 格的方案文件（有些发行版用 4 格）：patterns 必须按实际缩进找，
/// 否则会在 recognizer 里再插一个 patterns 键，同一个映射里两个 patterns → YAML
/// 非法。
#[test]
fn wide_indentation_schema_gets_vnum_without_a_second_patterns_key() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("wide.schema.yaml");
    let original = "\
schema:
    schema_id: wide
recognizer:
    import_preset: default
    patterns:
        punct: \"^/([0-9]0?|[A-Za-z]+)$\"
";
    write(&schema, original);

    data_patch::patch_file(&schema, "0.6.24").expect("patch");

    let text = read(&schema);
    assert_eq!(
        text.lines()
            .filter(|line| line.trim_start().starts_with("patterns:"))
            .count(),
        1,
        "不能出现第二个 patterns 键：{text}"
    );
    assert_eq!(vnum_key_count(&text), 1, "vnum 只能有一份");
    let value: serde_yaml::Value = serde_yaml::from_str(&text).expect("valid yaml");
    assert_eq!(
        value["recognizer"]["patterns"]["vnum"].as_str(),
        Some(data_patch::VNUM_PATTERN)
    );
}

/// 同目录下 <file>.rimesync-broken* 现场副本（按文件名排序）。
fn broken_copies(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().expect("file name").to_string_lossy().to_string();
    let prefix = format!("{name}.rimesync-broken");
    let mut found: Vec<PathBuf> = fs::read_dir(path.parent().expect("parent dir"))
        .expect("read dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.starts_with(&prefix))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found
}

/// 同一映射里两遍 vnum：真机现场的非法 YAML。
fn with_duplicate_vnum(text: &str) -> String {
    let patched = text.replace(
        "    vnum: '^[vV][0-9.]*$'\n",
        "    vnum: '^[vV][0-9.]*$'\n    vnum: '^[vV][0-9.]*$'\n",
    );
    assert!(patched != text, "注入重复 vnum 失败");
    patched
}

/// 有原始备份的损坏文件：自动恢复回来并重新打补丁。
#[test]
fn broken_file_with_backup_is_recovered_and_patched() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, STOCK);
    // 第一次补丁留下 .rimesync-orig 原始备份。
    data_patch::patch_file(&schema, "0.6.24").expect("first patch");
    let backup = data_patch::backup_path(&schema);
    let backup_bytes = fs::read(&backup).expect("备份存在");

    // 人为弄坏：同一个映射里两遍 vnum。
    let broken = with_duplicate_vnum(&read(&schema));
    assert!(
        serde_yaml::from_str::<serde_yaml::Value>(&broken).is_err(),
        "起点就是非法 YAML"
    );
    write(&schema, &broken);

    let outcome = data_patch::patch_file(&schema, "0.6.24").expect("有备份就必须自动恢复");
    assert_eq!(outcome, PatchOutcome::Recovered, "outcome 要说明是恢复的");

    let text = read(&schema);
    let value: serde_yaml::Value = serde_yaml::from_str(&text).expect("恢复后必须是合法 YAML");
    assert_eq!(vnum_key_count(&text), 1, "恢复后只有一份 vnum");
    assert_eq!(
        value["recognizer"]["patterns"]["vnum"].as_str(),
        Some(data_patch::VNUM_PATTERN)
    );
    assert_eq!(
        fs::read(&backup).expect("备份"),
        backup_bytes,
        "备份不能被覆盖"
    );
    let copies = broken_copies(&schema);
    assert_eq!(copies.len(), 1, "用户现场要留一份：{copies:?}");
    assert_eq!(read(&copies[0]), broken, "现场副本就是坏文件原样");
}

/// 没有备份的损坏文件：靠文本级去重自愈，原文件一个字节都不动是不对的
/// —— 但修复必须是「只删多余的那一行」，其余内容一个字都不能少。
#[test]
fn broken_file_without_backup_is_recovered_by_text_dedupe() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("broken.schema.yaml");
    let broken = STOCK.replace(
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n",
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n    vnum: \"^[vV][0-9.]*$\"\n    vnum: '^[vV][0-9.]*$'\n",
    );
    write(&schema, &broken);
    let bytes_before = fs::read(&schema).expect("read");

    data_patch::patch_file(&schema, "0.6.24").expect("没有备份也应当能自愈");

    let text = read(&schema);
    assert_eq!(vnum_key_count(&text), 1, "恢复后只能有一份 vnum");
    serde_yaml::from_str::<serde_yaml::Value>(&text).expect("valid yaml");
    assert_ne!(
        fs::read(&schema).expect("read"),
        bytes_before,
        "损坏文件应当被修好"
    );
    // 备份是修复之后才写的，且现场副本留一份。
    assert!(data_patch::backup_path(&schema).exists(), "写成功了就该有备份");
    let copies = broken_copies(&schema);
    assert_eq!(copies.len(), 1, "用户现场要留一份：{copies:?}");
}

/// 恢复之后再来一次 ensure：幂等，不再攒新的现场副本。
#[test]
fn recovery_is_idempotent_and_keeps_one_broken_copy() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, STOCK);
    data_patch::patch_file(&schema, "0.6.24").expect("first patch");
    let backup = data_patch::backup_path(&schema);
    let backup_bytes = fs::read(&backup).expect("备份存在");
    let broken = with_duplicate_vnum(&read(&schema));
    write(&schema, &broken);

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.24").expect("自动恢复"),
        PatchOutcome::Recovered
    );
    let after_recovery = read(&schema);
    assert_eq!(broken_copies(&schema).len(), 1, "恢复后留一份现场副本");

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.24").expect("第二次"),
        PatchOutcome::Unchanged,
        "恢复后已经是目标状态"
    );
    assert_eq!(read(&schema), after_recovery, "幂等：一个字节都不变");
    assert_eq!(
        broken_copies(&schema).len(),
        1,
        "同一文件只留一份 .rimesync-broken"
    );
    assert_eq!(
        fs::read(&backup).expect("备份"),
        backup_bytes,
        "备份不被覆盖"
    );

    // 再坏一次同样的内容：现场副本仍然只有一份（不攒一堆）。
    write(&schema, &broken);
    assert_eq!(
        data_patch::patch_file(&schema, "0.6.24").expect("再次恢复"),
        PatchOutcome::Recovered
    );
    assert_eq!(
        broken_copies(&schema).len(),
        1,
        "同一文件只留一份 .rimesync-broken"
    );
}
