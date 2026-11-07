//! rime-sync-cli：headless 入口，供 E2E 与无 GUI 环境使用。

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rime_sync_core::backup::{BackupManager, DEFAULT_KEEP};
use rime_sync_core::config::{self, Config};
use rime_sync_core::data_patch;
use rime_sync_core::deploy;
use rime_sync_core::engine::{Engine, Progress, ProgressSink, Report};
use rime_sync_core::history;
use rime_sync_core::ime_state;
use rime_sync_core::index::FileIndex;
use rime_sync_core::librime;
use rime_sync_core::linux_deploy;
use rime_sync_core::lock;
use rime_sync_core::log;
use rime_sync_core::network::NetworkPolicy;
use rime_sync_core::plan;
use rime_sync_core::platform;
use rime_sync_core::prebuilt;
use rime_sync_core::presets::Preset;
use rime_sync_core::prune;
use rime_sync_core::retry::RetryState;
use rime_sync_core::rime_dir;
use rime_sync_core::secret;
use rime_sync_core::watch;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use std::cell::OnceCell;
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
    /// 走代理（http:// / https:// / socks5:// / socks5h://），覆盖配置文件里的 proxy 段
    #[arg(long, global = true, value_name = "URL")]
    proxy: Option<String>,
    /// 本次不用配置文件里的代理：覆盖 proxy.enabled（HTTP_PROXY 等环境变量仍按 reqwest 默认生效）
    #[arg(long = "no-proxy", global = true)]
    no_proxy: bool,
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
    /// 铺入随包的预编译 build 产物（安装器钩子没跑 / WeaselDeployer 不产出时的兜底）
    #[command(name = "fix-ime")]
    FixIme {
        /// 覆盖已存在的 build/ 文件（config/*.yaml 永不覆盖）
        #[arg(long)]
        force: bool,
        /// 预编译产物目录；缺省按 exe / AppImage / deb 路径自动定位
        #[arg(long, value_name = "DIR")]
        payload: Option<PathBuf>,
        /// 只铺 build/，不给 Weasel/Linux 的 data 方案打补丁
        #[arg(long)]
        no_data_patch: bool,
        /// 把 JSON 报告同时写到文件（安装器自检从这里读字段）
        #[arg(long, value_name = "FILE")]
        report: Option<PathBuf>,
    },
    /// 只读：检查输入法产物是否可用（唯一判据 assess；NSIS 自检解析它的 JSON）
    #[command(name = "check-ime")]
    CheckIme {
        /// 预编译产物目录（只用于判断「payload 里带、用户目录里没有」的方案）
        #[arg(long, value_name = "DIR")]
        payload: Option<PathBuf>,
        /// 把 JSON 报告同时写到文件（NSIS 自检不再自己解析 stdout）
        #[arg(long, value_name = "FILE")]
        report: Option<PathBuf>,
    },
    /// 安装/修复输入法：唯一状态机 ensure（--mode install 时含输入法本体与模板）
    #[command(name = "ensure-ime")]
    EnsureIme {
        /// 入口身份：install = 安装完成后首次运行（装 Weasel + 播种模板 + 显式部署）；
        /// fix = 用户显式修复；bootstrap = 应用启动自举（同样装 Weasel + 播种模板，
        /// 但部署 deferred：绝不 taskkill、不跑部署器、不打断正在打字的用户）。
        #[arg(long, value_name = "MODE", default_value = "install")]
        #[arg(value_parser = ["install", "fix", "bootstrap"])]
        mode: String,
        /// 覆盖已存在的 build/ 文件（config/*.yaml 永不覆盖）
        #[arg(long)]
        force: bool,
        /// 预编译产物目录；缺省按 exe / AppImage / deb 路径自动定位
        #[arg(long, value_name = "DIR")]
        payload: Option<PathBuf>,
        /// 只铺 build/，不给 Weasel/Linux 的 data 方案打补丁
        #[arg(long)]
        no_data_patch: bool,
        /// 把 JSON 报告同时写到文件（安装器自检从这里读字段）
        #[arg(long, value_name = "FILE")]
        report: Option<PathBuf>,
    },
    /// 部署输入法：Linux 优先外部 rime_deployer，其次进程内 librime；Windows 停服务后进程内
    #[command(name = "deploy-ime")]
    DeployIme {
        /// shared data 目录（缺省按平台/注册表探测）
        #[arg(long = "shared-dir", value_name = "DIR")]
        shared_dir: Option<PathBuf>,
        /// librime 动态库路径（缺省自动定位：<WeaselRoot>\rime.dll / librime.so.1）
        #[arg(long = "library", value_name = "PATH")]
        library: Option<PathBuf>,
    },
    /// 把 Rime 方案裁剪到随包保留清单（安装器/自举用；RIMESYNC_PRUNE_SCHEMAS=0 关闭）
    #[command(name = "prune-schemas")]
    PruneSchemas {
        /// 只报告将删除什么，不落盘
        #[arg(long)]
        dry_run: bool,
        /// data 目录（可重复；缺省按平台/注册表探测，Linux: /usr/share/rime-data）
        #[arg(long = "data-dir", value_name = "DIR")]
        data_dir: Vec<PathBuf>,
        /// 用户 Rime 目录（缺省自动探测）
        #[arg(long = "user-dir", value_name = "DIR")]
        user_dir: Option<PathBuf>,
    },
}

