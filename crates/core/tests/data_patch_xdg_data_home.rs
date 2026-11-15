//! B3'：`user_fallback_candidates` 必须认 XDG_DATA_HOME。
//!
//! 背景：Linux 上 fcitx5 的用户数据目录按 XDG 规范是
//! `${XDG_DATA_HOME:-$HOME/.local/share}/fcitx5/rime`。以前
//! `data_patch::user_fallback_candidates` 只认硬编码的
//! `~/.local/share/fcitx5/rime`，把 XDG_DATA_HOME 指向别处时，那个目录
//! 根本不在候选里 ⇒ 降级写不进用户真正在用的目录。
//!
//! 本测试用环境变量把 HOME / XDG_DATA_HOME 指到临时目录，断言：
//!   1. XDG_DATA_HOME 下的 fcitx5/rime 进得了候选；
//!   2. HOME/.local/share/fcitx5/rime 仍然进得了候选（缺省兜底）；
//!   3. 两者都存在时，XDG_DATA_HOME 那个排在前面（XDG 优先）。

#![cfg(not(windows))]

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use rime_sync_core::data_patch;

/// 同一进程里改 HOME/XDG 是全局的 ⇒ 测试之间必须串行。
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 环境变量守卫：改完还原。
struct EnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, &PathBuf)]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            saved.push((*key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[test]
fn xdg_data_home_fcitx5_rime_is_a_fallback_candidate() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("data-patch-xdg-data-home");
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let xdg = root.join("xdg-data");
    let xdg_rime = xdg.join("fcitx5/rime");
    fs::create_dir_all(&xdg_rime).expect("create xdg rime dir");

    let _lock = env_lock();
    let _env = EnvGuard::set(&[
        ("HOME", &home),
        ("XDG_DATA_HOME", &xdg),
        // 让 rime_dir::detect 走候选列表而不是环境变量指定的位置，
        // 这样 user_fallback_candidates 的内容才是可测的。
        ("RIME_USER_DATA_DIR", &home.join("nonexistent-rime-dir")),
    ]);

    let candidates = data_patch::user_fallback_candidates();

    assert!(
        candidates.contains(&xdg_rime),
        "XDG_DATA_HOME 下的 fcitx5/rime 必须在候选里：{candidates:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn home_local_share_fcitx5_rime_is_still_a_candidate_without_xdg_data_home() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("data-patch-xdg-data-home-default");
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let home_rime = home.join(".local/share/fcitx5/rime");
    fs::create_dir_all(&home_rime).expect("create home rime dir");

    let _lock = env_lock();
    let _env = EnvGuard::set(&[
        ("HOME", &home),
        ("XDG_DATA_HOME", &root.join("nonexistent-xdg")),
        ("RIME_USER_DATA_DIR", &home.join("nonexistent-rime-dir")),
    ]);

    let candidates = data_patch::user_fallback_candidates();

    assert!(
        candidates.contains(&home_rime),
        "缺省的 ~/.local/share/fcitx5/rime 仍然必须在候选里：{candidates:?}"
    );

    let _ = fs::remove_dir_all(&root);
}
