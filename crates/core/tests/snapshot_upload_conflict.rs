//! 回归（缺陷 2）：快照上传必须带并发保护，不能静默覆盖另一台设备写过的词库。
//!
//! 改前：`push_snapshots` 上传快照时**不带 If-Match**，也没有比对远端 ETag，
//! 于是两台设备同时同步时，后上传的那台直接把对方的词库覆盖掉，用户毫无察觉。
//!
//! 现在：
//! * 本地基线里记着远端 ETag ⇒ 上传前先 HEAD 一次比对；不一致就**记成冲突**、
//!   本轮不覆盖；
//! * 上传本身带 If-Match（远端 ETag）⇒ HEAD 与 PUT 之间又被改动时服务端回 412，
//!   同样记成冲突。

mod support;

use std::fs;

use rime_sync_core::config::FirstSyncPolicy;
use rime_sync_core::index::{sha256_hex, FileIndex, FileMeta, INDEX_VERSION};

use support::{engine_for, etag_for, temp_dir, MockDav};

const SNAPSHOT_REL: &str = "sync/devA/luna_pinyin_simp.userdb.txt";

/// 远端 ETag 与本地基线记录的不一致 ⇒ 不上传，记冲突，远端内容原封不动。
#[test]
fn snapshot_upload_is_rejected_when_remote_etag_differs() {
    let rime = temp_dir("snap-conflict");
    let state = temp_dir("snap-conflict-state");
    let local_bytes = b"local snapshot content".to_vec();
    let remote_bytes = b"written by another device".to_vec();

    // 本机快照文件（sync/<device_id>/ 下）。
    let snapshot_dir = rime.join("sync").join("devA");
    fs::create_dir_all(&snapshot_dir).expect("mkdir snapshot dir");
    fs::write(
        snapshot_dir.join("luna_pinyin_simp.userdb.txt"),
        &local_bytes,
    )
    .expect("write snapshot");

    // 远端已有「另一台设备」写过的同名快照。
    let mock = MockDav::start(vec![(SNAPSHOT_REL, remote_bytes.clone())]);

    // 本地基线里记着一个**过期**的 ETag（上一轮同步时的远端版本）。
    let mut index = FileIndex {
        version: INDEX_VERSION,
        ..FileIndex::default()
    };
    index.snapshots.insert(
        SNAPSHOT_REL.to_string(),
        FileMeta {
            // 基线记的是**上一轮**的内容：本轮本地已经改过，所以要重新上传。
            sha256: sha256_hex(b"previous version"),
            size: local_bytes.len() as u64,
            mtime: "0".to_string(),
            etag: Some("\"stale-etag-from-last-sync\"".to_string()),
        },
    );
    fs::write(
        state.join("index.json"),
        serde_json::to_string_pretty(&index).expect("index json"),
    )
    .expect("write index");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let report = engine.transport();

    // 冲突不是「错误」：这一轮照常结束，但必须把冲突摆到明面上。
    assert!(
        report
            .conflicts
            .iter()
            .any(|item| item.contains("另一台设备刚写过这个词库")),
        "远端 ETag 与本地基线不一致时必须记成冲突：conflicts={:?} error={:?}",
        report.conflicts,
        report.error
    );
    assert!(
        report
            .conflicts
            .iter()
            .any(|item| item.contains(SNAPSHOT_REL)),
        "冲突记录必须点名是哪个文件：{:?}",
        report.conflicts
    );

    // 远端内容不许被覆盖。
    let remote = mock.snapshot();
    assert_eq!(
        remote.get(SNAPSHOT_REL).map(|bytes| bytes.as_slice()),
        Some(remote_bytes.as_slice()),
        "远端快照不许被静默覆盖：{:?}",
        remote.get(SNAPSHOT_REL)
    );

    // 也不能对同一个文件发过 PUT。
    assert!(
        !mock
            .requests()
            .iter()
            .any(|(method, path)| method == "PUT" && path == SNAPSHOT_REL),
        "ETag 不一致时不得上传：{:?}",
        mock.requests()
    );
}

/// 反面对照：远端 ETag 与本地基线一致时，正常上传（并发保护不能把同步堵死）。
#[test]
fn snapshot_upload_proceeds_when_remote_etag_matches() {
    let rime = temp_dir("snap-match");
    let state = temp_dir("snap-match-state");
    let local_bytes = b"local snapshot content".to_vec();
    let remote_bytes = b"same as last time".to_vec();

    let snapshot_dir = rime.join("sync").join("devA");
    fs::create_dir_all(&snapshot_dir).expect("mkdir snapshot dir");
    fs::write(
        snapshot_dir.join("luna_pinyin_simp.userdb.txt"),
        &local_bytes,
    )
    .expect("write snapshot");

    let mock = MockDav::start(vec![(SNAPSHOT_REL, remote_bytes.clone())]);

    // 本地基线里的 ETag 就是远端当前的 ETag。
    let mut index = FileIndex {
        version: INDEX_VERSION,
        ..FileIndex::default()
    };
    index.snapshots.insert(
        SNAPSHOT_REL.to_string(),
        FileMeta {
            // 本地内容改过（sha 与基线不同）⇒ 需要重新上传；
            // 但基线里的 ETag 与远端一致 ⇒ 没有别人抢写，可以正常传。
            sha256: sha256_hex(b"older content"),
            size: local_bytes.len() as u64,
            mtime: "0".to_string(),
            etag: Some(etag_for(&remote_bytes)),
        },
    );
    fs::write(
        state.join("index.json"),
        serde_json::to_string_pretty(&index).expect("index json"),
    )
    .expect("write index");

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let report = engine.transport();

    assert!(
        !report
            .conflicts
            .iter()
            .any(|item| item.contains("另一台设备刚写过这个词库")),
        "远端没被别人改过就不该报冲突：{:?}",
        report.conflicts
    );
    assert_eq!(
        mock.snapshot()
            .get(SNAPSHOT_REL)
            .map(|bytes| bytes.as_slice()),
        Some(local_bytes.as_slice()),
        "本机快照应当上传成功：{:?}",
        report.error
    );
}