impl Command {
    /// 只有真正要访问 WebDAV 的子命令才解析密码——解析会探测钥匙串，
    /// 而桌面会话里的 D-Bus 调用可能阻塞，detect/backup/history 不该为此买单。
    fn needs_credentials(&self) -> bool {
        matches!(
            self,
            Command::TestConn
                | Command::Status
                | Command::Transport
                | Command::Apply
                | Command::Probe
                | Command::Watch { .. }
        )
    }
}

/// 密码后端的懒解析：第一次真正读写密码时才探测钥匙串（探测本身有超时）。
struct Secrets {
    app_data: PathBuf,
    selection: OnceCell<secret::SecretSelection>,
}

impl Secrets {
    fn new(app_data: PathBuf) -> Self {
        Self {
            app_data,
            selection: OnceCell::new(),
        }
    }

    fn selection(&self) -> &secret::SecretSelection {
        self.selection
            .get_or_init(|| secret::detect(&self.app_data))
    }

    fn store(&self) -> &dyn secret::SecretStore {
        self.selection().store.as_ref()
    }

    fn describe(&self) -> String {
        self.selection().describe()
    }
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

/// fix-ime 铺完仍不可用：报告已经打印过，main 只负责退出码。
#[derive(Debug)]
struct FixImeFailed;

/// fix-ime 的 JSON 报告：PrebuiltReport 的字段平铺，再补 data 补丁与输入法结果。
#[derive(Serialize)]
struct FixImeJson<'a> {
    #[serde(flatten)]
    prebuilt: &'a prebuilt::PrebuiltReport,
    /// 唯一状态机的结论：ok / noop / partial / failed。
    result: &'a str,
    /// Partial/Failed 必非空（Ok/Noop 也带 reason 行）。
    detail: String,
    /// 本次实际做过的每一步（assess / install / data / deploy）。
    actions: &'a [ime_state::Action],
    /// 最终 assess（build_ok / reason / 逐方案状态）。
    assessment: &'a ime_state::Assessment,
    /// 本次真正补上的方案 id（与 build_repaired 同义）。
    repaired: Vec<String>,
    /// 本次真正写入的 data 方案个数（重复执行时为 0）。
    data_patched: usize,
    data: &'a data_patch::DataPatchReport,
    /// 非保留方案的清理结果（0.6.21 / A-5：裁剪已纳入 ensure）。
    prune: &'a prune::PruneReport,
    /// 输入法（Weasel）健康检查与自举结果（0.6.20）。
    weasel_present: bool,
    weasel_installed: bool,
    weasel_version: String,
    weasel_installer: String,
    weasel_installer_source: String,
    weasel_install: String,
    weasel_error: Option<String>,
    weasel_needs_elevation: bool,
    weasel_log: String,
}

