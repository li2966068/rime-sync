//! 服务商预设与写权限探测：薄转发 core::presets / DavClient::check_write。

use rime_sync_core::presets::Preset;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{dav_client, push_log, AppState};

#[derive(Serialize)]
pub struct PresetView {
    pub id: String,
    pub label: String,
    pub default_path: String,
    pub hint: String,
}

#[derive(Serialize)]
pub struct PresetApply {
    pub normalized_url: String,
    pub hint: String,
}

#[derive(Serialize)]
pub struct ProbeStepView {
    pub step: String,
    pub ok: bool,
    pub status: u16,
    pub detail: String,
}

#[derive(Serialize)]
pub struct WriteProbeView {
    pub ok: bool,
    pub step: String,
    pub http_status: u16,
    pub hint: Option<String>,
    pub steps: Vec<ProbeStepView>,
}

#[tauri::command]
pub fn cmd_list_presets() -> Vec<PresetView> {
    Preset::all()
        .into_iter()
        .map(|preset| PresetView {
            id: preset.id().to_string(),
            label: preset.label().to_string(),
            default_path: preset.default_path().to_string(),
            hint: preset.hint().to_string(),
        })
        .collect()
}

#[tauri::command]
pub fn cmd_apply_preset(preset_id: String, raw_url: String) -> Result<PresetApply, String> {
    let preset =
        Preset::from_id(&preset_id).ok_or_else(|| format!("未知的服务商预设：{preset_id}"))?;
    Ok(PresetApply {
        normalized_url: preset.normalize(&raw_url),
        hint: preset.hint().to_string(),
    })
}

#[tauri::command]
pub fn cmd_probe_write(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<WriteProbeView, String> {
    let (_config, client) = dav_client(&app, &state.0)?;
    // DavClient::check_write 内部已 ensure_base 并保证清理临时目录。
    let probe = client
        .check_write()
        .map_err(|error| format!("写权限探测失败：{error:#}"))?;
    let failed = probe.steps.iter().find(|step| !step.ok);
    let (step, http_status) = match failed {
        Some(step) => (step.step.clone(), step.status),
        None => ("OK".to_string(), 200),
    };
    if !probe.ok {
        push_log(
            &state.0,
            format!("写权限探测失败：{step} HTTP {http_status}"),
        );
    }
    Ok(WriteProbeView {
        ok: probe.ok,
        step,
        http_status,
        hint: probe.hint,
        steps: probe
            .steps
            .into_iter()
            .map(|step| ProbeStepView {
                step: step.step,
                ok: step.ok,
                status: step.status,
                detail: step.detail,
            })
            .collect(),
    })
}
