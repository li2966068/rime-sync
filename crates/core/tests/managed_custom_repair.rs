//! rime-sync 管理的用户级 <schema>.custom.yaml 的增量修复（缺陷三：小键盘 numpad）。
//!
//! 覆盖：旧模板缺 numpad → 补齐且其它内容逐字节不变；已经就位 → 完全不写盘；
//! 非 rime-sync 管理的文件 → 不碰；用户主动关闭 rimesync_* → 不加回；幂等；
//! 备份只建一次；CRLF / BOM / 结尾换行保持；缺失文件、非法 UTF-8 → 返回错误不 panic。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rime_sync_core::data_patch::backup_path;
use rime_sync_core::managed_custom::{
    is_managed, is_managed_file, repair_file, repair_user_dir, scan_user_dir, RepairOutcome,
    MANAGED_MARKER, MANAGED_MARKER_PATCHED,
};

/// v0.6.17 旧模板：processors 里有 rimesync_vselect，但没有 numpad。
const OLD_TEMPLATE: &str = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
# 关闭：删掉本文件后重新部署；或删掉下面 patch 里的 rimesync_vnum / rimesync_punct。
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_punct
    - rimesync_vselect
    - key_binder
    - speller
  engine/translators:
    - rimesync_vnum
    - table_translator
";

/// 现行模板：numpad 紧跟 rimesync_vselect。
const NEW_TEMPLATE: &str = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_punct
    - rimesync_vselect
    - numpad
    - key_binder
    - speller
  engine/translators:
    - rimesync_vnum
    - table_translator
";

/// 用户自己写的文件：没有 rime-sync 标记，一个字节都不许动。
const USER_FILE: &str = "patch:
  engine/processors:
    - recognizer
    - key_binder
";

/// 用户主动关闭 rimesync 定制：列表里 rimesync_* 全删了（注释里还留着说明）。
const DISABLED_FILE: &str = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# 关闭：删掉本文件后重新部署；或删掉下面 patch 里的 rimesync_vnum / rimesync_punct。
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - key_binder
    - speller
";

/// 管理型文件里一个 rimesync_* 名字都没有：定制已被完全关闭。
const NO_RIMESYNC_FILE: &str = "\
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
patch:
  engine/processors:
    - ascii_composer
    - recognizer
";

/// 测试用临时目录（core 不带 tempfile 依赖，手写一份就够）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rime-sync-managed-custom-{}-{tag}-{:?}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
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

fn write(path: &Path, body: &str) {
    fs::write(path, body.as_bytes()).expect("write fixture");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

fn custom_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.custom.yaml"))
}

/// 取 patch 下 processors 列表里的条目（只取这一份列表，不串到 translators）。
fn processors_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !inside {
            if trimmed == "processors:" || trimmed.ends_with("/processors:") {
                inside = true;
            }
            continue;
        }
        if trimmed.starts_with("- ") {
            out.push(line.to_string());
        } else if !trimmed.is_empty() {
            inside = false;
        }
    }
    out
}

/// 旧模板：补一行 numpad，其余内容逐字节不变。
#[test]
fn repairs_old_template_and_inserts_numpad_after_vselect() {
    let tmp = TempDir::new("old-template");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, OLD_TEMPLATE);

    let outcome = repair_file(&path).expect("repair");
    match &outcome {
        RepairOutcome::Repaired { backup } => {
            assert_eq!(
                backup.as_deref(),
                Some(backup_path(&path).as_path()),
                "第一次修复必须留一份备份"
            );
        }
        other => panic!("应当修复成功，实际 {other:?}"),
    }

    let expected = OLD_TEMPLATE.replace(
        "    - rimesync_vselect\n",
        "    - rimesync_vselect\n    - numpad\n",
    );
    let actual = read(&path);
    assert_eq!(actual, expected, "除新增那一行外，其余内容必须逐字节不变");

    // numpad 紧跟 rimesync_vselect。
    let processors = processors_of(&actual);
    assert_eq!(
        processors,
        vec![
            "    - ascii_composer",
            "    - recognizer",
            "    - rimesync_punct",
            "    - rimesync_vselect",
            "    - numpad",
            "    - key_binder",
            "    - speller",
        ]
    );
    let vselect = actual
        .lines()
        .position(|line| line.trim() == "- rimesync_vselect")
        .expect("vselect");
    assert_eq!(
        actual.lines().nth(vselect + 1),
        Some("    - numpad"),
        "numpad 必须紧跟 rimesync_vselect"
    );
}

