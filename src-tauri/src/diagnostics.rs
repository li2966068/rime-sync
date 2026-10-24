//! 诊断包：config（脱敏）+ 最近 500 行日志 + 远端清单 + 系统信息，打包成 zip。
//!
//! 不引入 zip 依赖：只用 STORE（不压缩）写一个最小可用的 zip，
//! 因为本模块只打包几十 KB 文本。

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

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
fn config_diagnostics(
    config: &rime_sync_core::config::Config,
    has_password: bool,
) -> serde_json::Value {
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

/// 离线依赖安装告警（由 packaging/linux/postinst 生成的 wrapper 写出）。
///
/// 链路：postinst → /var/lib/rime-sync/install-vendor-deps-wrapper.sh →
/// worker 结束后仍拿不到 fcitx5-rime/rime-inputmethod →
/// ~/.config/com.rimesync.app/install-warning.json（含时间、包名、日志路径）。
/// 诊断页读它显示红条；重试按钮用 pkexec 重跑同一个 wrapper（幂等）。
const INSTALL_WARNING_FILE: &str = "install-warning.json";
/// fcitx5-rime / rime-inputmethod 安装成功后都会落下这个文件（离线包与发行版包都一样）。
#[cfg(target_os = "linux")]
const RIME_ADDON_CONF: &str = "/usr/share/fcitx5/addon/rime.conf";
const RETRY_WRAPPER: &str = "/var/lib/rime-sync/install-vendor-deps-wrapper.sh";

#[derive(Serialize, Default)]
pub struct InstallWarning {
    /// 是否需要在前端显示红条。
    pub present: bool,
    /// 告警文件路径（无论是否存在都返回，便于用户手工检查）。
    pub path: String,
    pub written_at: Option<String>,
    pub package: Option<String>,
    pub reason: Option<String>,
    pub log_path: Option<String>,
    pub retry_command: Option<String>,
}

fn parse_install_warning(path: String, raw: &str) -> InstallWarning {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        // 文件坏了也要可见：红条退化成"告警文件无法解析"，不要静默吞掉。
        Err(error) => {
            return InstallWarning {
                present: true,
                path,
                reason: Some(format!("{INSTALL_WARNING_FILE} 无法解析：{error}")),
                ..Default::default()
            }
        }
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|entry| entry.as_str())
            .map(str::to_string)
    };
    InstallWarning {
        present: true,
        path,
        written_at: text("written_at"),
        package: text("package"),
        reason: text("reason"),
        log_path: text("log_path"),
        retry_command: text("retry_command"),
    }
}

/// 依赖已经装好时告警就过期了（例如用户后来手动 apt install），顺手清掉，
/// 免得红条永远消不掉。
#[cfg(target_os = "linux")]
fn install_deps_present() -> bool {
    std::path::Path::new(RIME_ADDON_CONF).exists()
}
#[cfg(not(target_os = "linux"))]
fn install_deps_present() -> bool {
    false
}

fn read_install_warning(app: &AppHandle) -> InstallWarning {
    let Ok(dir) = state_dir(app) else {
        return InstallWarning::default();
    };
    let path = dir.join(INSTALL_WARNING_FILE);
    let path_text = path.display().to_string();
    if !path.exists() {
        return InstallWarning {
            path: path_text,
            ..Default::default()
        };
    }
    if install_deps_present() {
        let _ = fs::remove_file(&path);
        crate::push_log(
            &app.state::<AppState>().0,
            "依赖已就绪，清除 install-warning.json",
        );
        return InstallWarning {
            path: path_text,
            ..Default::default()
        };
    }
    match fs::read_to_string(&path) {
        Ok(raw) => parse_install_warning(path_text, &raw),
        Err(error) => InstallWarning {
            present: true,
            path: path_text,
            reason: Some(format!("读取 {INSTALL_WARNING_FILE} 失败：{error}")),
            ..Default::default()
        },
    }
}

/// 诊断页读取：离线依赖是否装完（Linux 之外永远是 present=false）。
#[tauri::command]
pub fn cmd_install_warning(app: AppHandle) -> InstallWarning {
    read_install_warning(&app)
}

/// 诊断页「重试」：用 pkexec 重跑 postinst 生成的 wrapper（它会重新 apt/dpkg 安装，
/// 成功后自己删掉 install-warning.json）。脱离进程，不等结果：pkexec 要弹密码框。
#[tauri::command]
pub fn cmd_retry_install_deps(app: AppHandle) -> Result<String, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = &app;
        return Err("离线依赖安装告警只在 Linux 版存在".to_string());
    }
    #[cfg(target_os = "linux")]
    {
        if !std::path::Path::new(RETRY_WRAPPER).exists() {
            return Err(format!(
                "缺少 {RETRY_WRAPPER}：请重新安装 rime-sync（postinst 会生成该脚本）"
            ));
        }
        let warning = read_install_warning(&app);
        let mut command = std::process::Command::new("pkexec");
        command
            .arg("bash")
            .arg(RETRY_WRAPPER)
            .arg(&warning.path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
            .spawn()
            .map_err(|error| format!("启动 pkexec 失败：{error}"))?;
        crate::push_log(&app.state::<AppState>().0, "已通过 pkexec 重试离线依赖安装");
        Ok(format!(
            "已启动依赖重试（会弹授权窗口）。完成后刷新本页；日志：{}",
            warning
                .log_path
                .unwrap_or_else(|| "/var/log/rime-sync/vendor-install.log".to_string())
        ))
    }
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
    let install_warning = read_install_warning(&app);
    let install_warning_json = serde_json::json!({
        "present": install_warning.present,
        "path": install_warning.path,
        "written_at": install_warning.written_at,
        "package": install_warning.package,
        "reason": install_warning.reason,
        "log_path": install_warning.log_path,
        "retry_command": install_warning.retry_command,
    });
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
        (
            "install-warning.json".to_string(),
            serde_json::to_vec_pretty(&install_warning_json).map_err(|error| error.to_string())?,
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
        assert!(
            !text.contains("password"),
            "config view leaked a password field"
        );
        assert!(!text.contains("hunter2"), "config view leaked the password");
        assert!(!text.contains("token123"), "config view leaked a URL token");
        assert!(!text.contains("alice:"), "config view leaked URL userinfo");
        assert!(text.contains("example.com"));
        assert!(text.contains("a***"));
    }

    #[test]
    fn install_warning_parses_metadata() {
        let raw = r#"{"schema":1,"written_at":"2026-10-24T09:00:00Z","package":"fcitx5-rime",
            "reason":"离线依赖未安装完成","log_path":"/var/log/rime-sync/vendor-install.log",
            "retry_command":"pkexec bash /var/lib/rime-sync/install-vendor-deps-wrapper.sh"}"#;
        let warning = parse_install_warning(
            "/home/alice/.config/com.rimesync.app/install-warning.json".to_string(),
            raw,
        );
        assert!(warning.present);
        assert_eq!(warning.package.as_deref(), Some("fcitx5-rime"));
        assert_eq!(
            warning.log_path.as_deref(),
            Some("/var/log/rime-sync/vendor-install.log")
        );
        assert!(warning.reason.unwrap().contains("离线依赖"));
    }

    #[test]
    fn install_warning_survives_broken_json() {
        let warning = parse_install_warning("/tmp/install-warning.json".to_string(), "{not json");
        assert!(warning.present, "解析失败也必须可见");
        assert!(warning.reason.unwrap_or_default().contains("无法解析"));
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
