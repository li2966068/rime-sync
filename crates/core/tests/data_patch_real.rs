//! data 方案补丁的集成测试：临时目录 + 真实方案文本，不触碰系统目录。
//!
//! 覆盖任务书要求：幂等、备份只做一次、写前写后 YAML 合法、UTF-8 无 BOM、
//! 换行风格不变、权限错误可读、结构不适用时跳过。

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
        let path =
            std::env::temp_dir().join(format!("rime-sync-data-patch-{}-{seq}", std::process::id()));
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
    email: \"^[A-Za-z][-_.0-9A-Za-z]*@.*$\"
    punct: \"^/([0-9]0?|[A-Za-z]+)$\"
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

fn yaml(text: &str) -> serde_yaml::Value {
    serde_yaml::from_str(text).expect("valid yaml")
}

fn strings(value: &serde_yaml::Value, path: &[&str]) -> Vec<String> {
    let mut node = value;
    for key in path {
        node = node.get(*key).expect("yaml path exists");
    }
    node.as_sequence()
        .expect("sequence")
        .iter()
        .map(|item| item.as_str().expect("string item").to_string())
        .collect()
}

#[test]
fn patches_the_three_places_and_the_marker() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, STOCK);

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.18").expect("patch"),
        PatchOutcome::Patched
    );

    let text = read(&schema);
    assert!(
        text.starts_with("# patched by rime-sync 0.6.18\n"),
        "{text}"
    );
    let doc = yaml(&text);

    assert_eq!(
        strings(&doc, &["engine", "translators"])[0],
        data_patch::VNUM_TRANSLATOR
    );
    let processors = strings(&doc, &["engine", "processors"]);
    let recognizer = processors
        .iter()
        .position(|name| name == "recognizer")
        .expect("recognizer stays");
    assert_eq!(processors[recognizer + 1], data_patch::VSELECT_PROCESSOR);
    assert_eq!(processors[recognizer + 2], data_patch::NUMPAD_PROCESSOR);
    assert_eq!(processors[recognizer + 3], "key_binder");
    assert_eq!(
        doc["recognizer"]["patterns"]["vnum"].as_str(),
        Some(data_patch::VNUM_PATTERN)
    );
    // 其余配置原样保留
    assert_eq!(processors.len(), 10);
    assert_eq!(
        strings(&doc, &["engine", "translators"]).len(),
        5,
        "translators 只多一个 rimesync_vnum"
    );
}

#[test]
fn second_run_is_byte_identical_and_writes_no_backup_again() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, STOCK);
    let backup = data_patch::backup_path(&schema);

    data_patch::patch_file(&schema, "0.6.18").expect("first patch");
    let first = read(&schema);
    let backup_bytes = fs::read(&backup).expect("backup exists");
    assert_eq!(backup_bytes, STOCK.as_bytes(), "备份是原始内容");

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.18").expect("second patch"),
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
fn backup_survives_later_edits() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, STOCK);
    let backup = data_patch::backup_path(&schema);

    data_patch::patch_file(&schema, "0.6.18").expect("first patch");
    // 用户在补丁后的文件上又改了一笔：备份必须还是最初的原版。
    let mut edited = read(&schema);
    edited.push_str("# user edit\n");
    write(&schema, &edited);

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.18").expect("patch again"),
        PatchOutcome::Unchanged
    );
    assert_eq!(fs::read(&backup).expect("backup"), STOCK.as_bytes());
}

#[test]
fn keeps_crlf_and_missing_trailing_newline() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    let crlf = STOCK
        .replace('\n', "\r\n")
        .trim_end_matches("\r\n")
        .to_string();
    write(&schema, &crlf);

    data_patch::patch_file(&schema, "0.6.18").expect("patch");

    let text = read(&schema);
    let lines: Vec<&str> = text.split('\n').collect();
    // 除最后一行外都必须以 \r 结尾（最后一行本来就没有换行）。
    for line in &lines[..lines.len() - 1] {
        assert!(line.ends_with('\r'), "每一行都必须以 CRLF 结尾：{line:?}");
    }
    assert!(!text.ends_with('\n'), "原文没有结尾换行就也不能加");
    assert!(text.starts_with("# patched by rime-sync 0.6.18\r\n"));
    yaml(&text);
}

#[test]
fn writes_utf8_without_bom() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    // 带 BOM 的输入：输出必须是纯 UTF-8，且内容仍合法。
    write(&schema, &format!("\u{feff}{STOCK}"));

    data_patch::patch_file(&schema, "0.6.18").expect("patch");

    let bytes = fs::read(&schema).expect("read bytes");
    assert_ne!(&bytes[..3], b"\xef\xbb\xbf", "输出不能带 BOM");
    let text = String::from_utf8(bytes).expect("utf-8");
    assert!(text.starts_with("# patched by rime-sync "));
    yaml(&text);
}

