//! 远端设备：读 \`sync/*/owner.json\`，注销时删除整个设备目录。

use rime_sync_core::config::Config;
use rime_sync_core::plan;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{dav_client, push_log, AppState};

#[derive(Serialize)]
pub struct DeviceView {
    pub id: String,
    pub hostname: String,
    pub os: String,
    pub session: String,
    pub heartbeat_ts: u64,
    pub is_self: bool,
    pub reachable: bool,
}

#[derive(Serialize)]
pub struct RevokeView {
    pub id: String,
    pub removed: bool,
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|item| item.as_str()).map(str::to_string)
}

pub(crate) fn collect_devices(
    config: &Config,
    client: &DavClient,
) -> Result<Vec<DeviceView>, String> {
    let mut devices = Vec::new();
    for entry in client
        .propfind("sync", 1)
        .map_err(|error| format!("读取远端设备失败：{error:#}"))?
    {
        if !entry.is_dir {
            continue;
        }
        let id = entry.path.rsplit('/').next().unwrap_or("").to_string();
        if id.is_empty() || id == "sync" || id.starts_with('.') {
            continue;
        }
        if plan::validate_rel_path(&format!("sync/{id}")).is_err() {
            continue;
        }
        let owner = client
            .get(&format!("sync/{id}/owner.json"))
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let mut hostname = "—".to_string();
        let mut os = "—".to_string();
        let mut session = String::new();
        let mut heartbeat_ts = 0u64;
        let mut reachable = false;
        if let Some(owner) = owner.as_ref() {
            reachable = true;
            hostname = text_field(owner, "hostname").unwrap_or_else(|| "—".to_string());
            os = text_field(owner, "os").unwrap_or_else(|| "—".to_string());
            session = text_field(owner, "session").unwrap_or_default();
            heartbeat_ts = owner
                .get("heartbeat_ts")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
        }
        devices.push(DeviceView {
            is_self: id == config.device_id,
            id,
            hostname,
            os,
            session,
            heartbeat_ts,
            reachable,
        });
    }
    devices.sort_by(|left, right| {
        right
            .is_self
            .cmp(&left.is_self)
            .then(right.heartbeat_ts.cmp(&left.heartbeat_ts))
            .then(left.id.cmp(&right.id))
    });
    Ok(devices)
}

#[tauri::command]
pub fn cmd_list_devices(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<DeviceView>, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    collect_devices(&config, &client)
}

#[tauri::command]
pub fn cmd_revoke_device(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RevokeView, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    let id = id.trim().to_string();
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id == "."
        || id == ".."
        || id.starts_with('.')
    {
        return Err("设备 ID 无效".to_string());
    }
    if id == config.device_id {
        return Err("不能注销本机设备".to_string());
    }
    let rel = format!("sync/{id}");
    plan::validate_rel_path(&rel).map_err(|error| format!("设备路径无效：{error:#}"))?;
    client
        .delete(&rel)
        .map_err(|error| format!("注销设备失败：{error:#}"))?;
    push_log(&state.0, format!("已注销远端设备 {id}"));
    Ok(RevokeView { id, removed: true })
}
