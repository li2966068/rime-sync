//! rime-sync-cli：headless 入口，供 E2E 与无 GUI 环境使用。

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rime_sync_core::backup::{BackupManager, DEFAULT_KEEP};
use rime_sync_core::config::{self, Config};
use rime_sync_core::engine::{Engine, Progress, ProgressSink, Report};
use rime_sync_core::history;
use rime_sync_core::index::FileIndex;
use rime_sync_core::lock;
use rime_sync_core::log;
use rime_sync_core::network::NetworkPolicy;
use rime_sync_core::plan;
use rime_sync_core::platform;
use rime_sync_core::presets::Preset;
use rime_sync_core::retry::RetryState;
use rime_sync_core::rime_dir;
use rime_sync_core::secret;
use rime_sync_core::watch;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "rime-sync-cli",
    version,
    about = "Rime WebDAV sync engine (headless)"
)]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long = "rime-dir", global = true, value_name = "PATH")]
    rime_dir: Option<PathBuf>,
    #[arg(long = "dav-url", global = true, value_name = "URL")]
    dav_url: Option<String>,
    #[arg(long = "dav-user", global = true, value_name = "USER")]
    dav_user: Option<String>,
    #[arg(long = "dav-pass", global = true, value_name = "PASS")]
    dav_pass: Option<String>,
    /// 远端目录；传空串 = 服务器根（默认读 config.json）
    #[arg(long = "dav-dir", global = true, value_name = "DIR")]
    dav_dir: Option<String>,
    /// 服务商预设：openlist / alist / jianguoyun / nextcloud / synology / custom（会归一化 dav-url）
    #[arg(long, global = true, value_name = "PRESET")]
    preset: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 探测 Rime 用户目录
    Detect,
    /// 测试 WebDAV 连接
    #[command(name = "test-conn")]
    TestConn,
    /// 显示配置、本地文件与远端设备
    Status,
    /// 只搬文件（pull → merge → push）
    Transport,
    /// 合并 + 部署 + push
    Apply,
    /// 立即生成一份备份
    Backup {
        /// 备份原因，写入 manifest
        #[arg(long, default_value = "manual")]
        reason: String,
    },
    /// 列出已有备份
    Backups,
    /// 回滚到指定备份（写回前自动建 pre-restore 备份）
    Rollback { id: String },
    /// 前台监听 Rime 目录，变更后自动 transport
    Watch {
        /// 去抖秒数
        #[arg(long, default_value_t = 3)]
        debounce_secs: u64,
    },
    /// 写权限探测（MKCOL → PUT → DELETE）
    Probe,
    /// 查看同步历史（history.jsonl，最多 500 条）
    History {
        /// 只显示最近 N 条
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Serialize)]
struct DetectReport {
    path: String,
    source: String,
}

#[derive(Serialize)]
struct ConnReport {
    ok: bool,
    latency_ms: u64,
    server: String,
    dav_class: String,
    status: u16,
    dav: Vec<String>,
    preset: Option<String>,
    hint: Option<String>,
    base_url: String,
    remote_dir: String,
    remote_dir_missing: bool,
    message: String,
}

#[derive(Serialize)]
struct StatusReport {
    config_path: String,
    rime_dir: String,
    device_id: String,
    dav_url: String,
    remote_dir: String,
    local_shared: usize,
    local_snapshots: usize,
    index_files: usize,
    remote_devices: Vec<String>,
    remote_shared: Option<usize>,
    remote_error: Option<String>,
    secret_backend: String,
    password_stored: bool,
    network_state: String,
    network_message: String,
    network_available: bool,
    network_metered: bool,
    next_retry_at: Option<u64>,
    lock_held: bool,
    lock_stale: bool,
    lock_holder: Option<String>,
    /// 离线依赖安装告警（<config 目录>/install-warning.json）。
    /// None = 没有告警；文件存在但坏了也会是 Some（带 present/parse_error），不许静默。
    install_warning: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct BackupReport {
    ok: bool,
    id: String,
    path: String,
    reason: String,
    pruned: Vec<String>,
}

#[derive(Serialize)]
struct RollbackReport {
    ok: bool,
    backup_id: String,
    pre_restore_id: Option<String>,
    restored: usize,
}

/// probe 失败时已经打印了结构化结果，main 不再重复输出一行错误。
#[derive(Debug)]
struct ProbeFailed;

impl std::fmt::Display for ProbeFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("write probe failed")
    }
}

