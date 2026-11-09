//! 多设备之间 *.custom.yaml 的真实同步语义（回归测试）。
//!
//! 这三个用例把「设备配置（*.custom.yaml）到底怎么跨设备流动」钉死成可执行的事实，
//! 防止以后又写出「输入法修复会通过同步跨设备传播」这种错误结论：
//!
//! - P1：A 修好（engine/processors 里有 numpad）并同步后，B 同步，B 本地仍然缺 numpad。
//!   → 配置按设备各自存放（远端 sync/<device>/config/files），不跨设备传播。
//! - P2：新设备首次同步会从远端 shared/files 继承旧设备留下的旧配置（缺 numpad）。
//!   → 首次接入会拿到「旧设备留下的历史配置」，修复不会自动降临到新设备。
//! - P3：本地已有（修好的）配置不会被种子覆盖。
//!   → 种子只补本地缺失的文件，老用户升级时本地就是初值。
//!
//! 全部用 support::MockDav 起真 WebDAV（本地 socket，无外网），断言不依赖时间与网络。

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::Engine;
use rime_sync_core::platform::FakeTrigger;

mod support;

use support::{manifest_for, temp_dir, MockDav};

/// 被同步的方案配置（相对 rime 目录）。
const REL: &str = "luna_pinyin_simp.custom.yaml";

/// 旧配置：缺少 numpad（rime-sync 的输入增强组件之一）。
const OLD: &[u8] = b"patch:\n  engine/processors:\n    - rimesync_punct\n    - rimesync_vselect\n";

/// 修好的配置：engine/processors 里补上了 numpad。
const NEW: &[u8] =
    b"patch:\n  engine/processors:\n    - rimesync_punct\n    - rimesync_vselect\n    - numpad\n";

/// 判断配置里是否含 numpad（只看 engine/processors 列表项）。
fn has_numpad(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "- numpad")
}

/// 造一台设备的引擎：device_id 决定它的远端目录（sync/<device_id>/config/...）。
fn engine(mock: &MockDav, rime: &Path, state: &Path, device: &str) -> Engine {
    let mut config = Config::default();
    config.webdav.url = mock.url();
    config.webdav.username = "u".to_string();
    config.device_id = device.to_string();
    config.first_sync_policy = FirstSyncPolicy::Normal;
    Engine::new(
        config,
        rime.to_path_buf(),
        state.to_path_buf(),
        Box::new(FakeTrigger::new()),
    )
    .expect("engine")
}

/// 一台设备的独立工作区：root/{rime,state}。
fn workspace(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let rime = root.join("rime");
    fs::create_dir_all(&rime).expect("rime");
    let state = root.join("state");
    fs::create_dir_all(&state).expect("state");
    (root, rime, state)
}

/// 远端某个文件的内容（按相对路径）。
fn remote_text(mock: &MockDav, rel: &str) -> String {
    let snapshot = mock.snapshot();
    let bytes = snapshot.get(rel).unwrap_or_else(|| {
        panic!(
            "remote file missing: {rel}; present: {:?}",
            snapshot.keys().collect::<Vec<_>>()
        )
    });
    String::from_utf8(bytes.clone()).expect("utf8")
}

