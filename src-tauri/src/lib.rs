//! Tauri 薄壳：命令桥 + 配置路径解析 + 事件转发。
//!
//! 同步逻辑（文件分类、三向合并、WebDAV、librime 触发）全部在 rime-sync-core；
//! 本文件只负责把命令参数翻译成引擎调用，并把进度/结果转发给前端。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rime_sync_core::cancel::CancelToken;
use rime_sync_core::config::{self, Config};
use rime_sync_core::engine::{Engine, Progress, ProgressSink, Report};
use rime_sync_core::platform;
use rime_sync_core::rime_dir;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const LOG_CAPACITY: usize = 1000;

#[derive(Default)]
struct Inner {
    cancel: Mutex<Option<CancelToken>>,
    running: AtomicBool,
    log: Mutex<Vec<String>>,
}

#[derive(Clone, Default)]
struct AppState(Arc<Inner>);

fn push_log(inner: &Inner, message: impl Into<String>) {
    let line = format!(
        "[{}] {}",
        chrono::Local::now().format("%H:%M:%S"),
        message.into()
    );
    if let Ok(mut log) = inner.log.lock() {
        log.push(line);
        if log.len() > LOG_CAPACITY {
            let excess = log.len() - LOG_CAPACITY;
            log.drain(0..excess);
        }
    }
}

fn state_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("无法解析应用配置目录：{error}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建配置目录 {} 失败：{error}", dir.display()))?;
    Ok(dir)
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("config.json"))
}

fn detect_rime(config: &Config) -> Result<rime_dir::Detection, String> {
    rime_dir::detect(config.rime_dir.as_deref().map(Path::new))
        .or_else(|_| rime_dir::detect(None))
        .map_err(|error| {
            format!("未能定位 Rime 用户目录：{error:#}。请在「同步」页点击自动探测或手动填写目录路径。")
        })
}

/// 读取配置；device_id 为空时按 PLAN §6 生成并写回。
fn ensure_config(app: &AppHandle, inner: &Inner) -> Result<Config, String> {
    let path = config_path(app)?;
    let mut config = Config::load(&path).map_err(|error| format!("读取配置失败：{error:#}"))?;
    if config.device_id.trim().is_empty() {
        let detected = detect_rime(&config).ok();
        config.device_id = match detected {
            Some(detection) => config.resolve_device_id(&detection.path),
            None => config::generate_device_id(),
        };
        config
            .save(&path)
            .map_err(|error| format!("写入配置失败：{error:#}"))?;
        push_log(inner, format!("已生成设备 ID：{}", config.device_id));
    }
    Ok(config)
}

fn server_kind(server: Option<&str>, dav: &[String]) -> String {
    let raw = server.unwrap_or("").to_ascii_lowercase();
    if raw.contains("nextcloud") {
        "Nextcloud".to_string()
    } else if raw.contains("nutstore") || raw.contains("jianguoyun") {
        "坚果云".to_string()
    } else if raw.contains("synology") || raw.contains("diskstation") {
        "群晖 DSM".to_string()
    } else if raw.contains("apache") {
        "Apache mod_dav".to_string()
    } else if raw.contains("nginx") {
        "nginx WebDAV".to_string()
    } else if raw.contains("caddy") {
        "Caddy WebDAV".to_string()
    } else if let Some(name) = server {
        name.to_string()
    } else if dav.is_empty() {
        "未知 WebDAV 服务".to_string()
    } else {
        format!("WebDAV（{}）", dav.join(", "))
    }
}

#[tauri::command]
fn cmd_get_config(app: AppHandle, state: State<'_, AppState>) -> Result<Config, String> {
    ensure_config(&app, &state.0)
}

#[tauri::command]
fn cmd_set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    mut config: Config,
) -> Result<Config, String> {
    config.webdav.url = config.webdav.url.trim().to_string();
    config.webdav.username = config.webdav.username.trim().to_string();
    config.webdav.remote_dir = config
        .webdav
        .remote_dir
        .trim()
        .trim_matches('/')
        .to_string();
    if config.webdav.remote_dir.is_empty() {
        config.webdav.remote_dir = "rime-sync".to_string();
    }
    config.rime_dir = config
        .rime_dir
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty());

    if config.device_id.trim().is_empty() {
        let detected = config
            .rime_dir
            .as_deref()
            .map(Path::new)
            .and_then(|path| rime_dir::detect(Some(path)).ok());
        config.device_id = match detected {
            Some(detection) => config.resolve_device_id(&detection.path),
            None => config::generate_device_id(),
        };
    }
    config.device_id = config.device_id.trim().to_string();

    let path = config_path(&app)?;
    config
        .save(&path)
        .map_err(|error| format!("写入配置失败：{error:#}"))?;
    push_log(&state.0, "配置已保存");
    Ok(config)
}

