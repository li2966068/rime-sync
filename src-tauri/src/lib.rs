//! Tauri 薄壳：命令桥 + 配置路径解析 + 事件转发。
//!
//! 同步逻辑（文件分类、三向合并、WebDAV、librime 触发）全部在 rime-sync-core；
//! 本文件只负责把命令参数翻译成引擎调用，并把进度/结果转发给前端。

// 非测试代码一律不允许 unwrap/expect（命令层错误一律转成 anyhow/String 上报前端）。
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rime_sync_core::backup::BackupManager;
use rime_sync_core::cancel::CancelToken;
use rime_sync_core::config::{self, Config};
use rime_sync_core::engine::{Engine, PlanReport, Progress, ProgressSink, Report};
use rime_sync_core::network::NetworkPolicy;
use rime_sync_core::platform;
use rime_sync_core::proxy::PROXY_PASSWORD_ACCOUNT;
use rime_sync_core::retry::RetryState;
use rime_sync_core::rime_dir;
use rime_sync_core::secret::{self, SecretSelection};
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

mod app_log;
mod autostart;
mod backup_cmds;
mod conflict_cmds;
/// 冲突解决的文件操作（覆盖前备份、删副本前归档）：单独成模块便于集成测试。
pub mod conflict_files;
mod dedupe_cmds;
mod device_cmds;
mod diagnostics;
mod extra_ime;
/// 交给前端的配置（地址去凭据、密码不外泄）：单独成模块便于集成测试。
pub mod frontend_config;
/// 同步历史（只读）：把 core 写下的 history.jsonl 读给前端看。
pub mod history_cmds;
mod ime_text;
/// 共享状态目录的解析 + 旧目录的一次性迁移（单独成模块便于集成测试）。
pub mod paths;
mod platform_prefs;
mod prebuilt_cmds;
mod preset_cmds;
pub mod prune_cmds;
mod remote_tree;
mod schema_cmds;
mod tray;
mod update_cmds;
mod watch_cmds;

const LOG_CAPACITY: usize = 1000;

/// --setup 短命槽位的有效期：第二次启动带来的请求，超过这个时间就当没来过。
///
/// 槽位是**进程内**的（不落盘），所以程序重启后必然为空 ——
/// 这正是要的：落盘的话就会变成「下次重启程序时莫名弹出设置向导」。
const SETUP_SLOT_TTL: Duration = Duration::from_secs(5 * 60);

pub(crate) struct Inner {
    pub(crate) cancel: Mutex<Option<CancelToken>>,
    pub(crate) running: AtomicBool,
    /// 启动参数 --setup：前端取走一次后清除，保证只强制打开一次向导。
    pub(crate) setup_request: AtomicBool,
    /// 第二次启动带来的 --setup：短命槽位（到期时间），前端注册监听后再兜底取一次。
    pub(crate) setup_slot: Mutex<Option<Instant>>,
    /// 启动参数 --panel <页面>：前端取走一次后清除，保证只切一次页面。
    pub(crate) panel_request: Mutex<Option<String>>,
    pub(crate) log: Mutex<Vec<String>>,
    pub(crate) secret: Mutex<Option<Arc<SecretSelection>>>,
    pub(crate) watch: Mutex<Option<watch_cmds::WatchSession>>,
    /// 最近一次启动自举的输入法状态（设置页红条读它）。
    pub(crate) ime: Mutex<Option<prebuilt_cmds::ImeBootstrapStatus>>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            cancel: Mutex::new(None),
            running: AtomicBool::new(false),
            setup_request: AtomicBool::new(false),
            setup_slot: Mutex::new(None),
            panel_request: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            secret: Mutex::new(None),
            watch: Mutex::new(None),
            ime: Mutex::new(None),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct AppState(pub(crate) Arc<Inner>);

pub(crate) fn push_log(inner: &Inner, message: impl Into<String>) {
    let line = app_log::stamp(&message.into());
    if let Ok(mut log) = inner.log.lock() {
        log.push(line.clone());
        if log.len() > LOG_CAPACITY {
            let excess = log.len() - LOG_CAPACITY;
            log.drain(0..excess);
        }
    }
    // 同一份内容落盘：内存里那 1000 行只是给界面看的，程序崩了之后能查的是文件。
    app_log::append_line(&line);
}

/// 应用自己的配置目录（**历史位置**，状态已经搬走了）。
///
/// 里面只剩两样东西：用户的 config.json（**路径不动** —— 搬配置等于丢用户配置）
/// 与 Linux postinst 写的 install-warning.json（安装器写死在应用配置目录里，
/// 应用仍然从这里读它）。
pub(crate) fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("无法解析应用配置目录：{error}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建配置目录 {} 失败：{error}", dir.display()))?;
    Ok(dir)
}

/// 共享状态目录：桌面端与 CLI 共用同一份基线（index.json）与还原点（backups/）。
///
/// 为什么不再用应用自己的配置目录：CLI 的状态目录是「配置文件所在目录」
/// （`~/.config/rime-sync`），桌面端原来用的是应用配置目录
/// （`~/.config/com.rimesync.app`）—— 两端各存一份 index.json，
/// 基线算不到一处去，于是同一台机器上反复出现假冲突。
pub(crate) fn state_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = match paths::shared_state_dir() {
        Ok(dir) => dir,
        Err(_error) => {
            // 环境变量一个都没有（极少见）：退回系统配置目录，
            // 宁可让两端不一致，也不要让应用起不来。
            app.path()
                .config_dir()
                .map_err(|fallback_error| format!("无法定位用户配置目录：{fallback_error}"))?
                .join(paths::SHARED_DIR_NAME)
        }
    };
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建状态目录 {} 失败：{error}", dir.display()))?;
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

/// 配置文件路径：**保持原样**（仍在应用配置目录里，升级不搬用户的配置）。
pub(crate) fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("config.json"))
}

/// 读配置文件（磁盘原值，**不含**环境变量覆盖）。
fn read_stored_config(path: &Path) -> Result<Config, String> {
    Config::load(path).map_err(|error| format!("读取配置失败：{error:#}"))
}

/// 读配置文件并套用 `RIME_SYNC_*` 环境变量覆盖。
///
/// 与 CLI（crates/cli/src/main.rs）同口径：环境变量覆盖配置文件里的对应字段，
/// 且只在内存里生效（CLI 里就是 stored/config 两份，磁盘那份始终是用户自己填的值）。
/// 桌面端所有读配置的地方都必须走这里，否则会出现「有的界面认环境变量、有的不认」。
fn read_effective_config(path: &Path) -> Result<Config, String> {
    let mut config = read_stored_config(path)?;
    config.apply_env();
    Ok(config)
}

