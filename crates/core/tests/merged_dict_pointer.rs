//! 合并词典指针：<schema>.custom.yaml 里的 translator/dictionary。
//!
//! 覆盖：缺键 → 补写（且落在 patch 块内）；用户手写的指针（引号 / 非引号 /
//! 嵌套三种写法）一个字节都不动；方案用的词库不在合并词典的 import_tables 里
//! → 不写；没有 custom.yaml → 不新建；重复调用幂等；BOM 与混合行尾保持原样。
//!
//! 不依赖平台、不碰真实 Rime 目录：全部在临时目录里搭最小场景。

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::merged_dict::{
    detect, ensure_pointers, ensure_pointers_with, schema_dictionary, PointerChange,
};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-merged-pointer-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
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

/// 只含 patch 块、缺指针的 custom.yaml。
fn custom_yaml_without_pointer() -> String {
    "# 用户自己写的\npatch:\n  \"menu/page_size\": 8\n  engine/processors:\n    - ascii_composer\n    - recognizer\n".to_string()
}

fn change_for<'a>(changes: &'a [PointerChange], schema: &str) -> Option<&'a PointerChange> {
    changes.iter().find(|change| match change {
        PointerChange::Written { schema: name, .. }
        | PointerChange::Present { schema: name }
        | PointerChange::Skipped { schema: name, .. } => name == schema,
    })
}

/// 场景：共享 data 目录里有 luna_pinyin_simp（继承 luna_pinyin 的词库），
/// 用户目录里有合并词典包装器与待补指针的 custom.yaml。
fn scene(tag: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let rime_dir = root.join("rime");
    let data_dir = root.join("data");
    fs::create_dir_all(&rime_dir).expect("mkdir rime");
    fs::create_dir_all(&data_dir).expect("mkdir data");

    write(
        &data_dir.join("luna_pinyin_simp.schema.yaml"),
        "# 简化拼音\n__include: luna_pinyin.schema:/\n__patch:\n  - luna_pinyin_simp.custom:/patch?\n",
    );
    write(
        &data_dir.join("luna_pinyin.schema.yaml"),
        "schema:\n  schema_id: luna_pinyin\ntranslator:\n  dictionary: luna_pinyin\n",
    );
    write(
        &rime_dir.join("merged.dict.yaml"),
        &merged_dict_yaml("merged", &["luna_pinyin"]),
    );
    (rime_dir, data_dir)
}

#[test]
fn detect_finds_the_wrapper_with_import_tables() {
    let (rime_dir, _data) = scene("detect");
    // 另一个没有 import_tables 的词典不能被选中。
    write(
        &rime_dir.join("plain.dict.yaml"),
        "---\nname: plain\nversion: \"1\"\n...\n",
    );
    let merged = detect(&rime_dir).expect("merged wrapper");
    assert_eq!(merged.name, "merged");
    assert_eq!(merged.import_tables, vec!["luna_pinyin".to_string()]);
}

/// librime 的 LoadDictHeader 要求 name 与 version 都非空，
/// 缺 version 的词典头 librime 根本加载不了 —— 不能把指针指向它。
#[test]
fn detect_ignores_a_dict_header_without_version() {
    let dir = temp_dir("detect-no-version");
    write(
        &dir.join("broken.dict.yaml"),
        "name: broken\nimport_tables:\n  - luna_pinyin\n...\n",
    );
    assert!(
        detect(&dir).is_none(),
        "a dict header without version must not be treated as a merged dictionary"
    );
}

/// 缺 version 的词典头也不能让指针被写出去。
#[test]
fn a_dict_header_without_version_gets_no_pointer() {
    // 注意：不能用 scene()，它会自己建一个合法的 merged.dict.yaml。
    let rime_dir = temp_dir("no-version-pointer");
    let data_dir = rime_dir.join("data");
    fs::create_dir_all(&data_dir).expect("mkdir");
    write(
        &data_dir.join("luna_pinyin_simp.schema.yaml"),
        "schema:\n  schema_id: luna_pinyin_simp\ntranslator:\n  dictionary: luna_pinyin\n",
    );
    write(
        &rime_dir.join("broken.dict.yaml"),
        "name: broken\nimport_tables:\n  - luna_pinyin\n...\n",
    );
    let custom = rime_dir.join("luna_pinyin_simp.custom.yaml");
    write(&custom, "patch:\n  menu/page_size: 8\n");

    let changes = ensure_pointers(&rime_dir, &[data_dir]);
    assert!(
        changes.is_empty(),
        "no pointer must be written when the dict header is unusable: {changes:?}"
    );
    let text = fs::read_to_string(&custom).expect("custom");
    assert!(
        !text.contains("translator/dictionary"),
        "the custom file must stay untouched: {text}"
    );
}

