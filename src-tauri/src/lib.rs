//! Tauri 薄壳：命令桥 + 配置路径解析 + 事件转发。
//!
//! 同步逻辑（文件分类、三向合并、WebDAV、librime 触发）全部在 rime-sync-core；
//! 本文件只负责把命令参数翻译成引擎调用，并把进度/结果转发给前端。

// 非测试代码一律不允许 unwrap/expect（命令层错误一律转成 anyhow/String 上报前端）。
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rime_sync_core::backup::BackupManager;
use rime_sync_core::cancel::CancelToken;
use rime_sync_core::config::{self, Config};
use rime_sync_core::engine::{Engine, PlanReport, Progress, ProgressSink, Report};
use rime_sync_core::network::NetworkPolicy;
use rime_sync_core::platform;
use rime_sync_core::retry::RetryState;
use rime_sync_core::rime_dir;
use rime_sync_core::secret::{self, SecretSelection};
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

mod autostart;
mod backup_cmds;
mod conflict_cmds;
mod dedupe_cmds;
mod device_cmds;
mod diagnostics;
mod platform_prefs;
mod preset_cmds;
mod remote_tree;
mod schema_cmds;
mod tray;
mod update_cmds;
mod watch_cmds;

const LOG_CAPACITY: usize = 1000;

pub(crate) struct Inner {
    pub(crate) cancel: Mutex<Option<CancelToken>>,
    pub(crate) running: AtomicBool,
    /// 启动参数 --setup：前端取走一次后清除，保证只强制打开一次向导。
    pub(crate) setup_request: AtomicBool,
    pub(crate) log: Mutex<Vec<String>>,
    pub(crate) secret: Mutex<Option<Arc<SecretSelection>>>,
    pub(crate) watch: Mutex<Option<watch_cmds::WatchSession>>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            cancel: Mutex::new(None),
            running: AtomicBool::new(false),
            setup_request: AtomicBool::new(false),
            log: Mutex::new(Vec::new()),
            secret: Mutex::new(None),
            watch: Mutex::new(None),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct AppState(pub(crate) Arc<Inner>);

pub(crate) fn push_log(inner: &Inner, message: impl Into<String>) {
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

pub(crate) fn state_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("无法解析应用配置目录：{error}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建配置目录 {} 失败：{error}", dir.display()))?;
    Ok(dir)
}

pub(crate) fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法解析应用数据目录：{error}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建数据目录 {} 失败：{error}", dir.display()))?;
    Ok(dir)
}

pub(crate) fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("config.json"))
}

/// 密码后端：探测一次后缓存（钥匙串探测会写测试条目，不能每个命令都做）。
fn secret_selection(app: &AppHandle, inner: &Inner) -> Result<Arc<SecretSelection>, String> {
    if let Ok(guard) = inner.secret.lock() {
        if let Some(selection) = guard.as_ref() {
            return Ok(Arc::clone(selection));
        }
    }
    let selection = Arc::new(secret::detect(&data_dir(app)?));
    push_log(inner, format!("密码存储后端：{}", selection.describe()));
    if let Ok(mut guard) = inner.secret.lock() {
        *guard = Some(Arc::clone(&selection));
    }
    Ok(selection)
}

/// 读取配置并把密码装入内存（config 文件本身不含明文密码）。
pub(crate) fn load_config(app: &AppHandle, inner: &Inner) -> Result<Config, String> {
    let path = config_path(app)?;
    let selection = secret_selection(app, inner)?;
    let mut config = Config::load(&path).map_err(|error| format!("读取配置失败：{error:#}"))?;
    if config.legacy_password {
        match config.migrate_legacy_password(selection.store.as_ref(), &path) {
            Ok(true) => push_log(inner, "已把旧配置里的明文密码迁移到密码后端"),
            Ok(false) => {}
            Err(error) => push_log(inner, format!("迁移旧密码失败：{error:#}")),
        }
    }
    if let Err(error) = config.load_password(selection.store.as_ref()) {
        push_log(inner, format!("读取密码失败：{error:#}"));
    }
    Ok(config)
}