/// 已经就位：一个字节都不写（内容与 mtime 都不变）。
#[test]
fn already_repaired_file_is_unchanged() {
    let tmp = TempDir::new("already");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, NEW_TEMPLATE);
    let before = fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");

    let outcome = repair_file(&path).expect("repair");
    assert_eq!(outcome, RepairOutcome::Unchanged);
    assert_eq!(read(&path), NEW_TEMPLATE);
    let after = fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    assert_eq!(before, after, "真的不写盘：mtime 不能变");
    // 也没有备份。
    assert!(!backup_path(&path).exists());
}

/// 不是 rime-sync 管理的文件：跳过，字节不变。
#[test]
fn user_owned_file_is_skipped_untouched() {
    let tmp = TempDir::new("user-file");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, USER_FILE);

    let outcome = repair_file(&path).expect("repair");
    assert_eq!(outcome, RepairOutcome::Skipped("not-managed".to_string()));
    assert_eq!(read(&path), USER_FILE);
    assert!(!backup_path(&path).exists());
}

/// 用户主动关闭 rimesync 定制（删掉 rimesync_* 条目）：不许加回 numpad。
#[test]
fn disabled_rimesync_customization_is_skipped() {
    let tmp = TempDir::new("disabled");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, DISABLED_FILE);

    let outcome = repair_file(&path).expect("repair");
    assert!(
        matches!(outcome, RepairOutcome::Skipped(_)),
        "用户关闭了定制就不该改，实际 {outcome:?}"
    );
    assert_eq!(read(&path), DISABLED_FILE, "一个字节都不许动");
    assert!(!backup_path(&path).exists(), "跳过的文件不留备份");
}

/// 管理型文件里一个 rimesync_* 名字都没有：定制已关闭 → Skipped("rimesync-disabled")。
#[test]
fn file_without_any_rimesync_name_is_skipped() {
    let tmp = TempDir::new("no-rimesync");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, NO_RIMESYNC_FILE);

    let outcome = repair_file(&path).expect("repair");
    assert_eq!(
        outcome,
        RepairOutcome::Skipped("rimesync-disabled".to_string())
    );
    assert_eq!(read(&path), NO_RIMESYNC_FILE);
}

/// 幂等：修一次之后再修一次，第二次是 Unchanged，且备份不被覆盖。
#[test]
fn repair_is_idempotent_and_backup_is_written_once() {
    let tmp = TempDir::new("idempotent");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, OLD_TEMPLATE);
    let backup = backup_path(&path);

    let first = repair_file(&path).expect("first repair");
    assert!(matches!(first, RepairOutcome::Repaired { .. }));
    assert_eq!(
        read(&backup),
        OLD_TEMPLATE,
        "备份内容必须是修复前的原始字节"
    );

    let after_first = read(&path);
    let second = repair_file(&path).expect("second repair");
    assert_eq!(second, RepairOutcome::Unchanged);
    assert_eq!(read(&path), after_first);
    assert_eq!(read(&backup), OLD_TEMPLATE, "备份不能被覆盖");
}