/// P1：两台设备之间 *.custom.yaml 不互相传播。
///
/// 本用例防止再写出「输入法修复（补 numpad）会通过同步传播到其它设备」的结论：
/// 设备 A 修好并同步后，设备 B 同步，B 本地依然缺 numpad——配置是各设备各存一份
/// （远端 sync/<device_id>/config/files/），同步只发生在设备自己的目录里。
#[test]
fn fixed_config_does_not_propagate_between_devices() {
    let mock = MockDav::start(vec![]);

    // 设备 A：本地是修好的配置（engine/processors 含 numpad），同步上传。
    let (root_a, rime_a, state_a) = workspace("multi-device-a");
    fs::write(rime_a.join(REL), NEW).expect("write local A");
    let mut engine_a = engine(&mock, &rime_a, &state_a, "devA");
    let report_a = engine_a.transport();
    assert!(report_a.ok, "A transport failed: {report_a:?}");
    assert!(
        has_numpad(&remote_text(
            &mock,
            "sync/devA/config/files/luna_pinyin_simp.custom.yaml"
        )),
        "A 的修复必须上传到自己的远端目录: {report_a:?}"
    );

    // 设备 B：本地是旧的（缺 numpad）配置，同步后仍然缺 numpad。
    let (root_b, rime_b, state_b) = workspace("multi-device-b");
    fs::write(rime_b.join(REL), OLD).expect("write local B");
    let mut engine_b = engine(&mock, &rime_b, &state_b, "devB");
    let report_b = engine_b.transport();
    assert!(report_b.ok, "B transport failed: {report_b:?}");

    let local_b = fs::read_to_string(rime_b.join(REL)).expect("read local B");
    assert!(
        !has_numpad(&local_b),
        "B 的配置没有被 A 的修复覆盖（这正是本用例要钉死的语义）: {local_b}"
    );

    // 远端：B 有自己的目录，内容也仍是旧的；A 的目录不被 B 改动。
    assert!(
        !has_numpad(&remote_text(
            &mock,
            "sync/devB/config/files/luna_pinyin_simp.custom.yaml"
        )),
        "B 的远端配置也不该被 A 的修复污染"
    );
    assert!(
        has_numpad(&remote_text(
            &mock,
            "sync/devA/config/files/luna_pinyin_simp.custom.yaml"
        )),
        "A 的远端配置应保持修好的状态"
    );

    fs::remove_dir_all(&root_a).ok();
    fs::remove_dir_all(&root_b).ok();
}

/// P2：新设备首次同步会从远端 shared/files 继承旧设备留下的旧配置。
///
/// 本用例防止再写出「新设备接入后会拿到已修好的配置」这类结论：远端 shared/files 里
/// 躺着旧设备留下的、缺 numpad 的配置时，新设备首次同步后本地就是那份旧配置，
/// 修复不会自动降临到新设备。
#[test]
fn new_device_first_sync_inherits_old_shared_config() {
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest_for(&[(REL, OLD)])),
        ("shared/files/luna_pinyin_simp.custom.yaml", OLD.to_vec()),
    ]);

    // 设备 C：本地没有这个文件（全新接入）。
    let (root, rime, state) = workspace("multi-device-c");
    assert!(
        !rime.join(REL).exists(),
        "设备 C 本地必须没有该文件，才能测到种子路径"
    );

    let mut engine_c = engine(&mock, &rime, &state, "devC");
    let report_c = engine_c.transport();
    assert!(report_c.ok, "C transport failed: {report_c:?}");

    let local_c = fs::read_to_string(rime.join(REL)).expect("C 应被种入该配置");
    assert!(
        !has_numpad(&local_c),
        "新设备继承的是旧设备留下的旧配置（缺 numpad）: {local_c}"
    );
    assert!(
        report_c.notes.iter().any(|note| note.contains("首同步")),
        "应明确记录这是首同步种子: {report_c:?}"
    );

    fs::remove_dir_all(&root).ok();
}

/// P3：本地已有（修好的）配置不会被种子覆盖。
///
/// 本用例防止再写出「首次同步会用远端配置覆盖本机」这类结论：同样的远端，
/// 本地已有含 numpad 的配置时，首次同步后本地仍然是修好的那份。
#[test]
fn existing_local_config_survives_first_sync_seed() {
    let mock = MockDav::start(vec![
        ("shared/manifest.json", manifest_for(&[(REL, OLD)])),
        ("shared/files/luna_pinyin_simp.custom.yaml", OLD.to_vec()),
    ]);

    // 设备 D：本地已经有修好的配置。
    let (root, rime, state) = workspace("multi-device-d");
    fs::write(rime.join(REL), NEW).expect("write local D");

    let mut engine_d = engine(&mock, &rime, &state, "devD");
    let report_d = engine_d.transport();
    assert!(report_d.ok, "D transport failed: {report_d:?}");

    let local_d = fs::read_to_string(rime.join(REL)).expect("read local D");
    assert!(
        has_numpad(&local_d),
        "本地已有的修好配置不能被远端种子覆盖: {local_d}"
    );

    fs::remove_dir_all(&root).ok();
}