pub(crate) fn detect_rime(config: &Config) -> Result<rime_dir::Detection, String> {
    rime_dir::detect(config.rime_dir.as_deref().map(Path::new))
        .or_else(|_| rime_dir::detect(None))
        .map_err(|error| {
            format!(
                "未能定位 Rime 用户目录：{error:#}。请在「同步」页点击自动探测或手动填写目录路径。"
            )
        })
}

/// 读取配置；device_id 为空时按 PLAN §6 生成并写回。
pub(crate) fn ensure_config(app: &AppHandle, inner: &Inner) -> Result<Config, String> {
    let config = load_config(app, inner)?;
    ensure_device_id(app, inner, config)
}

/// 不解析密码的版本：只给不访问 WebDAV 的命令用。
///
/// 解析密码会探测钥匙串，而桌面会话里的 D-Bus 调用可能很慢（core 里有 2s 超时兜底），
/// 读设置/探测目录这类操作没必要为此等待。
pub(crate) fn ensure_config_meta(app: &AppHandle, inner: &Inner) -> Result<Config, String> {
    let path = config_path(app)?;
    let config = Config::load(&path).map_err(|error| format!("读取配置失败：{error:#}"))?;
    ensure_device_id(app, inner, config)
}

fn ensure_device_id(app: &AppHandle, inner: &Inner, mut config: Config) -> Result<Config, String> {
    let path = config_path(app)?;
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

pub(crate) fn rime_dir_for(app: &AppHandle, inner: &Inner) -> Result<PathBuf, String> {
    // 只要目录，不需要密码：不要为此探测钥匙串。
    let config = ensure_config_meta(app, inner)?;
    Ok(detect_rime(&config)?.path)
}

pub(crate) fn dav_client(app: &AppHandle, inner: &Inner) -> Result<(Config, DavClient), String> {
    let config = ensure_config(app, inner)?;
    if config.webdav.url.trim().is_empty() {
        return Err("请先填写 WebDAV 地址".to_string());
    }
    let client = DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
    )
    .map_err(|error| format!("WebDAV 配置无效：{error:#}"))?;
    Ok((config, client))
}

pub(crate) fn backup_manager(app: &AppHandle, inner: &Inner) -> Result<BackupManager, String> {
    let rime = rime_dir_for(app, inner)?;
    let root = state_dir(app)?.join("backups");
    Ok(BackupManager::new(&rime, &root))
}

pub(crate) fn log_tail(inner: &Inner, limit: usize) -> Vec<String> {
    let Ok(log) = inner.log.lock() else {
        return Vec::new();
    };
    let start = log.len().saturating_sub(limit);
    log[start..].to_vec()
}

