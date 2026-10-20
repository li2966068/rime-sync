//! 同步锁与残留锁回收。
//!
//! 锁文件是 JSON：{pid, hostname, started_at, boot_id}。持有者崩溃后锁会残留，
//! 因此 acquire 会判定 stale 并接管：pid 不存活 / 同 pid 但 started_at 对不上 /
//! boot_id 变化 / 超过 STALE_AFTER 兜底超时。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 兜底超时：任何无法判定的情况，超过 6h 一律视为残留。
pub const STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);
/// /proc/<pid>/stat 的 starttime 单位是 USER_HZ，Linux ABI 固定为 100。
const USER_HZ: u64 = 100;
const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: u32,
    pub hostname: String,
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

impl LockInfo {
    pub fn current() -> Self {
        Self {
            pid: std::process::id(),
            hostname: hostname(),
            started_at: process_start_epoch(std::process::id()).unwrap_or_else(now_secs),
            boot_id: boot_id(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LockStatus {
    pub path: String,
    pub held: bool,
    pub stale: bool,
    pub info: Option<LockInfo>,
    pub age_secs: u64,
    pub reason: Option<String>,
}

#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    token: LockInfo,
}

impl LockGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // 只删除仍是自己写的那份锁，避免误删别人接管的锁。
        if let Ok(text) = fs::read_to_string(&self.path) {
            if let Ok(existing) = serde_json::from_str::<LockInfo>(&text) {
                if existing.pid == self.token.pid && existing.started_at == self.token.started_at {
                    if let Err(error) = fs::remove_file(&self.path) {
                        // Drop 里不能 panic，也不能把失败当错误向上抛。
                        tracing::warn!("release lock {}: {error}", self.path.display());
                    }
                }
            }
        }
    }
}

/// 独占获取；发现残留锁则接管。
pub fn acquire(path: &Path) -> Result<LockGuard> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
    }
    for _ in 0..MAX_ATTEMPTS {
        match try_create(path) {
            Ok(guard) => return Ok(guard),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let status = inspect(path);
                if status.stale {
                    tracing::warn!(
                        "reclaiming stale sync lock {}: {}",
                        path.display(),
                        status.reason.as_deref().unwrap_or("unknown")
                    );
                    match fs::remove_file(path) {
                        Ok(()) => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error)
                                .with_context(|| format!("remove stale lock {}", path.display()))
                        }
                    }
                }
                let holder = status
                    .info
                    .as_ref()
                    .map(|info| format!("pid={} host={}", info.pid, info.hostname))
                    .unwrap_or_else(|| "unknown holder".to_string());
                bail!(
                    "another sync is running ({holder}); lock file: {}",
                    path.display()
                );
            }
            Err(error) => {
                return Err(error).with_context(|| format!("create lock {}", path.display()))
            }
        }
    }
    bail!("could not acquire lock {} after retries", path.display())
}

fn try_create(path: &Path) -> std::io::Result<LockGuard> {
    let token = LockInfo::current();
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let text = serde_json::to_string_pretty(&token).unwrap_or_else(|_| "{}".to_string());
    file.write_all(format!("{text}\n").as_bytes())?;
    file.sync_all()?;
    Ok(LockGuard {
        path: path.to_path_buf(),
        token,
    })
}

/// 只读检查，任何异常都按“不可信 → 可接管”处理，绝不报错。
pub fn inspect(path: &Path) -> LockStatus {
    let display = path.display().to_string();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LockStatus {
                path: display,
                held: false,
                stale: false,
                info: None,
                age_secs: 0,
                reason: None,
            }
        }
        Err(error) => {
            return LockStatus {
                path: display,
                held: true,
                stale: true,
                info: None,
                age_secs: 0,
                reason: Some(format!("unreadable lock file: {error}")),
            }
        }
    };
    let info = match serde_json::from_str::<LockInfo>(&text) {
        Ok(info) => info,
        Err(error) => {
            return LockStatus {
                path: display,
                held: true,
                stale: true,
                info: None,
                age_secs: 0,
                reason: Some(format!("corrupted lock file: {error}")),
            }
        }
    };
    let now = now_secs();
    let age_secs = now.saturating_sub(info.started_at);
    let (stale, reason) = judge(&info, now);
    LockStatus {
        path: display,
        held: !stale,
        stale,
        info: Some(info),
        age_secs,
        reason,
    }
}

