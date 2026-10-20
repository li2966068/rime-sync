//! 事件驱动：基于 notify 的目录监听 + 去抖 + 写入期静音窗口。
//!
//! 只发事件，不直接调用 engine——避免与手动操作抢锁。
//!
//! 回声坑：engine 自己会写 sync/ 与冲突副本，若不加过滤就会 watcher →
//! transport → 写文件 → watcher 的死循环。这里用两道闸：
//! 1) 路径过滤（build/、*.userdb/、*.bin、sync/ 一律忽略）；
//! 2) 静音窗口（调用方在跑 transport 前调用 mute()，窗口内的事件直接丢弃）。

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher as NotifyWatcher};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
pub struct WatchEvent {
    pub paths: Vec<String>,
    pub kinds: Vec<String>,
    pub at: u64,
}

pub struct WatchHandle {
    pub rx: Receiver<WatchEvent>,
    mute_until_ms: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WatchHandle {
    /// 在窗口内丢弃已去抖的事件；用于 transport 写盘期间的自我保护。
    pub fn mute(&self, window: Duration) {
        let until = now_ms().saturating_add(window.as_millis() as u64);
        self.mute_until_ms.store(until, Ordering::SeqCst);
    }

    pub fn is_muted(&self) -> bool {
        now_ms() < self.mute_until_ms.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop();
        // 线程最多在 debounce 之后自行退出；这里 join 会阻塞调用方，故 detach。
        if let Some(thread) = self.thread.take() {
            drop(thread);
        }
    }
}

/// 启动监听：返回句柄，句柄内 rx 收去抖后的事件。
pub fn start(rime_dir: &Path, debounce: Duration) -> Result<WatchHandle> {
    let root = rime_dir.to_path_buf();
    if !root.is_dir() {
        anyhow::bail!("watch target is not a directory: {}", root.display());
    }
    let (raw_tx, raw_rx) = channel::<(String, String)>();
    let (ready_tx, ready_rx) = channel::<Result<()>>();
    let (event_tx, event_rx) = channel::<WatchEvent>();
    let mute_until_ms = Arc::new(AtomicU64::new(0));
    let running = Arc::new(AtomicBool::new(true));

    let thread_running = Arc::clone(&running);
    let thread_mute = Arc::clone(&mute_until_ms);
    let thread_root = root.clone();
    let thread = std::thread::Builder::new()
        .name("rime-sync-watch".to_string())
        .spawn(move || {
            let setup = install_watcher(&thread_root, raw_tx.clone());
            match setup {
                Ok(watcher) => {
                    let _ = ready_tx.send(Ok(()));
                    run_loop(raw_rx, event_tx, thread_mute, thread_running, debounce);
                    drop(watcher);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        })
        .context("spawn watch thread")?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(WatchHandle {
            rx: event_rx,
            mute_until_ms,
            running,
            thread: Some(thread),
        }),
        Ok(Err(error)) => Err(error),
        Err(error) => Err(anyhow::anyhow!("watch thread died during setup: {error}")),
    }
}

fn install_watcher(
    root: &Path,
    tx: Sender<(String, String)>,
) -> Result<notify::RecommendedWatcher> {
    let filter_root = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let Some(kind) = kind_label(&event.kind) else {
            return;
        };
        for path in &event.paths {
            if let Some(rel) = relative_path(&filter_root, path) {
                if should_watch(&rel) {
                    let _ = tx.send((rel, kind.to_string()));
                }
            }
        }
    })
    .context("create notify watcher")?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("watch {}", root.display()))?;
    Ok(watcher)
}