fn server_kind(server: Option<&str>, dav: &[String]) -> String {
    let raw = server.unwrap_or("").to_ascii_lowercase();
    if raw.contains("nextcloud") {
        "Nextcloud".to_string()
    } else if raw.contains("nutstore") || raw.contains("jianguoyun") {
        "坚果云".to_string()
    } else if raw.contains("synology") || raw.contains("diskstation") {
        "群晖 DSM".to_string()
    } else if raw.contains("openlist") {
        "OpenList".to_string()
    } else if raw.contains("alist") {
        "AList".to_string()
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
    // 设置页要的是配置本身（密码字段序列化时已跳过），不必解析密码。
    let mut config = ensure_config_meta(&app, &state.0)?;
    // 配置里的 Rime 目录可能已被删除（或来自旧机器/旧测试），启动时校验一次。
    if let Some(dir) = config.rime_dir.clone() {
        if !Path::new(&dir).is_dir() {
            push_log(
                &state.0,
                format!("配置的 Rime 目录不存在，已重置以便重新探测：{dir}"),
            );
            config.rime_dir = None;
            config
                .save(&config_path(&app)?)
                .map_err(|error| format!("写入配置失败：{error:#}"))?;
        }
    }
    Ok(config)
}

#[tauri::command]
fn cmd_set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    mut config: Config,
) -> Result<Config, String> {
    config.webdav.url = config.webdav.url.trim().to_string();
    config.webdav.username = config.webdav.username.trim().to_string();
    // 远端目录留空 = 服务器根（SPEC-3 §A4），不再回填默认值。
    config.webdav.remote_dir = config
        .webdav
        .remote_dir
        .trim()
        .trim_matches('/')
        .to_string();
    config.rime_dir = config
        .rime_dir
        .map(|dir| dir.trim().to_string())
        .filter(|dir| !dir.is_empty());
    // 前端可能传来任意值（含 0）：统一夹到 1..=100 再落盘。
    config.backup_keep = config::clamp_backup_keep(config.backup_keep);

    // 用户在界面上显式填写的 ID（空 = 交给后端生成，不套用严格校验规则）。
    let submitted_id = config.device_id.trim().to_string();
    if submitted_id.is_empty() {
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
    let selection = secret_selection(&app, &state.0)?;
    let previous = load_config(&app, &state.0).ok();
    if let Some(previous) = previous.as_ref() {
        if !submitted_id.is_empty() && previous.device_id != config.device_id {
            config::validate_device_id(&config.device_id)
                .map_err(|error| format!("设备 ID 无效：{error:#}"))?;
        }
    }
    let typed_password = std::mem::take(&mut config.webdav.password);
    if typed_password.is_empty() {
        // 前端只拿得到空的密码字段（Config 序列化时已跳过 password）。
        // 用户名换了就把旧账号的密码搬到新账号名下，避免用户被迫重新输入。
        let old_account = previous
            .as_ref()
            .map(|old| old.password_account())
            .unwrap_or_default();
        let new_account = config.password_account();
        if !old_account.is_empty() && old_account != new_account {
            if let Ok(Some(secret)) = selection.store.get(&old_account) {
                if selection.store.set(&new_account, &secret).is_ok() {
                    config.password_ref = selection.backend.password_ref();
                }
            }
        }
        if let Ok(Some(secret)) = selection.store.get(&config.password_account()) {
            config.webdav.password = secret;
        }
    } else {
        selection
            .store
            .set(&config.password_account(), &typed_password)
            .map_err(|error| format!("保存密码失败：{error:#}"))?;
        config.password_ref = selection.backend.password_ref();
        config.webdav.password = typed_password;
        push_log(&state.0, "密码已写入系统密码后端");
    }

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
    remote_dir: String,
    remote_dir_exists: bool,
    remote_dir_missing: bool,
    preset: Option<String>,
    message: String,
    hint: Option<String>,
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
    let client = DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
    )
    .map_err(|error| format!("WebDAV 配置无效：{error:#}"))?;
    // SPEC-3 §A3：远端目录不存在是正常状态，只有连不上/认证失败才 ok=false。
    let check = client
        .check_connection()
        .map_err(|error| format!("连接失败：{error:#}"))?;
    let server_type = match check.preset.as_deref() {
        Some("openlist") => "OpenList".to_string(),
        Some("alist") => "AList".to_string(),
        Some("nextcloud") => "Nextcloud".to_string(),
        Some("jianguoyun") => "坚果云".to_string(),
        Some("synology") => "群晖 DSM".to_string(),
        _ => server_kind(check.server.as_deref(), &check.dav),
    };
    Ok(ConnectionTest {
        ok: check.ok,
        latency_ms: check.latency_ms,
        server: check.server,
        dav_class: check.dav_class,
        server_type,
        status: check.status,
        remote_dir_exists: check.ok && !check.remote_dir_missing,
        remote_dir_missing: check.remote_dir_missing,
        remote_dir: check.remote_dir,
        preset: check.preset,
        message: check.message,
        hint: check.hint,
    })
}