impl std::error::Error for ProbeFailed {}

fn main() {
    let cli = Cli::parse();
    let wants_json = cli.json;
    if let Err(error) = run(cli) {
        if error.downcast_ref::<ProbeFailed>().is_some() {
            std::process::exit(1);
        }
        if wants_json {
            println!(
                "{}",
                serde_json::json!({ "ok": false, "error": format!("{error:#}") })
            );
        } else {
            eprintln!("error: {error:#}");
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(config::default_config_path);
    init_logging(&config_path);
    let mut stored = Config::load(&config_path)?;
    let mut config = stored.clone();
    config.apply_env();
    config.apply_overrides(
        cli.dav_url.as_deref(),
        cli.dav_user.as_deref(),
        cli.dav_pass.as_deref(),
        cli.dav_dir.as_deref(),
    );
    if let Some(raw) = cli.preset.as_deref() {
        match Preset::from_id(raw) {
            Some(preset) => {
                if !config.webdav.url.trim().is_empty() {
                    config.webdav.url = preset.normalize(&config.webdav.url);
                }
            }
            None => eprintln!("warning: unknown preset {raw}, ignored"),
        }
    }

    let app_data = state_dir(&config_path);
    let secrets = secret::detect(&app_data);
    if config.legacy_password || config.schema_version < config::CONFIG_SCHEMA_VERSION {
        match config.migrate(secrets.store.as_ref(), &config_path) {
            Ok(report) if report.changed => {
                stored.legacy_password = false;
                stored.password_ref = config.password_ref;
                stored.schema_version = config.schema_version;
                if !cli.json {
                    eprintln!(
                        "config schema v{} -> v{} 迁移完成（密码后端 {}）",
                        report.from,
                        report.to,
                        secrets.backend.as_str()
                    );
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("warning: 配置迁移失败（原文件保留）: {error:#}"),
        }
    }
    if let Err(error) = config.load_password(secrets.store.as_ref()) {
        eprintln!("warning: 读取已保存密码失败: {error:#}");
    }

    match cli.command {
        Command::Detect => {
            let explicit = cli
                .rime_dir
                .clone()
                .or_else(|| config.rime_dir.clone().map(PathBuf::from));
            let detection = rime_dir::detect(explicit.as_deref())?;
            let report = DetectReport {
                path: detection.path.display().to_string(),
                source: detection.source,
            };
            if cli.json {
                print_json(&report)
            } else {
                println!("rime_dir={} source={}", report.path, report.source);
                Ok(())
            }
        }
        Command::TestConn => {
            let dav = dav_client(&config)?;
            let check = dav.check_connection()?;
            let report = ConnReport {
                ok: check.ok,
                latency_ms: check.latency_ms,
                server: check.base_url.clone(),
                dav_class: check.dav_class,
                status: check.status,
                dav: check.dav,
                preset: check.preset,
                hint: check.hint,
                base_url: check.base_url,
                remote_dir: check.remote_dir.clone(),
                remote_dir_missing: check.remote_dir_missing,
                message: check.message,
            };
            if cli.json {
                print_json(&report)
            } else {
                println!(
                    "ok={} status={} latency={}ms server={} dav_class={} preset={} remote_dir_missing={}",
                    report.ok,
                    report.status,
                    report.latency_ms,
                    report.server,
                    report.dav_class,
                    report.preset.as_deref().unwrap_or("-"),
                    report.remote_dir_missing
                );
                println!(
                    "remote_dir={} base={}",
                    if report.remote_dir.is_empty() {
                        "(server root)"
                    } else {
                        report.remote_dir.as_str()
                    },
                    report.base_url
                );
                println!("message={}", report.message);
                if let Some(hint) = report.hint {
                    println!("{hint}");
                }
                Ok(())
            }
        }
        Command::Status => {
            let detection = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let device_id = if config.device_id.trim().is_empty() {
                rime_dir::installation_id(&detection).unwrap_or_else(|| "(unset)".to_string())
            } else {
                config.device_id.clone()
            };
            let (local_shared, local_snapshots) = count_local(&detection)?;
            let index = FileIndex::load(&state_dir(&config_path).join("index.json"))?;
            let mut remote_devices = Vec::new();
            let mut remote_shared = None;
            let mut remote_error = None;
            match dav_client(&config) {
                Ok(dav) => {
                    match dav.propfind("sync", 1) {
                        Ok(entries) => {
                            remote_devices = entries
                                .into_iter()
                                .filter(|entry| entry.is_dir && entry.path.starts_with("sync/"))
                                .map(|entry| entry.path)
                                .collect();
                        }
                        Err(error) => remote_error = Some(format!("{error:#}")),
                    }
                    match dav.propfind("shared/files", 1) {
                        Ok(entries) => {
                            remote_shared =
                                Some(entries.iter().filter(|entry| !entry.is_dir).count())
                        }
                        Err(error) => {
                            if remote_error.is_none() {
                                remote_error = Some(format!("{error:#}"));
                            }
                        }
                    }
                }
                Err(error) => remote_error = Some(format!("{error:#}")),
            }
            let lock_status = lock::inspect(&state_dir(&config_path).join("sync.lock"));
            let network = NetworkPolicy::system();
            let retry =
                RetryState::load(&state_dir(&config_path).join("retry.json")).unwrap_or_default();
            let install_warning = read_install_warning(&state_dir(&config_path));
            let report = StatusReport {
                config_path: config_path.display().to_string(),
                rime_dir: detection.display().to_string(),
                device_id,
                dav_url: config.webdav.url.clone(),
                remote_dir: config.webdav.remote_dir.clone(),
                local_shared,
                local_snapshots,
                index_files: index.files.len(),
                remote_devices,
                remote_shared,
                remote_error,
                secret_backend: secrets.describe(),
                password_stored: config.password_ref != config::PasswordRef::None,
                network_state: network.state().as_str().to_string(),
                network_message: network.state().message().to_string(),
                network_available: network.available(),
                network_metered: network.is_metered(),
                next_retry_at: retry.next_retry_at,
                lock_held: lock_status.held,
                lock_stale: lock_status.stale,
                lock_holder: lock_status
                    .info
                    .map(|info| format!("pid={} host={}", info.pid, info.hostname)),
                install_warning,
            };
            if cli.json {
                print_json(&report)
            } else {
                println!("config={}", report.config_path);
                println!("rime_dir={}", report.rime_dir);
                println!("device_id={}", report.device_id);
                println!("remote={} dir={}", report.dav_url, report.remote_dir);
                println!(
                    "local_shared={} local_snapshots={} index_files={}",
                    report.local_shared, report.local_snapshots, report.index_files
                );
                println!(
                    "remote_devices={:?} remote_shared={:?}",
                    report.remote_devices, report.remote_shared
                );
                println!(
                    "secret_backend={} password_stored={}",
                    report.secret_backend, report.password_stored
                );
                println!(
                    "network={} available={} metered={} next_retry_at={}",
                    report.network_state,
                    report.network_available,
                    report.network_metered,
                    report
                        .next_retry_at
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string())
                );
                println!("network_message={}", report.network_message);
                println!(
                    "lock_held={} lock_stale={} holder={}",
                    report.lock_held,
                    report.lock_stale,
                    report.lock_holder.as_deref().unwrap_or("-")
                );
                // 离线依赖没装完时这一行是 JSON；没有告警时是 "-"，脚本可稳定解析。
                println!("{}", install_warning_line(&report.install_warning));
                if let Some(error) = report.remote_error {
                    eprintln!("remote_error: {error}");
                }
                Ok(())
            }
        }
        Command::Backup { reason } => {
            let detection = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let manager = BackupManager::new(&detection, &app_data.join("backups"));
            let id = manager.create(&reason)?;
            let pruned = manager.prune(DEFAULT_KEEP).unwrap_or_default();
            let report = BackupReport {
                ok: true,
                path: manager.root().join(&id).display().to_string(),
                id,
                reason,
                pruned,
            };
            if cli.json {
                print_json(&report)
            } else {
                println!(
                    "backup={} path={} reason={} pruned={}",
                    report.id,
                    report.path,
                    report.reason,
                    report.pruned.len()
                );
                Ok(())
            }
        }
        Command::Backups => {
            let detection = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let manager = BackupManager::new(&detection, &app_data.join("backups"));
            let infos = manager.list()?;
            if cli.json {
                print_json(&infos)
            } else if infos.is_empty() {
                println!("no backups in {}", manager.root().display());
                Ok(())
            } else {
                for info in &infos {
                    println!(
                        "{}  files={} bytes={} reason={}",
                        info.id, info.file_count, info.total_bytes, info.reason
                    );
                }
                Ok(())
            }
        }
        Command::Rollback { id } => {
            let detection = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let manager = BackupManager::new(&detection, &app_data.join("backups"));
            let restored = manager.restore(&id)?;
            let report = RollbackReport {
                ok: true,
                backup_id: restored.backup_id,
                pre_restore_id: restored.pre_restore_id,
                restored: restored.restored,
            };
            if cli.json {
                print_json(&report)
            } else {
                println!(
                    "rollback={} restored={} pre_restore={}",
                    report.backup_id,
                    report.restored,
                    report.pre_restore_id.as_deref().unwrap_or("-")
                );
                Ok(())
            }
        }
        Command::Probe => {
            let dav = dav_client(&config)?;
            let probe = dav.check_write()?;
            if cli.json {
                print_json(&probe)?;
            } else {
                println!(
                    "ok={} preset={} server={}",
                    probe.ok,
                    probe.preset.as_deref().unwrap_or("-"),
                    probe.server.as_deref().unwrap_or("-")
                );
                for step in &probe.steps {
                    println!(
                        "  {:<8} status={} ok={} {}",
                        step.step, step.status, step.ok, step.detail
                    );
                }
                if let Some(hint) = &probe.hint {
                    println!("hint: {hint}");
                }
            }
            if probe.ok {
                Ok(())
            } else {
                Err(anyhow::Error::new(ProbeFailed))
            }
        }
        Command::History { limit } => {
            let entries = history::read(&state_dir(&config_path).join("history.jsonl"), limit)?;
            if cli.json {
                print_json(&entries)
            } else {
                for entry in &entries {
                    println!(
                        "ts={} mode={} ok={} pulled={} pushed={} merged={} conflicts={} duration={}ms backup={} error={}",
                        entry.ts,
                        entry.mode,
                        entry.ok,
                        entry.pulled,
                        entry.pushed,
                        entry.merged,
                        entry.conflicts,
                        entry.duration_ms,
                        entry.backup_id.as_deref().unwrap_or("-"),
                        entry.error.as_deref().unwrap_or("-")
                    );
                }
                Ok(())
            }
        }
        Command::Transport | Command::Apply => {
            let detection = prepare_sync(
                &mut config,
                &mut stored,
                &config_path,
                cli.rime_dir.as_deref(),
            )?;
            let apply_mode = matches!(cli.command, Command::Apply);
            let trigger = platform::default_trigger(config.stop_fcitx5_during_apply);
            let mut engine = Engine::new(config, detection, state_dir(&config_path), trigger)?;
            if !cli.json {
                engine = engine.with_progress(ProgressSink::new(|progress: &Progress| {
                    eprintln!(
                        "[{}] {}/{} {}",
                        progress.phase, progress.current, progress.total, progress.message
                    );
                }));
            }
            let report = if apply_mode {
                engine.apply()
            } else {
                engine.transport()
            };
            if cli.json {
                print_json(&report)
            } else {
                print_report(&report);
                Ok(())
            }
        }
        Command::Watch { debounce_secs } => {
            let detection = prepare_sync(
                &mut config,
                &mut stored,
                &config_path,
                cli.rime_dir.as_deref(),
            )?;
            let debounce = Duration::from_secs(debounce_secs.max(1));
            let handle = watch::start(&detection, debounce)?;
            if !cli.json {
                eprintln!(
                    "watching {} (debounce {}s), Ctrl-C 退出",
                    detection.display(),
                    debounce.as_secs()
                );
            }
            loop {
                let event = match handle.rx.recv() {
                    Ok(event) => event,
                    Err(error) => {
                        eprintln!("watch channel closed: {error}");
                        return Ok(());
                    }
                };
                if cli.json {
                    let _ = print_json(&event);
                    eprintln!("watch: transport triggered");
                } else {
                    eprintln!(
                        "watch: transport triggered ({})",
                        watch::event_summary(&event)
                    );
                }
                // 写盘期间静音，避免引擎自己的写入再触发一轮。
                handle.mute(Duration::from_secs(15));
                let trigger = platform::default_trigger(config.stop_fcitx5_during_apply);
                let mut engine = match Engine::new(
                    config.clone(),
                    detection.clone(),
                    state_dir(&config_path),
                    trigger,
                ) {
                    Ok(engine) => engine,
                    Err(error) => {
                        eprintln!("watch: engine init failed: {error:#}");
                        continue;
                    }
                };
                if !cli.json {
                    engine = engine.with_progress(ProgressSink::new(|progress: &Progress| {
                        eprintln!(
                            "[{}] {}/{} {}",
                            progress.phase, progress.current, progress.total, progress.message
                        );
                    }));
                }
                // SPEC-3 §B7：定时/自动搬运在离线或计量网络下跳过，手动同步不受影响。
                if let Some(reason) = NetworkPolicy::system().scheduled_block_reason(false) {
                    eprintln!("watch: {reason}");
                    continue;
                }
                let report = engine.transport();
                if cli.json {
                    let _ = print_json(&report);
                } else {
                    print_report(&report);
                }
            }
        }
    }
}

/// 解析 rime 目录、补齐 device_id / rime_dir 并写回（不写密码）。
fn prepare_sync(
    config: &mut Config,
    stored: &mut Config,
    config_path: &Path,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    let detection = resolve_rime_dir(explicit, config)?;
    let device_was_empty = config.device_id.trim().is_empty();
    if device_was_empty {
        config.resolve_device_id(&detection);
    }
    let rime_was_empty = stored.rime_dir.is_none();
    if device_was_empty || rime_was_empty {
        stored.device_id = config.device_id.clone();
        stored.rime_dir = Some(detection.display().to_string());
        stored.save(config_path)?;
    }
    Ok(detection)
}

fn dav_client(config: &Config) -> Result<DavClient> {
    DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
    )
}

fn resolve_rime_dir(explicit: Option<&Path>, config: &Config) -> Result<PathBuf> {
    let configured = config.rime_dir.clone().map(PathBuf::from);
    let detection = rime_dir::detect(explicit.or(configured.as_deref()))?;
    fs::canonicalize(&detection.path)
        .with_context(|| format!("canonicalize {}", detection.path.display()))
}

fn state_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 读 <config 目录>/install-warning.json（packaging/linux/postinst 的 wrapper 写）。
///
/// 返回 None 表示没有告警。文件存在但读不动 / 不是合法 JSON 时也要返回
/// present=true + parse_error：这条链路的目的就是"别静默"，解析失败同样要可见。
fn read_install_warning(dir: &Path) -> Option<serde_json::Value> {
    let path = dir.join("install-warning.json");
    if !path.exists() {
        return None;
    }
    let path_text = path.display().to_string();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            return Some(serde_json::json!({
                "present": true,
                "path": path_text,
                "parse_error": format!("读取 install-warning.json 失败：{error}"),
            }))
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(mut map)) => {
            let _ = map.insert("present".to_string(), serde_json::Value::Bool(true));
            if !map.contains_key("path") {
                let _ = map.insert("path".to_string(), serde_json::Value::String(path_text));
            }
            Some(serde_json::Value::Object(map))
        }
        Ok(other) => Some(serde_json::json!({
            "present": true,
            "path": path_text,
            "parse_error": format!("install-warning.json 不是 JSON 对象：{other}"),
        })),
        Err(error) => Some(serde_json::json!({
            "present": true,
            "path": path_text,
            "parse_error": format!("install-warning.json 无法解析：{error}"),
        })),
    }
}

