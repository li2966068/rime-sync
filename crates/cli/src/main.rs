//! rime-sync-cli：headless 入口，供 E2E 与无 GUI 环境使用。

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rime_sync_core::config::{self, Config};
use rime_sync_core::engine::{Engine, Progress, ProgressSink, Report};
use rime_sync_core::index::FileIndex;
use rime_sync_core::plan;
use rime_sync_core::platform;
use rime_sync_core::rime_dir;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

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
}

#[derive(Serialize)]
struct DetectReport {
    path: String,
    source: String,
}

#[derive(Serialize)]
struct ConnReport {
    ok: bool,
    latency_ms: u128,
    server: String,
    dav_class: String,
    status: u16,
    dav: Vec<String>,
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
}

fn main() {
    let cli = Cli::parse();
    let wants_json = cli.json;
    if let Err(error) = run(cli) {
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
    let mut loaded = Config::load(&config_path)?;
    let mut config = loaded.clone();
    config.apply_env();
    config.apply_overrides(
        cli.dav_url.as_deref(),
        cli.dav_user.as_deref(),
        cli.dav_pass.as_deref(),
    );

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
            let dav = DavClient::new(
                &config.webdav.url,
                &config.webdav.username,
                &config.webdav.password,
                &config.webdav.remote_dir,
            )?;
            let started = Instant::now();
            let probe = dav.probe()?;
            let latency_ms = started.elapsed().as_millis();
            let ok = probe.status != 401 && probe.status != 403 && probe.status < 500;
            let dav_class = if probe.status == 207
                || probe.status < 300
                || probe.dav.iter().any(|value| value.contains('1'))
            {
                "webdav"
            } else if probe.status == 404 {
                "missing-dir"
            } else {
                "unknown"
            };
            let report = ConnReport {
                ok,
                latency_ms,
                server: dav.base_url().to_string(),
                dav_class: dav_class.to_string(),
                status: probe.status,
                dav: probe.dav,
            };
            if cli.json {
                print_json(&report)
            } else {
                println!(
                    "ok={} status={} latency={}ms server={} dav_class={}",
                    report.ok, report.status, report.latency_ms, report.server, report.dav_class
                );
                Ok(())
            }
        }
        Command::Status => {
            let detection = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let device_id = if config.device_id.trim().is_empty() {
                rime_dir::installation_id(&detection)
                    .unwrap_or_else(|| "(unset)".to_string())
            } else {
                config.device_id.clone()
            };
            let (local_shared, local_snapshots) = count_local(&detection)?;
            let index = FileIndex::load(&state_dir(&config_path).join("index.json"))?;
            let mut remote_devices = Vec::new();
            let mut remote_shared = None;
            let mut remote_error = None;
            match DavClient::new(
                &config.webdav.url,
                &config.webdav.username,
                &config.webdav.password,
                &config.webdav.remote_dir,
            ) {
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
                            remote_shared = Some(entries.iter().filter(|entry| !entry.is_dir).count())
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
            };
            if cli.json {
                print_json(&report)
            } else {
                println!("config={}", report.config_path);
                println!("rime_dir={}", report.rime_dir);
                println!("device_id={}", report.device_id);
                println!(
                    "remote={} dir={}",
                    report.dav_url, report.remote_dir
                );
                println!(
                    "local_shared={} local_snapshots={} index_files={}",
                    report.local_shared, report.local_snapshots, report.index_files
                );
                println!("remote_devices={:?} remote_shared={:?}", report.remote_devices, report.remote_shared);
                if let Some(error) = report.remote_error {
                    eprintln!("remote_error: {error}");
                }
                Ok(())
            }
        }
        Command::Transport | Command::Apply => {
            let detection = resolve_rime_dir(cli.rime_dir.as_deref(), &config)?;
            let device_was_empty = config.device_id.trim().is_empty();
            if device_was_empty {
                config.resolve_device_id(&detection);
            }
            let rime_str = detection.display().to_string();
            let rime_dir_was_empty = loaded.rime_dir.is_none();
            if device_was_empty || rime_dir_was_empty {
                loaded.device_id = config.device_id.clone();
                loaded.rime_dir = Some(rime_str);
                loaded.save(&config_path)?;
            }

            let apply_mode = matches!(cli.command, Command::Apply);
            let trigger = platform::default_trigger(config.stop_fcitx5_during_apply);
            let mut engine = Engine::new(
                config,
                detection,
                state_dir(&config_path),
                trigger,
            )?;
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
    }
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

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_report(report: &Report) {
    println!(
        "ok={} pulled={} pushed={} merged={} conflicts={} skipped={} duration={}ms",
        report.ok,
        report.pulled,
        report.pushed,
        report.merged,
        report.conflicts.len(),
        report.skipped.len(),
        report.duration_ms
    );
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
                                file.file_name()
                                    .to_string_lossy()
                                    .ends_with(".userdb.txt")
                            })
                            .count();
                    }
                }
            }
        }
    }
    Ok((shared, snapshots))
}
