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
    // 原子写：先写同目录临时文件再改名。
    // 以前是直接 fs::write ⇒ 断电/进程被杀会留下**半个** JSON，
    // 下次启动解析失败 ⇒ 用户在设置里选的「关闭窗口时退出」被静默变回「隐藏到托盘」，
    // 而且界面上看不出任何异常。
    rime_sync_core::atomic::write_atomic(&path, text.as_bytes())
        .map_err(|error| format!("写入 {} 失败：{error:#}", path.display()))
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
            // 读不出来（文件坏了/没权限）时**放行关闭**：隐藏到托盘一旦托盘也没起来，
            // 用户就既没窗口也没托盘，进程成了僵尸。宁可这次窗口直接关掉。
            push_log(
                inner,
                format!("读取窗口偏好失败（按关闭窗口即退出处理）：{error}"),
            );
            false
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
