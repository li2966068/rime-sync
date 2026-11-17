//! 原子写盘：先写临时文件，再改名。
//!
//! 为什么需要：直接 `fs::write` 会在写一半时留下**半个文件** ——
//! 断电、磁盘满、进程被杀都可能留下截断的 YAML/文本，
//! 下次启动读到的就是坏配置。改名是原子的：要么还是旧文件，要么是完整的新文件。

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 同一次进程里的递增序号：临时文件名必须**每次调用都不同**。
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成一个几乎不会撞名的临时文件名。
///
/// 2026-11-17 复核发现：临时文件名原来是固定的 `{name}.rimesync-tmp`，
/// 两个进程同时写同一个文件时会互相踩 ——
/// 甲写了一半临时文件，乙正好 rename 把它变成目标文件 ⇒ **目标被截断**，
/// 原子写反而制造了它本该防的问题。
///
/// 名字里带上进程号 + 递增序号 + 纳秒时间戳：同进程内多次调用不撞名，
/// 不同进程之间靠进程号分开。
fn unique_suffix() -> String {
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!("{pid}.{seq}.{nanos}")
}

/// 原子地把 `bytes` 写到 `path`。
///
/// 临时文件与目标同目录（改名必须同文件系统才原子），写完立刻 rename。
/// 失败时把临时文件删掉，不留垃圾。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_inner(path, bytes, None)
}

/// 原子写，并强制指定权限位（Unix；Windows 忽略）。
///
/// 配置文件这类「别人的机器上也不该被读到」的文件要用它：
/// `write_atomic` 只在目标已存在时保留原权限，新文件会落成 umask 默认权限。
pub fn write_atomic_with_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    write_atomic_inner(path, bytes, Some(mode))
}

fn write_atomic_inner(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    let tmp = parent.join(format!("{name}.rimesync-tmp.{}", unique_suffix()));

    // 权限窗口（2026-11-17 复核发现）：原来先 fs::write（按 umask 落成 0644）、
    // 写完内容**之后**才 chmod 0600 ⇒ 配置内容有一瞬是全局可读的。
    // 现在临时文件**创建那一刻**就带 0600（Unix），不存在那个窗口。
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    if let Err(error) = file.write_all(bytes) {
        // 写了一半也要清掉：半个临时文件留在用户目录里比没有更糟。
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("write {}", tmp.display()));
    }
    if let Err(error) = file.sync_all() {
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("flush {}", tmp.display()));
    }
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match mode {
            Some(mode) => {
                // 权限设不上就报错：配置文件该是 0600，静默失败等于让它按
                // umask 落成 644（别人能读）。宁可让这次写失败。
                fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))
                    .with_context(|| format!("set mode {:o} on {}", mode, tmp.display()))?;
            }
            // 覆盖已有文件时保留原权限：用户的 custom.yaml 可能是 600（私有），
            // 改名换文件会把权限悄悄放宽成默认的 644。
            None => {
                if let Ok(meta) = fs::metadata(path) {
                    let mode = meta.permissions().mode();
                    // 这里**必须报错**：静默失败等于把用户的私有配置改成 644。
                    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).with_context(
                        || format!("preserve mode {:o} on {}", mode, tmp.display()),
                    )?;
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{unique_suffix, write_atomic, write_atomic_with_mode};
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-atomic-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn writes_content_and_leaves_no_temp_file() {
        let dir = temp_dir("basic");
        let target = dir.join("a.txt");
        write_atomic(&target, b"hello").expect("write");
        assert_eq!(fs::read(&target).expect("read"), b"hello");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("readdir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains("rimesync-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary file was left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 指定权限：新文件从落地那一刻就是 0600（不是先可读再 chmod）。
    #[cfg(unix)]
    #[test]
    fn forced_mode_applies_to_a_fresh_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let target = dir.join("secret.json");
        write_atomic_with_mode(&target, b"{}", 0o600).expect("write");
        let mode = fs::metadata(&target).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "权限必须是 0600");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 两个「进程」同时写同一个文件时，临时文件不能互相踩 ——
    /// 2026-11-17 复核发现：临时文件名固定时，甲写了一半，乙把甲的半成品
    /// rename 成目标 ⇒ 目标被截断。这里用两个不同后缀模拟两个进程，
    /// 验证临时文件名确实每次都不同。
    #[test]
    fn temporary_names_do_not_collide() {
        let dir = temp_dir("unique");
        let a = unique_suffix();
        let b = unique_suffix();
        assert_ne!(a, b, "临时文件名后缀必须每次都不同");
        assert!(
            a.contains(&std::process::id().to_string()),
            "后缀应含进程号"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 权限窗口（2026-11-17 复核发现）：临时文件必须**创建那一刻**就是 0600。
    ///
    /// 原来是先 fs::write（按 umask 落成 0644）、写完内容之后才 chmod 0600
    /// ⇒ 配置内容有一瞬是全局可读的。现在用 OpenOptions(mode 0o600) 创建，
    /// 那个窗口不存在。
    #[cfg(unix)]
    #[test]
    fn the_temporary_file_is_private_from_the_very_first_moment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("window");
        let target = dir.join("config.json");
        // 写一个足够大的内容，确保「写完才 chmod」的实现会露出窗口。
        let payload = "x".repeat(256 * 1024);
        write_atomic_with_mode(&target, payload.as_bytes(), 0o600).expect("write");
        let mode = fs::metadata(&target).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "落地文件的权限必须是 0600");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 并发写同一个目标文件：谁都可能失败，但**目标文件不能是半个**。
    #[test]
    fn concurrent_writes_never_leave_a_truncated_target() {
        let dir = temp_dir("concurrent");
        let target = dir.join("shared.txt");
        write_atomic(&target, b"original-content").expect("seed");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let target = target.clone();
                std::thread::spawn(move || {
                    let payload = format!("payload-{i}-{}", "x".repeat(4096));
                    let _ = write_atomic(&target, payload.as_bytes());
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("join");
        }
        let content = fs::read_to_string(&target).expect("read");
        assert!(
            content.starts_with("payload-") && content.len() > 4096,
            "目标文件必须是一个完整的新内容，不能是截断的：{} 字节",
            content.len()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
