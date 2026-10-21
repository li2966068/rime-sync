//! 开机自启（SPEC-4 §B3）：薄封装 tauri-plugin-autostart。
//!
//! Linux 由插件写 ~/.config/autostart/*.desktop，Windows 写 HKCU Run。

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::{push_log, AppState};

#[derive(Serialize)]
pub struct AutostartView {
    pub enabled: bool,
    pub supported: bool,
    pub note: Option<String>,
}

#[tauri::command]
pub fn cmd_get_autostart(app: AppHandle) -> Result<AutostartView, String> {
    match app.autolaunch().is_enabled() {
        Ok(enabled) => Ok(AutostartView {
            enabled,
            supported: true,
            note: None,
        }),
        // 查询失败（例如 Linux 下读不到 autostart 目录）按“未开启 + 不支持”上报，
        // 让前端禁用开关而不是弹一个无法恢复的错误。
        Err(error) => Ok(AutostartView {
            enabled: false,
            supported: false,
            note: Some(format!("查询开机自启失败：{error}")),
        }),
    }
}

#[tauri::command]
pub fn cmd_set_autostart(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    let manager = app.autolaunch();
    let outcome = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    match outcome {
        Ok(()) => {
            push_log(
                &state.0,
                format!("开机自启已{}", if enabled { "开启" } else { "关闭" }),
            );
            Ok(enabled)
        }
        Err(error) => Err(format!("设置开机自启失败：{error}")),
    }
}
