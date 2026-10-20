//! 自动更新：薄转发 tauri-plugin-updater。
//!
//! 端点与公钥来自 tauri.conf.json；本模块只负责把结果/错误文本化。

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

#[derive(Serialize)]
pub struct UpdateView {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Serialize)]
pub struct InstallView {
    pub installed: bool,
    pub version: String,
    pub message: String,
}

#[tauri::command]
pub async fn cmd_check_update(app: AppHandle) -> Result<UpdateView, String> {
    let current = app.package_info().version.to_string();
    let updater = app
        .updater()
        .map_err(|error| format!("更新器不可用：{error}"))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(UpdateView {
            available: true,
            current_version: update.current_version.clone(),
            version: Some(update.version.clone()),
            notes: update.body.clone(),
            date: update.date.map(|date| date.to_string()),
        }),
        Ok(None) => Ok(UpdateView {
            available: false,
            current_version: current,
            version: None,
            notes: None,
            date: None,
        }),
        Err(error) => Err(format!("检查更新失败：{error}")),
    }
}

#[tauri::command]
pub async fn cmd_install_update(app: AppHandle) -> Result<InstallView, String> {
    let updater = app
        .updater()
        .map_err(|error| format!("更新器不可用：{error}"))?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|error| format!("检查更新失败：{error}"))?
    else {
        return Err("当前已是最新版本".to_string());
    };
    let version = update.version.clone();
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|error| format!("安装更新失败：{error}"))?;
    Ok(InstallView {
        installed: true,
        version: version.clone(),
        message: format!("版本 {version} 已安装，重启应用后生效。"),
    })
}
