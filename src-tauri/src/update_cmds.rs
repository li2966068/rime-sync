//! 自动更新：薄转发 tauri-plugin-updater。
//!
//! 端点与公钥来自 tauri.conf.json；本模块只负责把结果/错误文本化。
//!
//! 代理：更新器有独立的 reqwest client，插件提供 `UpdaterBuilder::proxy(Url)`。
//! http/https 代理（含 basic 认证）可以用同一份 proxy 配置接进来；socks 代理
//! 会跳过（插件内置的 reqwest 没有编译 socks feature），此时检查更新可能直连。

use rime_sync_core::proxy::ProxyConfig;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Updater, UpdaterExt};

use crate::AppState;

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

/// 更新器只接受一个 URL：把 proxy 配置翻译成带 userinfo 的 URL（reqwest 会解析成
/// basic 认证）。socks 方案返回 None——留给文档里的已知限制。
fn updater_proxy_url(proxy: &ProxyConfig) -> Option<tauri::Url> {
    let spec = proxy.spec().ok()?;
    if spec.kind.is_socks() {
        return None;
    }
    let mut url = tauri::Url::parse(&spec.to_url()).ok()?;
    let username = proxy.username.trim();
    if !username.is_empty() {
        url.set_username(username).ok()?;
        url.set_password(Some(&proxy.password)).ok()?;
    }
    Some(url)
}

/// 读配置会探测密钥后端（最多 2s）：放到阻塞线程里，别卡 async runtime。
fn proxy_for_updater(app: &AppHandle, inner: &Arc<crate::Inner>) -> Option<tauri::Url> {
    match crate::ensure_config(app, inner) {
        Ok(config) => {
            if !config.proxy.enabled {
                return None;
            }
            let url = updater_proxy_url(&config.proxy);
            if url.is_none() {
                crate::push_log(
                    inner,
                    "更新检查：代理是 socks 类型，更新器不支持（内置 client 无 socks），本次可能直连",
                );
            }
            url
        }
        Err(error) => {
            crate::push_log(
                inner,
                format!("更新检查：读取代理配置失败（按直连处理）：{error}"),
            );
            None
        }
    }
}

async fn build_updater(app: &AppHandle, state: &AppState) -> Result<Updater, String> {
    let app_for_config = app.clone();
    let inner = Arc::clone(&state.0);
    let proxy =
        tauri::async_runtime::spawn_blocking(move || proxy_for_updater(&app_for_config, &inner))
            .await
            .map_err(|error| format!("更新器初始化失败：{error}"))?;
    let mut builder = app.updater_builder();
    if let Some(proxy) = proxy {
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("更新器不可用：{error}"))
}

#[tauri::command]
pub async fn cmd_check_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<UpdateView, String> {
    let current = app.package_info().version.to_string();
    let updater = build_updater(&app, &state).await?;
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
pub async fn cmd_install_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<InstallView, String> {
    let updater = build_updater(&app, &state).await?;
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
