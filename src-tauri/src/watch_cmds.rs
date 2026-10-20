//! 文件监听：把 core::watch 的去抖事件转成 Tauri 事件，并可选地自动搬运。
//!
//! WatchHandle 的 Receiver 不能克隆，所以句柄留在线程里，
//! 外部只持有停止标志、运行标志和最近事件时间。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::Duration;

use rime_sync_core::watch;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::{push_log, rime_dir_for, run_sync, AppState, Inner, SyncMode};

pub(crate) struct WatchSession {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) running: Arc<AtomicBool>,
    pub(crate) last_event_at: Arc<AtomicU64>,
}

#[derive(Serialize)]
pub struct WatchStatus {
    pub running: bool,
    pub last_event_at: Option<u64>,
}

fn status_of(inner: &Inner) -> WatchStatus {
    let Ok(guard) = inner.watch.lock() else {
        return WatchStatus {
            running: false,
            last_event_at: None,
        };
    };
    match guard.as_ref() {
        Some(session) if session.running.load(Ordering::SeqCst) => WatchStatus {
            running: true,
            last_event_at: match session.last_event_at.load(Ordering::SeqCst) {
                0 => None,
                value => Some(value),
            },
        },
        _ => WatchStatus {
            running: false,
            last_event_at: None,
        },
    }
}

#[tauri::command]
pub fn cmd_watch_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<WatchStatus, String> {
    {
        let guard = state.0.watch.lock().map_err(|_| "监听状态不可用".to_string())?;
        if let Some(session) = guard.as_ref() {
            if session.running.load(Ordering::SeqCst) {
                return Ok(status_of(&state.0));
            }
        }
    }
    let rime = rime_dir_for(&app, &state.0)?;
    let handle = watch::start(&rime, watch::DEFAULT_DEBOUNCE)
        .map_err(|error| format!("启动文件监听失败：{error:#}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let last_event_at = Arc::new(AtomicU64::new(0));
    let thread_stop = Arc::clone(&stop);
    let thread_running = Arc::clone(&running);
    let thread_last = Arc::clone(&last_event_at);
    let thread_app = app.clone();
    let thread_inner = Arc::clone(&state.0);
    std::thread::Builder::new()
        .name("rime-sync-watch-ui".to_string())
        .spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match handle.rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(event) => {
                        thread_last.store(event.at, Ordering::SeqCst);
                        let kind = event
                            .kinds
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "modify".to_string());
                        for path in event.paths.iter().take(32) {
                            let _ = thread_app.emit(
                                "sync://watch",
                                serde_json::json!({
                                    "kind": kind,
                                    "path": path,
                                    "at": event.at,
                                }),
                            );
                        }
                        push_log(
                            &thread_inner,
                            format!("文件变化：{}", watch::event_summary(&event)),
                        );
                        run_watched_transport(&thread_app, &thread_inner);
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            thread_running.store(false, Ordering::SeqCst);
            drop(handle);
        })
        .map_err(|error| format!("启动监听线程失败：{error}"))?;

    if let Ok(mut guard) = state.0.watch.lock() {
        *guard = Some(WatchSession {
            stop,
            running,
            last_event_at,
        });
    }
    push_log(&state.0, "已开启实时同步监听");
    Ok(status_of(&state.0))
}

fn run_watched_transport(app: &AppHandle, inner: &Arc<Inner>) {
    if inner.running.load(Ordering::SeqCst) {
        push_log(inner, "已有同步任务在运行，跳过本次自动搬运");
        return;
    }
    if let Err(error) = run_sync(app, inner, SyncMode::Transport) {
        push_log(inner, format!("自动搬运失败：{error}"));
    }
}

#[tauri::command]
pub fn cmd_watch_stop(state: State<'_, AppState>) -> Result<WatchStatus, String> {
    if let Ok(mut guard) = state.0.watch.lock() {
        if let Some(session) = guard.take() {
            session.stop.store(true, Ordering::SeqCst);
            session.running.store(false, Ordering::SeqCst);
            push_log(&state.0, "已关闭实时同步监听");
        }
    }
    Ok(WatchStatus {
        running: false,
        last_event_at: None,
    })
}

#[tauri::command]
pub fn cmd_watch_status(state: State<'_, AppState>) -> Result<WatchStatus, String> {
    Ok(status_of(&state.0))
}