#[derive(Serialize)]
struct NetworkStatus {
    state: String,
    message: String,
    available: bool,
    metered: bool,
    failures: u32,
    next_retry_at: Option<u64>,
    next_retry_in_secs: Option<u64>,
}

/// SPEC-3 §B/C10：UI 展示网络状态与下次重试时间。
#[tauri::command]
fn cmd_network_status(app: AppHandle) -> Result<NetworkStatus, String> {
    let policy = NetworkPolicy::system();
    let state = policy.state();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let retry = RetryState::load(&state_dir(&app)?.join("retry.json")).unwrap_or_default();
    Ok(NetworkStatus {
        state: state.as_str().to_string(),
        message: state.message().to_string(),
        available: policy.available(),
        metered: policy.is_metered(),
        failures: retry.failures,
        next_retry_at: retry.next_retry_at,
        next_retry_in_secs: retry.next_retry_at.map(|next| next.saturating_sub(now)),
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
    let (_, client) = dav_client(&app, &state.0)?;

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
pub(crate) enum SyncMode {
    Transport,
    Apply,
}

pub(crate) fn run_sync(
    app: &AppHandle,
    inner: &Arc<Inner>,
    mode: SyncMode,
) -> Result<Report, String> {
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
    let mut config = load_config(app, inner)?;

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
    let sink_inner = Arc::clone(inner);
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
    // 冲突页需要“哪个键冲突”：把本轮 Report.conflicts 落到 state_dir。
    conflict_cmds::record_conflicts(app, &report.conflicts);
    let _ = app.emit("sync://done", &report);
    Ok(report)
}

// core 的 WebDAV 客户端是 reqwest::blocking，它会自带一个 tokio runtime；
// 在 async 上下文里 drop 会 panic（"Cannot drop a runtime..."）。
// 所以阻塞的同步流程一律放到 spawn_blocking 的线程里跑。
#[tauri::command]
async fn cmd_transport(app: AppHandle, state: State<'_, AppState>) -> Result<Report, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || run_sync(&app, &inner, SyncMode::Transport))
        .await
        .map_err(|error| format!("同步任务异常结束：{error}"))?
}

#[tauri::command]
async fn cmd_apply(app: AppHandle, state: State<'_, AppState>) -> Result<Report, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || run_sync(&app, &inner, SyncMode::Apply))
        .await
        .map_err(|error| format!("同步任务异常结束：{error}"))?
}

/// 预览同步（dry-run）：只 GET/HEAD/PROPFIND + 本地比较，不写本地、不写远端、不动 index。
///
/// 刻意不做的事：不存配置、不做备份、不记录 history/retry、不创建锁、不触发输入法。
#[tauri::command]
async fn cmd_plan_sync(app: AppHandle, state: State<'_, AppState>) -> Result<PlanReport, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || run_plan(&app, &inner))
        .await
        .map_err(|error| format!("预览任务异常结束：{error}"))?
}

pub(crate) fn run_plan(app: &AppHandle, inner: &Arc<Inner>) -> Result<PlanReport, String> {
    if inner
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有同步任务正在运行，请等待完成或先取消".to_string());
    }
    let outcome = run_plan_inner(app, inner);
    inner.running.store(false, Ordering::SeqCst);
    if let Err(error) = &outcome {
        push_log(inner, format!("预览失败：{error}"));
    }
    outcome
}

fn run_plan_inner(app: &AppHandle, inner: &Arc<Inner>) -> Result<PlanReport, String> {
    let mut config = load_config(app, inner)?;
    let detected = detect_rime(&config)?;
    // 只在内存里补全：预览不落盘，配置文件的字节数必须保持原样。
    if config.rime_dir.as_deref() != Some(detected.path.to_string_lossy().as_ref()) {
        config.rime_dir = Some(detected.path.display().to_string());
    }
    if config.device_id.trim().is_empty() {
        config.device_id = config.resolve_device_id(&detected.path);
    }
    config.device_id = config.device_id.trim().to_string();
    if let Err(error) = config.validate() {
        return Err(format!("配置不完整：{error:#}"));
    }

    // FakeTrigger：预览绝不调用 rime_dict_manager / WeaselDeployer / fcitx5。
    let trigger: Box<dyn rime_sync_core::platform::RimeTrigger> =
        Box::new(rime_sync_core::platform::FakeTrigger::new());
    let mut engine = Engine::new(config, detected.path, state_dir(app)?, trigger)
        .map_err(|error| format!("初始化同步引擎失败：{error:#}"))?;
    let report = engine.plan();
    push_log(
        inner,
        format!(
            "预览完成（未写入任何数据）：拉取 {}，推送 {}，冲突 {}，跳过 {}，耗时 {} ms",
            report.counts.pull,
            report.counts.push,
            report.counts.conflict,
            report.counts.skip,
            report.duration_ms
        ),
    );
    if let Some(error) = &report.error {
        push_log(inner, format!("预览错误：{error}"));
    }
    Ok(report)
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
    Ok(log_tail(&state.0, limit))
}

#[tauri::command]
fn cmd_needs_setup(app: AppHandle, _state: State<'_, AppState>) -> Result<bool, String> {
    let path = config_path(&app)?;
    if !path.exists() {
        return Ok(true);
    }
    // 启动路径：只看 URL 是否为空，绝不探测钥匙串。
    let config = Config::load(&path).map_err(|error| format!("读取配置失败：{error:#}"))?;
    Ok(config.webdav.url.trim().is_empty())
}

/// 前端启动时读取并清除 --setup 请求：true 表示本次必须进入同步设置向导。
///
/// 用 swap 而不是单纯读取：一次启动只强制打开一次，之后用户手动关闭不会被重新弹出。
/// 原生目录选择器（设置页 / 向导里的「浏览…」）。取消时返回 None。
#[tauri::command]
async fn cmd_pick_folder(app: AppHandle, title: Option<String>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        app2.dialog()
            .file()
            .set_title(title.unwrap_or_else(|| "选择 Rime 用户目录".to_string()))
            .pick_folder(move |path| {
                let _ = tx.send(path);
            });
        rx.recv_timeout(std::time::Duration::from_secs(600))
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
    .and_then(|path| path.into_path().ok())
    .map(|path| path.display().to_string())
}

#[tauri::command]
fn cmd_take_setup_request(state: State<'_, AppState>) -> bool {
    let requested = state.0.setup_request.swap(false, Ordering::SeqCst);
    if requested {
        push_log(&state.0, "前端已接管 --setup：打开同步设置向导");
        eprintln!(
            "[rime-sync] setup wizard: frontend consumed --setup request; opening sync setup dialog"
        );
    }
    requested
}

#[derive(Serialize)]
struct SecretStatus {
    backend: String,
    has_password: bool,
    note: Option<String>,
}

#[tauri::command]
fn cmd_secret_status(app: AppHandle, state: State<'_, AppState>) -> Result<SecretStatus, String> {
    let selection = secret_selection(&app, &state.0)?;
    let config = load_config(&app, &state.0)?;
    Ok(SecretStatus {
        backend: selection.backend.as_str().to_string(),
        has_password: !config.webdav.password.is_empty(),
        note: selection.note.clone(),
    })
}

#[derive(Serialize)]
struct FirstRunView {
    report: Report,
    strategy: String,
    note: Option<String>,
}

/// 首次运行向导第 4 步：跑一次传输。
///
/// pull = 只拉取：用临时 device_id + 关闭全部 sync_kinds 运行 transport。
/// 这样引擎会拉取所有远端快照，但不写 shared、也不上传本机快照；
/// 运行结束后删除临时远端目录（只有 owner.json）。
#[tauri::command]
async fn cmd_first_transport(
    app: AppHandle,
    state: State<'_, AppState>,
    strategy: String,
) -> Result<FirstRunView, String> {
    let inner = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || first_transport(&app, &inner, &strategy))
        .await
        .map_err(|error| format!("首次同步异常结束：{error}"))?
}

fn first_transport(
    app: &AppHandle,
    inner: &Arc<Inner>,
    strategy: &str,
) -> Result<FirstRunView, String> {
    match strategy.trim() {
        "bidirectional" => {
            let report = run_sync(app, inner, SyncMode::Transport)?;
            Ok(FirstRunView {
                report,
                strategy: "bidirectional".to_string(),
                note: None,
            })
        }
        "pull" => {
            let rime = rime_dir_for(app, inner)?;
            let state_path = state_dir(app)?;
            let backups = BackupManager::new(&rime, &state_path.join("backups"));
            let backup_id = backups
                .create("pre-first-pull")
                .map_err(|error| format!("首次拉取前备份失败：{error:#}"))?;
            // 只在内存里换临时 device_id：不写 config.json，
            // 这样即使中途失败也不会把用户的设备 ID / 同步范围改坏。
            let mut config = load_config(app, inner)?;
            if config.device_id.trim().is_empty() {
                return Err("设备 ID 尚未生成，无法执行首次拉取".to_string());
            }
            let temporary = format!("pull-{}", rime_sync_core::backup::now_secs());
            config.device_id = temporary.clone();
            config.sync_kinds.userdict = false;
            config.sync_kinds.custom_yaml = false;
            config.sync_kinds.dict_sources = false;

            let trigger = platform::default_trigger(config.stop_fcitx5_during_apply);
            let mut engine = Engine::new(config, rime, state_path, trigger)
                .map_err(|error| format!("初始化同步引擎失败：{error:#}"))?;
            let mut report = engine.transport();
            report.backup_id = Some(backup_id);

            let (_, client) = dav_client(app, inner)?;
            let cleanup = client.delete(&format!("sync/{temporary}"));
            let mut note = "只拉取：本次没有上传本机文件。".to_string();
            if let Err(error) = cleanup {
                note.push_str(&format!(
                    "临时远端目录 sync/{temporary} 清理失败（可手动删除）：{error:#}"
                ));
            }
            Ok(FirstRunView {
                report,
                strategy: "pull".to_string(),
                note: Some(note),
            })
        }
        other => Err(format!("未知的首次同步策略：{other}")),
    }
}

const SETUP_FLAG: &str = "--setup";
const AUTOSTART_FLAG: &str = "--autostart";
/// 旧版自启条目用的参数，继续兼容：行为与 --autostart 相同。
const MINIMIZED_FLAG: &str = "--minimized";

/// 安装完成后用 --setup 启动：无论是否首次运行，都强制进入同步设置向导。
fn setup_requested(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|arg| arg == SETUP_FLAG)
}

