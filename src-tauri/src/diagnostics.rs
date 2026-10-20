//! 诊断包：config（脱敏）+ 最近 500 行日志 + 远端清单 + 系统信息，打包成 zip。
//!
//! 不引入 zip 依赖：只用 STORE（不压缩）写一个最小可用的 zip，
//! 因为本模块只打包几十 KB 文本。

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{data_dir, ensure_config, log_tail, state_dir, AppState};

const LOG_LINES: usize = 500;

#[derive(Serialize)]
pub struct DiagnosticsView {
    pub path: String,
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 == 1 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *slot = value;
    }
    table
}

fn crc32(bytes: &[u8], table: &[u32; 256]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc = table[((crc ^ u32::from(*byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

fn dos_datetime() -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let year = now.year().clamp(1980, 2107) as u16;
    let date = ((year - 1980) << 9) | ((now.month() as u16) << 5) | now.day() as u16;
    let time =
        ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2);
    (time, date)
}

fn write_zip(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let table = crc_table();
    let (time, date) = dos_datetime();
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<(String, u32, u32, u32)> = Vec::new();

    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data, &table);
        let size = data.len() as u32;
        push_u32(&mut out, 0x0403_4b50);
        push_u16(&mut out, 20);
        push_u16(&mut out, 0x0800);
        push_u16(&mut out, 0);
        push_u16(&mut out, time);
        push_u16(&mut out, date);
        push_u32(&mut out, crc);
        push_u32(&mut out, size);
        push_u32(&mut out, size);
        push_u16(&mut out, name.len() as u16);
        push_u16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.push((name.clone(), crc, size, offset));
    }

    let cd_start = out.len() as u32;
    for (name, crc, size, offset) in &central {
        push_u32(&mut out, 0x0201_4b50);
        push_u16(&mut out, 20);
        push_u16(&mut out, 20);
        push_u16(&mut out, 0x0800);
        push_u16(&mut out, 0);
        push_u16(&mut out, time);
        push_u16(&mut out, date);
        push_u32(&mut out, *crc);
        push_u32(&mut out, *size);
        push_u32(&mut out, *size);
        push_u16(&mut out, name.len() as u16);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u32(&mut out, 0);
        push_u32(&mut out, *offset);
        out.extend_from_slice(name.as_bytes());
    }
    let cd_size = out.len() as u32 - cd_start;
    let count = central.len() as u16;
    push_u32(&mut out, 0x0605_4b50);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, count);
    push_u16(&mut out, count);
    push_u32(&mut out, cd_size);
    push_u32(&mut out, cd_start);
    push_u16(&mut out, 0);
    Ok(out)
}

/// 只保留 scheme://host[:port]，路径与 userinfo 一律丢弃（可能含 token）。
fn sanitize_url(raw: &str) -> String {
    let raw = raw.trim();
    let Some((scheme, rest)) = raw.split_once("://") else {
        return String::new();
    };
    let authority = rest.split('/').next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.is_empty() {
        return String::new();
    }
    format!("{scheme}://{authority}")
}

fn mask(value: &str) -> String {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => format!("{first}***"),
        None => String::new(),
    }
}

/// 诊断包里允许出现的配置字段（白名单）。密码永不进入，URL 只保留 host。
fn config_diagnostics(config: &rime_sync_core::config::Config, has_password: bool) -> serde_json::Value {
    serde_json::json!({
        "rime_dir": config.rime_dir,
        "device_id": config.device_id,
        "transport_interval_minutes": config.transport_interval_minutes,
        "sync_kinds": {
            "userdict": config.sync_kinds.userdict,
            "custom_yaml": config.sync_kinds.custom_yaml,
            "dict_sources": config.sync_kinds.dict_sources,
        },
        "exclude_globs": config.exclude_globs,
        "stop_fcitx5_during_apply": config.stop_fcitx5_during_apply,
        "webdav": {
            "url": sanitize_url(&config.webdav.url),
            "username": mask(&config.webdav.username),
            "remote_dir": config.webdav.remote_dir,
            "secret_backend": format!("{:?}", config.password_ref).to_lowercase(),
            "secret_configured": has_password,
        },
    })
}