fn run_loop(
    raw_rx: Receiver<(String, String)>,
    event_tx: Sender<WatchEvent>,
    mute_until_ms: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    debounce: Duration,
) {
    let mut pending: BTreeSet<String> = BTreeSet::new();
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    let mut first_seen: Option<Instant> = None;

    while running.load(Ordering::SeqCst) {
        match raw_rx.recv_timeout(debounce) {
            Ok((path, kind)) => {
                pending.insert(path);
                kinds.insert(kind);
                if first_seen.is_none() {
                    first_seen = Some(Instant::now());
                }
                // 事件持续到达时也不能无限推迟：首个事件超过 debounce 就 flush。
                if let Some(started) = first_seen {
                    if started.elapsed() >= debounce {
                        flush(&mut pending, &mut kinds, &event_tx, &mute_until_ms);
                        first_seen = None;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !pending.is_empty() {
                    flush(&mut pending, &mut kinds, &event_tx, &mute_until_ms);
                }
                first_seen = None;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn flush(
    pending: &mut BTreeSet<String>,
    kinds: &mut BTreeSet<String>,
    event_tx: &Sender<WatchEvent>,
    mute_until_ms: &AtomicU64,
) {
    if pending.is_empty() {
        return;
    }
    let muted = now_ms() < mute_until_ms.load(Ordering::SeqCst);
    let event = WatchEvent {
        paths: pending.iter().cloned().collect(),
        kinds: kinds.iter().cloned().collect(),
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0),
    };
    pending.clear();
    kinds.clear();
    if muted {
        tracing::debug!("watch event suppressed by mute window");
        return;
    }
    let _ = event_tx.send(event);
}

fn kind_label(kind: &EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Create(_) => Some("create"),
        EventKind::Modify(_) => Some("modify"),
        EventKind::Remove(_) => Some("remove"),
        _ => None,
    }
}

fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let rel = relative.to_string_lossy().replace('\\', "/");
    if rel.is_empty() {
        None
    } else {
        Some(rel)
    }
}

/// 路径过滤：build/、*.userdb/、.rime-sync/、sync/、*.bin、*.gram 与临时文件。
pub fn should_watch(rel: &str) -> bool {
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return false;
    }
    let components: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    let Some(name) = components.last().copied() else {
        return false;
    };
    for component in &components {
        if *component == "build"
            || *component == "sync"
            || *component == ".rime-sync"
            || component.starts_with('.')
            || component.ends_with(".userdb")
        {
            return false;
        }
    }
    if name.ends_with(".bin")
        || name.ends_with(".gram")
        || name.ends_with(".tmp")
        || name.ends_with(".swp")
        || name.ends_with('~')
    {
        return false;
    }
    true
}

/// 把 rime_dir 下的相对路径转成「值得触发一次同步」的判断（给 CLI 复用）。
pub fn event_summary(event: &WatchEvent) -> String {
    let kinds = event.kinds.join(",");
    let sample = event
        .paths
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if event.paths.len() > 3 {
        format!("{kinds}: {sample} (+{} more)", event.paths.len() - 3)
    } else {
        format!("{kinds}: {sample}")
    }
}

/// 监听全部 rime 目录（kind 过滤在 should_watch 里做）。
pub fn watch_root(rime_dir: &Path) -> PathBuf {
    rime_dir.to_path_buf()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-watch-{}-{}-{}",
            tag,
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn filters_engine_artifacts_and_own_writes() {
        assert!(!should_watch("build/main.bin"));
        assert!(!should_watch("build/main.table.bin"));
        assert!(!should_watch("main.userdb/CURRENT"));
        assert!(!should_watch("nested/foo.userdb/data/LOG"));
        assert!(!should_watch("sync/devA/main.userdb.txt"));
        assert!(!should_watch("main.bin"));
        assert!(!should_watch(".rime-sync/index.json"));
        assert!(!should_watch("x.tmp"));
        assert!(!should_watch(""));
        assert!(should_watch("default.custom.yaml"));
        assert!(should_watch("rime_ice.schema.yaml"));
        assert!(should_watch("words.dict.yaml"));
        assert!(should_watch("sub/dir/user.yaml"));
    }

    #[test]
    fn mute_window_suppresses_until_expired() {
        let dir = temp_dir("mute");
        let handle = start(&dir, Duration::from_millis(50)).expect("start");
        assert!(!handle.is_muted());
        handle.mute(Duration::from_millis(80));
        assert!(handle.is_muted());
        std::thread::sleep(Duration::from_millis(120));
        assert!(!handle.is_muted());
        drop(handle);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_rejects_missing_directory() {
        let path = std::env::temp_dir().join("rime-sync-watch-definitely-missing");
        assert!(start(&path, Duration::from_millis(50)).is_err());
    }

    #[test]
    fn emits_debounced_event_for_real_change() {
        let dir = temp_dir("live");
        let handle = start(&dir, Duration::from_millis(120)).expect("start");
        fs::write(dir.join("default.custom.yaml"), b"patch:\n").expect("write");
        let event = handle
            .rx
            .recv_timeout(Duration::from_secs(5))
            .expect("watch event");
        assert!(event
            .paths
            .iter()
            .any(|path| path.ends_with("default.custom.yaml")));
        assert!(!event.kinds.is_empty());
        assert!(event.at > 0);
        assert!(event_summary(&event).contains("default.custom.yaml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_sync_tree_writes() {
        let dir = temp_dir("synctree");
        fs::create_dir_all(dir.join("sync/devA")).expect("mkdir sync");
        let handle = start(&dir, Duration::from_millis(80)).expect("start");
        fs::write(dir.join("sync/devA/main.userdb.txt"), b"snapshot").expect("write");
        assert!(handle.rx.recv_timeout(Duration::from_millis(600)).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