/// 自启启动参数：--autostart（自启条目写入的参数）或旧的 --minimized。
/// 命中时只驻留托盘，不显示主窗口。
fn autostart_requested(args: impl IntoIterator<Item = String>) -> Option<&'static str> {
    let mut minimized = false;
    for arg in args {
        if arg == AUTOSTART_FLAG {
            return Some(AUTOSTART_FLAG);
        }
        if arg == MINIMIZED_FLAG {
            minimized = true;
        }
    }
    if minimized {
        Some(MINIMIZED_FLAG)
    } else {
        None
    }
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = app.state::<AppState>();
            push_log(&state.0, "Rime 同步已启动");
            let setup_requested = setup_requested(std::env::args());
            state
                .0
                .setup_request
                .store(setup_requested, Ordering::SeqCst);
            if setup_requested {
                push_log(&state.0, "检测到 --setup：强制进入同步设置向导");
                eprintln!("[rime-sync] setup wizard: --setup detected; forcing sync setup dialog");
            }
            // 托盘在 headless / 缺 appindicator 时会创建失败：只降级记日志，不 panic。
            match tray::setup(&handle) {
                Ok(()) => push_log(&state.0, "托盘已就绪（关闭窗口默认隐藏到托盘）"),
                Err(error) => {
                    push_log(&state.0, format!("托盘不可用，关闭窗口将直接退出：{error}"))
                }
            }
            // 开机自启：只建托盘，不弹主窗口。托盘没建起来时保留窗口，
            // 否则会变成既没有窗口也没有托盘的僵尸进程。
            if let Some(flag) = autostart_requested(std::env::args()) {
                if tray::ready() {
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.hide();
                        push_log(
                            &state.0,
                            format!("以 {flag} 启动：不显示主窗口，仅驻留托盘"),
                        );
                        eprintln!("[rime-sync] autostart: started with {flag}; main window hidden");
                    }
                } else {
                    push_log(
                        &state.0,
                        format!("以 {flag} 启动但托盘不可用：保留主窗口，避免无界面可操作"),
                    );
                }
            }
            Ok(())
        })
        // SPEC-4 §B2：关闭窗口默认隐藏到托盘；托盘不可用或 close_to_tray=false 时放行退出。
        .on_window_event(|window, event| {
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            if window.label() != "main" {
                return;
            }
            let app = window.app_handle();
            let state = app.state::<AppState>();
            if platform_prefs::should_hide_on_close(app, &state.0) {
                api.prevent_close();
                let _ = window.hide();
                push_log(&state.0, "窗口已隐藏到托盘（托盘菜单「退出」才真正退出）");
            }
        })
        .invoke_handler(tauri::generate_handler![
            cmd_get_config,
            cmd_set_config,
            cmd_detect_rime_dir,
            cmd_test_connection,
            cmd_network_status,
            cmd_list_remote,
            cmd_transport,
            cmd_apply,
            cmd_plan_sync,
            cmd_cancel,
            cmd_get_log,
            cmd_needs_setup,
            cmd_take_setup_request,
            cmd_pick_folder,
            cmd_secret_status,
            cmd_first_transport,
            conflict_cmds::cmd_list_conflicts,
            conflict_cmds::cmd_resolve_conflict,
            conflict_cmds::cmd_conflict_remote_sizes,
            device_cmds::cmd_list_devices,
            device_cmds::cmd_revoke_device,
            device_cmds::cmd_reset_local_remote_dir,
            device_cmds::cmd_local_identity,
            device_cmds::cmd_check_device_id,
            device_cmds::cmd_migrate_device,
            device_cmds::cmd_publish_identity,
            dedupe_cmds::cmd_dedupe_preview,
            dedupe_cmds::cmd_dedupe_apply,
            backup_cmds::cmd_list_backups,
            backup_cmds::cmd_create_backup,
            backup_cmds::cmd_restore_backup,
            backup_cmds::cmd_delete_backup,
            backup_cmds::cmd_prune_backups,
            preset_cmds::cmd_list_presets,
            preset_cmds::cmd_apply_preset,
            schema_cmds::cmd_list_schemas,
            schema_cmds::cmd_set_schemas,
            preset_cmds::cmd_probe_write,
            diagnostics::cmd_export_diagnostics,
            // 离线依赖安装告警（packaging/linux/postinst 写 install-warning.json）
            diagnostics::cmd_install_warning,
            diagnostics::cmd_retry_install_deps,
            watch_cmds::cmd_watch_start,
            watch_cmds::cmd_watch_stop,
            watch_cmds::cmd_watch_status,
            update_cmds::cmd_check_update,
            update_cmds::cmd_install_update,
            tray::cmd_tray_status,
            autostart::cmd_autostart_status,
            autostart::cmd_set_autostart,
            platform_prefs::cmd_get_close_to_tray,
            platform_prefs::cmd_set_close_to_tray
        ]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("rime-sync 启动失败：{error}");
        std::process::exit(1);
    }
}
