//! 预编译 build 产物自举（0.6.16）。
//!
//! 0.6.15 实测：/S 静默安装时 NSIS 钩子根本没执行（日志里连 phase=DONE 都没有），
//! WeaselDeployer /deploy 也可能返回 0 却不写任何 build 文件。兜底不能依赖安装器，
//! 必须由程序自己在启动时（或用户点「修复输入法」时）跑一遍。
//!
//! 全部文件操作在 rime-sync-core::prebuilt；这里只负责：
//! - 定位 payload 与 Rime 用户目录；
//! - 启动自举：后台线程、失败只记日志、绝不阻塞窗口；
//! - cmd_fix_ime：把同一套逻辑暴露给设置页。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use rime_sync_core::prebuilt::{self, BuildProbe, InstallOptions, PayloadLocation, PrebuiltReport};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::{detect_rime, ensure_config_meta, push_log, state_dir, AppState};

/// 与安装器一致的开关：0 = 关闭自举；force = 覆盖已存在的 build 文件。
/// 返回 None 表示关闭，Some(force) 表示是否覆盖。
fn env_mode() -> Option<bool> {
    match std::env::var(prebuilt::ENV_MODE) {
        Ok(value) if value.trim() == "0" => None,
        Ok(value) if value.trim().eq_ignore_ascii_case("force") => Some(true),
        _ => Some(false),
    }
}

/// 当前可执行文件所在目录：安装版把预编译产物放在 <exe 目录>/resources/prebuilt。
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn perform(rime_dir: &Path, force: bool) -> Result<(PayloadLocation, PrebuiltReport), String> {
    let location = prebuilt::resolve_payload(exe_dir().as_deref())
        .map_err(|error| format!("未找到预编译产物：{error:#}"))?;
    let report =
        prebuilt::install_prebuilt_with(&location.path, rime_dir, InstallOptions { force })
            .map_err(|error| format!("铺入预编译产物失败：{error:#}"))?;
    Ok((location, report))
}

/// 一句话结论；界面与日志共用。
fn summary(report: &PrebuiltReport) -> String {
    if report.build_ok {
        format!(
            "输入法产物已就绪（新铺 {}，沿用 {}，覆盖 {}，配置新增 {}）",
            report.copied, report.kept, report.overwritten, report.config_copied
        )
    } else {
        format!(
            "铺入 {} 个文件后 build/ 仍不可用（probe={}，失败 {}）；把日志里的 fix-ime 行发给维护者",
            report.copied + report.overwritten,
            report.probe_after,
            report.failed
        )
    }
}

#[derive(Debug, Serialize)]
pub struct FixImeReport {
    pub rime_dir: String,
    pub payload: String,
    pub payload_source: String,
    pub probe_before: String,
    pub message: String,
    #[serde(flatten)]
    pub report: PrebuiltReport,
}

/// 设置页「修复输入法」：与启动自举走同一套逻辑，force = 覆盖 build/。
#[tauri::command]
pub fn cmd_fix_ime(
    app: AppHandle,
    state: State<'_, AppState>,
    force: Option<bool>,
) -> Result<FixImeReport, String> {
    let config = ensure_config_meta(&app, &state.0)?;
    let rime_dir = detect_rime(&config)?.path;
    let probe_before = prebuilt::probe_build(&rime_dir);
    let (location, report) = perform(&rime_dir, force.unwrap_or(false))?;
    let message = summary(&report);
    push_log(
        &state.0,
        format!(
            "[fix-ime] {message} payload={} source={} probe_before={}",
            location.path.display(),
            location.source,
            probe_before
        ),
    );
    Ok(FixImeReport {
        rime_dir: rime_dir.display().to_string(),
        payload: location.path.display().to_string(),
        payload_source: location.source,
        probe_before: probe_before.as_str().to_string(),
        message,
        report,
    })
}

/// 启动自举的排障日志。
///
/// Windows 在 `%APPDATA%\com.rimesync.app\bootstrap.log`（Tauri 的 app_config_dir），
/// Linux 在 `~/.config/com.rimesync.app/bootstrap.log`。0.6.16 的实测里
/// `%APPDATA%\Rime\.rimesync-prebuilt.log` 根本没生成，分不清「自举没跑」还是
/// 「跑了但静默失败」；所以这里把每一步都先落盘再执行下一步，失败也写。
///
/// 每条单独 append + flush：自举在后台线程里，进程被关掉也要留下已完成的步骤。
/// 超过 1 MiB 轮转成 bootstrap.log.1（只留一份），避免长期启动把日志撑大。
const BOOTSTRAP_LOG: &str = "bootstrap.log";
const BOOTSTRAP_LOG_LIMIT: u64 = 1024 * 1024;

struct BootstrapLog {
    path: PathBuf,
}

