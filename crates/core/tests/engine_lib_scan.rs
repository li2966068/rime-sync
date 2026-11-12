//! 引擎库扫描与增强组件建议文案的回归测试。
//!
//! * 扫描函数必须能钻到两级子目录 —— Windows 的 Weasel 装在
//!   `<Program Files>/Rime/weasel-<版本>/` 一层里，rime.dll 就在那一层；
//!   这件事在 Linux 上用临时目录就能测，不必真有一台 Windows 机器。
//! * advice() 的文案是直接给用户看的，不能混进连续空格。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rime_sync_core::ime_state::{self, ExtraSupport};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "rime-sync-core-engine-lib-scan-{}-{seq}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn write_file(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, content).expect("write file");
}

/// 两级子目录里的 rime.dll 能被扫到（Windows 的 weasel-<版本> 一层）。
#[test]
fn finds_rime_dll_two_levels_deep() {
    let root = temp_dir("two-levels");
    let lib = root.join("Rime/weasel-0.17.0.0/rime.dll");
    write_file(&lib, "fake rime.dll");

    let found = ime_state::engine_libs_under(&root, "rime.dll", 2);
    assert_eq!(
        found,
        vec![lib.clone()],
        "扫两级子目录必须找到 weasel 那一层里的 rime.dll"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 只允许扫一级子目录时，两级深度的 rime.dll 不该被找到（深度是可控的）。
#[test]
fn depth_one_stops_at_first_level() {
    let root = temp_dir("one-level");
    let lib = root.join("Rime/weasel-0.17.0.0/rime.dll");
    write_file(&lib, "fake rime.dll");

    let found = ime_state::engine_libs_under(&root, "rime.dll", 1);
    assert!(
        found.is_empty(),
        "只扫一级子目录时不该找到两层深的 rime.dll：{found:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// 一级子目录里的 librime.so 能被扫到（Unix 侧 x86_64-linux-gnu 那种形状）。
#[test]
fn finds_librime_so_in_first_level_subdir() {
    let root = temp_dir("unix");
    let lib = root.join("x86_64-linux-gnu/librime.so.1.17.0");
    write_file(&lib, "fake librime");

    let found = ime_state::engine_libs_under(&root, "librime.so", 1);
    assert_eq!(found, vec![lib.clone()]);

    let _ = fs::remove_dir_all(&root);
}

/// 目录里没有该前缀的文件 → 返回空（不是判成「不支持」的依据）。
#[test]
fn returns_empty_when_no_matching_prefix() {
    let root = temp_dir("empty");
    write_file(&root.join("Rime/weasel-0.17.0.0/rime.exe"), "not a lib");
    write_file(&root.join("Rime/other.dll"), "not a lib");

    let found = ime_state::engine_libs_under(&root, "rime.dll", 2);
    assert!(found.is_empty(), "没有该前缀文件时必须返回空：{found:?}");

    let _ = fs::remove_dir_all(&root);
}

/// 目录根本不存在时也不该炸（返回空）。
#[test]
fn missing_dir_yields_empty() {
    let root = temp_dir("missing");
    let gone = root.join("nope");
    assert!(ime_state::engine_libs_under(&gone, "rime.dll", 2).is_empty());

    let _ = fs::remove_dir_all(&root);
}

/// Unix 侧候选表里不能有可执行文件所在目录（那是构建目录，扫它纯属浪费）。
#[cfg(unix)]
#[test]
fn unix_targets_exclude_exe_dirs() {
    let exe = std::env::current_exe().expect("current exe");
    let parent = exe.parent().expect("exe parent").to_path_buf();
    let grandparent = parent.parent().map(|p| p.to_path_buf());
    for target in ime_state::engine_lib_targets() {
        assert_ne!(
            target.dir, parent,
            "Unix 侧不该扫可执行文件所在目录（那是构建目录）"
        );
        if let Some(gp) = &grandparent {
            assert_ne!(&target.dir, gp, "Unix 侧不该扫可执行文件的祖父目录");
        }
    }
}

/// 预算闸门生效：访问目录数到上限就停止下钻，返回已找到的结果，不 panic。
#[test]
fn scan_budget_stops_descending() {
    let root = temp_dir("budget");
    // 造 6 个子目录，每个子目录里放一个 rime.dll。
    for i in 0..6 {
        write_file(&root.join(format!("sub{i}/rime.dll")), "fake rime.dll");
    }

    // 预算 3 = 最多访问 3 个目录（顶层 + 2 个子目录），到顶就停。
    let limited = ime_state::engine_libs_under_with_budget(&root, "rime.dll", 2, 3);
    assert!(limited.len() <= 3, "预算用完就该停止下钻：{limited:?}");

    // 预算充足时全部都能找到（闸门不是把结果掐死）。
    let all = ime_state::engine_libs_under_with_budget(&root, "rime.dll", 2, 100);
    assert_eq!(all.len(), 6);

    // 预算为 0 → 一个目录都不访问。
    assert!(
        ime_state::engine_libs_under_with_budget(&root, "rime.dll", 2, 0).is_empty(),
        "预算为 0 时不该访问任何目录"
    );

    let _ = fs::remove_dir_all(&root);
}

/// advice() 的文案是直接给用户看的，不能出现连续两个以上的空格。
#[test]
fn advice_text_has_no_repeated_spaces() {
    for support in [
        ExtraSupport::Supported,
        ExtraSupport::Unsupported,
        ExtraSupport::Unknown,
    ] {
        let text = support.advice();
        assert!(!text.is_empty(), "advice() 不能是空串");
        assert!(
            !text.contains("  "),
            "advice() 不能含连续空格：{support:?} -> {text}"
        );
    }
}

/// 把三态的建议文案原样打印出来（人话模式就是把它直接打给用户看）。
#[test]
fn print_advice_texts() {
    for support in [
        ExtraSupport::Supported,
        ExtraSupport::Unsupported,
        ExtraSupport::Unknown,
    ] {
        println!("extra_support={}", support.as_str());
        println!("{}", support.advice());
    }
}

/// Unsupported / Unknown 的文案要点还在（修文案不能把信息删没了）。
#[test]
fn advice_text_keeps_key_points() {
    let unsupported = ExtraSupport::Unsupported.advice();
    assert!(unsupported.contains("rime-sync 自建版"));
    assert!(unsupported.contains("删除原来的输入法"));
    assert!(unsupported.contains("Linux"));
    assert!(unsupported.contains("Windows"));

    let unknown = ExtraSupport::Unknown.advice();
    assert!(unknown.contains("修复输入法"));
    assert!(unknown.contains("完整安装包"));
}