/// check-ime 的只读报告（NSIS 自检解析这些字段，不再自己看文件）。
#[derive(Serialize)]
struct CheckImeJson<'a> {
    /// ok / failed（唯一的判据结论；与 build_ok 同步）。
    result: &'a str,
    build_ok: bool,
    reason: &'a str,
    detail: String,
    rime_dir: String,
    payload: String,
    payload_source: String,
    /// 找不到预编译产物时的原因（只读探测不因此失败）。
    payload_error: Option<String>,
    /// 参与判定的方案个数。
    targets: usize,
    schemas: &'a [ime_state::SchemaState],
    missing: Vec<String>,
}

/// ensure-ime 的报告：PrebuiltReport 的字段平铺，再加状态机结论与 actions。
#[derive(Serialize)]
struct EnsureImeJson<'a> {
    #[serde(flatten)]
    report: &'a prebuilt::PrebuiltReport,
    result: &'a str,
    ok: bool,
    detail: String,
    reason: &'a str,
    rime_dir: String,
    payload: String,
    payload_source: String,
    payload_error: Option<String>,
    repaired: Vec<String>,
    actions: &'a [ime_state::Action],
    schemas: &'a [ime_state::SchemaState],
    data_patched: usize,
    /// 入口身份：install / fix / bootstrap。
    mode: &'a str,
    /// 输入法本体（Weasel）检查/安装结果（install / bootstrap 会真的安装）。
    weasel_present: bool,
    weasel_installed: bool,
    weasel_version: String,
    weasel_install: String,
    weasel_installer: String,
    weasel_installer_source: String,
    weasel_error: Option<String>,
    weasel_needs_elevation: bool,
    weasel_log: String,
    /// config 模板播种结果（install / bootstrap + Windows）。
    config_seeded: &'a ime_state::SeedReport,
}

/// 解析 --payload / 自动定位；找不到不是错误（只读探测要照常出报告）。
fn resolve_check_payload(
    payload: Option<PathBuf>,
) -> (Option<prebuilt::PayloadLocation>, Option<String>) {
    match payload {
        Some(dir) => (
            Some(prebuilt::PayloadLocation {
                path: dir,
                source: "argument:--payload".to_string(),
            }),
            None,
        ),
        None => match prebuilt::resolve_payload(exe_dir().as_deref()) {
            Ok(location) => (Some(location), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        },
    }
}

impl std::fmt::Display for FixImeFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("fix-ime could not restore the input method")
    }
}

