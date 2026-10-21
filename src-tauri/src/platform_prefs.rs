//! 窗口行为偏好（SPEC-4 §B2 的 close_to_tray）。
//!
//! core 的 Config 只管同步语义，且 crates/** 不属于桌面壳；
//! 因此壳层偏好单独存 platform.json，与 config.json 同目录。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::{push_log, state_dir, AppState, Inner};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformPrefs {
    pub close_to_tray: bool,
}

impl Default for PlatformPrefs {
    fn default() -> Self {
        Self {
            close_to_tray: true,
        }
    }
}

fn prefs_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("platform.json"))
}

pub(crate) fn load(app: &AppHandle) -> Result<PlatformPrefs, String> {
    let path = prefs_path(app)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("解析 {} 失败：{error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PlatformPrefs::default()),
        Err(error) => Err(format!("读取 {} 失败：{error}", path.display())),
    }
}

fn save(app: &AppHandle, prefs: &PlatformPrefs) -> Result<(), String> {
    let path = prefs_path(app)?;
    let text = serde_json::to_string_pretty(prefs)
        .map_err(|error| format!("序列化窗口偏好失败：{error}"))?;
    std::fs::write(&path, text).map_err(|error| format!("写入 {} 失败：{error}", path.display()))
}

/// 关窗是否隐藏。托盘没建起来时必须放行关闭：
/// 否则应用会变成既没窗口也没托盘的僵尸进程。
pub(crate) fn should_hide_on_close(app: &AppHandle, inner: &Inner) -> bool {
    if !crate::tray::ready() {
        return false;
    }
    match load(app) {
        Ok(prefs) => prefs.close_to_tray,
        Err(error) => {
            push_log(inner, format!("读取窗口偏好失败（按默认隐藏处理）：{error}"));
            true
        }
    }
}

#[tauri::command]
pub fn cmd_get_close_to_tray(app: AppHandle) -> Result<bool, String> {
    Ok(load(&app)?.close_to_tray)
}

#[tauri::command]
pub fn cmd_set_close_to_tray(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    save(
        &app,
        &PlatformPrefs {
            close_to_tray: enabled,
        },
    )?;
    push_log(
        &state.0,
        format!(
            "关闭窗口时{}",
            if enabled {
                "隐藏到托盘"
            } else {
                "退出应用"
            }
        ),
    );
    Ok(enabled)
}