#[test]
fn detect_returns_none_without_import_tables() {
    let dir = temp_dir("detect-none");
    write(
        &dir.join("plain.dict.yaml"),
        "---\nname: plain\nversion: \"1\"\n...\n",
    );
    assert!(detect(&dir).is_none());
}

#[test]
fn schema_dictionary_follows_include_chain() {
    let (_rime_dir, data_dir) = scene("schema-dict");
    let dirs = vec![data_dir];
    assert_eq!(
        schema_dictionary("luna_pinyin_simp", &dirs),
        Some("luna_pinyin".to_string())
    );
    assert_eq!(schema_dictionary("not_there", &dirs), None);
}

#[test]
fn missing_pointer_gets_written_into_the_patch_block() {
    let (rime_dir, data_dir) = scene("write");
    let custom = rime_dir.join("luna_pinyin_simp.custom.yaml");
    write(&custom, &custom_yaml_without_pointer());
    let dirs = vec![data_dir];

    let changes = ensure_pointers(&rime_dir, &dirs);
    let change = change_for(&changes, "luna_pinyin_simp").expect("change");
    assert!(
        matches!(change, PointerChange::Written { .. }),
        "{change:?}"
    );

    let text = fs::read_to_string(&custom).expect("read");
    // 指针落在 patch 块内（缩进 2 空格），值是词典头里的 name。
    assert!(
        text.contains("  \"translator/dictionary\": merged\n"),
        "pointer not written:\n{text}"
    );
    // 原有内容一字不改。
    assert!(text.contains("  \"menu/page_size\": 8\n"), "{text}");
    assert!(text.contains("    - ascii_composer\n"), "{text}");
    assert!(text.contains("# 用户自己写的\n"), "{text}");
}

#[test]
fn existing_user_pointer_is_never_overwritten() {
    // 三种写法都要认：引号键、非引号键、嵌套结构。
    let cases = [
        (
            "quoted",
            "# 手写\npatch:\n  \"translator/dictionary\": my_own_dict\n",
        ),
        (
            "plain",
            "# 手写\npatch:\n  translator/dictionary: my_own_dict\n",
        ),
        (
            "nested",
            "# 手写\npatch:\n  translator:\n    dictionary: my_own_dict\n",
        ),
    ];
    for (tag, content) in cases {
        let (rime_dir, data_dir) = scene(&format!("keep-{tag}"));
        let custom = rime_dir.join("luna_pinyin_simp.custom.yaml");
        write(&custom, content);
        let before = fs::read(&custom).expect("read before");
        let dirs = vec![data_dir];

        let changes = ensure_pointers(&rime_dir, &dirs);
        let change = change_for(&changes, "luna_pinyin_simp").expect("change");
        assert!(
            matches!(change, PointerChange::Present { .. }),
            "{tag}: {change:?}"
        );
        assert_eq!(before, fs::read(&custom).expect("read after"), "{tag}");
    }
}

#[test]
fn dictionary_outside_import_tables_is_not_written() {
    let (rime_dir, data_dir) = scene("outside");
    // 方案用的是 wubi86，不在合并词典的 import_tables 里 → 不能指过来。
    write(
        &data_dir.join("wubi86.schema.yaml"),
        "schema:\n  schema_id: wubi86\ntranslator:\n  dictionary: wubi86\n",
    );
    let custom = rime_dir.join("wubi86.custom.yaml");
    write(&custom, &custom_yaml_without_pointer());
    let before = fs::read(&custom).expect("read");
    let dirs = vec![data_dir];

    let changes = ensure_pointers(&rime_dir, &dirs);
    let change = change_for(&changes, "wubi86").expect("change");
    assert!(
        matches!(change, PointerChange::Skipped { reason, .. } if reason == "dictionary-not-merged"),
        "{change:?}"
    );
    assert_eq!(before, fs::read(&custom).expect("read after"));
}

#[test]
fn missing_custom_file_is_not_created() {
    // 约定：只处理已经存在的 custom.yaml —— 不存在的既不新建，也不在结果里出现。
    let (rime_dir, data_dir) = scene("missing");
    let dirs = vec![data_dir];
    let changes = ensure_pointers(&rime_dir, &dirs);
    assert!(changes.is_empty(), "{changes:?}");
    assert!(!rime_dir.join("luna_pinyin_simp.custom.yaml").exists());
}