fn judge(info: &LockInfo, now: u64) -> (bool, Option<String>) {
    if let Some(current_boot) = boot_id() {
        if let Some(recorded) = info.boot_id.as_deref() {
            if recorded != current_boot {
                return (true, Some("boot_id changed (machine rebooted)".to_string()));
            }
        }
    }
    if now.saturating_sub(info.started_at) > STALE_AFTER.as_secs() {
        return (
            true,
            Some(format!("lock older than {}h", STALE_AFTER.as_secs() / 3600)),
        );
    }
    if info.hostname != hostname() {
        // 别的机器留下的锁：本进程无法验 pid，只能靠兜底超时。
        return (false, None);
    }
    if !pid_alive(info.pid) {
        return (true, Some(format!("pid {} is not alive", info.pid)));
    }
    match process_start_epoch(info.pid) {
        Some(started_at) if started_at.abs_diff(info.started_at) > 2 => (
            true,
            Some(format!("pid {} was reused (started_at mismatch)", info.pid)),
        ),
        _ => (false, None),
    }
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows/其它平台没有 std 级别的 pid 探测手段，交给 6h 兜底超时。
        true
    }
}

/// 进程启动时刻（epoch 秒）。Linux 读 /proc；其它平台返回 None（走兜底超时）。
pub fn process_start_epoch(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm 字段可能含空格与括号，按最后一个 ')' 切分。
        let rest = stat.rsplit_once(')')?.1;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // rest 从 state 开始，starttime 是第 22 个字段 → 下标 19。
        let ticks: u64 = fields.get(19)?.parse().ok()?;
        let boot = boot_time_epoch()?;
        Some(boot + ticks / USER_HZ)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn boot_time_epoch() -> Option<u64> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("btime ") {
            return value.trim().parse().ok();
        }
    }
    None
}

pub fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            fs::read_to_string("/etc/hostname")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn temp_lock(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-lock-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir.join("sync.lock")
    }

    fn write_lock(path: &Path, info: &LockInfo) {
        fs::write(path, serde_json::to_string_pretty(info).expect("json")).expect("write");
    }

    #[test]
    fn acquire_then_release_roundtrip() {
        let path = temp_lock("roundtrip");
        {
            let guard = acquire(&path).expect("acquire");
            assert!(path.is_file());
            assert_eq!(guard.path(), path.as_path());
            let status = inspect(&path);
            assert!(status.held && !status.stale);
            assert_eq!(status.info.expect("info").pid, std::process::id());
        }
        assert!(!path.exists());
        let _ = fs::remove_dir_all(path.parent().unwrap_or(Path::new("/tmp")));
    }

    #[test]
    fn second_acquire_is_refused_while_held() {
        let path = temp_lock("held");
        let _guard = acquire(&path).expect("acquire");
        let error = acquire(&path).expect_err("must refuse");
        assert!(format!("{error:#}").contains("another sync is running"));
    }

    #[test]
    fn dead_pid_lock_is_reclaimed() {
        let path = temp_lock("deadpid");
        // 伪造一个必然不存在的 pid。
        write_lock(
            &path,
            &LockInfo {
                pid: 4_000_000,
                hostname: hostname(),
                started_at: now_secs(),
                boot_id: boot_id(),
            },
        );
        let status = inspect(&path);
        assert!(status.stale, "dead pid should be stale: {:?}", status);
        let _guard = acquire(&path).expect("reclaim");
        let status = inspect(&path);
        assert!(status.held);
        assert_eq!(status.info.expect("info").pid, std::process::id());
    }

    #[test]
    fn corrupted_lock_file_is_reclaimed() {
        let path = temp_lock("corrupt");
        fs::write(&path, b"not json").expect("write");
        assert!(inspect(&path).stale);
        let _guard = acquire(&path).expect("reclaim");
        assert!(inspect(&path).held);
    }

    #[test]
    fn expired_lock_is_reclaimed_even_with_live_pid() {
        let path = temp_lock("expired");
        write_lock(
            &path,
            &LockInfo {
                pid: std::process::id(),
                hostname: hostname(),
                started_at: now_secs() - STALE_AFTER.as_secs() - 60,
                boot_id: boot_id(),
            },
        );
        let status = inspect(&path);
        assert!(status.stale);
        assert!(status
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("older than"));
    }

    #[test]
    fn reused_pid_started_at_mismatch_is_stale() {
        let path = temp_lock("reused");
        write_lock(
            &path,
            &LockInfo {
                pid: std::process::id(),
                hostname: hostname(),
                started_at: now_secs(),
                boot_id: boot_id(),
            },
        );
        // 当前进程的 started_at 与记录值相差 > 2s 时判为 pid 复用。
        assert!(process_start_epoch(std::process::id()).is_some());
        write_lock(
            &path,
            &LockInfo {
                pid: std::process::id(),
                hostname: hostname(),
                started_at: now_secs().saturating_sub(120),
                boot_id: boot_id(),
            },
        );
        let status = inspect(&path);
        assert!(status.stale, "pid reuse should be stale: {:?}", status);
    }

    #[test]
    fn missing_lock_reports_not_held() {
        let path = temp_lock("missing");
        let status = inspect(&path);
        assert!(!status.held && !status.stale && status.info.is_none());
    }

    #[test]
    fn inspect_is_fast_and_pure() {
        let path = temp_lock("fast");
        let _guard = acquire(&path).expect("acquire");
        let started = Instant::now();
        let _ = inspect(&path);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
