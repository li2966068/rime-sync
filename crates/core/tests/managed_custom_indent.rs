//! 回归：YAML 列表项与父键**同级**（缩进相同）时，必须被正确识别。
//!
//! 2026-11-10 现场缺陷：用户真实 custom.yaml 里 `engine/processors:` 与它的
//! 列表项同缩进（2），这是合法 YAML；rime-sync 的块扫描却按「缩进 <= 父键就
//! 收尾」判定，于是这个列表被读成**空**，进而误判「用户的 custom.yaml 里还缺
//! numpad」，「修复输入法」实际什么都没改。
//!
//! 覆盖：同级（缩进 2）列表被识别；子级（缩进 4）列表仍被识别；两种写法都能
//! 补上 numpad 且插在 rimesync_vselect 紧后；同级写法下新插入的行沿用同级
//! 缩进（不擅自改成缩进 4）；其余内容逐字节不变；幂等。

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::managed_custom::{repair_file, RepairOutcome};

/// 与父键同级的列表（缩进 2）—— 用户真机的写法。
const SAME_LEVEL: &str = "# patched by rime-sync 0.6.22
patch:
  engine/processors:
  - ascii_composer
  - recognizer
  - rimesync_vselect
  - key_binder
  engine/translators:
  - rimesync_vnum
  - punct_translator
";

/// 比父键多缩进 2 的列表（缩进 4）—— 仓库模板的写法。
const NESTED_LEVEL: &str = "# patched by rime-sync 0.6.22
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_vselect
    - key_binder
  engine/translators:
    - rimesync_vnum
    - punct_translator
";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("rime-sync-indent-{tag}-{}", std::process::id()));
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
    fs::write(path, body).expect("write");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read")
}

/// 取出 processors 列表里每一项（保留原缩进）。
fn processors_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim_end() == "  engine/processors:" || line.trim_end() == "    engine/processors:"
        {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") {
            out.push(line.to_string());
            continue;
        }
        // 遇到非列表项：列表结束（下一个键）。
        if !line.trim().is_empty() {
            break;
        }
    }
    out
}

/// 同级写法：列表必须被读出来，并补上 numpad（同级缩进）。
#[test]
fn same_level_list_is_recognized_and_repaired() {
    let tmp = TempDir::new("same");
    let path = tmp.path().join("luna_pinyin_simp.custom.yaml");
    write(&path, SAME_LEVEL);

    let outcome = repair_file(&path).expect("repair");
    assert!(
        matches!(outcome, RepairOutcome::Repaired { .. }),
        "同级列表必须被识别并修复，实际 {outcome:?}"
    );

    let actual = read(&path);
    let processors = processors_of(&actual);
    assert_eq!(
        processors,
        vec![
            "  - ascii_composer",
            "  - recognizer",
            "  - rimesync_vselect",
            "  - numpad",
            "  - key_binder",
        ],
        "numpad 必须插在 rimesync_vselect 紧后，且沿用同级缩进"
    );
    assert!(
        actual.contains("  engine/translators:\n  - rimesync_vnum\n"),
        "同级写法下其它列表不能被改动"
    );
}

/// 子级写法（缩进 4）：行为与修复前一致，不回归。
#[test]
fn nested_level_list_still_works() {
    let tmp = TempDir::new("nested");
    let path = tmp.path().join("luna_pinyin_simp.custom.yaml");
    write(&path, NESTED_LEVEL);

    let outcome = repair_file(&path).expect("repair");
    assert!(
        matches!(outcome, RepairOutcome::Repaired { .. }),
        "子级列表必须被识别并修复，实际 {outcome:?}"
    );

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
    );
}

/// 已经是目标状态：同级写法也必须一个字节都不动（幂等）。
#[test]
fn same_level_already_repaired_is_unchanged() {
    let tmp = TempDir::new("idem");
    let path = tmp.path().join("luna_pinyin_simp.custom.yaml");
    let already = SAME_LEVEL.replace(
        "  - rimesync_vselect\n",
        "  - rimesync_vselect\n  - numpad\n",
    );
    write(&path, &already);

    let outcome = repair_file(&path).expect("repair");
    assert_eq!(
        outcome,
        RepairOutcome::Unchanged,
        "已含 numpad 的同级写法文件必须判定为 Unchanged"
    );
    assert_eq!(read(&path), already, "Unchanged 时不得写盘");
}