#[tauri::command]
pub fn cmd_export_diagnostics(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DiagnosticsView, String> {
    let config = ensure_config(&app, &state.0)?;
    let backups = crate::backup_manager(&app, &state.0)
        .and_then(|manager| manager.list().map_err(|error| format!("{error:#}")))
        .unwrap_or_default();
    let watching = state
        .0
        .watch
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false);

    let config_view = config_diagnostics(&config, !config.webdav.password.is_empty());

    let remote = match crate::dav_client(&app, &state.0) {
        Ok((config, client)) => {
            let devices = crate::device_cmds::collect_devices(&config, &client)
                .map(|list| serde_json::to_value(list).unwrap_or(serde_json::Value::Null))
                .unwrap_or_else(|error| serde_json::json!({ "error": error }));
            let mut shared = Vec::new();
            if let Ok(entries) = client.propfind("shared/files", 1) {
                for entry in entries {
                    if entry.is_dir {
                        continue;
                    }
                    shared.push(serde_json::json!({
                        "path": entry.path,
                        "size": entry.size,
                    }));
                }
            }
            serde_json::json!({ "devices": devices, "shared": shared })
        }
        Err(error) => serde_json::json!({ "error": error }),
    };

    let system = serde_json::json!({
        "app_version": app.package_info().version.to_string(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "rime_dir": config.rime_dir,
        "device_id": config.device_id,
        "secret_backend": format!("{:?}", config.password_ref).to_lowercase(),
        "config_dir": state_dir(&app).map(|path| path.display().to_string()).unwrap_or_default(),
        "backup_count": backups.len(),
        "watch_running": watching,
        "generated_at": rime_sync_core::backup::now_secs(),
        "generated_at_text": rime_sync_core::backup::format_timestamp(rime_sync_core::backup::now_secs()),
    });

    let log = log_tail(&state.0, LOG_LINES).join("\n");
    let entries = vec![
        (
            "config.json".to_string(),
            serde_json::to_vec_pretty(&config_view).map_err(|error| error.to_string())?,
        ),
        ("log.txt".to_string(), log.into_bytes()),
        (
            "devices.json".to_string(),
            serde_json::to_vec_pretty(&remote).map_err(|error| error.to_string())?,
        ),
        (
            "system.json".to_string(),
            serde_json::to_vec_pretty(&system).map_err(|error| error.to_string())?,
        ),
    ];

    let zip = write_zip(&entries)?;
    let dir = data_dir(&app)?;
    let name = format!(
        "diagnostics-{}.zip",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let path: PathBuf = dir.join(name);
    let mut file = fs::File::create(&path)
        .map_err(|error| format!("创建诊断包 {} 失败：{error}", path.display()))?;
    file.write_all(&zip)
        .map_err(|error| format!("写入诊断包失败：{error}"))?;
    file.flush()
        .map_err(|error| format!("写入诊断包失败：{error}"))?;
    crate::push_log(&state.0, format!("已导出诊断包：{}", path.display()));
    Ok(DiagnosticsView {
        path: path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_sync_core::config::Config;

    #[test]
    fn config_diagnostics_contains_no_secret_fields() {
        let mut config = Config::default();
        config.webdav.url = "https://alice:hunter2@example.com/dav/token123".to_string();
        config.webdav.username = "alice".to_string();
        config.webdav.password = "hunter2".to_string();
        config.webdav.remote_dir = "rime-sync".to_string();
        config.rime_dir = Some("/home/alice/.local/share/fcitx5/rime".to_string());

        let text = serde_json::to_string(&config_diagnostics(&config, true))
            .unwrap()
            .to_lowercase();
        assert!(!text.contains("password"), "config view leaked a password field");
        assert!(!text.contains("hunter2"), "config view leaked the password");
        assert!(!text.contains("token123"), "config view leaked a URL token");
        assert!(!text.contains("alice:"), "config view leaked URL userinfo");
        assert!(text.contains("example.com"));
        assert!(text.contains("a***"));
    }

    #[test]
    fn zip_has_local_and_central_headers() {
        let zip = write_zip(&[("config.json".to_string(), b"{}".to_vec())]).unwrap();
        assert_eq!(&zip[0..4], b"PK");
        assert!(zip.windows(4).any(|window| window == b"PK"));
        assert!(zip.windows(4).any(|window| window == b"PK"));
        assert!(zip
            .windows(b"config.json".len())
            .any(|window| window == b"config.json"));
    }

    #[test]
    fn url_sanitizer_drops_path_and_userinfo() {
        assert_eq!(
            sanitize_url("https://user:pass@host.example:8443/dav/secret"),
            "https://host.example:8443"
        );
        assert_eq!(sanitize_url("not a url"), "");
    }
}