/// CRLF 文件：修完还是 CRLF，不能出现混合换行。
#[test]
fn crlf_file_stays_crlf() {
    let tmp = TempDir::new("crlf");
    let path = custom_path(tmp.path(), "wubi86");
    let original = OLD_TEMPLATE.replace('\n', "\r\n");
    fs::write(&path, original.as_bytes()).expect("write");

    let outcome = repair_file(&path).expect("repair");
    assert!(matches!(outcome, RepairOutcome::Repaired { .. }));

    let bytes = fs::read(&path).expect("read");
    let text = String::from_utf8(bytes).expect("utf-8");
    assert!(text.contains("\r\n    - numpad\r\n"), "{text:?}");
    // 混合换行检查：每一个 \n 前面都必须有 \r。
    let body = text.strip_suffix("\r\n").unwrap_or(&text);
    assert!(
        !body.replace("\r\n", "").contains('\n'),
        "不允许出现裸 \n（混合换行）：{text:?}"
    );
    // 备份同样是原始字节（含 CRLF）。
    let backup = fs::read(backup_path(&path)).expect("backup");
    assert_eq!(backup, original.as_bytes());
}

/// BOM 保留。
#[test]
fn bom_is_preserved() {
    let tmp = TempDir::new("bom");
    let path = custom_path(tmp.path(), "wubi86");
    let original = format!("\u{feff}{OLD_TEMPLATE}");
    fs::write(&path, original.as_bytes()).expect("write");

    let outcome = repair_file(&path).expect("repair");
    assert!(matches!(outcome, RepairOutcome::Repaired { .. }));
    let bytes = fs::read(&path).expect("read");
    assert_eq!(bytes[0], 0xef, "BOM 必须保留");
    let text = String::from_utf8(bytes).expect("utf-8");
    assert!(text.contains("    - numpad\n"), "{text:?}");
}

/// numpad 位置不对（跑到列表末尾去了）：挪回 rimesync_vselect 后面，且只留一份。
#[test]
fn misplaced_numpad_is_moved_next_to_vselect() {
    let tmp = TempDir::new("misplaced");
    let path = custom_path(tmp.path(), "wubi86");
    let original = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_vselect
    - key_binder
    - numpad
  engine/translators:
    - rimesync_vnum
";
    write(&path, original);

    let outcome = repair_file(&path).expect("repair");
    assert!(matches!(outcome, RepairOutcome::Repaired { .. }));
    let actual = read(&path);
    let processors = processors_of(&actual);
    assert_eq!(
        processors,
        vec![
            "    - ascii_composer",
            "    - recognizer",
            "    - rimesync_vselect",
            "    - numpad",
            "    - key_binder",
        ],
        "numpad 必须挪到 rimesync_vselect 后面，并且只留一份"
    );
}

/// vselect 不在列表里：保守跳过，不替用户加处理器（Skipped("no-vselect")）。
#[test]
fn missing_vselect_is_skipped() {
    let tmp = TempDir::new("no-vselect");
    let path = custom_path(tmp.path(), "wubi86");
    let original = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_punct
    - key_binder
  engine/translators:
    - rimesync_vnum
";
    write(&path, original);

    let outcome = repair_file(&path).expect("repair");
    assert_eq!(outcome, RepairOutcome::Skipped("no-vselect".to_string()));
    assert_eq!(read(&path), original, "一个字节都不许动");
    assert!(!backup_path(&path).exists());
}

/// 嵌套写法（patch -> engine -> processors）也认。
#[test]
fn nested_processors_list_is_supported() {
    let tmp = TempDir::new("nested");
    let path = custom_path(tmp.path(), "wubi86");
    let original = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
patch:
  engine:
    processors:
      - ascii_composer
      - recognizer
      - rimesync_vselect
      - key_binder
  translators:
    - rimesync_vnum
";
    write(&path, original);

    let outcome = repair_file(&path).expect("repair");
    assert!(matches!(outcome, RepairOutcome::Repaired { .. }));
    let actual = read(&path);
    let processors = processors_of(&actual);
    assert_eq!(
        processors,
        vec![
            "      - ascii_composer",
            "      - recognizer",
            "      - rimesync_vselect",
            "      - numpad",
            "      - key_binder",
        ]
    );
}