#[derive(Serialize)]
struct DetectionView {
    path: String,
    source: String,
    exists: bool,
}

#[tauri::command]
fn cmd_detect_rime_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DetectionView, String> {
    let config = ensure_config(&app, &state.0)?;
    let detection = detect_rime(&config)?;
    Ok(DetectionView {
        exists: detection.path.is_dir(),
        path: detection.path.display().to_string(),
        source: detection.source,
    })
}

#[derive(Serialize)]
struct ConnectionTest {
    ok: bool,
    latency_ms: u64,
    server: Option<String>,
    dav_class: String,
    server_type: String,
    status: u16,
    remote_dir_exists: bool,
    message: String,
}

#[tauri::command]
fn cmd_test_connection(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ConnectionTest, String> {
    let config = ensure_config(&app, &state.0)?;
    if config.webdav.url.trim().is_empty() {
        return Err("请先填写 WebDAV 地址".to_string());
    }
    let root = DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        "",
    )
    .map_err(|error| format!("WebDAV 地址无效：{error:#}"))?;
    let started = Instant::now();
    let probe = root
        .probe()
        .map_err(|error| format!("连接失败：{error:#}"))?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let dav_class = probe.dav.join(", ");
    let server_type = server_kind(probe.server.as_deref(), &probe.dav);
    let base = DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
    )
    .map_err(|error| format!("远端目录无效：{error:#}"))?;
    let remote_dir_exists = !base
        .propfind("", 0)
        .map_err(|error| format!("远端目录不可访问：{error:#}"))?
        .is_empty();
    let ok = (200..300).contains(&probe.status);
    let message = if ok {
        "认证与 WebDAV 协议可用".to_string()
    } else {
        format!("服务器返回 HTTP {}", probe.status)
    };
    Ok(ConnectionTest {
        ok,
        latency_ms,
        server: probe.server,
        dav_class,
        server_type,
        status: probe.status,
        remote_dir_exists,
        message,
    })
}

#[derive(Serialize)]
struct RemoteDevice {
    id: String,
    files: usize,
}

#[derive(Serialize)]
struct RemoteShared {
    path: String,
    size: Option<u64>,
}

#[derive(Serialize)]
struct RemoteList {
    devices: Vec<RemoteDevice>,
    shared: Vec<RemoteShared>,
}

#[tauri::command]
fn cmd_list_remote(app: AppHandle, state: State<'_, AppState>) -> Result<RemoteList, String> {
    let config = ensure_config(&app, &state.0)?;
    let client = DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
    )
    .map_err(|error| format!("WebDAV 配置无效：{error:#}"))?;

    let mut devices = Vec::new();
    for entry in client
        .propfind("sync", 1)
        .map_err(|error| format!("读取远端设备失败：{error:#}"))?
    {
        if !entry.is_dir {
            continue;
        }
        let name = entry.path.rsplit('/').next().unwrap_or("");
        if name.is_empty() || name == "sync" || name.starts_with('.') {
            continue;
        }
        let files = client
            .propfind(&entry.path, 1)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| !item.is_dir && item.path.ends_with(".userdb.txt"))
                    .count()
            })
            .unwrap_or(0);
        devices.push(RemoteDevice {
            id: name.to_string(),
            files,
        });
    }
    devices.sort_by(|a, b| a.id.cmp(&b.id));

    let mut shared = Vec::new();
    for entry in client
        .propfind("shared/files", 1)
        .map_err(|error| format!("读取共享文件失败：{error:#}"))?
    {
        if entry.is_dir {
            continue;
        }
        let path = entry
            .path
            .strip_prefix("shared/files/")
            .unwrap_or(&entry.path)
            .to_string();
        shared.push(RemoteShared {
            path,
            size: entry.size,
        });
    }
    shared.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(RemoteList { devices, shared })
}

#[derive(Clone, Copy)]
enum SyncMode {
    Transport,
    Apply,
}

fn run_sync(app: &AppHandle, inner: &Arc<Inner>, mode: SyncMode) -> Result<Report, String> {
    if inner
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有同步任务正在运行，请等待完成或先取消".to_string());
    }
    let outcome = run_sync_inner(app, inner, mode);
    inner.running.store(false, Ordering::SeqCst);
    if let Ok(mut guard) = inner.cancel.lock() {
        guard.take();
    }
    if let Err(error) = &outcome {
        push_log(inner, format!("任务失败：{error}"));
    }
    outcome
}