/// 哪些字段正被环境变量接管。
struct EnvOverrides {
    url: bool,
    username: bool,
    remote_dir: bool,
    rime_dir: bool,
    first_sync_policy: bool,
}

impl EnvOverrides {
    /// 用「原值 vs 套过 apply_env 的值」做对比，而不是另抄一份变量清单：
    /// core::apply_env 增删变量时这里自动跟着变，不会两处漂移。
    fn detect(stored: &Config) -> Self {
        let mut probe = stored.clone();
        probe.apply_env();
        Self {
            url: probe.webdav.url != stored.webdav.url,
            username: probe.webdav.username != stored.webdav.username,
            remote_dir: probe.webdav.remote_dir != stored.webdav.remote_dir,
            rime_dir: probe.rime_dir != stored.rime_dir,
            first_sync_policy: probe.first_sync_policy != stored.first_sync_policy,
        }
    }

    /// 给人看的名字（只用于日志提示，永远不含密码）。
    fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.url {
            names.push("WebDAV 地址");
        }
        if self.username {
            names.push("WebDAV 用户名");
        }
        if self.remote_dir {
            names.push("远端目录");
        }
        if self.rime_dir {
            names.push("Rime 目录");
        }
        if self.first_sync_policy {
            names.push("首次同步策略");
        }
        names
    }
}

/// 落盘配置：被环境变量接管的字段保持磁盘上的原值。
///
/// 为什么：环境变量是**运行时**覆盖（与 CLI 的 stored/config 两份同口径）——
/// 它决定「这次用哪个地址」，但不该改写用户自己的配置文件；否则用户撤掉环境变量后，
/// 配置文件里还留着被固化的那份值，就再也回不去了。
/// 密码本来就不会落盘（webdav.password 不序列化），环境变量里的密码同样只在内存里。
pub(crate) fn save_config(path: &Path, config: &Config) -> Result<(), String> {
    let stored = read_stored_config(path)?;
    let overrides = EnvOverrides::detect(&stored);
    let mut to_save = config.clone();
    if overrides.url {
        to_save.webdav.url = stored.webdav.url.clone();
    }
    if overrides.username {
        to_save.webdav.username = stored.webdav.username.clone();
    }
    if overrides.remote_dir {
        to_save.webdav.remote_dir = stored.webdav.remote_dir.clone();
    }
    if overrides.rime_dir {
        to_save.rime_dir = stored.rime_dir.clone();
    }
    if overrides.first_sync_policy {
        to_save.first_sync_policy = stored.first_sync_policy;
    }
    to_save
        .save(path)
        .map_err(|error| format!("写入配置失败：{error:#}"))
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
    let mut config = read_stored_config(&path)?;
    if config.legacy_password {
        match config.migrate_legacy_password(selection.store.as_ref(), &path) {
            Ok(true) => push_log(inner, "已把旧配置里的明文密码迁移到密码后端"),
            Ok(false) => {}
            Err(error) => push_log(inner, format!("迁移旧密码失败：{error:#}")),
        }
    }
    // 环境变量覆盖放在解析密码**之前**：内存里的密码优先于密码后端，
    // 这样 RIME_SYNC_DAV_PASS 的优先级与 CLI 完全一致（CLI 也是先 apply_env 再 load_password）。
    config.apply_env();
    if let Err(error) = config.load_password(selection.store.as_ref()) {
        push_log(inner, format!("读取密码失败：{error:#}"));
    }
    if let Err(error) = config.load_proxy_password(selection.store.as_ref()) {
        push_log(inner, format!("读取代理密码失败：{error:#}"));
    }
    Ok(config)
}

