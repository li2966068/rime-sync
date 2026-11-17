//! 回归：共用状态目录后，基线必须**按远端隔离**。
//!
//! 背景：CLI 与桌面端现在共用同一个状态目录（`~/.config/rime-sync`），
//! 两边的基线（index.json）只有一份。但两边的 config.json 仍是两份 ——
//! 用户完全可能把两端配成**不同的 WebDAV 服务器**。
//!
//! 缺陷（改前）：共用基线时，端 A（服务器 1）的基线会被端 B（服务器 2）读走。
//! 端 B 看到「本地文件与基线不符」⇒ 判定成冲突；更糟的是端 B 会把端 A 的
//! 哈希当成远端当前版本，写进自己那台服务器的合并结果。
//!
//! 现在：index.json 记下这份基线所属的远端指纹（服务器地址 + 远端目录）。
//! 指纹对不上 ⇒ 这份基线当作「不存在」⇒ 该设备按**首次接入**处理
//! （首同步保护接管，绝不静默推送）。
//!
//! 老文件没有这个字段（空串）⇒ 不做强校验，直接沿用，避免升级作废老基线。

mod support;

use std::fs;

use rime_sync_core::config::FirstSyncPolicy;
use rime_sync_core::index::{sha256_hex, FileIndex, FileMeta, INDEX_VERSION};

use support::{engine_for, temp_dir, MockDav};

/// 基线属于**别的**远端 ⇒ 引擎把它当没基线（首次接入），不拿它去比。
#[test]
fn an_index_from_another_remote_is_treated_as_no_baseline() {
    let rime = temp_dir("scope-rime");
    let state = temp_dir("scope-state");
    let mock = MockDav::start(vec![]);

    // 造一份「属于别的服务器」的基线：remote 字段写的是另一台服务器的指纹。
    let mut index = FileIndex {
        version: INDEX_VERSION,
        ..FileIndex::default()
    };
    index.remote = "0000000000000000deadbeefdeadbeef".to_string();
    index.files.insert(
        "shared/dict.txt".to_string(),
        FileMeta {
            sha256: sha256_hex(b"some other server's content"),
            size: 24,
            mtime: "0".to_string(),
            etag: None,
        },
    );
    fs::write(
        state.join("index.json"),
        serde_json::to_string_pretty(&index).expect("index json"),
    )
    .expect("write index");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);

    // 这份基线不该被认领：跑一轮搬运后，基线必须被**换成本远端的**，
    // 别的远端留下的文件条目不许还在里面（否则下一轮就会拿它去比远端，
    // 凭空判成冲突）。
    let report = engine.transport();
    assert!(report.ok, "搬运应当成功：{report:?}");

    let index_text = fs::read_to_string(state.join("index.json")).expect("read index");
    let index: FileIndex = serde_json::from_str(&index_text).expect("parse index");
    assert_ne!(
        index.remote, "0000000000000000deadbeefdeadbeef",
        "基线必须换成本远端的指纹"
    );
    assert!(
        !index.files.contains_key("shared/dict.txt"),
        "别的远端留下的基线条目不许被沿用：{:?}",
        index.files.keys().collect::<Vec<_>>()
    );
}

/// 基线属于**本**远端 ⇒ 正常沿用，不会被作废。
#[test]
fn an_index_for_this_remote_is_kept() {
    let rime = temp_dir("scope-keep-rime");
    let state = temp_dir("scope-keep-state");
    let mock = MockDav::start(vec![]);

    // 先用同一个 mock 建一次引擎，让它把当前远端的指纹写进 index.json。
    {
        let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
        let report = engine.transport();
        assert!(report.ok, "首轮搬运应当成功：{report:?}");
    }
    let index_text = fs::read_to_string(state.join("index.json")).expect("read index");
    let index: FileIndex = serde_json::from_str(&index_text).expect("parse index");
    assert!(
        !index.remote.is_empty(),
        "写盘时必须记下远端指纹，否则下次没法判断归属"
    );

    // 再建一次：指纹相同 ⇒ 基线照常用（不该被当成首次接入）。
    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let plan = engine.plan();
    assert!(
        !plan.first_sync,
        "同远端的基线必须照常使用，不该被当成首次接入：{plan:?}"
    );
}
