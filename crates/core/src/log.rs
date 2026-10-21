//! 本地日志：单文件 5 MB，保留 3 份滚动（log.txt → log.1.txt → … → log.3.txt）。
//!
//! 轮转失败绝不向上传播：日志写不进去不能中断同步。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_KEEP: usize = 3;

/// 按大小阈值滚动的日志文件。path 通常是 <state_dir>/log.txt。
pub struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    /// 串行化同一进程内的多次写入，避免阈值判断与落盘交错。
    lock: Mutex<()>,
}

impl RotatingLog {
    pub fn new(path: PathBuf, max_bytes: u64, keep: usize) -> Self {
        Self {
            path,
            max_bytes: max_bytes.max(1),
            keep: keep.max(1),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一行（自动补换行）。轮转失败只返回错误，由调用方决定是否忽略。
    pub fn append_line(&self, line: &str) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        self.write_bytes(&bytes)
    }

    /// 追加任意字节块；写入前按阈值滚动。
    pub fn write_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let size = fs::metadata(&self.path)
            .ok()
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len())
            .unwrap_or(0);
        if size > 0 && size.saturating_add(bytes.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(bytes)
    }

    /// log.txt → log.1.txt → log.2.txt → log.3.txt；最旧一份丢弃。
    fn rotate(&self) -> io::Result<()> {
        for index in (1..=self.keep).rev() {
            let from = if index == 1 {
                self.path.clone()
            } else {
                self.rotated(index - 1)?
            };
            let to = self.rotated(index)?;
            if !from.is_file() {
                continue;
            }
            if to.exists() {
                fs::remove_file(&to)?;
            }
            fs::rename(&from, &to)?;
        }
        Ok(())
    }

    fn rotated(&self, index: usize) -> io::Result<PathBuf> {
        let stem = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "log path has no file name")
            })?;
        let extension = self
            .path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("log");
        Ok(self
            .path
            .with_file_name(format!("{stem}.{index}.{extension}")))
    }
}

/// 供 tracing_subscriber 使用的 writer：错误一律吞掉（日志不能反过来打断同步）。
#[derive(Clone)]
pub struct LogWriter {
    inner: Arc<RotatingLog>,
}

impl LogWriter {
    pub fn new(log: Arc<RotatingLog>) -> Self {
        Self { inner: log }
    }
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.inner.write_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_log(tag: &str, max_bytes: u64) -> (PathBuf, PathBuf) {
        let _ = max_bytes;
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-log-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("log.txt");
        (dir, path)
    }

    #[test]
    fn rotates_three_backups_and_drops_oldest() {
        let (dir, path) = temp_log("rotate", 16);
        let log = RotatingLog::new(path.clone(), 16, DEFAULT_KEEP);
        for index in 0..5 {
            log.append_line(&format!("line-{index}-pad"))
                .expect("append");
        }
        // log.txt + log.1..3，共 4 个文件；更旧的已被丢弃。
        let mut names: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["log.1.txt", "log.2.txt", "log.3.txt", "log.txt"]
        );
        let current = fs::read_to_string(&path).expect("current");
        assert!(current.starts_with("line-4"), "{current}");
        let oldest = fs::read_to_string(dir.join("log.3.txt")).expect("oldest");
        assert!(oldest.starts_with("line-1"), "{oldest}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn small_writes_stay_in_one_file() {
        let (dir, path) = temp_log("single", 1024);
        let log = RotatingLog::new(path.clone(), 1024, DEFAULT_KEEP);
        log.append_line("a").expect("a");
        log.append_line("b").expect("b");
        assert_eq!(fs::read_to_string(&path).expect("read"), "a\nb\n");
        assert!(!dir.join("log.1.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_errors_are_reported_but_never_panic() {
        let (dir, _path) = temp_log("broken", 16);
        // 父路径是普通文件 → 建目录与写文件都必然失败。
        fs::write(dir.join("blocked"), b"not a dir").expect("block parent");
        let log = RotatingLog::new(dir.join("blocked").join("log.txt"), 16, DEFAULT_KEEP);
        assert!(log.append_line("x").is_err());
        // LogWriter 吞掉错误，绝不 panic，也不向调用方报错。
        let mut writer = LogWriter::new(Arc::new(log));
        assert!(writer.write_all(b"y").is_ok());
        fs::remove_dir_all(&dir).ok();
    }
}
