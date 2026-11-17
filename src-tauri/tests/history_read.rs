//! 回归：桌面端「同步历史」卡片读的那份 history.jsonl 必须读得出来。
//!
//! 数据源是 core 写的 `<状态目录>/history.jsonl`（每次 transport/apply 追加一条）。
//! 桌面端只读它，不写。这里覆盖两种最要紧的情况：
//! 1. 还没同步过 ⇒ 文件不存在 ⇒ 界面显示空列表（不是报错）；
//! 2. 有记录 ⇒ 自己写进去的几行必须原样读回来（字段不缺、顺序不变）。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_core::history;

/// 测试临时目录的**统一根目录**（与 crates/core、crates/cli 一致）。
const TEST_TMP_ROOT: &str = "rime-sync-tests";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(TEST_TMP_ROOT).join(format!(
        "rime-sync-tauri-history-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

/// 还没同步过：history.jsonl 不存在 ⇒ 读出来是空的（界面显示空态，不报错）。
#[test]
fn missing_history_file_reads_as_empty() {
    let dir = temp_dir("missing");
    let path = dir.join("history.jsonl");
    assert!(!path.exists(), "文件本来就不该存在");

    let entries = history::read(&path, 50).expect("读历史");
    assert!(entries.is_empty(), "没有历史文件时应读出空列表");

    let _ = fs::remove_dir_all(&dir);
}

/// 自己写的 history.jsonl 必须能原样读回来。
#[test]
fn written_history_is_read_back() {
    let dir = temp_dir("read-back");
    let path = dir.join("history.jsonl");
    fs::write(
        &path,
        concat!(
            r#"{"ts":1700000000,"mode":"transport","ok":true,"pulled":3,"pushed":1,"merged":0,"conflicts":0,"duration_ms":1200}"#,
            "\n",
            r#"{"ts":1700000100,"mode":"apply","ok":false,"pulled":0,"pushed":0,"merged":0,"conflicts":2,"duration_ms":8600,"error":"远端连接失败"}"#,
            "\n",
        ),
    )
    .expect("写历史");

    let entries = history::read(&path, 50).expect("读历史");
    assert_eq!(entries.len(), 2, "两行都该读出来");

    let first = &entries[0];
    assert_eq!(first.ts, 1_700_000_000);
    assert_eq!(first.mode, "transport");
    assert!(first.ok);
    assert_eq!(first.pulled, 3);
    assert_eq!(first.pushed, 1);
    assert_eq!(first.merged, 0);
    assert_eq!(first.conflicts, 0);
    assert_eq!(first.duration_ms, 1200);
    assert!(first.error.is_none());

    let second = &entries[1];
    assert_eq!(second.mode, "apply");
    assert!(!second.ok);
    assert_eq!(second.conflicts, 2);
    assert_eq!(second.duration_ms, 8600);
    assert_eq!(second.error.as_deref(), Some("远端连接失败"));

    // limit 只取最近几条，且保持时间正序。
    let recent = history::read(&path, 1).expect("读最近一条");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].mode, "apply");

    let _ = fs::remove_dir_all(&dir);
}

/// 坏行不能把整份历史拖垮：跳过它，其余照常读。
#[test]
fn corrupt_line_is_skipped() {
    let dir = temp_dir("corrupt");
    let path = dir.join("history.jsonl");
    fs::write(
        &path,
        concat!(
            "not-json\n",
            r#"{"ts":1700000200,"mode":"transport","ok":true,"pulled":0,"pushed":0,"merged":0,"conflicts":0,"duration_ms":10}"#,
            "\n",
        ),
    )
    .expect("写历史");

    let entries = history::read(&path, 50).expect("读历史");
    assert_eq!(entries.len(), 1, "坏行要跳过，好的那行照样读出来");
    assert_eq!(entries[0].ts, 1_700_000_200);

    let _ = fs::remove_dir_all(&dir);
}