fn run_sync_inner(app: &AppHandle, inner: &Arc<Inner>, mode: SyncMode) -> Result<Report, String> {
    let label = match mode {
        SyncMode::Transport => "搬运",
        SyncMode::Apply => "合并生效",
    };
    let path = config_path(app)?;
    let mut config = Config::load(&path).map_err(|error| format!("读取配置失败：{error:#}"))?;

    let detected = detect_rime(&config)?;
    let rime_path = detected.path;
    let rime_text = rime_path.display().to_string();
    if config.rime_dir.as_deref() != Some(rime_text.as_str()) {
        config.rime_dir = Some(rime_text);
    }
    if config.device_id.trim().is_empty() {
        config.device_id = config.resolve_device_id(&rime_path);
    }
    config.device_id = config.device_id.trim().to_string();
    config
        .save(&path)
        .map_err(|error| format!("保存配置失败：{error:#}"))?;
    if let Err(error) = config.validate() {
        return Err(format!("配置不完整：{error:#}"));
    }

    let token = CancelToken::new();
    if let Ok(mut guard) = inner.cancel.lock() {
        *guard = Some(token.clone());
    }
    let sink_app = app.clone();
    let sink_inner = inner.clone();
    let sink = ProgressSink::new(move |progress: &Progress| {
        let _ = sink_app.emit("sync://progress", progress);
        let counts = if progress.total > 0 {
            format!("（{}/{}）", progress.current, progress.total)
        } else {
            String::new()
        };
        push_log(
            &sink_inner,
            format!("[{}] {}{}", progress.phase, progress.message, counts),
        );
    });

    let trigger = platform::default_trigger(config.stop_fcitx5_during_apply);
    let mut engine = Engine::new(config, rime_path, state_dir(app)?, trigger)
        .map_err(|error| format!("初始化同步引擎失败：{error:#}"))?
        .with_progress(sink)
        .with_cancel(token);

    push_log(inner, format!("开始{label}"));
    let report = match mode {
        SyncMode::Transport => engine.transport(),
        SyncMode::Apply => engine.apply(),
    };
    let verdict = if report.ok { "成功" } else { "失败" };
    push_log(
        inner,
        format!(
            "{label}{verdict}：拉取 {}，推送 {}，合并 {}，冲突 {}，耗时 {} ms",
            report.pulled,
            report.pushed,
            report.merged,
            report.conflicts.len(),
            report.duration_ms
        ),
    );
    if let Some(error) = &report.error {
        push_log(inner, format!("{label}错误：{error}"));
    }
    let _ = app.emit("sync://done", &report);
    Ok(report)
}

#[tauri::command(async)]
fn cmd_transport(app: AppHandle, state: State<'_, AppState>) -> Result<Report, String> {
    run_sync(&app, &state.0, SyncMode::Transport)
}

#[tauri::command(async)]
fn cmd_apply(app: AppHandle, state: State<'_, AppState>) -> Result<Report, String> {
    run_sync(&app, &state.0, SyncMode::Apply)
}

#[tauri::command]
fn cmd_cancel(state: State<'_, AppState>) -> Result<(), String> {
    let guard = state
        .0
        .cancel
        .lock()
        .map_err(|_| "取消状态不可用".to_string())?;
    match guard.as_ref() {
        Some(token) => {
            token.cancel();
            push_log(&state.0, "已请求取消");
            Ok(())
        }
        None => Err("当前没有正在运行的任务".to_string()),
    }
}

#[tauri::command]
fn cmd_get_log(state: State<'_, AppState>, limit: Option<usize>) -> Result<Vec<String>, String> {
    let limit = limit.unwrap_or(200).clamp(1, LOG_CAPACITY);
    let log = state
        .0
        .log
        .lock()
        .map_err(|_| "日志缓冲不可用".to_string())?;
    let start = log.len().saturating_sub(limit);
    Ok(log[start..].to_vec())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .manage(AppState::default())
        .setup(|app| {
            let state = app.state::<AppState>();
            push_log(&state.0, "Rime 同步已启动");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            cmd_get_config,
            cmd_set_config,
            cmd_detect_rime_dir,
            cmd_test_connection,
            cmd_list_remote,
            cmd_transport,
            cmd_apply,
            cmd_cancel,
            cmd_get_log
        ]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("rime-sync 启动失败：{error}");
        std::process::exit(1);
    }
}
