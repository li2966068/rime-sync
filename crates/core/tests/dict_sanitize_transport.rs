//! 端到端：同步时自动清洗合并词典里的第三方词库。
//!
//! 场景（227 实测的病根）：用户目录里的第三方词库（搜狗一类的）混着英文词，
//! code 被逐字母拆开（`web` → `w e b`）。这些单音节会挤进 librime 的音节表，
//! 把首字母缩写索引顶掉 ⇒ 敲 `bhzt` 查不出「渤海钻探」。
//!
//! 期望：一次 transport 之后
//! * 第三方词库里的坏词条被剔除（文件被改写）；
//! * 报告里 `dict_sanitized` 为真；
//! * 官方词库一个字节都没动；
//! * 触发了重新部署（否则 build/ 里还是旧的 .table.bin，首字母依旧是坏的）；
//! * 清洗后的版本被推给远端（修复随同步传播到其它设备）。

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::config::FirstSyncPolicy;
use rime_sync_core::engine::Engine;
use rime_sync_core::platform::FakeTrigger;

use support::{manifest_for, temp_dir, MockDav};

/// 官方 luna_pinyin：单音节只有 a/o/e。
const LUNA: &str = "---\nname: luna_pinyin\nversion: \"2026.11.11\"\n...\n\
中文\tdong wen\t100\n啊\ta\t90\n哦\to\t80\n";

/// 第三方词库：合法中文 + 逐字母拆开的英文。
const EXTRA: &str = "---\nname: extra_sogou\nversion: \"2026.11.11\"\n...\n\
渤海\tdeng hai\t100\nweb\tw e b\t34\ndocker\td o c k e r\t43\n";

/// 合并词典包装器。
const MERGED: &str = "name: merged\nversion: \"2026.11.11\"\nimport_tables:\n  - luna_pinyin\n  - extra_sogou\n...\n";

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir parent");
    }
    fs::write(path, content.as_bytes()).expect("write");
}

fn workspace(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let rime = root.join("rime");
    let state = root.join("state");
    fs::create_dir_all(&rime).expect("rime");
    fs::create_dir_all(&state).expect("state");
    (root, rime, state)
}

/// 远端：合并词典 + 两个词库（与本地一样脏）。
fn remote() -> MockDav {
    MockDav::start(vec![
        ("shared/files/merged.dict.yaml", MERGED.as_bytes().to_vec()),
        (
            "shared/files/luna_pinyin.dict.yaml",
            LUNA.as_bytes().to_vec(),
        ),
        (
            "shared/files/extra_sogou.dict.yaml",
            EXTRA.as_bytes().to_vec(),
        ),
        (
            "shared/manifest.json",
            manifest_for(&[
                ("merged.dict.yaml", MERGED.as_bytes()),
                ("luna_pinyin.dict.yaml", LUNA.as_bytes()),
                ("extra_sogou.dict.yaml", EXTRA.as_bytes()),
            ]),
        ),
    ])
}

fn engine_for(
    dav: &MockDav,
    rime: &Path,
    state: &Path,
) -> (Engine, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let mut config = rime_sync_core::config::Config::default();
    config.webdav.url = dav.url();
    config.webdav.username = "u".to_string();
    config.device_id = "devA".to_string();
    config.first_sync_policy = FirstSyncPolicy::Normal;
    let trigger = FakeTrigger::new();
    let calls = trigger.calls_shared();
    let engine = Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(trigger),
    )
    .expect("engine");
    (engine, calls)
}

#[test]
fn transport_cleans_third_party_dict() {
    let (root, rime, state) = workspace("dict-sanitize-transport");
    let dav = remote();

    // 本机：合并词典包装器 + 脏的第三方词库 + 官方 luna（放在用户目录当查表来源）。
    write(&rime.join("merged.dict.yaml"), MERGED);
    write(&rime.join("luna_pinyin.dict.yaml"), LUNA);
    write(&rime.join("extra_sogou.dict.yaml"), EXTRA);

    let (mut engine, calls) = engine_for(&dav, &rime, &state);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);

    // 坏词条被剔除。
    let extra = fs::read_to_string(rime.join("extra_sogou.dict.yaml")).expect("read");
    assert!(!extra.contains("w e b"), "broken entry must be gone");
    assert!(!extra.contains("d o c k e r"), "broken entry must be gone");
    assert!(extra.contains("渤海"), "valid entry must survive");

    // 官方词库一个字节都没动。
    let luna = fs::read_to_string(rime.join("luna_pinyin.dict.yaml")).expect("read");
    assert_eq!(luna, LUNA);

    // 报告要如实记录。
    assert!(report.dict_sanitized, "report must record the sanitize");
    assert!(
        report.config_changed,
        "a changed dictionary must trigger a rebuild"
    );

    // 必须重新部署（Windows 上就是进程内重编译；Linux 上是 rime_deployer --build）。
    let recorded = calls.lock().expect("lock").clone();
    assert!(
        recorded
            .iter()
            .any(|call| call == "redeploy_after_config_change" || call == "manual_deploy"),
        "a rebuild must follow the sanitize, got {:?}",
        recorded
    );

    // 第 2 轮：干净版本推给远端（修复随同步传播到其它设备）。
    let (mut engine, _calls) = engine_for(&dav, &rime, &state);
    let report2 = engine.transport();
    assert!(report2.ok, "second transport failed: {:?}", report2.error);
    let snapshot = dav.snapshot();
    let pushed = snapshot
        .iter()
        .find(|(key, _)| key.ends_with("shared/files/extra_sogou.dict.yaml"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned());
    let pushed = pushed.expect("pushed dict");
    assert!(!pushed.contains("w e b"), "pushed dict must be clean");
    assert!(
        pushed.contains("渤海"),
        "pushed dict must keep valid entries"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn clean_dict_is_left_alone() {
    let (root, rime, state) = workspace("dict-sanitize-noop");
    let dav = remote();

    write(&rime.join("merged.dict.yaml"), MERGED);
    write(&rime.join("luna_pinyin.dict.yaml"), LUNA);
    // 已经是干净的第三方词库。
    let clean = "---\nname: extra_sogou\nversion: \"2026.11.11\"\n...\n渤海\tdeng hai\t100\n";
    write(&rime.join("extra_sogou.dict.yaml"), clean);

    let (mut engine, _calls) = engine_for(&dav, &rime, &state);
    let report = engine.transport();
    assert!(report.ok, "transport failed: {:?}", report.error);
    assert!(!report.dict_sanitized, "nothing to clean");
    assert_eq!(
        fs::read_to_string(rime.join("extra_sogou.dict.yaml")).expect("read"),
        clean
    );

    let _ = fs::remove_dir_all(&root);
}