#[test]
fn file_without_patch_block_is_left_alone() {
    let (rime_dir, data_dir) = scene("no-patch");
    let custom = rime_dir.join("luna_pinyin_simp.custom.yaml");
    write(&custom, "customization:\n  import_preset: default\n");
    let before = fs::read(&custom).expect("read");
    let dirs = vec![data_dir];

    let changes = ensure_pointers(&rime_dir, &dirs);
    let change = change_for(&changes, "luna_pinyin_simp").expect("change");
    assert!(
        matches!(change, PointerChange::Skipped { reason, .. } if reason == "no-patch-block"),
        "{change:?}"
    );
    assert_eq!(before, fs::read(&custom).expect("read after"));
}

#[test]
fn second_run_is_idempotent() {
    let (rime_dir, data_dir) = scene("idempotent");
    let custom = rime_dir.join("luna_pinyin_simp.custom.yaml");
    write(&custom, &custom_yaml_without_pointer());
    let dirs = vec![data_dir];

    let first = ensure_pointers(&rime_dir, &dirs);
    assert!(change_for(&first, "luna_pinyin_simp")
        .expect("change")
        .wrote());
    let after_first = fs::read(&custom).expect("read");

    let second = ensure_pointers(&rime_dir, &dirs);
    let change = change_for(&second, "luna_pinyin_simp").expect("change");
    assert!(
        matches!(change, PointerChange::Present { .. }),
        "{change:?}"
    );
    assert_eq!(after_first, fs::read(&custom).expect("read"));
}

#[test]
fn bom_and_mixed_line_endings_survive() {
    let (rime_dir, data_dir) = scene("bom");
    let custom = rime_dir.join("luna_pinyin_simp.custom.yaml");
    // BOM + 正文 LF + 一行 CRLF + 末尾无换行。
    let content = "\u{feff}# 手写\r\npatch:\n  \"menu/page_size\": 8";
    fs::write(&custom, content.as_bytes()).expect("write");
    let dirs = vec![data_dir];

    let changes = ensure_pointers(&rime_dir, &dirs);
    assert!(change_for(&changes, "luna_pinyin_simp")
        .expect("change")
        .wrote());

    let bytes = fs::read(&custom).expect("read");
    assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]), "BOM lost");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(
        text.contains("  \"translator/dictionary\": merged"),
        "{text}"
    );
    assert!(text.contains("\r\n"), "CRLF lost");
    assert!(text.contains("\n"), "LF lost");
    // 末尾仍没有换行（RawLines 的属性保持）。
    assert!(!text.ends_with('\n'), "trailing newline changed");
}

// -------------------------------------------------------------------------
// 方案没有 custom.yaml 时：用模板建一份完整的（增强功能 + 指针）。
// -------------------------------------------------------------------------

/// 随包模板：patch 块里带增强组件（小键盘 / 标点直出 / v 转数字）。
fn template_with_extra() -> String {
    "# 随包模板\npatch:\n  engine/processors:\n    - ascii_composer\n    - recognizer\n    - rimesync_punct\n    - rimesync_vselect\n    - numpad\n    - speller\n    - punctuator\n    - selector\n  engine/translators:\n    - rimesync_vnum\n    - script_translator\n".to_string()
}

/// 这台机器的引擎支持增强组件：已部署的 build 产物里有 rimesync 标记。
fn mark_engine_supports_extra(rime_dir: &Path, schema: &str) {
    write(
        &rime_dir.join("build").join(format!("{schema}.schema.yaml")),
        "engine:\n  processors:\n    - rimesync_vnum\n    - rimesync_punct\n",
    );
}

/// 方案没有 custom.yaml + 有模板 + 引擎支持 → 建一份完整的出来。
#[test]
fn a_missing_custom_file_is_seeded_from_the_template() {
    let (rime_dir, data_dir) = scene("seed-from-template");
    let templates = rime_dir.parent().expect("root").join("templates");
    write(
        &templates.join("luna_pinyin_simp.custom.yaml"),
        &template_with_extra(),
    );
    mark_engine_supports_extra(&rime_dir, "luna_pinyin_simp");

    let changes = ensure_pointers_with(
        &rime_dir,
        std::slice::from_ref(&data_dir),
        std::slice::from_ref(&templates),
    );

    let path = rime_dir.join("luna_pinyin_simp.custom.yaml");
    assert!(path.is_file(), "the missing custom.yaml must be seeded");
    let text = fs::read_to_string(&path).expect("read");
    assert!(
        text.contains("\"translator/dictionary\": merged"),
        "the pointer must be written into the seeded file, got:\n{text}"
    );
    assert!(
        text.contains("rimesync_vnum"),
        "the seeded file must keep the enhancement components, got:\n{text}"
    );
    match change_for(&changes, "luna_pinyin_simp") {
        Some(PointerChange::Written { .. }) => {}
        other => panic!("expected Written, got {other:?}"),
    }
}