/// 找不到 processors 列表：跳过，不新建列表。
#[test]
fn file_without_processors_list_is_skipped() {
    let tmp = TempDir::new("no-list");
    let path = custom_path(tmp.path(), "wubi86");
    let original = "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
patch:
  engine/translators:
    - rimesync_vnum
";
    write(&path, original);

    let outcome = repair_file(&path).expect("repair");
    assert_eq!(
        outcome,
        RepairOutcome::Skipped("no-processors-list".to_string())
    );
    assert_eq!(read(&path), original);
}

/// 缺失文件 / 非法 UTF-8：返回错误，不 panic。
#[test]
fn broken_inputs_return_errors_without_panic() {
    let tmp = TempDir::new("broken");
    let missing = custom_path(tmp.path(), "not_there");
    assert!(repair_file(&missing).is_err(), "缺失文件必须报错");

    let bad = custom_path(tmp.path(), "bad");
    fs::write(&bad, [0xff, 0xfe, 0x00, 0x01]).expect("write");
    assert!(repair_file(&bad).is_err(), "非法 UTF-8 必须报错");
    // 原文件没被动过。
    assert_eq!(fs::read(&bad).expect("read"), vec![0xff, 0xfe, 0x00, 0x01]);
}

/// 标记识别。
#[test]
fn managed_markers_are_detected() {
    assert!(is_managed(OLD_TEMPLATE));
    assert!(is_managed(NEW_TEMPLATE));
    assert!(!is_managed(USER_FILE));
    assert!(is_managed(DISABLED_FILE));
    // data_patch 写的标记也算。
    assert!(is_managed("# patched by rime-sync 0.6.25\npatch:\n"));
    assert!(MANAGED_MARKER.starts_with("# 由 rime-sync"));
    assert_eq!(MANAGED_MARKER_PATCHED, "# patched by rime-sync");

    let tmp = TempDir::new("marker");
    let path = custom_path(tmp.path(), "wubi86");
    write(&path, OLD_TEMPLATE);
    assert!(is_managed_file(&path));
    let other = custom_path(tmp.path(), "user");
    write(&other, USER_FILE);
    assert!(!is_managed_file(&other));
    assert!(!is_managed_file(&custom_path(tmp.path(), "missing")));
}

/// 只读探测 + 批量修复。
#[test]
fn scan_and_batch_repair_report_per_schema() {
    let tmp = TempDir::new("scan");
    let dir = tmp.path();
    write(&custom_path(dir, "wubi86"), OLD_TEMPLATE);
    write(&custom_path(dir, "luna_pinyin_simp"), USER_FILE);
    let ids = vec!["wubi86".to_string(), "luna_pinyin_simp".to_string()];

    let scanned = scan_user_dir(dir, &ids);
    assert_eq!(scanned.len(), 2);
    assert!(scanned[0].managed);
    assert!(scanned[0].active);
    assert!(scanned[0].needs_num_pad, "旧模板缺 numpad");
    assert!(!scanned[1].managed);
    assert!(!scanned[1].needs_num_pad);

    let results = repair_user_dir(dir, &ids).expect("batch repair");
    assert_eq!(results.len(), 2);
    assert!(matches!(results[0].1, RepairOutcome::Repaired { .. }));
    assert_eq!(
        results[1].1,
        RepairOutcome::Skipped("not-managed".to_string())
    );
    assert!(read(&custom_path(dir, "wubi86")).contains("- numpad"));

    // 再跑一遍：第一个已经是目标状态。
    let again = repair_user_dir(dir, &ids).expect("batch repair again");
    assert_eq!(again[0].1, RepairOutcome::Unchanged);
}

/// 批量修复时缺失的文件不报错，记 Skipped("missing")。
#[test]
fn batch_repair_skips_missing_files() {
    let tmp = TempDir::new("batch-missing");
    let ids = vec!["nope".to_string()];
    let results = repair_user_dir(tmp.path(), &ids).expect("batch repair");
    assert_eq!(results[0].1, RepairOutcome::Skipped("missing".to_string()));
}