impl BootstrapLog {
    fn open(app: &AppHandle) -> Result<Self, String> {
        let path = state_dir(app)?.join(BOOTSTRAP_LOG);
        let oversized = std::fs::metadata(&path)
            .map(|meta| meta.len() > BOOTSTRAP_LOG_LIMIT)
            .unwrap_or(false);
        if oversized {
            let _ = std::fs::rename(&path, path.with_extension("log.1"));
        }
        Ok(Self { path })
    }

    fn write(&self, message: impl std::fmt::Display) {
        let line = format!(
            "[{}] {message}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// 启动自举：probe 不是 prebuilt 时才动手，失败只记日志。
///
/// 拷贝放后台线程：`setup` 里做磁盘 IO 会拖慢窗口出现，而且 Windows 上
/// 首次访问被占用的 build/ 可能卡住，绝不能阻塞启动。
pub(crate) fn bootstrap(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        // 日志本身也可能失败（配置目录不可写）：降级成只写应用日志，自举继续。
        let logger = match BootstrapLog::open(&handle) {
            Ok(logger) => Some(logger),
            Err(error) => {
                push_log(
                    &state.0,
                    format!("[bootstrap] 无法创建自举日志（继续自举）：{error}"),
                );
                None
            }
        };
        let log = |line: String| {
            if let Some(logger) = &logger {
                logger.write(line);
            }
        };

        let exe = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|error| format!("<unavailable: {error}>"));
        log(format!(
            "bootstrap begin version={} exe={exe} RIMESYNC_PREBUILT={}",
            env!("CARGO_PKG_VERSION"),
            std::env::var(prebuilt::ENV_MODE).unwrap_or_else(|_| "<unset>".to_string())
        ));

        let config = match ensure_config_meta(&handle, &state.0) {
            Ok(config) => config,
            Err(error) => {
                log(format!("config failed error={error}"));
                log("bootstrap end result=failed stage=config".to_string());
                push_log(
                    &state.0,
                    format!("[bootstrap] 读取配置失败，跳过预编译产物自举：{error}"),
                );
                return;
            }
        };
        let detection = match detect_rime(&config) {
            Ok(detection) => detection,
            Err(error) => {
                log(format!("rime_dir failed error={error}"));
                log("bootstrap end result=failed stage=rime-dir".to_string());
                push_log(
                    &state.0,
                    format!("[bootstrap] 未定位 Rime 用户目录，跳过预编译产物自举：{error}"),
                );
                return;
            }
        };
        log(format!(
            "rime_dir={} source={}",
            detection.path.display(),
            detection.source
        ));
        let rime_dir = detection.path;

        let probe = prebuilt::probe_build(&rime_dir);
        log(format!("probe_before={probe}"));
        if probe == BuildProbe::Prebuilt {
            log("bootstrap end result=skip reason=probe-prebuilt".to_string());
            return;
        }
        let Some(force) = env_mode() else {
            log("bootstrap end result=skip reason=env-disabled RIMESYNC_PREBUILT=0".to_string());
            push_log(
                &state.0,
                "[bootstrap] RIMESYNC_PREBUILT=0，跳过预编译产物自举",
            );
            return;
        };

        // 与 perform() 同一顺序（resolve → install），这里展开是因为要在两步之间
        // 把「命中了哪个候选目录」写进日志。
        let location = match prebuilt::resolve_payload(exe_dir().as_deref()) {
            Ok(location) => location,
            Err(error) => {
                log(format!("payload failed stage=resolve error={error:#}"));
                log("bootstrap end result=failed stage=payload".to_string());
                push_log(
                    &state.0,
                    format!("[bootstrap] 未找到预编译产物（不影响启动）：{error:#}"),
                );
                return;
            }
        };
        log(format!(
            "payload hit path={} source={}",
            location.path.display(),
            location.source
        ));

        log(format!("install start force={force}"));
        match prebuilt::install_prebuilt_with(&location.path, &rime_dir, InstallOptions { force }) {
            Ok(report) => {
                log(format!(
                    "install done copied={} kept={} overwritten={} config_copied={} config_kept={} failed={} probe_after={} build_ok={} escalated={} core_log={}",
                    report.copied,
                    report.kept,
                    report.overwritten,
                    report.config_copied,
                    report.config_kept,
                    report.failed,
                    report.probe_after,
                    report.build_ok,
                    report.escalated,
                    report.log_path.as_deref().unwrap_or("-"),
                ));
                log(format!(
                    "bootstrap end result={}",
                    if report.build_ok { "ok" } else { "degraded" }
                ));
                push_log(
                    &state.0,
                    format!(
                        "[bootstrap] probe={} 不可用，已自举：{} payload={}（{}）",
                        probe,
                        summary(&report),
                        location.path.display(),
                        location.source
                    ),
                );
            }
            Err(error) => {
                log(format!("install failed error={error:#}"));
                log("bootstrap end result=failed stage=install".to_string());
                push_log(
                    &state.0,
                    format!("[bootstrap] 预编译产物自举失败（不影响启动）：{error}"),
                );
            }
        }
    });
}
