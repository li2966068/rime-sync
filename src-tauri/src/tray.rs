//! 托盘常驻（SPEC-4 §B2）。
//!
//! 菜单动作只调用壳层已有的 run_sync / 窗口操作，不新增业务实现。
//! headless 或缺少 appindicator 时托盘创建会失败：此时只记日志并降级
//! （关闭窗口=真正退出），绝不让托盘错误把进程带崩。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rime_sync_core::engine::Report;
use serde::Serialize;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::{push_log, run_sync, AppState, SyncMode};

/// 托盘是否真的建起来了。关窗隐藏必须先看这个标志。
static READY: AtomicBool = AtomicBool::new(false);

pub(crate) fn ready() -> bool {
    READY.load(Ordering::SeqCst)
}

#[derive(Serialize)]
pub struct TrayStatus {
    pub ready: bool,
    pub menu: Vec<&'static str>,
}

/// 冒烟/诊断用：托盘是否真的建起来，以及菜单项（headless 也能核对）。
#[tauri::command]
pub fn cmd_tray_status() -> TrayStatus {
    TrayStatus {
        ready: ready(),
        menu: vec!["open", "transport", "apply", "conflicts", "quit"],
    }
}

pub(crate) fn setup(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开主窗口", true, None::<&str>)?;
    let transport = MenuItem::with_id(app, "transport", "立即搬运", true, None::<&str>)?;
    let apply = MenuItem::with_id(app, "apply", "立即合并生效", true, None::<&str>)?;
    let conflicts = MenuItem::with_id(app, "conflicts", "查看冲突", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &transport, &apply, &conflicts, &separator, &quit],
    )?;

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;
    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("Rime 同步")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    READY.store(true, Ordering::SeqCst);
    Ok(())
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_main(app),
        "transport" => spawn_sync(app, SyncMode::Transport),
        "apply" => spawn_sync(app, SyncMode::Apply),
        "conflicts" => {
            show_main(app);
            // 前端按需监听；没有监听方时 emit 只是空操作。
            let _ = app.emit(
                "tray://action",
                serde_json::json!({ "action": "conflicts" }),
            );
            push_log(&app.state::<AppState>().0, "托盘：切换到冲突页");
        }
        "quit" => {
            push_log(&app.state::<AppState>().0, "托盘：用户请求退出");
            app.exit(0);
        }
        _ => {}
    }
}

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 同步是阻塞流程（reqwest::blocking 自带 runtime），必须离开 UI 线程。
///
/// 失败必须让用户看见：从托盘触发的同步没有界面在等结果，
/// 只写日志的话用户只看到「点了没反应」。所以失败时也发 `sync://done`
/// （ok=false + error），前端收到后会弹提示。
/// 成功的路径由 run_sync_inner 自己发 `sync://done`，这里不重复发。
fn spawn_sync(app: &AppHandle, mode: SyncMode) {
    let app = app.clone();
    std::thread::spawn(move || {
        let inner = Arc::clone(&app.state::<AppState>().0);
        if let Err(error) = run_sync(&app, &inner, mode) {
            push_log(&inner, format!("托盘任务失败：{error}"));
            let _ = app.emit("sync://done", &failure_report(mode, &error));
        }
    });
}

/// 托盘同步失败时发给前端的报告（纯函数，方便单测钉住字段）。
///
/// 前端只要看到 ok=false 就弹提示并把错误显示出来；
/// error 里带原始错误文本，mode 让用户知道是搬运还是合并生效失败。
pub(crate) fn failure_report(mode: SyncMode, error: &str) -> Report {
    Report {
        ok: false,
        error: Some(error.to_string()),
        mode: mode_label(mode).to_string(),
        ..Report::default()
    }
}

/// 同步模式的短标签（写进失败报告里，前端能看出是搬运还是合并生效失败）。
fn mode_label(mode: SyncMode) -> &'static str {
    match mode {
        SyncMode::Transport => "transport",
        SyncMode::Apply => "apply",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 托盘同步失败时发给前端的报告：ok=false + 原始错误 + 模式。
    ///
    /// 前端收到 `sync://done` 且 ok=false 就会弹提示 —— 托盘触发的同步
    /// 没有界面在等结果，不发这个事件用户就只看到「点了没反应」。
    /// （emit 本身需要 AppHandle，headless 单测跑不了；这里钉住的是事件载荷。）
    #[test]
    fn failure_report_marks_the_sync_as_failed() {
        let report = failure_report(
            SyncMode::Transport,
            "已有同步任务正在运行，请等待完成或先取消",
        );
        assert!(!report.ok, "失败的报告必须是 ok=false");
        assert_eq!(
            report.error.as_deref(),
            Some("已有同步任务正在运行，请等待完成或先取消")
        );
        assert_eq!(report.mode, "transport");
        assert_eq!(report.pulled, 0);
        assert_eq!(report.pushed, 0);

        let apply = failure_report(SyncMode::Apply, "网络不通");
        assert_eq!(apply.mode, "apply");
        assert_eq!(apply.error.as_deref(), Some("网络不通"));
    }
}
