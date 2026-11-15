//! D4 回归：密码文件从落地那一刻起权限就是 0600。
//!
//! 缺陷现象（改前）：`fs::write` 先落盘（umask 默认权限，常见 0644），
//! 之后才 `set_permissions(0o600)` ⇒ 同机其它用户有一个窗口能读到密码文件。
//!
//! 现在：先写一个**从创建那一刻起就是 0600** 的临时文件，再改名到最终路径。

use std::fs;
use std::path::PathBuf;

use rime_sync_core::secret::{FileStore, SecretStore};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-d4-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// 密码文件落地就是 0600（不是"先可读再 chmod"）。
#[cfg(unix)]
#[test]
fn secret_file_is_private_from_the_moment_it_lands() {
    let root = scratch("mode");
    let path = root.join("secrets.json");
    let store = FileStore::new(&path);

    store.set("dav", "s3cret").expect("set secret");

    assert!(path.is_file(), "密码文件应当存在");
    assert_eq!(
        mode_of(&path),
        0o600,
        "密码文件必须是 0600（改前：落盘时先是 umask 默认权限）"
    );
    assert_eq!(
        store.get("dav").expect("get secret").as_deref(),
        Some("s3cret"),
        "内容必须写对"
    );
    fs::remove_dir_all(&root).ok();
}

/// 临时文件用完即删：目录里不能留下密码的副本。
#[cfg(unix)]
#[test]
fn temporary_file_is_removed_after_the_rename() {
    let root = scratch("tmp");
    let path = root.join("secrets.json");
    let store = FileStore::new(&path);

    store.set("dav", "s3cret").expect("set secret");
    store.set("proxy", "p4ss").expect("set secret again");

    let leftovers: Vec<String> = fs::read_dir(&root)
        .expect("read dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name != "secrets.json")
        .collect();
    assert!(
        leftovers.is_empty(),
        "不该留下临时文件（里面有密码）：{leftovers:?}"
    );
    fs::remove_dir_all(&root).ok();
}

/// 覆盖写（改名替换）之后权限仍然是 0600。
#[cfg(unix)]
#[test]
fn overwriting_keeps_the_file_private() {
    let root = scratch("overwrite");
    let path = root.join("secrets.json");
    let store = FileStore::new(&path);

    store.set("dav", "one").expect("set secret");
    store.set("dav", "two").expect("overwrite secret");
    store.delete("dav").expect("delete secret");

    assert_eq!(mode_of(&path), 0o600, "覆盖写之后权限不能被放松");
    assert_eq!(store.get("dav").expect("get secret"), None);
    fs::remove_dir_all(&root).ok();
}

/// 内容与删除语义不变（这次改动只碰落盘顺序，不碰格式）。
#[test]
fn store_round_trips_values() {
    let root = scratch("roundtrip");
    let path = root.join("secrets.json");
    let store = FileStore::new(&path);

    store.set("a", "1").expect("set a");
    store.set("b", "2").expect("set b");
    assert_eq!(store.get("a").expect("get a").as_deref(), Some("1"));
    assert_eq!(store.get("b").expect("get b").as_deref(), Some("2"));
    store.delete("a").expect("delete a");
    assert_eq!(store.get("a").expect("get a"), None);
    assert_eq!(store.get("b").expect("get b").as_deref(), Some("2"));
    fs::remove_dir_all(&root).ok();
}