/// 文本模式的一行输出；没有告警也打印 "-"，脚本能稳定解析这个字段。
fn install_warning_line(warning: &Option<serde_json::Value>) -> String {
    match warning {
        Some(value) => format!("install_warning: {value}"),
        None => "install_warning: -".to_string(),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn init_logging(config_path: &Path) {
    let path = state_dir(config_path).join("log.txt");
    let log = std::sync::Arc::new(log::RotatingLog::new(
        path,
        log::DEFAULT_MAX_BYTES,
        log::DEFAULT_KEEP,
    ));
    let writer = log::LogWriter::new(log);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // 日志初始化失败（例如同进程重复调用）不影响命令本身。
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(move || writer.clone())
        .try_init();
}

fn print_report(report: &Report) {
    println!(
        "ok={} mode={} first_sync={} pulled={} pushed={} merged={} conflicts={} skipped={} duration={}ms backup={}",
        report.ok,
        if report.mode.is_empty() { "-" } else { report.mode.as_str() },
        report.first_sync,
        report.pulled,
        report.pushed,
        report.merged,
        report.conflicts.len(),
        report.skipped.len(),
        report.duration_ms,
        report.backup_id.as_deref().unwrap_or("-")
    );
    if let Some(details) = &report.conflict_details {
        for detail in details {
            eprintln!(
                "conflict-detail: {} only_local={} only_remote={} both_changed={}",
                detail.path, detail.only_local, detail.only_remote, detail.both_changed
            );
        }
    }
    if let Some(next) = report.next_retry_at {
        eprintln!("next_retry_at: {next}");
    }
    if let Some(error) = &report.error {
        eprintln!("error: {error}");
    }
    for conflict in &report.conflicts {
        eprintln!("conflict: {conflict}");
    }
    for skipped in &report.skipped {
        eprintln!("skipped: {skipped}");
    }
}

fn count_local(root: &Path) -> Result<(usize, usize)> {
    let mut shared = 0;
    let mut snapshots = 0;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 4 {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                if plan::is_excluded_dir(&name) {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
                continue;
            }
            let file_path = entry.path();
            let Ok(relative) = file_path.strip_prefix(root) else {
                continue;
            };
            let rel = relative.to_string_lossy().replace('\\', "/");
            if plan::classify(&rel).category.is_shared() {
                shared += 1;
            }
        }
    }
    let sync_root = rime_dir::sync_dir(root);
    if let Ok(entries) = fs::read_dir(sync_root) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    if let Ok(files) = fs::read_dir(entry.path()) {
                        snapshots += files
                            .flatten()
                            .filter(|file| {
                                file.file_name().to_string_lossy().ends_with(".userdb.txt")
                            })
                            .count();
                    }
                }
            }
        }
    }
    Ok((shared, snapshots))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-cli-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    /// status 的 install_warning 必须在 JSON 与文本两条路径上都能看到；
    /// 坏 JSON 也要 present=true + parse_error（不许静默、不许 panic）。
    #[test]
    fn install_warning_reaches_json_and_text_output() {
        let dir = temp_dir("warning");
        let warning_path = dir.join("install-warning.json");
        fs::write(
            &warning_path,
            r#"{"written_at":"2026-10-24T09:00:00Z","package":"fcitx5-rime","reason":"离线依赖未安装完成","log_path":"/var/log/rime-sync/vendor-install.log"}"#,
        )
        .expect("写 warning");

        let warning = read_install_warning(&dir).expect("应当读到告警");
        assert_eq!(warning["present"], serde_json::Value::Bool(true));
        assert_eq!(warning["package"].as_str(), Some("fcitx5-rime"));
        assert_eq!(
            warning["log_path"].as_str(),
            Some("/var/log/rime-sync/vendor-install.log")
        );

        // JSON 模式：StatusReport 直接序列化这个字段。
        let json = serde_json::to_string(&warning).expect("序列化");
        assert!(json.contains("\"present\":true"), "json={json}");
        assert!(json.contains("fcitx5-rime"), "json={json}");

        // 文本模式：一行 install_warning: {...}
        let line = install_warning_line(&Some(warning));
        assert!(line.starts_with("install_warning: {"), "line={line}");
        assert!(line.contains("fcitx5-rime"), "line={line}");

        // 坏 JSON 也必须可见，且不 panic。
        fs::write(&warning_path, "{not json").expect("写坏 JSON");
        let broken = read_install_warning(&dir).expect("坏 JSON 也要可见");
        assert_eq!(broken["present"], serde_json::Value::Bool(true));
        assert!(
            broken["parse_error"]
                .as_str()
                .unwrap_or_default()
                .contains("无法解析"),
            "broken={broken}"
        );
        assert!(install_warning_line(&Some(broken)).contains("parse_error"));

        // 没有文件时是 None，文本模式打印 "-"。
        let empty = temp_dir("empty");
        assert!(read_install_warning(&empty).is_none());
        assert_eq!(install_warning_line(&None), "install_warning: -");

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&empty);
    }
}
