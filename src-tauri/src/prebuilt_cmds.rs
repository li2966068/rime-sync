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

use rime_sync_core::data_patch::{self, DataPatchReport};
use rime_sync_core::ime::{self, ImeReport};
use rime_sync_core::prebuilt::{self, BuildProbe, InstallOptions, PayloadLocation, PrebuiltReport};
use rime_sync_core::prune;
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

/// 补丁注释里写的版本：用发布版本（tauri.conf.json 的 version），不是 crate 的 0.1.0。
fn app_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
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

/// data 方案补丁：定位 Weasel 的 data 目录并直接改方案源文件。
///
/// 0.6.18 的起因：227 上 build/ 里已经有插件标记，但引擎没用它（按反斜杠不直出
/// 「、」、v1234 没候选），部署链又不可用——只能把补丁写进 data。
/// 每次启动都跑（幂等：没变化就一个字节都不写）。
fn patch_data(version: &str, source: data_patch::FixSource) -> DataPatchReport {
    // 0.6.20：data 目录按 WeaselRoot → weasel-* → InstallDir → %ProgramFiles%\Rime
    // 逐项探测，报告里带全部候选（227 的目录不存在问题就出在这里）。
    // source = Bootstrap 时不会主动重载 fcitx5（不打断正在打字的用户）。
    data_patch::patch_auto(version, source)
}

