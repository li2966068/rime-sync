//! core 侧「增强组件可用性」判定的回归（CLI 报告的基准）。
//!
//! CLI 的 check-ime / fix-ime 只是把这里的结论暴露出去，所以这里钉死三件事：
//! * 三个取值的机器可读值固定（supported / unsupported / unknown）；
//! * 只要产物或用户配置里带 rime-sync 标记，判定就是「确定支持」；
//! * 判定永远落在三个取值之内，且 advice() 永远给得出话（不能是空串）。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rime_sync_core::ime_state::{self, ExtraSupport};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "rime-sync-core-extra-support-{}-{seq}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// 三个取值的机器可读值固定不变（CLI 的 JSON 字段直接用这个值）。
#[test]
fn extra_support_values_are_stable() {
    assert_eq!(ExtraSupport::Supported.as_str(), "supported");
    assert_eq!(ExtraSupport::Unsupported.as_str(), "unsupported");
    assert_eq!(ExtraSupport::Unknown.as_str(), "unknown");
    assert!(ExtraSupport::Supported.usable());
    assert!(!ExtraSupport::Unsupported.usable());
    assert!(!ExtraSupport::Unknown.usable());
}

/// 编译产物里带 rime-sync 标记 → 确定支持（与系统装的是哪个引擎无关）。
#[test]
fn marked_artifact_means_supported() {
    let dir = temp_dir("artifact");
    fs::create_dir_all(dir.join("build")).expect("create build dir");
    fs::write(
        dir.join("build/luna_pinyin_simp.schema.yaml"),
        "patch:\n  rimesync_vnum: '['\n  rimesync_punct: ']'\n",
    )
    .expect("write schema");

    assert_eq!(
        ime_state::assess_extra_support(&dir),
        ExtraSupport::Supported
    );

    let _ = fs::remove_dir_all(&dir);
}

/// 用户配置里带 rime-sync 标记 → 确定支持（装输入法时铺的模板就是这个形状）。
#[test]
fn marked_user_custom_means_supported() {
    let dir = temp_dir("custom");
    fs::write(
        dir.join("wubi86.custom.yaml"),
        "patch:\n  engine/processors:\n    - rimesync_vselect\n    - numpad\n",
    )
    .expect("write custom");

    assert_eq!(
        ime_state::assess_extra_support(&dir),
        ExtraSupport::Supported
    );

    let _ = fs::remove_dir_all(&dir);
}

/// 判定永远落在三个取值之内，且 advice() 永远给得出话。
#[test]
fn assessment_always_yields_a_known_value_with_advice() {
    let dir = temp_dir("empty");
    let support = ime_state::assess_extra_support(&dir);

    assert!(
        matches!(
            support,
            ExtraSupport::Supported | ExtraSupport::Unsupported | ExtraSupport::Unknown
        ),
        "判定必须落在三个取值之内：{support:?}"
    );
    assert!(
        !support.advice().is_empty(),
        "advice() 必须给得出话（不能是空串）"
    );

    let _ = fs::remove_dir_all(&dir);
}