/// 没有模板 → 不新建文件（不能凭空造一个没有增强功能的空壳）。
#[test]
fn a_missing_custom_file_is_not_created_without_a_template() {
    let (rime_dir, data_dir) = scene("seed-no-template");
    let empty_templates = rime_dir.parent().expect("root").join("empty-templates");
    fs::create_dir_all(&empty_templates).expect("mkdir");
    mark_engine_supports_extra(&rime_dir, "luna_pinyin_simp");

    let changes = ensure_pointers_with(
        &rime_dir,
        std::slice::from_ref(&data_dir),
        std::slice::from_ref(&empty_templates),
    );

    assert!(
        !rime_dir.join("luna_pinyin_simp.custom.yaml").is_file(),
        "no template means no file is created"
    );
    // 模板目录空 = 根本不知道该方案存在，因此没有任何候选、什么都不做。
    assert!(
        changes.iter().all(|change| !change.wrote()),
        "nothing may be written without a template, got {changes:?}"
    );
}

/// 引擎不认识增强组件（发行版 librime）→ 不建模板。
/// 建了会让方案报 error creating processor，整个输入法不可用。
#[test]
fn a_missing_custom_file_is_not_created_on_an_engine_without_the_extra_plugins() {
    let (rime_dir, data_dir) = scene("seed-unsupported-engine");
    let templates = rime_dir.parent().expect("root").join("templates");
    write(
        &templates.join("luna_pinyin_simp.custom.yaml"),
        &template_with_extra(),
    );
    // 故意不建带 rimesync 标记的 build 产物，用户目录里也没有受管文件。

    let changes = ensure_pointers_with(
        &rime_dir,
        std::slice::from_ref(&data_dir),
        std::slice::from_ref(&templates),
    );

    assert!(
        !rime_dir.join("luna_pinyin_simp.custom.yaml").is_file(),
        "an engine without the plugins must not get a template referencing them"
    );
    match change_for(&changes, "luna_pinyin_simp") {
        Some(PointerChange::Skipped { reason, .. }) => {
            assert_eq!(reason, "engine-unsupported")
        }
        other => panic!("expected Skipped(engine-unsupported), got {other:?}"),
    }
}

/// 补齐之后重复跑，文件一个字节都不再动。
#[test]
fn seeding_the_missing_custom_file_is_idempotent() {
    let (rime_dir, data_dir) = scene("seed-idempotent");
    let templates = rime_dir.parent().expect("root").join("templates");
    write(
        &templates.join("luna_pinyin_simp.custom.yaml"),
        &template_with_extra(),
    );
    mark_engine_supports_extra(&rime_dir, "luna_pinyin_simp");

    ensure_pointers_with(
        &rime_dir,
        std::slice::from_ref(&data_dir),
        std::slice::from_ref(&templates),
    );
    let path = rime_dir.join("luna_pinyin_simp.custom.yaml");
    let first = fs::read_to_string(&path).expect("read");

    let changes = ensure_pointers_with(
        &rime_dir,
        std::slice::from_ref(&data_dir),
        std::slice::from_ref(&templates),
    );
    let second = fs::read_to_string(&path).expect("read");

    assert_eq!(first, second, "the second run must not change the file");
    match change_for(&changes, "luna_pinyin_simp") {
        Some(PointerChange::Present { .. }) => {}
        other => panic!("expected Present, got {other:?}"),
    }
}

/// 模板里的 default.custom.yaml / weasel.custom.yaml 不是方案，不该被建出来。
#[test]
fn non_schema_templates_are_not_seeded() {
    let (rime_dir, data_dir) = scene("seed-non-schema");
    let templates = rime_dir.parent().expect("root").join("templates");
    write(
        &templates.join("default.custom.yaml"),
        "patch:\n  schema_list: []\n",
    );
    write(&templates.join("weasel.custom.yaml"), "patch: {}\n");
    mark_engine_supports_extra(&rime_dir, "luna_pinyin_simp");

    let changes = ensure_pointers_with(
        &rime_dir,
        std::slice::from_ref(&data_dir),
        std::slice::from_ref(&templates),
    );

    assert!(
        !rime_dir.join("default.custom.yaml").is_file(),
        "default.custom.yaml is a config file, not a schema"
    );
    assert!(
        !rime_dir.join("weasel.custom.yaml").is_file(),
        "weasel.custom.yaml is a config file, not a schema"
    );
    assert!(changes.iter().all(|change| !change.wrote()));
}
