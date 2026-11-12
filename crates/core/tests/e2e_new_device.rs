//! 端到端：新设备接入 -> 自动写好合并词典指针。
//!
//! 用 MockDav 模拟远端：shared/files 里有 merged.dict.yaml（合并词典）
//! 与 local_extra.dict.yaml。本机是「全新设备」：装输入法时铺好的 custom.yaml，
//! 里面还没有合并词典指针。
//! 期望：transport 之后指针自动写进本机 <schema>.custom.yaml，并触发重新部署。
//! 反向：用户自己写好的指针一个字都不许动。

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::config::{Config, FirstSyncPolicy};
use rime_sync_core::engine::Engine;
use rime_sync_core::platform::FakeTrigger;

use support::{manifest_for, temp_dir, MockDav};

const MERGED: &str = "merged.dict.yaml";
const CUSTOM: &str = "luna_pinyin_simp.custom.yaml";

const MERGED_YAML: &str =
    "name: merged\nversion: \"2026.11.11\"\nimport_tables:\n  - luna_pinyin\n  - local_extra\n...\n";
const EXTRA_YAML: &str = "name: local_extra\nversion: \"1\"\n...\n";

fn workspace(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let rime = root.join("rime");
    let state = root.join("state");
    fs::create_dir_all(&rime).expect("rime");
    fs::create_dir_all(&state).expect("state");
    (root, rime, state)
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, content).expect("write");
}

/// 搭一个远端：只有合并词典与一个词库源。
fn remote() -> MockDav {
    MockDav::start(vec![
        (
            "shared/files/merged.dict.yaml",
            MERGED_YAML.as_bytes().to_vec(),
        ),
        (
            "shared/files/local_extra.dict.yaml",
            EXTRA_YAML.as_bytes().to_vec(),
        ),
        (
            "shared/manifest.json",
            manifest_for(&[
                ("merged.dict.yaml", MERGED_YAML.as_bytes()),
                ("local_extra.dict.yaml", EXTRA_YAML.as_bytes()),
            ]),
        ),
    ])
}

/// 方案本体在 data 目录（真机上就是 /usr/share/rime-data）。
fn write_schema(data: &Path) {
    write(
        &data.join("luna_pinyin_simp.schema.yaml"),
        "schema:\n  schema_id: luna_pinyin_simp\ntranslator:\n  dictionary: luna_pinyin\n",
    );
}

fn engine_for(
    dav: &MockDav,
    rime: &Path,
    state: &Path,
) -> (Engine, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let mut config = Config::default();
    config.webdav.url = dav.url();
    config.webdav.username = "u".to_string();
    config.device_id = "brand-new-device".to_string();
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
fn new_device_gets_the_pointer_automatically() {
    let (root, rime, state) = workspace("e2e-new-device");
    let dav = remote();

    // 新设备本地自带 custom.yaml（安装时铺的模板 / fix-ime 修好的那份），
    // 里面还没有合并词典指针 —— 这正是「新设备接入时自动写好」要处理的场景。
    write(&rime.join(CUSTOM), "patch:\n  \"menu/page_size\": 8\n");

    let data = root.join("data");
    write_schema(&data);

    let (mut engine, calls) = engine_for(&dav, &rime, &state);
    let report = engine.transport();

    println!(
        "report ok={} pulled={} pushed={} pointer={}",
        report.ok, report.pulled, report.pushed, report.merged_pointer_written
    );
    assert!(report.ok, "transport must succeed: {:?}", report.error);

    // 合并词典应该被拉到本机用户目录顶层。
    assert!(
        rime.join(MERGED).is_file(),
        "merged.dict.yaml must land in rime_dir top level"
    );

    // 关键断言：指针自动写进去了，且用户自己写的内容没被动。
    let text = fs::read_to_string(rime.join(CUSTOM)).expect("custom");
    println!("--- local custom.yaml ---\n{}", text);
    assert!(
        text.contains("translator/dictionary") && text.contains("merged"),
        "the pointer must be written automatically, got: {}",
        text
    );
    assert!(
        text.contains("menu/page_size"),
        "the user's own settings must survive: {}",
        text
    );
    assert!(
        report.merged_pointer_written,
        "the report must record the write"
    );

    // 写了指针就必须重新部署。
    let recorded = calls.lock().expect("lock").clone();
    println!("trigger calls: {:?}", recorded);
    assert!(
        recorded.iter().any(|s| s == "redeploy_after_config_change"),
        "a redeploy must follow the pointer write, got {:?}",
        recorded
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_users_own_pointer_is_never_overwritten() {
    let (root, rime, state) = workspace("e2e-keep-pointer");
    let dav = remote();

    // 用户自己写好的指针：指向别的词库，不许被改。
    let original =
        "patch:\n  \"translator/dictionary\": luna_pinyin_extra\n  \"menu/page_size\": 8\n";
    write(&rime.join(CUSTOM), original);

    let data = root.join("data");
    write_schema(&data);

    let (mut engine, calls) = engine_for(&dav, &rime, &state);
    let report = engine.transport();

    assert!(report.ok, "transport must succeed: {:?}", report.error);
    assert!(
        !report.merged_pointer_written,
        "no pointer may be written when the user already has one"
    );

    let text = fs::read_to_string(rime.join(CUSTOM)).expect("custom");
    println!("--- local custom.yaml ---\n{}", text);
    assert!(
        text.contains("luna_pinyin_extra") && !text.contains("merged"),
        "the user's own pointer must survive untouched: {}",
        text
    );

    // 没写指针就不该白折腾一次重新部署。
    let recorded = calls.lock().expect("lock").clone();
    println!("trigger calls: {:?}", recorded);
    assert!(
        !recorded.iter().any(|s| s == "redeploy_after_config_change"),
        "no pointer written -> no redeploy, got {:?}",
        recorded
    );

    let _ = fs::remove_dir_all(&root);
}

/// apply 路径（合并词库）里如果新写了合并词典指针，配置必须真的重编译。
///
/// Linux 的 deploy 就是 rime_deployer --build（会重编译），所以走普通 deploy 即可；
/// Windows 的 deploy 只结束 WeaselServer（**不**重编译），必须改走 manual_deploy
/// ——否则指针写了却没生效，用户会看到「配了没反应」。
#[test]
fn apply_recompiles_when_the_pointer_was_written() {
    let (root, rime, state) = workspace("e2e-apply-recompile");
    let dav = remote();

    // 本地有 custom.yaml（无指针）+ 方案用 luna_pinyin。
    write(&rime.join(CUSTOM), "patch:\n  \"menu/page_size\": 8\n");
    let data = root.join("data");
    write_schema(&data);

    // 模拟 Windows：deploy 不重编译。
    let trigger = FakeTrigger::new().with_no_recompile();
    let calls = trigger.calls_shared();

    let mut config = Config::default();
    config.webdav.url = dav.url();
    config.webdav.username = "u".to_string();
    config.device_id = "apply-device".to_string();
    config.first_sync_policy = FirstSyncPolicy::Normal;

    let mut engine =
        Engine::new(config, rime.clone(), state.clone(), Box::new(trigger)).expect("engine");

    let report = engine.apply();
    assert!(report.ok, "apply must succeed: {:?}", report.error);
    assert!(
        report.merged_pointer_written,
        "apply must have written the pointer"
    );

    let recorded = calls.lock().expect("lock").clone();
    println!("trigger calls: {:?}", recorded);
    assert!(
        recorded.iter().any(|s| s == "manual_deploy"),
        "a platform whose deploy does not recompile must use manual_deploy, got {:?}",
        recorded
    );

    let _ = fs::remove_dir_all(&root);
}
