//! 原子写盘：先写临时文件，再改名。
//!
//! 为什么需要：直接 `fs::write` 会在写一半时留下**半个文件** ——
//! 断电、磁盘满、进程被杀都可能留下截断的 YAML/文本，
//! 下次启动读到的就是坏配置。改名是原子的：要么还是旧文件，要么是完整的新文件。

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

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
    let tmp = parent.join(format!("{name}.rimesync-tmp"));
    if let Err(error) = fs::write(&tmp, bytes) {
        // 写了一半也要清掉：半个临时文件留在用户目录里比没有更糟。
        let _ = fs::remove_file(&tmp);
        return Err(error).with_context(|| format!("write {}", tmp.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match mode {
            Some(mode) => {
                let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
            }
            // 覆盖已有文件时保留原权限：用户的 custom.yaml 可能是 600（私有），
            // 改名换文件会把权限悄悄放宽成默认的 644。
            None => {
                if let Ok(meta) = fs::metadata(path) {
                    let mode = meta.permissions().mode();
                    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
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
    use super::{write_atomic, write_atomic_with_mode};
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
}
