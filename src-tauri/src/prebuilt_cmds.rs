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

use std::path::{Path, PathBuf};

use rime_sync_core::prebuilt::{self, BuildProbe, InstallOptions, PayloadLocation, PrebuiltReport};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::{detect_rime, ensure_config_meta, push_log, AppState};

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

/// 启动自举：probe 不是 prebuilt 时才动手，失败只记日志。
///
/// 拷贝放后台线程：`setup` 里做磁盘 IO 会拖慢窗口出现，而且 Windows 上
/// 首次访问被占用的 build/ 可能卡住，绝不能阻塞启动。
pub(crate) fn bootstrap(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        let config = match ensure_config_meta(&handle, &state.0) {
            Ok(config) => config,
            Err(error) => {
                push_log(
                    &state.0,
                    format!("[bootstrap] 读取配置失败，跳过预编译产物自举：{error}"),
                );
                return;
            }
        };
        let rime_dir = match detect_rime(&config) {
            Ok(detection) => detection.path,
            Err(error) => {
                push_log(
                    &state.0,
                    format!("[bootstrap] 未定位 Rime 用户目录，跳过预编译产物自举：{error}"),
                );
                return;
            }
        };
        let probe = prebuilt::probe_build(&rime_dir);
        if probe == BuildProbe::Prebuilt {
            return;
        }
        let Some(force) = env_mode() else {
            push_log(
                &state.0,
                "[bootstrap] RIMESYNC_PREBUILT=0，跳过预编译产物自举",
            );
            return;
        };
        match perform(&rime_dir, force) {
            Ok((location, report)) => push_log(
                &state.0,
                format!(
                    "[bootstrap] probe={} 不可用，已自举：{} payload={}（{}）",
                    probe,
                    summary(&report),
                    location.path.display(),
                    location.source
                ),
            ),
            Err(error) => push_log(
                &state.0,
                format!("[bootstrap] 预编译产物自举失败（不影响启动）：{error}"),
            ),
        }
    });
}