#[test]
fn invalid_yaml_is_never_touched() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("broken.schema.yaml");
    let broken = "engine: [\n";
    write(&schema, broken);

    let error = data_patch::patch_file(&schema, "0.6.18").expect_err("must fail");
    assert!(
        format!("{error:#}").contains("不是合法 YAML"),
        "错误原因要可读：{error:#}"
    );
    assert_eq!(read(&schema), broken);
    assert!(!data_patch::backup_path(&schema).exists());
}

#[test]
fn schema_without_engine_is_skipped() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("plain.schema.yaml");
    let plain = "schema:\n  schema_id: plain\n";
    write(&schema, plain);

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.18").expect("skip"),
        PatchOutcome::Skipped
    );
    assert_eq!(read(&schema), plain);
    assert!(!data_patch::backup_path(&schema).exists());
}

#[test]
fn flow_style_lists_are_left_alone() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("flow.schema.yaml");
    let flow = "\
engine:
  processors: [ascii_composer, recognizer, speller]
  translators: [punct_translator]
recognizer:
  patterns: {punct: '^/([0-9]+)$'}
";
    write(&schema, flow);

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.18").expect("skip"),
        PatchOutcome::Skipped
    );
    assert_eq!(read(&schema), flow, "flow 风格不动，也不能写坏");
    assert!(!data_patch::backup_path(&schema).exists());
}

#[test]
fn existing_user_vnum_pattern_is_kept() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("custom.schema.yaml");
    let custom = STOCK.replace(
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n",
        "    punct: \"^/([0-9]0?|[A-Za-z]+)$\"\n    vnum: '^v[0-9]+$'\n",
    );
    write(&schema, &custom);

    data_patch::patch_file(&schema, "0.6.18").expect("patch");

    let text = read(&schema);
    assert!(
        text.contains("vnum: '^v[0-9]+$'"),
        "用户自己的 vnum 覆盖不被改写"
    );
    yaml(&text);
}

#[test]
fn patch_dirs_reports_what_it_did() {
    let tmp = TempDir::new();
    let data = tmp.path().join("data");
    write(&data.join("luna_pinyin.schema.yaml"), STOCK);
    write(&data.join("notes.yaml"), "not: a schema\n");
    write(&data.join("broken.schema.yaml"), "punctuator: [\n");

    let missing = tmp.path().join("missing");
    let report = data_patch::patch_dirs(&[data.clone(), missing], "0.6.18");

    assert_eq!(report.schemas_patched.len(), 1, "{report:?}");
    assert_eq!(report.errors.len(), 1, "坏 YAML 只记失败，不中断整批");
    assert_eq!(report.backup_dir, data.display().to_string());
    assert_eq!(report.data_dirs.len(), 2);
    assert!(
        report
            .schemas_skipped
            .iter()
            .any(|item| item.contains("目录不存在")),
        "{:?}",
        report.schemas_skipped
    );

    let again = data_patch::patch_dirs(&[data], "0.6.18");
    assert_eq!(again.schemas_patched.len(), 0, "重复执行不再写入");
    assert_eq!(again.schemas_unchanged.len(), 1);
    assert_eq!(again.backup_dir, "", "没有新备份就没有 backup_dir");
}

#[test]
fn permission_denied_is_a_readable_error() {
    let tmp = TempDir::new();
    let schema = tmp.path().join("luna_pinyin.schema.yaml");
    write(&schema, STOCK);
    let mut permissions = fs::metadata(&schema).expect("metadata").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&schema, permissions).expect("chmod");

    // root 下只读文件照样可写：这种环境跳过（不是被测行为）。
    let probe = fs::OpenOptions::new().write(true).open(&schema);
    if probe.is_ok() {
        return;
    }

    let error = data_patch::patch_file(&schema, "0.6.18").expect_err("must fail");
    let message = format!("{error:#}");
    assert!(message.contains("管理员"), "错误要提示提权：{message}");
    assert_eq!(read(&schema), STOCK, "失败时原文件不动");
}

#[test]
fn synthetic_schema_keeps_its_shape() {
    // engine/processors 里没有 recognizer / key_binder / speller 时也要能插进去。
    let tmp = TempDir::new();
    let schema = tmp.path().join("odd.schema.yaml");
    let odd = "\
schema:
  schema_id: odd
engine:
  processors:
    - ascii_composer
  translators:
    - punct_translator
recognizer:
  import_preset: default
";
    write(&schema, odd);

    assert_eq!(
        data_patch::patch_file(&schema, "0.6.18").expect("patch"),
        PatchOutcome::Patched
    );
    let text = read(&schema);
    let doc = yaml(&text);
    assert_eq!(
        strings(&doc, &["engine", "processors"]),
        vec![
            "ascii_composer".to_string(),
            data_patch::VSELECT_PROCESSOR.to_string(),
            data_patch::NUMPAD_PROCESSOR.to_string()
        ]
    );
    assert_eq!(
        doc["recognizer"]["patterns"]["vnum"].as_str(),
        Some(data_patch::VNUM_PATTERN),
        "没有 patterns 时要建一个"
    );
}