/// 输入法健康检查 + 缺失时用随包安装器装回去（0.6.20）。
///
/// Linux 返回 skipped(platform)（由包管理器负责）；Windows 缺失时会跑随包
/// weasel-*-installer.exe /S /NOREBOOT，最多等 300 秒。
fn ensure_ime() -> ImeReport {
    ime::ensure(exe_dir().as_deref())
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
    /// 本次真正写入的 data 方案个数（重复执行时为 0）。
    pub data_patched: usize,
    /// 是否需要提示用户以管理员身份重试。
    pub needs_elevation: bool,
    pub data: DataPatchReport,
    /// 输入法（Weasel）检查与自举结果（0.6.20）。
    pub weasel_present: bool,
    /// 本次是否真的把输入法装回来。
    pub weasel_installed: bool,
    pub weasel_version: String,
    /// 随包安装器路径；空 = 没找到。
    pub weasel_installer: String,
    pub weasel_installer_source: String,
    /// installed / already-present / skipped(platform) / disabled / failed。
    pub weasel_install: String,
    pub weasel_error: Option<String>,
    pub weasel_needs_elevation: bool,
    pub weasel_log: String,
    pub weasel_message: String,
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
    // 0.6.20：先修输入法本体——它和 build/ 无关；即使后面的 payload 缺失，
    // 也不能让「机器上没有输入法」这件事被跳过。
    let weasel = ensure_ime();
    let probe_before = prebuilt::probe_build(&rime_dir);
    let (location, report) = perform(&rime_dir, force.unwrap_or(false))?;
    // 用户点「修复输入法」= 显式操作，允许触发重载。
    let data = patch_data(&app_version(&app), data_patch::FixSource::Explicit);
    let weasel_message = weasel.summary();
    let message = format!(
        "{}；{}；{}",
        summary(&report),
        data.summary(),
        weasel_message
    );
    let candidates: Vec<&str> = data
        .data_candidates
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    let mut log = format!(
        "[fix-ime] {message} payload={} source={} probe_before={} data_patched={} punctuator_patched={} punctuator_skipped={} data_mode={} custom_written={} deploy={} data_dir={} data_candidates=[{}] weasel_present={} weasel_installed={} weasel_version={} weasel_install={} weasel_installer={}",
        location.path.display(),
        location.source,
        probe_before,
        data.patched(),
        data.punctuator_patched,
        data.punctuator_skipped.len(),
        data.mode,
        data.custom_written.len(),
        data.deploy_triggered,
        if data.data_dir.is_empty() {
            "-"
        } else {
            data.data_dir.as_str()
        },
        candidates.join(", "),
        weasel.present,
        weasel.installed,
        if weasel.version.is_empty() {
            "-"
        } else {
            weasel.version.as_str()
        },
        weasel.install,
        if weasel.installer.is_empty() {
            "-"
        } else {
            weasel.installer.as_str()
        },
    );
    if let Some(error) = &data.data_error {
        log.push_str(&format!(" data_error={error}"));
    }
    for skipped in &data.punctuator_skipped {
        log.push_str(&format!(" punctuator_skip={skipped}"));
    }
    if let Some(error) = &weasel.error {
        log.push_str(&format!(" weasel_error={error}"));
    }
    if weasel.needs_elevation {
        log.push_str("（输入法安装需要管理员权限：请以管理员身份重启 rime-sync 后重试）");
    }
    if data.needs_elevation() {
        log.push_str("（data 目录需要管理员权限：请以管理员身份重启 rime-sync 后重试）");
    } else if data.mode == data_patch::MODE_CUSTOM {
        log.push_str(&format!(
            "（data 目录不可写，已写用户级 custom.yaml {} 个：{}；部署={}）",
            data.custom_written.len(),
            data.custom_written.join(", "),
            data.deploy_triggered
        ));
    }
    push_log(&state.0, log);
    Ok(FixImeReport {
        rime_dir: rime_dir.display().to_string(),
        payload: location.path.display().to_string(),
        payload_source: location.source,
        probe_before: probe_before.as_str().to_string(),
        message,
        data_patched: data.patched(),
        needs_elevation: data.needs_elevation() || weasel.needs_elevation,
        data,
        weasel_present: weasel.present,
        weasel_installed: weasel.installed,
        weasel_version: weasel.version,
        weasel_installer: weasel.installer,
        weasel_installer_source: weasel.installer_source,
        weasel_install: weasel.install,
        weasel_error: weasel.error,
        weasel_needs_elevation: weasel.needs_elevation,
        weasel_log: weasel.log,
        weasel_message,
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
            app_version(&handle),
            std::env::var(prebuilt::ENV_MODE).unwrap_or_else(|_| "<unset>".to_string())
        ));

        // 0.6.20：先保证输入法本体在。0.6.19 的 /S 静默安装里 NSIS 钩子没执行，
        // 旧 Weasel 又已被卸载器卸掉，机器上一台输入法都没有——必须启动时自愈。
        let weasel = ensure_ime();
        log(format!(
            "ime check present={} installed={} version={} install={} installer={}",
            weasel.present,
            weasel.installed,
            if weasel.version.is_empty() {
                "-"
            } else {
                weasel.version.as_str()
            },
            weasel.install,
            if weasel.installer.is_empty() {
                "-"
            } else {
                weasel.installer.as_str()
            },
        ));
        for line in weasel.log.lines() {
            log(format!("ime {line}"));
        }
        if let Some(error) = &weasel.error {
            log(format!("ime error={error}"));
        }
        if weasel.installed || !weasel.ok() {
            // 装回来了要说一声；装不上更不能静默（GUI 日志与设置页都能看到）。
            push_log(&state.0, format!("[bootstrap] {}", weasel.summary()));
        }
        if weasel.needs_elevation {
            log("ime needs elevation: 以管理员身份重启 rime-sync 后点「修复输入法」".to_string());
        }

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
        let force = env_mode();
        let mut end = "skip";
        if probe == BuildProbe::Prebuilt {
            log("skip stage=prebuilt reason=probe-prebuilt".to_string());
        } else if let Some(force) = force {
            // 与 perform() 同一顺序（resolve → install），这里展开是因为要在两步之间
            // 把「命中了哪个候选目录」写进日志。
            match prebuilt::resolve_payload(exe_dir().as_deref()) {
                Ok(location) => {
                    log(format!(
                        "payload hit path={} source={}",
                        location.path.display(),
                        location.source
                    ));
                    log(format!("install start force={force}"));
                    match prebuilt::install_prebuilt_with(
                        &location.path,
                        &rime_dir,
                        InstallOptions { force },
                    ) {
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
                            end = if report.build_ok { "ok" } else { "degraded" };
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
                            end = "failed stage=install";
                            push_log(
                                &state.0,
                                format!("[bootstrap] 预编译产物自举失败（不影响启动）：{error}"),
                            );
                        }
                    }
                }
                Err(error) => {
                    log(format!("payload failed stage=resolve error={error:#}"));
                    end = "failed stage=payload";
                    push_log(
                        &state.0,
                        format!("[bootstrap] 未找到预编译产物（不影响启动）：{error:#}"),
                    );
                }
            }
        } else {
            log("skip stage=prebuilt reason=env-disabled RIMESYNC_PREBUILT=0".to_string());
            push_log(
                &state.0,
                "[bootstrap] RIMESYNC_PREBUILT=0，跳过预编译产物自举",
            );
        }

        // data 方案补丁（0.6.18）：与 build/ 状态无关，每次启动都跑。
        // 227 实测：build/ 里已经是带插件标记的产物，引擎却仍按 data 下的原版方案
        // 工作；部署链不可用，只能直接改 <WeaselRoot>\data\*.schema.yaml。
        if force.is_some() {
            // 启动自举：只写文件、记录 deploy=deferred，绝不重载用户正在用的输入法。
            let data = patch_data(&app_version(&handle), data_patch::FixSource::Bootstrap);
            if data.data_dirs.is_empty() {
                // Linux 下 /usr/share/rime-data 不存在；Windows 下没读到 WeaselRoot。
                log("skip stage=data reason=no-dirs".to_string());
            }
            log(format!(
                "data patch dirs={} data_dir={} version={} mode={} fallback={} patched={} unchanged={} skipped={} skipped_not_retained={} failed={} backup_dir={} custom_written={} deploy={} files={}",
                data.data_dirs.join(";"),
                data.data_dir,
                data.version,
                data.mode,
                data.fallback,
                data.patched(),
                data.schemas_unchanged.len(),
                data.schemas_skipped.len(),
                data.skipped_not_retained,
                data.errors.len(),
                data.backup_dir,
                data.custom_written.join(";"),
                data.deploy_triggered,
                data.schemas_patched.join(";"),
            ));
            for error in &data.errors {
                log(format!("data patch error={error}"));
            }
            for skipped in &data.schemas_skipped {
                log(format!("data patch skipped={skipped}"));
            }
            // 启动时不把权限错误推给用户（Linux 的 /usr/share/rime-data 属 root，
            // 每次启动都会失败）；诊断信息留在 bootstrap.log，设置页的
            // 「修复输入法」仍会把 needs_elevation 显示出来。
            if data.patched() > 0 {
                push_log(&state.0, format!("[bootstrap] {}", data.summary()));
            }
            if data.deploy_triggered == data_patch::DEPLOY_DEFERRED {
                // 自举很可能发生在用户正在打字时：不重载，把「什么时候生效」交给用户。
                push_log(
                    &state.0,
                    "[bootstrap] 已更新输入法配置，请点「重新部署」或重启输入法",
                );
            }
            if data.needs_elevation() {
                log(
                    "data patch needs elevation: 以管理员身份运行 rime-sync-cli fix-ime"
                        .to_string(),
                );
            }
        } else {
            log("skip stage=data reason=env-disabled".to_string());
        }

        // 方案裁剪（0.6.21）：随包只保留 retained-schemas.txt 里的 3 个方案，其余
        // data/用户/build 文件在安装后清掉（幂等、带 .rimesync-removed-<时间戳> 备份，
        // RIMESYNC_PRUNE_SCHEMAS=0 关闭）。启动时静默跑；权限不足只记日志。
        let pruned = prune::prune_auto();
        if pruned.disabled {
            log("skip stage=prune reason=env-disabled".to_string());
        } else {
            log(format!(
                "prune keep={} kept_by_env={} removed={} backups={} failed={} warnings={} dirs={} user={}",
                pruned.keep.join(";"),
                pruned.kept_by_env.join(";"),
                pruned.removed(),
                pruned.backups.len(),
                pruned.failures.len(),
                pruned.warnings.len(),
                pruned.data_dirs.join(";"),
                pruned.user_dir,
            ));
            for warning in &pruned.warnings {
                log(format!("prune warning={warning}"));
            }
            for path in &pruned.removed {
                log(format!("prune removed={path}"));
            }
            for error in &pruned.failures {
                log(format!("prune error={error}"));
            }
            if pruned.removed() > 0 {
                push_log(&state.0, format!("[bootstrap] {}", pruned.summary()));
            }
        }
        log(format!("bootstrap end result={end}"));
    });
}