impl std::error::Error for FixImeFailed {}

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
        if error.downcast_ref::<ProbeFailed>().is_some()
            || error.downcast_ref::<FixImeFailed>().is_some()
        {
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
    if cli.proxy.is_some() && cli.no_proxy {
        bail!("--proxy 与 --no-proxy 不能同时使用");
    }
    if let Some(url) = cli.proxy.as_deref() {
        config.proxy.enabled = true;
        config.proxy.url = url.trim().to_string();
        // 解析失败立即终止：既不落盘也不发起任何请求。
        config
            .proxy
            .validate()
            .with_context(|| format!("代理配置无效：{url}"))?;
    } else if cli.no_proxy {
        config.proxy.enabled = false;
    }

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
    // 密码后端懒解析：没用到密码就不探测钥匙串（桌面会话里 D-Bus 调用可能永久阻塞）。
    let secrets = Secrets::new(app_data.clone());
    if config.legacy_password || config.schema_version < config::CONFIG_SCHEMA_VERSION {
        // 只有要把明文密码搬进后端时才需要解析后端。
        let migration = if config.legacy_password {
            config.migrate(secrets.store(), &config_path)
        } else {
            config.migrate_schema_only(&config_path)
        };
        match migration {
            Ok(report) if report.changed => {
                stored.legacy_password = false;
                stored.password_ref = config.password_ref;
                stored.schema_version = config.schema_version;
                if !cli.json {
                    if report.password_migrated {
                        eprintln!(
                            "config schema v{} -> v{} 迁移完成（密码后端 {}）",
                            report.from,
                            report.to,
                            secrets.describe()
                        );
                    } else {
                        eprintln!("config schema v{} -> v{} 迁移完成", report.from, report.to);
                    }
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("warning: 配置迁移失败（原文件保留）: {error:#}"),
        }
    }
    if cli.command.needs_credentials() {
        if let Err(error) = config.load_password(secrets.store()) {
            eprintln!("warning: 读取已保存密码失败（继续，按无密码/环境变量处理）: {error:#}");
        }
        if let Err(error) = config.load_proxy_password(secrets.store()) {
            eprintln!("warning: 读取已保存的代理密码失败（继续，按无代理认证处理）: {error:#}");
        }
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
        Command::FixIme {
            force,
            payload,
            no_data_patch,
            report,
        } => {
            let rime_dir = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let location = match payload {
                Some(dir) => prebuilt::PayloadLocation {
                    path: dir,
                    source: "argument:--payload".to_string(),
                },
                None => prebuilt::resolve_payload(exe_dir().as_deref())?,
            };
            let data_dirs = data_patch::data_dirs();
            // 0.6.23 / A-1：唯一状态机。判据/顺序/结论都在 core 里，CLI 不再自己推。
            //
            // A-1：Mode::Fix 现在**自己**负责装输入法本体（Options::fix() 的
            // install_ime=true），并在装完后重新解析 data_dirs（调用方那份是
            // 「装之前」的，全新机器上为空 → data 补丁会打在不存在的目录上）。
            // 所以这里**不再**单独调 ime::ensure —— 那条重复路径已经删掉。
            let outcome = ime_state::ensure(
                &rime_dir,
                Some(location.path.as_path()),
                &data_dirs,
                &ime_state::Options::fix()
                    .with_force(force)
                    .with_patch_data(!no_data_patch)
                    .with_exe_dir(exe_dir())
                    .with_source(location.source.clone()),
            );
            // A-5：方案裁剪已纳入 ensure（CLI 与 GUI 同一入口、同一动作序列）。
            let pruned = &outcome.prune;
            // 输入法本体的结果由 ensure 内部的 ime::ensure 给出（同一份数据）。
            let weasel = &outcome.ime;
            let data = &outcome.data;
            let report_json = FixImeJson {
                prebuilt: &outcome.report,
                result: outcome.result.as_str(),
                detail: outcome.detail.clone(),
                actions: &outcome.actions,
                assessment: &outcome.assessment,
                repaired: outcome.repaired.clone(),
                data_patched: data.patched(),
                data,
                prune: pruned,
                weasel_present: weasel.present,
                weasel_installed: weasel.installed,
                weasel_version: weasel.version.clone(),
                weasel_installer: weasel.installer.clone(),
                weasel_installer_source: weasel.installer_source.clone(),
                weasel_install: weasel.install.clone(),
                weasel_error: weasel.error.clone(),
                weasel_needs_elevation: weasel.needs_elevation,
                weasel_log: weasel.log.clone(),
            };
            if cli.json {
                print_json(&report_json)?;
            } else {
                println!("rime_dir={}", rime_dir.display());
                println!(
                    "payload={} source={}",
                    location.path.display(),
                    location.source
                );
                println!(
                    "result={} build_ok={} detail={}",
                    outcome.result.as_str(),
                    outcome.report.build_ok,
                    outcome.detail
                );
                println!(
                    "copied={} kept={} overwritten={} config_copied={} config_kept={} failed={} escalated={}",
                    outcome.report.copied,
                    outcome.report.kept,
                    outcome.report.overwritten,
                    outcome.report.config_copied,
                    outcome.report.config_kept,
                    outcome.report.failed,
                    outcome.report.escalated
                );
                // B-2：config 被 data 门禁挡下 = 用户拿到的输入法没有 schema_list。
                // 这条必须在文本输出里出现（CLI 的 --json 之外的可读输出）。
                println!(
                    "config_skipped=[{}]",
                    outcome.report.config_skipped.join(",")
                );
                for item in &outcome.report.config_skipped {
                    eprintln!("warning: config 模板被跳过：{item}（该输入法将没有 schema_list）");
                }
                if !outcome.report.manifest_failed.is_empty() {
                    println!(
                        "manifest_failed=[{}]",
                        outcome.report.manifest_failed.join(",")
                    );
                    for item in &outcome.report.manifest_failed {
                        eprintln!("warning: 预编译产物校验失败：{item}");
                    }
                }
                println!(
                    "build_ok={} probe_after={} build_source={}",
                    outcome.report.build_ok,
                    outcome.report.probe_after,
                    outcome.report.build_source
                );
                println!("build_schemas={}", outcome.report.build_schemas_line());
                println!("build_repaired={}", outcome.report.build_repaired.join(","));
                println!("build_missing={}", outcome.report.build_missing.join(","));
                println!(
                    "deploy_method={} deploy_result={} deploy_ok={} deploy_ms={}",
                    outcome.report.deploy_method,
                    outcome.report.deploy_result,
                    outcome.report.deploy_ok,
                    outcome.report.deploy_ms
                );
                println!("deploy_detail={}", outcome.report.deploy_detail);
                println!("deploy_inproc={}", outcome.report.deploy_inproc);
                for action in &outcome.actions {
                    println!(
                        "action={} changed={} {}",
                        action.kind, action.changed, action.detail
                    );
                }
                if let Some(log) = &outcome.report.log_path {
                    println!("log={log}");
                }
                if no_data_patch {
                    println!("data_patch=disabled (--no-data-patch)");
                } else {
                    println!(
                        "data_patched={} data_unchanged={} data_skipped={} data_failed={} backup_dir={}",
                        data.patched(),
                        data.schemas_unchanged.len(),
                        data.schemas_skipped.len(),
                        data.errors.len(),
                        if data.backup_dir.is_empty() {
                            "-"
                        } else {
                            &data.backup_dir
                        }
                    );
                    println!(
                        "punctuator_patched={} punctuator_skipped={}",
                        data.punctuator_patched,
                        data.punctuator_skipped.len()
                    );
                    for item in &data.punctuator_skipped {
                        println!("punctuator_skipped_item={item}");
                    }
                    println!(
                        "data_dir={}",
                        if data.data_dir.is_empty() {
                            "-"
                        } else {
                            data.data_dir.as_str()
                        }
                    );
                    println!("data_version={}", data.version);
                    println!("data_fallback={}", data.fallback);
                    println!("data_mode={}", data.mode);
                    println!("data_deploy={}", data.deploy_triggered);
                    println!("data_skipped_not_retained={}", data.skipped_not_retained);
                    for file in &data.custom_written {
                        println!("data_custom_written={file}");
                    }
                    let candidates: Vec<&str> = data
                        .data_candidates
                        .iter()
                        .map(|candidate| candidate.path.as_str())
                        .collect();
                    println!("data_candidates=[{}]", candidates.join(", "));
                    if let Some(reason) = &data.data_error {
                        println!("data_error={reason}");
                        eprintln!("warning: {reason}");
                    }
                    for file in &data.schemas_patched {
                        println!("data_file={file}");
                    }
                    for file in &data.schemas_skipped {
                        println!("data_skipped={file}");
                    }
                    for error in &data.errors {
                        eprintln!("warning: {error}");
                    }
                    if data.needs_elevation() {
                        eprintln!(
                            "hint: 写入 data 方案需要管理员权限；请以管理员身份重新运行 rime-sync-cli fix-ime（或先以管理员身份启动 rime-sync 再点「修复输入法」）"
                        );
                    }
                    if data.mode == data_patch::MODE_CUSTOM
                        && data.deploy_triggered != data_patch::DEPLOY_OK
                    {
                        eprintln!(
                            "hint: 已写用户级 custom.yaml，但没能自动触发部署；请执行 fcitx5-remote -r 或右键托盘「重新部署」"
                        );
                    }
                }
                println!("{}", pruned.summary());
                println!("prune_disabled={}", pruned.disabled);
                println!("prune_removed={}", pruned.removed());
                println!("prune_backups={}", pruned.backups.len());
                println!("prune_failed={}", pruned.failures.len());
                for path in &pruned.removed {
                    println!("prune_removed_file={path}");
                }
                for path in &pruned.backups {
                    println!("prune_backup={path}");
                }
                for error in &pruned.failures {
                    eprintln!("warning: {error}");
                }
                println!(
                    "weasel_present={} weasel_installed={} weasel_version={} weasel_install={}",
                    weasel.present,
                    weasel.installed,
                    if weasel.version.is_empty() {
                        "-"
                    } else {
                        weasel.version.as_str()
                    },
                    weasel.install
                );
                if !weasel.installer.is_empty() {
                    println!(
                        "weasel_installer={} source={}",
                        weasel.installer, weasel.installer_source
                    );
                }
                if let Some(error) = &weasel.error {
                    eprintln!("warning: {error}");
                }
                if weasel.needs_elevation {
                    eprintln!(
                        "hint: 安装输入法需要管理员权限；请以管理员身份重新启动 rime-sync 后再点「修复输入法」，或手动运行随包安装器"
                    );
                }
            }
            if let Some(path) = report.as_deref() {
                write_json_file(path, &report_json)?;
            }
            if outcome.result.exit_ok() && weasel.ok() {
                Ok(())
            } else {
                Err(anyhow::Error::new(FixImeFailed))
            }
        }
        Command::CheckIme { payload, report } => {
            // 只读：不铺文件、不打补丁、不部署。NSIS 自检与 CI 都用它当唯一判据。
            let rime_dir = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let (payload_location, payload_error) = resolve_check_payload(payload);
            let data_dirs = data_patch::data_dirs();
            let keep = ime_state::KeepSet::resolve(
                &rime_dir,
                &data_dirs,
                payload_location
                    .as_ref()
                    .map(|location| location.path.as_path()),
            );
            let assessment = ime_state::assess(&rime_dir, &data_dirs, &keep);
            let check = CheckImeJson {
                result: if assessment.build_ok { "ok" } else { "failed" },
                build_ok: assessment.build_ok,
                reason: assessment.reason.as_str(),
                detail: assessment.detail(),
                rime_dir: rime_dir.display().to_string(),
                payload: payload_location
                    .as_ref()
                    .map(|location| location.path.display().to_string())
                    .unwrap_or_default(),
                payload_source: payload_location
                    .as_ref()
                    .map(|location| location.source.clone())
                    .unwrap_or_default(),
                payload_error,
                targets: assessment.schemas.len(),
                schemas: &assessment.schemas,
                missing: assessment.missing_ids(),
            };
            if cli.json {
                print_json(&check)?;
            } else {
                println!("rime_dir={}", rime_dir.display());
                println!(
                    "result={} build_ok={} reason={}",
                    check.result, check.build_ok, check.reason
                );
                println!("detail={}", check.detail);
                println!("schemas={}", assessment.line());
                println!("missing={}", check.missing.join(","));
                if let Some(error) = &check.payload_error {
                    println!("payload_error={error}");
                }
            }
            if let Some(path) = report.as_deref() {
                write_json_file(path, &check)?;
            }
            if assessment.build_ok {
                Ok(())
            } else {
                Err(anyhow::Error::new(FixImeFailed))
            }
        }
        Command::EnsureIme {
            mode,
            force,
            payload,
            no_data_patch,
            report,
        } => {
            // --mode install = 安装版首次启动：输入法本体 + config 模板也归它管
            // （安装器不再装/判输入法）。fix/bootstrap 保持旧语义。
            let mode = match mode.as_str() {
                "fix" => ime_state::Mode::Fix,
                "bootstrap" => ime_state::Mode::Bootstrap,
                _ => ime_state::Mode::Install,
            };
            let options = match mode {
                ime_state::Mode::Install => ime_state::Options::install(),
                ime_state::Mode::Fix => ime_state::Options::fix(),
                ime_state::Mode::Bootstrap => ime_state::Options::bootstrap(),
            };
            let rime_dir = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let (payload_location, payload_error) = resolve_check_payload(payload);
            let data_dirs = data_patch::data_dirs();
            let outcome = ime_state::ensure(
                &rime_dir,
                payload_location
                    .as_ref()
                    .map(|location| location.path.as_path()),
                &data_dirs,
                &options
                    .with_force(force)
                    .with_patch_data(!no_data_patch)
                    .with_exe_dir(exe_dir())
                    .with_source(
                        payload_location
                            .as_ref()
                            .map(|location| location.source.clone())
                            .unwrap_or_default(),
                    ),
            );
            let json = EnsureImeJson {
                report: &outcome.report,
                result: outcome.result.as_str(),
                ok: outcome.ok(),
                detail: outcome.detail.clone(),
                reason: outcome.assessment.reason.as_str(),
                rime_dir: rime_dir.display().to_string(),
                payload: payload_location
                    .as_ref()
                    .map(|location| location.path.display().to_string())
                    .unwrap_or_default(),
                payload_source: payload_location
                    .as_ref()
                    .map(|location| location.source.clone())
                    .unwrap_or_default(),
                payload_error,
                repaired: outcome.repaired.clone(),
                actions: &outcome.actions,
                schemas: &outcome.assessment.schemas,
                data_patched: outcome.data.patched(),
                mode: mode.as_str(),
                weasel_present: outcome.ime.present,
                weasel_installed: outcome.ime.installed,
                weasel_version: outcome.ime.version.clone(),
                weasel_install: outcome.ime.install.clone(),
                weasel_installer: outcome.ime.installer.clone(),
                weasel_installer_source: outcome.ime.installer_source.clone(),
                weasel_error: outcome.ime.error.clone(),
                weasel_needs_elevation: outcome.ime.needs_elevation,
                weasel_log: outcome.ime.log.clone(),
                config_seeded: &outcome.seeded,
            };
            if cli.json {
                print_json(&json)?;
            } else {
                println!("rime_dir={}", rime_dir.display());
                println!(
                    "result={} ok={} build_ok={} reason={}",
                    json.result, json.ok, outcome.report.build_ok, json.reason
                );
                println!("detail={}", json.detail);
                println!(
                    "mode={} weasel_present={} weasel_installed={} weasel_install={}",
                    json.mode, json.weasel_present, json.weasel_installed, json.weasel_install
                );
                println!("config_seeded={}", outcome.seeded.summary());
                println!("schemas={}", outcome.assessment.line());
                println!("build_repaired={}", json.repaired.join(","));
                println!(
                    "copied={} kept={} overwritten={} config_copied={} failed={}",
                    outcome.report.copied,
                    outcome.report.kept,
                    outcome.report.overwritten,
                    outcome.report.config_copied,
                    outcome.report.failed
                );
                for action in &outcome.actions {
                    println!(
                        "action={} changed={} {}",
                        action.kind, action.changed, action.detail
                    );
                }
            }
            if let Some(path) = report.as_deref() {
                write_json_file(path, &json)?;
            }
            if outcome.result.exit_ok() {
                Ok(())
            } else {
                Err(anyhow::Error::new(FixImeFailed))
            }
        }

        Command::DeployIme {
            shared_dir,
            library,
        } => {
            // 0.6.23：进程内 librime 部署。不链接 librime（运行时 dlopen/LoadLibraryW），
            // 找不到库或符号就返回可读错误，绝不 panic。
            let user_dir = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let shared = match shared_dir {
                Some(dir) => dir,
                None => data_patch::data_dirs()
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| PathBuf::from("/usr/share/rime-data")),
            };
            // 0.6.23 d：先停服务（并确认退出）再部署，避免和输入法抢 user.yaml。
            // Linux：外部 rime_deployer > 输入法在跑则跳过 > 进程内（见 linux_deploy），
            // 三步都有超时；绝不去杀用户正在用的输入法。
            let report = if cfg!(windows) {
                let run = || match &library {
                    Some(path) => librime::deploy_with_library(
                        path,
                        &librime::Traits::for_dirs(&shared, &user_dir),
                    ),
                    None => librime::deploy_with_dirs(&shared, &user_dir),
                };
                let stop = deploy::stop_before_deploy(&deploy::SystemRunner);
                match deploy::inproc_after_stop(&stop, run) {
                    Some(report) => report,
                    None => bail!(
                        "结束 WeaselServer 失败（server={} {}），已跳过进程内部署",
                        stop.state.as_str(),
                        stop.detail
                    ),
                }
            } else {
                linux_deploy::deploy(&user_dir, &shared, library.as_deref())
            };
            if cli.json {
                print_json(&report)?;
            } else {
                println!("deploy_method={}", report.deploy_method);
                println!("deploy_ok={}", report.deploy_ok);
                println!("deploy_ms={}", report.deploy_ms);
                println!(
                    "library={}",
                    if report.library.is_empty() {
                        "-"
                    } else {
                        &report.library
                    }
                );
                println!("shared_data_dir={}", report.shared_data_dir);
                println!("user_data_dir={}", report.user_data_dir);
                println!("deploy_detail={}", report.detail);
            }
            if report.deploy_ok {
                Ok(())
            } else if report.deploy_method == linux_deploy::METHOD_SKIPPED {
                // 跳过不是失败，但部署确实没发生：退出码非 0，细则里带重试建议。
                Err(anyhow::anyhow!("deploy skipped: {}", report.detail))
            } else if report.deploy_method == linux_deploy::METHOD_EXTERNAL_FAILED {
                Err(anyhow::anyhow!("external deploy failed: {}", report.detail))
            } else {
                Err(anyhow::anyhow!(
                    "in-process deploy failed: {}",
                    report.detail
                ))
            }
        }
        Command::PruneSchemas {
            dry_run,
            data_dir,
            user_dir,
        } => {
            let report = prune::prune_auto_with(
                if data_dir.is_empty() {
                    None
                } else {
                    Some(data_dir.as_slice())
                },
                user_dir.as_deref(),
                prune::PruneOptions { dry_run },
            );
            if cli.json {
                print_json(&report)
            } else {
                println!("{}", report.summary());
                println!("prune_disabled={}", report.disabled);
                println!("prune_removed={}", report.removed());
                println!("prune_backups={}", report.backups.len());
                println!("prune_failed={}", report.failures.len());
                println!("prune_dry_run={}", report.dry_run);
                println!("keep={}", report.keep.join(","));
                println!("kept_by_env={}", report.kept_by_env.join(","));
                for warning in &report.warnings {
                    eprintln!("warning: {warning}");
                }
                for path in &report.removed {
                    println!("prune_removed_file={path}");
                }
                for path in &report.backups {
                    println!("prune_backup={path}");
                }
                for error in &report.failures {
                    eprintln!("warning: {error}");
                }
                if report.needs_elevation() {
                    eprintln!(
                        "hint: 清理 data 目录需要管理员权限；请以管理员/root 身份重新运行 rime-sync-cli prune-schemas"
                    );
                }
                Ok(())
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
    DavClient::with_proxy(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
        Some(&config.proxy),
    )
    .with_context(|| {
        format!(
            "初始化 WebDAV 客户端失败（代理 {}）",
            config.proxy.describe()
        )
    })
}

/// 当前可执行文件所在目录：安装器把预编译产物放在它下面的 resources/prebuilt。
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
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

/// 把 JSON 报告写到文件（安装器自检读它，不靠被截断的 stdout）。
///
/// 用 pretty 格式：每行一个字段，NSIS 逐行做子串匹配（行长短于 NSIS 的 1024 上限）。
fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建报告目录失败：{}", parent.display()))?;
        }
    }
    let text = format!(
        "{}
",
        serde_json::to_string_pretty(value)?
    );
    fs::write(path, text).with_context(|| format!("写报告失败：{}", path.display()))?;
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

    /// detect/backup/history 这类命令绝不能解析密码（= 不探测钥匙串）。
    #[test]
    fn only_webdav_commands_need_credentials() {
        let command = |args: &[&str]| -> Command {
            Cli::try_parse_from(args).expect("解析命令行").command
        };
        assert!(!command(&["rime-sync-cli", "detect"]).needs_credentials());
        assert!(!command(&["rime-sync-cli", "backups"]).needs_credentials());
        assert!(!command(&["rime-sync-cli", "history"]).needs_credentials());
        assert!(!command(&["rime-sync-cli", "backup"]).needs_credentials());
        assert!(!command(&["rime-sync-cli", "rollback", "id"]).needs_credentials());
        let needing: [&[&str]; 6] = [
            &["rime-sync-cli", "test-conn"],
            &["rime-sync-cli", "status"],
            &["rime-sync-cli", "transport"],
            &["rime-sync-cli", "apply"],
            &["rime-sync-cli", "probe"],
            &["rime-sync-cli", "watch"],
        ];
        for args in needing {
            assert!(command(args).needs_credentials(), "{args:?}");
        }
    }
}