/// 配置里显式填的 Rime 目录（空 = 没填，走自动探测）。
fn configured_rime_dir(config: &Config) -> Option<&str> {
    config
        .rime_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// 解析 Rime 用户目录。
///
/// 用户手填的目录不存在时**明确报错**，不再悄悄换成探测到的另一个目录：
/// 悄悄换目录 = 用户以为在同步 A 机器、其实在同步 B 机器，而他完全不知情。
/// 口径与 CLI（crates/cli/src/main.rs::resolve_rime_dir）一致，同样做 canonicalize，
/// 软链目录在两端显示同一个真实路径。
pub(crate) fn detect_rime(config: &Config) -> Result<rime_dir::Detection, String> {
    match configured_rime_dir(config) {
        Some(dir) => {
            let path = config::resolve_configured_rime_dir(Some(dir))
                .map_err(|error| format!("{error:#}"))?;
            Ok(rime_dir::Detection {
                path,
                source: "config".to_string(),
            })
        }
        None => detect_rime_fresh(),
    }
}

/// 自动探测 Rime 用户目录（忽略配置里那个可能已经失效的路径）。
///
/// 「自动探测」按钮必须走这里：用户点它就是为了救回一个填错的路径，
/// 拿配置里的坏路径去探测只会再报一次同样的错。
pub(crate) fn detect_rime_fresh() -> Result<rime_dir::Detection, String> {
    let detection = rime_dir::detect(None).map_err(|error| {
        format!("未能定位 Rime 用户目录：{error:#}。请在「同步」页点击自动探测或手动填写目录路径。")
    })?;
    // 与 CLI 同口径：canonicalize 之后软链目录两边显示同一个路径。
    let path = std::fs::canonicalize(&detection.path).map_err(|error| {
        format!(
            "无法解析 Rime 用户目录的真实路径：{}（{error}）",
            detection.path.display()
        )
    })?;
    Ok(rime_dir::Detection {
        path,
        source: detection.source,
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
    let config = read_effective_config(&path)?;
    ensure_device_id(app, inner, config)
}

/// 「尽力而为」地拿 Rime 目录：配置里的目录失效时退回自动探测。
///
/// 只用于**派生信息**（设备 ID 这类「拿不到也无害」的场合）：设备 ID 要跟
/// installation.yaml 对齐，配置路径填错时不能因此把设备 ID 换成随机值
/// （那会让同步落到另一个远端目录）。真正要读写用户目录的操作一律走 detect_rime。
pub(crate) fn detect_rime_lenient(config: &Config) -> Option<rime_dir::Detection> {
    detect_rime(config)
        .ok()
        .or_else(|| detect_rime_fresh().ok())
}

fn ensure_device_id(app: &AppHandle, inner: &Inner, mut config: Config) -> Result<Config, String> {
    let path = config_path(app)?;
    if config.device_id.trim().is_empty() {
        let detected = detect_rime_lenient(&config);
        config.device_id = match detected {
            Some(detection) => config.resolve_device_id(&detection.path),
            None => config::generate_device_id(),
        };
        save_config(&path, &config)?;
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
    let client = DavClient::with_proxy(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
        Some(&config.proxy),
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
    let config = ensure_config_meta(&app, &state.0)?;
    // 配置里的 Rime 目录可能已被删除（或来自旧机器/旧测试）。
    // 这里**只提示、不改配置**：把用户手填的路径清空并写回磁盘 = 静默吃掉用户的输入，
    // 用户根本不知道自己填错了（缺陷 D1）。要修就在设置页改，或点「自动探测」。
    if let Some(problem) = config::rime_dir_problem(config.rime_dir.as_deref()) {
        push_log(&state.0, problem);
    }
    // 磁盘上的地址还带着凭据 ⇒ 说明这台机器的密码后端用不了，密码只能活在
    // 地址里。界面上的地址已经隐去账号密码（见 frontend_config），这里告诉
    // 用户一声，免得以为密码被清掉了。提示里绝不含密码本身。
    if rime_sync_core::webdav::has_userinfo(&config.webdav.url) {
        push_log(
            &state.0,
            "WebDAV 密码目前保存在地址里：界面上的地址已隐去账号密码，保存时会原样保留（若密码后端可用，下次启动会自动搬进钥匙串）",
        );
    }
    // 交给前端的配置：地址里不许有 `user:pass`，密码字段也不许是明文。
    Ok(frontend_config::sanitize(config))
}

#[tauri::command]
fn cmd_set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    mut config: Config,
) -> Result<Config, String> {
    config.webdav.url = config.webdav.url.trim().to_string();
    config.webdav.username = config.webdav.username.trim().to_string();
    // 地址里带账号密码（https://user:pass@host/dav）：**不再直接拒绝**，
    // 而是自己拆开（与 CLI 完全一致）：账号填进「用户名」，密码存进密码后端，
    // 地址只留主机与路径 ⇒ 密码从此不进配置文件、不进日志、不进诊断包。
    //
    // 拆不出来（畸形写法）或账号跟「用户名」字段打架时才报错。
    let mut url_password = String::new();
    if rime_sync_core::webdav::has_userinfo(&config.webdav.url) {
        match rime_sync_core::webdav::split_userinfo(&config.webdav.url) {
            Some(creds) => {
                let typed = config.webdav.username.trim().to_string();
                if !typed.is_empty() && !creds.username.is_empty() && typed != creds.username {
                    return Err(
                        "WebDAV 地址里的账号与「用户名」字段填的账号不一致，请把账号与密码分别填在「用户名」「密码」里"
                            .to_string(),
                    );
                }
                if typed.is_empty() {
                    config.webdav.username = creds.username.clone();
                }
                url_password = creds.password.clone().unwrap_or_default();
                config.webdav.url = creds.clean_url.clone();
            }
            None => {
                return Err(
                    "WebDAV 地址里的账号密码解析不出来，请把账号与密码分别填在「用户名」「密码」里"
                        .to_string(),
                );
            }
        }
    }
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
    config.proxy.url = config.proxy.url.trim().to_string();
    config.proxy.username = config.proxy.username.trim().to_string();
    // 非法代理地址必须在这里拒绝：错误信息直接回到界面，配置文件保持原样。
    // 未启用时允许地址为空/不完整（用户可能刚清空还没填新的）。
    if config.proxy.is_active() {
        config
            .proxy
            .spec()
            .map_err(|error| format!("代理配置无效：{error:#}"))?;
    }
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
    let mut typed_password = std::mem::take(&mut config.webdav.password);
    // 地址里带出来的密码（用户把 `https://user:pass@host/dav` 粘进了地址框）
    // ⇒ 与手工填进「密码」框完全同等待遇：一样存进密码后端。
    if typed_password.is_empty() && !url_password.is_empty() {
        typed_password = url_password;
    }
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
        if config.webdav.password.is_empty() {
            // 密码后端里没有这个账号的密码 ⇒ 可能是「降级模式」：
            // 密码只存在于磁盘配置的地址里（`https://user:pass@host/dav`）。
            // 这时绝不能让用户改个地址就把唯一那份密码弄丢 ——
            // 把地址里的凭据随新地址一起搬过去（地址更新，密码也还在）。
            //
            // 磁盘原值从**当前进程重新读出的配置**里取，密码从磁盘读，
            // 不从界面那边取（界面上根本没有密码）。
            if let Ok(disk) = read_stored_config(&path) {
                if config.carry_webdav_credentials(&disk) {
                    push_log(
                        &state.0,
                        "系统密码后端不可用：已把地址里的账号密码随新地址一起保留（密码仍在地址里，未写进日志）",
                    );
                }
            }
        }
    } else {
        // 存进密码后端。存不进去（这台机器没有可用的系统钥匙串）⇒ 别把密码丢掉：
        // 退回到「密码留在地址里」的老形态（磁盘上的地址仍然带凭据，
        // 但日志/诊断包里不会出现明文），功能照样能用。
        match selection
            .store
            .set(&config.password_account(), &typed_password)
        {
            Ok(()) => {
                config.password_ref = selection.backend.password_ref();
                config.webdav.password = typed_password;
                push_log(&state.0, "密码已写入系统密码后端");
            }
            Err(error) => {
                config.webdav.url = config::url_with_userinfo(
                    &config.webdav.url,
                    &config.webdav.username,
                    &typed_password,
                );
                config.webdav.password = typed_password;
                push_log(
                    &state.0,
                    format!(
                        "系统密码后端不可用（{error:#}）：密码已保留在 WebDAV 地址里（未写进日志）"
                    ),
                );
            }
        }
    }

    // 代理密码：与 WebDAV 同一套语义——前端只拿得到空字段，空 = 沿用后端已有值。
    let typed_proxy_password = std::mem::take(&mut config.proxy.password);
    if typed_proxy_password.is_empty() {
        if config.proxy.password_ref != config::PasswordRef::None {
            if let Ok(Some(secret)) = selection.store.get(PROXY_PASSWORD_ACCOUNT) {
                config.proxy.password = secret;
            }
        }
    } else {
        config
            .proxy
            .store_password(selection.store.as_ref(), &typed_proxy_password)
            .map_err(|error| format!("保存代理密码失败：{error:#}"))?;
        push_log(&state.0, "代理密码已写入系统密码后端");
    }
    // 拿到「最终生效」的密码后再校验一次（用户名缺失时密码不会被发出去）。
    config
        .proxy
        .validate()
        .map_err(|error| format!("代理配置无效：{error:#}"))?;

    // 落盘：被环境变量接管的字段保持磁盘上的原值（环境变量是运行时覆盖，不改写用户配置）。
    // 顺手告诉用户「这几项改不动」，免得他以为是自己没保存成功。
    if let Ok(path) = config_path(&app) {
        if let Ok(stored) = read_stored_config(&path) {
            let names = EnvOverrides::detect(&stored).names();
            if !names.is_empty() {
                push_log(
                    &state.0,
                    format!(
                        "以下配置项由环境变量接管，配置文件里的值保持不变：{}",
                        names.join("、")
                    ),
                );
            }
        }
    }
    save_config(&path, &config)?;
    push_log(&state.0, "配置已保存");
    // 返回给界面的是**生效值**（含环境变量覆盖）：界面显示的必须等于真正会用上的。
    let mut effective = config;
    effective.apply_env();
    // 密码不进界面：前端只要知道「密码已设置」，具体值它拿不到、也不需要。
    effective.webdav.password = String::new();
    effective.proxy.password = String::new();
    Ok(effective)
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
    // 配置里的目录还能用就照旧用它；已经失效（用户填错 / 机器换了）时退回真正的
    // 自动探测 —— 这个按钮就是用来把坏路径救回来的，不能因为它坏就什么都做不了。
    let detection = match configured_rime_dir(&config) {
        Some(dir) if Path::new(dir).is_dir() => detect_rime(&config)?,
        _ => detect_rime_fresh()?,
    };
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
    let client = DavClient::with_proxy(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
        Some(&config.proxy),
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
    /// 本次结果是不是真的探测出来的。false = 还没探测过（只回了默认值）。
    detected: bool,
    failures: u32,
    next_retry_at: Option<u64>,
    next_retry_in_secs: Option<u64>,
}

/// SPEC-3 §B/C10：UI 展示网络状态与下次重试时间。
///
/// **这里不探测**。Windows 上探测「是否按流量计费」要起一个 powershell.exe，
/// 以前这条命令被界面刷新和同步结束后的自动刷新带着跑，等于每 10 分钟
/// 白白起两个 PowerShell 进程。现在只有用户点「检测网络状态」才探一次。
#[tauri::command]
fn cmd_network_status(app: AppHandle) -> Result<NetworkStatus, String> {
    let policy = NetworkPolicy::system();
    // 只回已知结论，绝不启动子进程；没检测过就是 None。
    let snapshot = policy.last_known_snapshot();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let retry = RetryState::load(&state_dir(&app)?.join("retry.json")).unwrap_or_default();
    Ok(NetworkStatus {
        state: snapshot
            .map(|item| item.state.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        message: snapshot
            .map(|item| item.state.message().to_string())
            .unwrap_or_else(|| "尚未检测网络状态".to_string()),
        // 没检测过时按既有语义兜底：可用 / 非计量（绝不能因此阻止同步）。
        available: snapshot.map(|item| item.available).unwrap_or(true),
        metered: snapshot.map(|item| item.metered).unwrap_or(false),
        detected: snapshot.is_some(),
        failures: retry.failures,
        next_retry_at: retry.next_retry_at,
        next_retry_in_secs: retry.next_retry_at.map(|next| next.saturating_sub(now)),
    })
}

/// 用户点「检测网络状态」时才调用：真正探一次，拿到全部结论。
///
/// 一次探测就够了（available / metered / state 都从同一份结果里取），
/// 不像以前那样 state()、available()、is_metered() 各探一遍。
#[tauri::command]
fn cmd_probe_network_status(app: AppHandle) -> Result<NetworkStatus, String> {
    let snapshot = NetworkPolicy::system().probe_once();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let retry = RetryState::load(&state_dir(&app)?.join("retry.json")).unwrap_or_default();
    Ok(NetworkStatus {
        state: snapshot.state.as_str().to_string(),
        message: snapshot.state.message().to_string(),
        available: snapshot.available,
        metered: snapshot.metered,
        detected: true,
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
    // 只在「本来就没填」时补写：用户手填的路径原样保留（canonicalize 只是
    // 解析口径，不该反过来改写用户的输入）。
    if config
        .rime_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        config.rime_dir = Some(rime_path.display().to_string());
    }
    if config.device_id.trim().is_empty() {
        config.device_id = config.resolve_device_id(&rime_path);
    }
    config.device_id = config.device_id.trim().to_string();
    // 落盘用 save_config：环境变量接管的字段不被固化进配置文件（与 CLI 同口径）。
    save_config(&path, &config)?;
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
    // 上传冲突（远端词库已被别的设备改写）也算失败：
    // 用户看到的必须是「没推上去」，不能是「成功」。
    let verdict = if report.ok && !report.upload_conflict {
        "成功"
    } else {
        "失败"
    };
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
    // 启动路径：只看 URL 是否为空，绝不探测钥匙串。
    // 环境变量也算「已经配置过」：RIME_SYNC_DAV_URL 给出了地址就不必再进向导。
    let config = read_effective_config(&path)?;
    Ok(config.webdav.url.trim().is_empty())
}

/// 原生目录选择器（设置页 / 向导里的「浏览…」）。取消时返回 None。
///
/// 不占 worker 线程：原生对话框的回调跑在插件自己起的线程上，
/// 这里用一个异步通道把结果送回来，命令本身 await 即可。
/// （旧写法是 spawn_blocking + 10 分钟超时：用户不点「取消」时那个线程会被占住十分钟，
///  六个 spawn_blocking 命令一起排队，表现就是「点了没反应」。）
///
/// 为什么不再设超时：等待期间占的是「异步任务」而不是「线程池名额」，
/// 对另外五个命令没有任何影响；而插件的回调在「用户选定」和「用户取消」时
/// 都会准时送达（取消 = None），所以「等不到结果」只会出现在对话框压根没弹出来的
/// 异常情况上。为了不把用户选好的目录丢掉，这里选择等到底而不是超时放弃。
#[tauri::command]
async fn cmd_pick_folder(app: AppHandle, title: Option<String>) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    tracing::info!("打开原生目录选择器");
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    app.dialog()
        .file()
        .set_title(title.unwrap_or_else(|| "选择 Rime 用户目录".to_string()))
        .pick_folder(move |path| {
            // 通道容量 1 且只回一次，try_send 不会失败。
            // 万一前端已经不等了（命令被丢弃），把用户选中的目录记进日志，
            // 别让它悄无声息地消失。
            if tx.try_send(path.clone()).is_err() {
                if let Some(picked) = path {
                    tracing::info!("前端已不再等待，用户选中的目录：{}", picked);
                }
            }
        });
    let picked = rx.recv().await.flatten();
    match picked.and_then(|path| path.into_path().ok()) {
        Some(path) => {
            tracing::info!("用户选定了目录：{}", path.display());
            Some(path.display().to_string())
        }
        None => {
            tracing::info!("用户取消了目录选择");
            None
        }
    }
}

/// 把「切到某页」的请求交给前端。
///
/// 两条路径共用它：
/// - 首次启动：命令行里的 --panel（前端可能还没加载完）
/// - 已有实例在跑：单实例插件把第二次启动的参数转过来（前端早就在跑了）
///
/// 状态里放一份（前端启动时取走），同时发一个事件（前端已在跑时用）。
/// 两条都留：事件可能早于前端注册监听，状态则能兜住那一刻的请求。
fn request_panel(app: &AppHandle, panel: Option<String>, source: &str) {
    let Some(panel) = panel else { return };
    let inner = &app.state::<AppState>().0;
    if let Ok(mut slot) = inner.panel_request.lock() {
        *slot = Some(panel.clone());
    }
    push_log(inner, format!("收到切页请求：{panel}（来源：{source}）"));
    let _ = app.emit("panel://request", panel.clone());
}

/// 前端启动时读取并清除 --setup 请求：true 表示本次必须进入同步设置向导。
///
/// 用 swap 而不是单纯读取：一次启动只强制打开一次，之后用户手动关闭不会被重新弹出。
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

/// 记下一次「第二次启动带来的 --setup」请求（短命槽位）。
///
/// 与 `setup_request` 状态位分开：那个位是「本次启动的参数」（只在本进程启动时读一次），
/// 这个槽位是「运行期间收到的请求」，前端注册完监听后还要再兜底取一次。
/// 槽位只在进程内活着且带有效期 → 程序重启后读不到，不会莫名再弹向导。
fn write_setup_slot(inner: &Inner) {
    if let Ok(mut slot) = inner.setup_slot.lock() {
        *slot = Some(Instant::now() + SETUP_SLOT_TTL);
    }
}

/// 取出短命槽位：读到即清（只开一次向导），过期的当作没有。
fn take_setup_slot(inner: &Inner) -> bool {
    let deadline = match inner.setup_slot.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    match deadline {
        Some(deadline) => Instant::now() <= deadline,
        None => false,
    }
}

/// 短命槽位：前端注册完监听后再兜底取一次（取走即清）。
#[tauri::command]
fn cmd_take_setup_slot(state: State<'_, AppState>) -> bool {
    let requested = take_setup_slot(&state.0);
    if requested {
        push_log(&state.0, "前端已接管 --setup 短命槽位：打开同步设置向导");
        eprintln!(
            "[rime-sync] setup wizard: frontend consumed the short-lived --setup slot; opening sync setup dialog"
        );
    }
    requested
}

/// --panel 请求：读取即清除，返回要切换到的页面名（没有则 None）。
#[tauri::command]
fn cmd_take_panel_request(state: State<'_, AppState>) -> Option<String> {
    let requested = state
        .0
        .panel_request
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    if let Some(panel) = &requested {
        push_log(&state.0, format!("前端已接管 --panel：切换到 {panel} 页"));
        eprintln!("[rime-sync] panel request: frontend consumed --panel {panel}");
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
/// 启动参数 --panel <页面>：唤起主程序并直接落到指定页面。
const PANEL_FLAG: &str = "--panel";

/// 五个可切换的页面（与 index.html 的 data-panel 一致）。
const PANEL_NAMES: [&str; 5] = ["connection", "sync", "conflicts", "devices", "settings"];

/// 解析 --panel <页面>：返回合法的页面名，缺失或非法时返回 None。
///
/// 入口侧（输入法配置页、开始菜单、托盘）用它把用户送到指定页面，
/// 而不是拉起一个只能看到默认页的窗口。非法页面名一律忽略，
/// 绝不让外部参数决定前端切到哪个视图。
fn panel_requested(args: impl IntoIterator<Item = String>) -> Option<String> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg != PANEL_FLAG {
            continue;
        }
        let value = args.next()?;
        if PANEL_NAMES.contains(&value.as_str()) {
            return Some(value);
        }
        return None;
    }
    None
}

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

/// 第二次启动（主程序已在跑）时，把这次的请求转给已有实例。
///
/// argv 是第二次启动的完整命令行。处理两件事：
/// - `--panel <页面>`：切到该页
/// - `--setup`：强制打开同步设置向导
///
/// 然后把主窗口带到前台——用户在输入法设置里点了按钮，
/// 期望看到窗口，而不是「程序已在运行，什么都没发生」。
fn handle_second_instance(app: &AppHandle, argv: Vec<String>) {
    let inner = &app.state::<AppState>().0;
    push_log(inner, "收到第二次启动的请求：转交给已在运行的实例");
    eprintln!("[rime-sync] second instance: forwarding request to the running instance");

    // --setup 发事件 + 写短命槽位（两条都留）：
    // 事件可能早于前端注册监听而丢失，槽位让前端在注册完监听后还能兜底取一次。
    // 但**不写** setup_request 状态位：那个位是「本次启动的参数」，
    // 运行期间写进去会一直挂着，变成**下次重启程序时莫名弹出设置向导**。
    if setup_requested(argv.iter().cloned()) {
        push_log(inner, "第二次启动带 --setup：让已有实例打开同步设置向导");
        eprintln!("[rime-sync] second instance: --setup requested; asking the running instance to open the wizard");
        write_setup_slot(inner);
        let _ = app.emit("setup://request", ());
    }
    if let Some(panel) = panel_requested(argv.iter().cloned()) {
        eprintln!("[rime-sync] second instance: --panel {panel} requested");
        request_panel(app, Some(panel), "第二次启动");
    }

    // 窗口带到前台：unminimize + show + set_focus。
    // 顺序有讲究：先 unminimize 再 show，否则最小化的窗口可能仍看不见。
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        push_log(inner, "已有实例的窗口已带到前台");
    }
}

pub fn run() {
    // panic 钩子装在最外层入口：release 是 panic = "abort"，
    // panic 展开后直接 abort，catch_unwind 永远抓不到（prebuilt_cmds 里的兜底同理），
    // 这个钩子是唯一能把「消息 + 位置 + 线程名」留下来并让用户看见的地方。
    app_log::install_panic_hook();
    let builder = tauri::Builder::default()
        // 单实例：输入法设置页、开始菜单、托盘点「打开同步」时，
        // 如果主程序已在跑（多半缩在托盘里），就把请求转给已有实例，
        // 由它切到对应页面并把窗口带到前台；第二次启动的进程随即退出。
        // 这样「输入法里的入口」与「主程序的界面」才是同一个界面。
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_second_instance(app, argv);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = app.state::<AppState>();
            // 旧目录 → 共享目录的一次性迁移（状态与备份，配置文件不动）。
            //
            // 为什么排在建日志**之前**：迁移会把旧日志与崩溃标记一起搬过去，
            // 搬完再建日志，才能既保住上次的日志，又认得出「上次崩了」。
            //
            // 迁移失败只记日志、绝不中断启动：拿不到旧数据顶多是从零开始，
            // 而起不来就是彻底用不了。
            let migration = match (config_dir(&handle), state_dir(&handle)) {
                (Ok(legacy), Ok(shared)) => Some(paths::migrate_legacy_state(&legacy, &shared)),
                (Err(error), _) | (_, Err(error)) => {
                    eprintln!("[rime-sync] 状态目录迁移跳过：{error}");
                    None
                }
            };
            // 落盘日志先建起来：后面的 push_log、core 的 tracing::、panic 钩子
            // 全都写进这同一个文件。
            match app_log::init(&handle) {
                Ok(path) => push_log(&state.0, format!("日志文件：{}", path.display())),
                Err(error) => push_log(&state.0, format!("日志文件不可用：{error}")),
            }
            // 迁移结果说清楚：搬了什么、跳过了什么、什么没搬成。
            if let Some(report) = &migration {
                if !report.is_noop() {
                    push_log(&state.0, format!("状态目录迁移：{}", report.summary()));
                    for line in report.failure_lines() {
                        push_log(&state.0, format!("状态目录迁移失败：{line}"));
                    }
                }
            }
            // 状态目录（基线与备份都在这里）说给用户听，便于排查两端是否同一处。
            if let Ok(dir) = state_dir(&handle) {
                push_log(&state.0, format!("状态目录：{}", dir.display()));
            }
            // 上次启动崩过的话，弹一次提示（abort 会带走同进程的弹窗，
            // 跨进程的这次提示才是真正能被用户看到的）。
            app_log::notify_previous_crash();
            push_log(&state.0, "Rime 同步已启动");
            // 环境变量覆盖：启动时先说一声，免得用户以为设置没生效
            //（只报被接管的字段名，绝不打印密码）。
            if let Ok(path) = config_path(&handle) {
                if let Ok(stored) = read_stored_config(&path) {
                    let names = EnvOverrides::detect(&stored).names();
                    if !names.is_empty() {
                        push_log(&state.0, format!("环境变量覆盖生效：{}", names.join("、")));
                    }
                }
            }
            let setup_requested = setup_requested(std::env::args());
            state
                .0
                .setup_request
                .store(setup_requested, Ordering::SeqCst);
            if setup_requested {
                push_log(&state.0, "检测到 --setup：强制进入同步设置向导");
                eprintln!("[rime-sync] setup wizard: --setup detected; forcing sync setup dialog");
            }
            // --panel <页面>：启动即落在该页（输入法设置页 / 开始菜单的入口）。
            // 放进状态里，前端起来后取走；前端已在跑时走事件（见 request_panel）。
            let panel = panel_requested(std::env::args());
            if panel.is_some() {
                request_panel(&handle, panel, "启动参数");
            }
            // 0.6.16：预编译产物自举。安装器钩子没跑（/S 静默安装实测不执行）、
            // WeaselDeployer 返回 0 却不写 build 时，由程序自己把随包产物铺进用户目录。
            // 后台线程 + 失败只记日志：绝不阻塞启动。
            prebuilt_cmds::bootstrap(&handle);
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
            cmd_probe_network_status,
            cmd_list_remote,
            cmd_transport,
            cmd_apply,
            cmd_plan_sync,
            cmd_cancel,
            cmd_get_log,
            cmd_needs_setup,
            cmd_take_setup_request,
            cmd_take_setup_slot,
            cmd_take_panel_request,
            cmd_pick_folder,
            cmd_secret_status,
            cmd_first_transport,
            prebuilt_cmds::cmd_set_locale,
            prebuilt_cmds::cmd_fix_ime,
            prebuilt_cmds::cmd_ime_status,
            prebuilt_cmds::cmd_restart_elevated,
            // 方案裁剪：先预演（不动磁盘），用户确认后才真的删。
            prune_cmds::cmd_prune_preview,
            prune_cmds::cmd_prune_confirm,
            prune_cmds::cmd_prune_cancel,
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
            // 同步历史（只读）：最近几次同步的记录。
            history_cmds::cmd_list_history,
            preset_cmds::cmd_list_presets,
            preset_cmds::cmd_apply_preset,
            schema_cmds::cmd_list_schemas,
            schema_cmds::cmd_set_schemas,
            preset_cmds::cmd_probe_write,
            diagnostics::cmd_export_diagnostics,
            // 离线依赖安装告警（packaging/linux/postinst 写 install-warning.json）
            diagnostics::cmd_install_warning,
            diagnostics::cmd_retry_install_deps,
            // 输入法增强功能（数字小键盘 / 中文标点直出 / v 转数字）检测与 Linux 一键换装。
            extra_ime::cmd_extra_support,
            extra_ime::cmd_install_extra_ime,
            extra_ime::cmd_pick_extra_package,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量测试的全局锁：std::env 是进程级的，测试并行跑时会互相踩。
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        &LOCK
    }

    /// 环境变量测试涉及的变量（测完必须清干净，免得影响别的测试）。
    const ENV_KEYS: [&str; 6] = [
        "RIME_SYNC_DAV_URL",
        "RIME_SYNC_DAV_USER",
        "RIME_SYNC_DAV_PASS",
        "RIME_SYNC_DAV_DIR",
        "RIME_SYNC_RIME_DIR",
        "RIME_SYNC_FIRST_SYNC_POLICY",
    ];

    fn clear_env() {
        for key in ENV_KEYS {
            std::env::remove_var(key);
        }
    }

    /// 建一个临时配置文件，返回它的路径（内容由调用方写）。
    fn temp_config_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-tauri-env-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        dir.join("config.json")
    }

    /// 造一份「文件里已经填好值」的配置。
    fn stored_config() -> Config {
        let mut stored = Config::default();
        stored.webdav.url = "https://file.example/dav".to_string();
        stored.webdav.username = "file-user".to_string();
        stored.webdav.remote_dir = "from-file".to_string();
        stored.rime_dir = Some("/from/file".to_string());
        stored.first_sync_policy = config::FirstSyncPolicy::Preview;
        stored
    }

    /// B4：桌面端读配置时，环境变量必须覆盖配置文件里的对应字段。
    ///
    /// 与 CLI（crates/cli/src/main.rs 的 apply_env）同口径：环境变量赢，
    /// 且只在内存里生效——磁盘上的 config.json 仍保留用户自己填的值。
    #[test]
    fn env_overrides_stored_config_fields() {
        let _guard = env_lock().lock().unwrap_or_else(|error| error.into_inner());
        clear_env();
        let path = temp_config_path("override");
        stored_config().save(&path).expect("写入配置文件");

        std::env::set_var("RIME_SYNC_DAV_URL", "https://env.example/dav");
        std::env::set_var("RIME_SYNC_DAV_USER", "env-user");
        std::env::set_var("RIME_SYNC_DAV_DIR", "from-env");
        std::env::set_var("RIME_SYNC_RIME_DIR", "/from/env");
        std::env::set_var("RIME_SYNC_FIRST_SYNC_POLICY", "normal");

        let effective = read_effective_config(&path).expect("读取生效配置");
        assert_eq!(effective.webdav.url, "https://env.example/dav");
        assert_eq!(effective.webdav.username, "env-user");
        assert_eq!(effective.webdav.remote_dir, "from-env");
        assert_eq!(effective.rime_dir.as_deref(), Some("/from/env"));
        assert_eq!(effective.first_sync_policy, config::FirstSyncPolicy::Normal);

        // 磁盘上的原值不受影响：环境变量是运行时覆盖，不是配置改写。
        let on_disk = read_stored_config(&path).expect("读取磁盘配置");
        assert_eq!(on_disk.webdav.url, "https://file.example/dav");
        assert_eq!(on_disk.webdav.username, "file-user");
        assert_eq!(on_disk.webdav.remote_dir, "from-file");
        assert_eq!(on_disk.rime_dir.as_deref(), Some("/from/file"));
        assert_eq!(on_disk.first_sync_policy, config::FirstSyncPolicy::Preview);

        clear_env();
        let _ = std::fs::remove_dir_all(path.parent().expect("取临时目录"));
    }

    /// 没有环境变量时，配置文件里的值照旧生效（别把原来的行为改坏）。
    #[test]
    fn without_env_the_stored_config_wins() {
        let _guard = env_lock().lock().unwrap_or_else(|error| error.into_inner());
        clear_env();
        let path = temp_config_path("no-env");
        stored_config().save(&path).expect("写入配置文件");

        let effective = read_effective_config(&path).expect("读取生效配置");
        assert_eq!(effective.webdav.url, "https://file.example/dav");
        assert_eq!(effective.webdav.username, "file-user");
        assert_eq!(effective.webdav.remote_dir, "from-file");
        assert_eq!(effective.rime_dir.as_deref(), Some("/from/file"));
        assert_eq!(
            effective.first_sync_policy,
            config::FirstSyncPolicy::Preview
        );

        let _ = std::fs::remove_dir_all(path.parent().expect("取临时目录"));
    }

    /// 落盘时不能把环境变量的值固化进配置文件。
    ///
    /// 否则用户撤掉环境变量后，配置文件里还留着被固化的那份值，就再也回不去了。
    #[test]
    fn saving_config_keeps_env_overridden_fields_untouched() {
        let _guard = env_lock().lock().unwrap_or_else(|error| error.into_inner());
        clear_env();
        let path = temp_config_path("save");
        stored_config().save(&path).expect("写入配置文件");

        std::env::set_var("RIME_SYNC_DAV_URL", "https://env.example/dav");
        std::env::set_var("RIME_SYNC_RIME_DIR", "/from/env");

        let mut effective = read_effective_config(&path).expect("读取生效配置");
        effective.device_id = "device-under-test".to_string();
        save_config(&path, &effective).expect("保存配置");

        let on_disk = read_stored_config(&path).expect("读取磁盘配置");
        // 被环境变量接管的字段：仍是用户自己填的值。
        assert_eq!(on_disk.webdav.url, "https://file.example/dav");
        assert_eq!(on_disk.rime_dir.as_deref(), Some("/from/file"));
        // 其它改动照常落盘。
        assert_eq!(on_disk.device_id, "device-under-test");

        clear_env();
        let _ = std::fs::remove_dir_all(path.parent().expect("取临时目录"));
    }

    /// 环境变量里的密码只在内存里用：既不落盘，也不进日志。
    #[test]
    fn env_password_never_reaches_the_config_file() {
        let _guard = env_lock().lock().unwrap_or_else(|error| error.into_inner());
        clear_env();
        let path = temp_config_path("password");
        stored_config().save(&path).expect("写入配置文件");

        std::env::set_var("RIME_SYNC_DAV_PASS", "s3cret-from-env");
        let effective = read_effective_config(&path).expect("读取生效配置");
        assert_eq!(effective.webdav.password, "s3cret-from-env");

        save_config(&path, &effective).expect("保存配置");
        let text = std::fs::read_to_string(&path).expect("读取配置文件");
        assert!(
            !text.contains("s3cret-from-env"),
            "环境变量里的密码绝不能写进配置文件：{text}"
        );

        clear_env();
        let _ = std::fs::remove_dir_all(path.parent().expect("取临时目录"));
    }

    /// D1：GUI 不许再静默改掉用户手填的 Rime 目录。
    ///
    /// 改前 `detect_rime` 在配置目录不存在时会 `.or_else(|_| rime_dir::detect(None))`
    /// 悄悄换一个目录，用户完全不知道自己填错了；改后必须明确报错。
    #[test]
    fn detect_rime_refuses_a_missing_configured_directory() {
        let config = Config {
            rime_dir: Some("/definitely/not/a/rime/dir".to_string()),
            ..Config::default()
        };

        let error = detect_rime(&config).expect_err("目录不存在时必须报错，不能悄悄换目录");
        assert!(
            error.contains("/definitely/not/a/rime/dir"),
            "报错必须点名用户填的目录：{error}"
        );
    }

    /// 没填目录时才能走自动探测（探测失败也只在这里发生）。
    #[test]
    fn detect_rime_without_configuration_falls_back_to_detection() {
        let config = Config {
            rime_dir: None,
            ..Config::default()
        };
        // 本机有没有 Rime 目录不确定：两种结果都合法，但都不许 panic。
        let _ = detect_rime(&config);
    }

    /// --panel 指向五个合法页面之一时，必须解析出该页面名。
    #[test]
    fn panel_request_parses_every_known_panel() {
        for name in PANEL_NAMES {
            let args = vec![
                "rime-sync-app".to_string(),
                "--panel".to_string(),
                name.to_string(),
            ];
            assert_eq!(
                panel_requested(args),
                Some(name.to_string()),
                "--panel {name} 必须被解析出来"
            );
        }
    }

    /// 没有 --panel、--panel 后缺值、或页面名非法时，一律不切页。
    #[test]
    fn panel_request_ignores_missing_or_unknown_panel() {
        // 没有 --panel
        assert_eq!(panel_requested(vec!["rime-sync-app".to_string()]), None);
        // --panel 后面没有值
        assert_eq!(
            panel_requested(vec!["rime-sync-app".to_string(), "--panel".to_string()]),
            None
        );
        // 页面名不在白名单里：绝不能让外部参数决定前端切到哪个视图
        assert_eq!(
            panel_requested(vec![
                "rime-sync-app".to_string(),
                "--panel".to_string(),
                "evil".to_string()
            ]),
            None
        );
        // 页面名是空串
        assert_eq!(
            panel_requested(vec![
                "rime-sync-app".to_string(),
                "--panel".to_string(),
                String::new()
            ]),
            None
        );
    }

    /// --panel 与其它启动参数混用时，仍能正确解析出页面名。
    #[test]
    fn panel_request_works_alongside_other_flags() {
        let args = vec![
            "rime-sync-app".to_string(),
            "--autostart".to_string(),
            "--panel".to_string(),
            "sync".to_string(),
        ];
        assert_eq!(panel_requested(args), Some("sync".to_string()));
    }

    /// panel_request 状态槽的语义：取走即清，且可以再存一次。
    ///
    /// 前端会取两次（初始化时取一次、注册监听后再取一次兜底），所以
    /// 「取走即清」和「取空后还能再存」这两条都必须成立。
    #[test]
    fn panel_request_slot_is_take_once_but_can_be_refilled() {
        let slot = Mutex::new(None);

        // 存一次、取一次
        *slot.lock().unwrap() = Some("sync".to_string());
        let first = slot.lock().unwrap().take();
        assert_eq!(first, Some("sync".to_string()));

        // 取走即清：再取是空的（前端第一次取值后，不该重复切页）
        let second = slot.lock().unwrap().take();
        assert_eq!(second, None, "取走必须清空，不能重复切同一页");

        // 取空后可以再存：兜底路径依赖这一点
        // （第一次取走之后、监听注册之前到达的请求，就靠这第二次存取到）
        *slot.lock().unwrap() = Some("conflicts".to_string());
        let third = slot.lock().unwrap().take();
        assert_eq!(third, Some("conflicts".to_string()));
    }

    /// --setup 短命槽位的语义：写一次、取一次（取走即清），过期的一律当作没有。
    ///
    /// 这是「第二次启动带 --setup，但事件早于前端注册监听而丢失」的兜底通道：
    /// 前端注册完监听后再取一次，取到就开向导。
    #[test]
    fn setup_slot_is_take_once_and_expires() {
        let inner = Inner::default();

        // 没有请求时取不到
        assert!(!take_setup_slot(&inner), "没有请求时不能凭空开向导");

        // 写一次、取一次
        write_setup_slot(&inner);
        assert!(take_setup_slot(&inner), "刚写进去的请求必须能取到");

        // 取走即清：不会重复开向导
        assert!(!take_setup_slot(&inner), "取走必须清空，不能重复弹向导");

        // 过期的一律当作没有（免得几小时前的请求在用户回来时突然弹窗）
        *inner.setup_slot.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
        assert!(!take_setup_slot(&inner), "过期的请求必须被忽略");
    }

    /// 短命槽位只在进程内：Inner 是纯内存状态，重启后必然读不到。
    ///
    /// 这条是「下次重启程序时莫名弹出设置向导」的防线：只要不落盘就不会中招。
    #[test]
    fn setup_slot_never_survives_a_restart() {
        let inner = Inner::default();
        write_setup_slot(&inner);
        assert!(take_setup_slot(&inner));

        // 模拟重启：全新的进程状态里没有任何槽位
        let restarted = Inner::default();
        assert!(
            !take_setup_slot(&restarted),
            "重启后不该还留着 --setup 请求"
        );
    }

    /// push_log 必须同时落到磁盘文件：内存里那 1000 行只是给界面看的，
    /// 程序崩了之后能查的只有文件。
    #[test]
    fn push_log_writes_to_the_log_file() {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-pushlog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        // 全局句柄只能装一次：拿到「实际生效」的路径再断言。
        let path = app_log::use_log_for_test(dir.join("log.txt")).expect("日志句柄已就绪");

        let inner = Inner::default();
        push_log(&inner, "落盘自检：这一行必须能在文件里找到");

        let content = std::fs::read_to_string(&path).expect("读取日志文件");
        assert!(content.contains("落盘自检"), "{content}");
        // 界面那份内存日志也要有同一行。
        assert!(log_tail(&inner, 10)
            .iter()
            .any(|line| line.contains("落盘自检")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
