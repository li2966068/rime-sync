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
use std::sync::atomic::{AtomicBool, Ordering};

use rime_sync_core::data_patch::{self, DataPatchReport};
use rime_sync_core::ime::{self, ImeReport};
use rime_sync_core::ime_state;
use rime_sync_core::prebuilt::{self, PrebuiltReport};
use rime_sync_core::prune;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::MessageDialogResult;

use crate::ime_text;
use crate::{detect_rime, ensure_config_meta, push_log, state_dir, AppState};

/// 同一时刻只允许一个「修复输入法」在跑（0.6.23）。
///
/// 前端已经在请求期间禁用按钮，但设置页、托盘、重复点击仍可能同时打进来；两个
/// 任务会互相覆盖同一个 build/，还会抢输入法进程。这里用进程内原子标志兜底：
/// 第二次调用**立即**返回 busy，不排队（排队只会让用户以为卡住了）。
static FIX_IME_BUSY: AtomicBool = AtomicBool::new(false);

/// 机器可读的 busy 标记：前端据此换成 i18n 文案（fixIme.busy）。
pub(crate) const BUSY_MARKER: &str = "fix-ime-busy";
/// 可读的 busy 错误（日志里也能看懂）。
///
/// 这把锁被**启动自举**和**手动修复**共用（自举最长占住 Weasel 安装器 300 秒），
/// 所以文案不能说「修复任务」—— 用户点修复时可能根本没有修复在跑，是自举在跑。
pub(crate) const BUSY_MESSAGE: &str = "输入法任务已在执行中（启动自举或修复），请等待当前任务完成";

/// 占位守卫：离开作用域就复位（正常返回、Err、panic 展开都会走到 Drop）。
struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// 启动自举是否正在跑（进程内）。
///
/// 提权重启会拉起**另一个进程**的新实例：进程内的排他锁跨不了进程，新旧两个
/// 实例可能同时跑 ensure（两个 Weasel 安装器同时写 %APPDATA%\Rime 与 build/）。
/// 提权前先等本进程的自举结束，等不到就明确报错，不硬上。
static BOOTSTRAP_RUNNING: AtomicBool = AtomicBool::new(false);

/// 自举结束即复位（正常返回、panic 展开都会走到 Drop）。
struct BootstrapGuard;

impl Drop for BootstrapGuard {
    fn drop(&mut self) {
        BOOTSTRAP_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// 自举超时的判定阈值（Weasel 安装器 300 秒 + 余量）。
const BOOTSTRAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(360);

/// 启动看门狗：自举超过 BOOTSTRAP_TIMEOUT 仍未结束 → 发布 failed 状态。
///
/// 为什么需要：running 状态表示「自举进行中」，界面据此不弹红条。如果自举卡死
/// （安装器挂住 / IO 阻塞），状态会永远停在 running，用户看到的是一片空白。
fn spawn_bootstrap_watchdog(app: AppHandle) {
    let started = std::time::Instant::now();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let running = BOOTSTRAP_RUNNING.load(Ordering::SeqCst);
            if !running {
                return; // 自举已结束（正常路径会发布终态）
            }
            if bootstrap_timed_out(running, started.elapsed(), BOOTSTRAP_TIMEOUT) {
                break;
            }
        }
        let state = app.state::<AppState>();
        push_log(
            &state.0,
            format!(
                "[bootstrap] 自举超时（>{:?}），标记为失败",
                BOOTSTRAP_TIMEOUT
            ),
        );
        publish_failed_if_not_terminal(
            &app,
            "timeout",
            format!(
                "输入法自举超时（超过 {} 秒未完成）",
                BOOTSTRAP_TIMEOUT.as_secs()
            ),
        );
    });
}

/// 只在「还没有终态」时补发 failed 状态。
///
/// 两种竞态都要挡住：
/// 1. 自举刚跑完、发布终态（发布 → 守卫复位之间）看门狗/panic 兜底也可能到点：
///    抢写会把成功的结论覆盖成失败。
/// 2. 状态槽里**从没发过**状态（自举还没来得及发 running 就崩了/卡死了）：
///    这时界面一片空白，**必须**补发 —— 不能当成「已经结束」。
fn publish_failed_if_not_terminal(app: &AppHandle, reason: &str, detail: String) {
    let state = app.state::<AppState>();
    let current = state.0.ime.lock().ok().and_then(|slot| slot.clone());
    // 有状态且不是 running = 已经有终态了，别覆盖。
    let has_terminal = current.map(|status| !status.running).unwrap_or(false);
    if has_terminal {
        return;
    }
    publish_status(
        app,
        &state.0,
        &failed_status_with_reason("failed", reason, detail),
    );
}

/// 超时判定：还在跑 && 已超过阈值 → 该把状态标成 failed（纯函数，可测）。
fn bootstrap_timed_out(
    running: bool,
    elapsed: std::time::Duration,
    timeout: std::time::Duration,
) -> bool {
    running && elapsed >= timeout
}

/// 等自举结束（最多 timeout）。返回 false = 超时还没结束。
///
/// 只有 Windows 的「以管理员重启」用得上（Linux 提权走别的路径），所以按平台编译。
#[cfg(windows)]
fn wait_bootstrap_idle(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while BOOTSTRAP_RUNNING.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    true
}

/// 抢标志；已有任务在跑时返回 None。
fn try_acquire(flag: &AtomicBool) -> Option<BusyGuard<'_>> {
    if flag.swap(true, Ordering::SeqCst) {
        return None;
    }
    Some(BusyGuard(flag))
}

/// 等锁（最多 timeout）：提权重启时旧实例正在退出，锁会很快放开。
///
/// 只用于「提权实例马上要接手修复」这一条路径；普通调用方仍然立即返回 busy，
/// 不排队（排队会让用户以为程序卡死）。
fn wait_acquire(flag: &AtomicBool, timeout: std::time::Duration) -> Option<BusyGuard<'_>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(guard) = try_acquire(flag) {
            return Some(guard);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// busy 错误文案（带标记，前端不必解析中文）。
fn busy_error() -> String {
    format!("{BUSY_MESSAGE}（{BUSY_MARKER}）")
}

/// 立即返回 busy 的排他执行器：单测用它证明「第二次调用不排队」。
///
/// 只有测试需要这层包装；生产路径直接用 try_acquire（守卫要跨 await 持有）。
#[cfg(test)]
fn run_exclusive<T>(flag: &AtomicBool, work: impl FnOnce() -> T) -> Result<T, String> {
    let Some(_guard) = try_acquire(flag) else {
        return Err(busy_error());
    };
    Ok(work())
}

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

/// 输入法健康检查 + 缺失时用随包安装器装回去（0.6.20）。
///
/// Linux 返回 skipped(platform)（由包管理器负责）；Windows 缺失时会跑随包
/// weasel-*-installer.exe /S /NOREBOOT，最多等 300 秒。
fn ensure_ime() -> ImeReport {
    ime::ensure(exe_dir().as_deref())
}

/// 一句话结论；界面与日志共用。
fn summary(report: &PrebuiltReport, outcome: &ime_state::Outcome) -> String {
    match outcome.result {
        ime_state::OutcomeResult::Ok => format!(
            "输入法产物已就绪（result=ok，新铺 {}，沿用 {}，覆盖 {}，配置新增 {}）",
            report.copied, report.kept, report.overwritten, report.config_copied
        ),
        ime_state::OutcomeResult::Noop => {
            "输入法产物已就绪（result=noop，本次没有改动任何文件）".to_string()
        }
        ime_state::OutcomeResult::Partial => format!(
            "输入法产物已可用，但本次有改动失败（result=partial）：{}",
            outcome.detail
        ),
        ime_state::OutcomeResult::Failed => format!(
            "铺入 {} 个文件后 build/ 仍不可用（result=failed）：{}；把日志里的 fix-ime 行发给维护者",
            report.copied + report.overwritten,
            outcome.detail
        ),
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
    // 建议项：build_* / deploy_* 这些字段**不再**在这里重复声明 ——
    // 它们本来就来自下面 flatten 的 PrebuiltReport；重复声明会让序列化出的
    // JSON 出现同名键（前端拿到的是哪一个取决于解析器，不可控）。
    // 需要它们的前端一律读 flatten 出来的那份。
    /// 唯一状态机的结论：ok / noop / partial / failed。
    pub result: String,
    /// Partial/Failed 必非空。
    pub detail: String,
    /// 本次实际做过的每一步。
    pub actions: Vec<ime_state::Action>,
    /// 小键盘判定（core 的 NumpadStatus 原样带过来：ensure 实际用的那一份，不重算）。
    pub numpad: ime_state::NumpadStatus,
    /// 最终 assess（逐方案状态）。
    pub assessment: ime_state::Assessment,
    /// B-2：payload config/ 里被 data 门禁挡下的模板（= 用户拿到的输入法**没有**
    /// schema_list）。非空即意味着结论降级为 partial（result 不会是 ok）。
    ///
    /// **不要**在这里再声明一份 config_skipped：下面 flatten 的 PrebuiltReport 里
    /// 已经有一份（crates/core/src/prebuilt.rs），两份同名键会让序列化出的 JSON
    /// 出现重复键 —— 前端 JSON.parse 只保留最后一个，值会变成空数组，
    /// 「输入法没有 schema_list」的提示就永远不触发。
    #[serde(flatten)]
    pub report: PrebuiltReport,
}

/// 设置页「修复输入法」：与启动自举走同一套逻辑，force = 覆盖 build/。
///
/// 0.6.23：改成 async + spawn_blocking——输入法安装最多等 300 秒，同步命令会把
/// 整个窗口（WebView 的 IPC 线程）冻住；改成后台线程后界面可以正常显示「处理中…」。
#[tauri::command]
pub async fn cmd_fix_ime(
    app: AppHandle,
    state: State<'_, AppState>,
    force: Option<bool>,
) -> Result<FixImeReport, String> {
    // 0.6.23：并发保护。抢不到标志就立刻返回 busy（不排队、不执行任何 IO）。
    //
    // B6：守卫交给**后台任务**持有，不留在 future 里。前端导航/取消把 future drop 掉时，
    // 阻塞任务其实还在跑；如果守卫跟着 future 一起 drop，标志会被提前复位，第二个修复
    // 就会和第一个同时改 build/。现在只有任务真的结束（成功/失败/panic 展开）才复位。
    let Some(guard) = try_acquire(&FIX_IME_BUSY) else {
        return Err(busy_error());
    };
    let inner = std::sync::Arc::clone(&state.0);
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // 守卫随任务走：panic 展开时 Drop 同样会复位。
        let _guard = guard;
        fix_ime_blocking(&handle, &inner, force)
    })
    .await
    .map_err(|error| format!("修复输入法任务异常结束：{error}"))?
}

/// cmd_fix_ime 的同步主体：原样搬过来，只是不再占着 IPC 线程。
fn fix_ime_blocking(
    app: &AppHandle,
    inner: &crate::Inner,
    force: Option<bool>,
) -> Result<FixImeReport, String> {
    let config = ensure_config_meta(app, inner)?;
    let rime_dir = detect_rime(&config)?.path;
    // 只用于报告字段：判据与决策都在 ime_state::ensure 内部（同一个 assess）。
    let probe_before = prebuilt::probe_build(&rime_dir);
    let location = prebuilt::resolve_payload(exe_dir().as_deref())
        .map_err(|error| format!("未找到预编译产物：{error:#}"))?;
    let data_dirs = data_patch::data_dirs();
    // 0.6.23 / A-1：唯一状态机。铺产物 → data 补丁 → 部署 → 再 assess，
    // 顺序与判据都不在这一层；结论也只看 ensure 返回的 result。
    //
    // A-1：Mode::Fix 自己负责装输入法本体（Options::fix() 的 install_ime=true），
    // 并在装完后重新解析 data_dirs。GUI 原来在 ensure 之前**自己**调了一次
    // ensure_ime()：那是重复路径（两条安装流程），而且顺序不对（装完再解析
    // data 的好处全丢了）。现在两边口径一致：都只调 ensure(Mode::Fix)。
    let outcome = ime_state::ensure(
        &rime_dir,
        Some(location.path.as_path()),
        &data_dirs,
        &ime_state::Options::fix()
            .with_force(force.unwrap_or(false))
            .with_version(Some(app_version(app)))
            .with_exe_dir(exe_dir())
            .with_source(location.source.clone()),
    );
    let data = &outcome.data;
    let report = &outcome.report;
    // 输入法本体结果由 ensure 内部给出（同一份数据，不再有第二条安装路径）。
    let weasel = &outcome.ime;
    let weasel_message = weasel.summary();
    let message = format!(
        "{}；{}；{}",
        summary(report, &outcome),
        data.summary(),
        weasel_message
    );
    let candidates: Vec<&str> = data
        .data_candidates
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    let mut log = format!(
        "[fix-ime] {message} payload={} source={} probe_before={} build_source={} build_schemas={} build_repaired={} build_missing={} deploy_method={} deploy_result={} deploy_ok={} deploy_ms={} deploy_detail={} deploy_inproc={} data_patched={} punctuator_patched={} punctuator_skipped={} data_mode={} custom_written={} deploy={} data_dir={} data_candidates=[{}] weasel_present={} weasel_installed={} weasel_version={} weasel_install={} weasel_installer={} result={} detail={} actions={}",
        location.path.display(),
        location.source,
        probe_before,
        report.build_source,
        report.build_schemas_line(),
        report.build_repaired.join(","),
        report.build_missing.join(","),
        report.deploy_method,
        report.deploy_result,
        report.deploy_ok,
        report.deploy_ms,
        report.deploy_detail,
        report.deploy_inproc,
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
        outcome.result.as_str(),
        outcome.detail,
        outcome
            .actions
            .iter()
            .map(|action| format!("{}:{}", action.kind, action.changed))
            .collect::<Vec<_>>()
            .join(";"),
    );
    if let Some(error) = &data.data_error {
        log.push_str(&format!(" data_error={error}"));
    }
    // B-2：config 跳过必须写进 fix-ime 日志行（用户反馈日志时能看到）。
    for item in &report.config_skipped {
        log.push_str(&format!(" config_skipped={item}"));
    }
    for item in &report.manifest_failed {
        log.push_str(&format!(" manifest_failed={item}"));
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
    push_log(inner, log);
    // 手动修复成功后必须刷新启动自举状态快照：否则设置页红条一直挂着**上一次**
    // 失败的状态，用户明明修好了也要重启程序才看得掉。
    publish_status(app, inner, &ImeBootstrapStatus::from_outcome(&outcome));
    Ok(FixImeReport {
        rime_dir: rime_dir.display().to_string(),
        payload: location.path.display().to_string(),
        payload_source: location.source,
        probe_before: probe_before.as_str().to_string(),
        message,
        data_patched: data.patched(),
        needs_elevation: data.needs_elevation() || weasel.needs_elevation,
        data: data.clone(),
        weasel_present: weasel.present,
        weasel_installed: weasel.installed,
        weasel_version: weasel.version.clone(),
        weasel_installer: weasel.installer.clone(),
        weasel_installer_source: weasel.installer_source.clone(),
        weasel_install: weasel.install.clone(),
        weasel_error: weasel.error.clone(),
        weasel_needs_elevation: weasel.needs_elevation,
        weasel_log: weasel.log.clone(),
        weasel_message,
        result: outcome.result.as_str().to_string(),
        detail: outcome.detail.clone(),
        // B-2：config 跳过必须出现在报告里（前端据此提示用户）。
        actions: outcome.actions.clone(),
        numpad: outcome.numpad.clone(),
        assessment: outcome.assessment.clone(),
        report: report.clone(),
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
/// 启动自举的结果快照：设置页红条与排障都读它。
#[derive(Debug, Clone, Serialize)]
pub struct ImeBootstrapStatus {
    /// 输入法本体与产物都就绪（红条据此显示/隐藏）。
    pub ok: bool,
    /// ok / noop / partial / failed / skipped。
    pub result: String,
    /// 失败时给出用户能执行的下一步。
    pub detail: String,
    /// 需要管理员权限（普通用户启动时最常见）。
    pub needs_elevation: bool,
    /// installed / already-present / skipped(platform) / disabled / failed。
    pub ime_install: String,
    pub ime_error: Option<String>,
    pub ime_version: String,
    pub ime_installed: bool,
    /// config 模板本次新写入的文件。
    pub seeded_written: Vec<String>,
    /// 最近一次自举的时间（便于判断红条是不是陈旧状态）。
    pub updated_at: String,
    /// 自举还在跑（result="running"）：不是失败，别当红条弹。
    pub running: bool,
    /// 机器可读的失败原因码（ok/noop/partial/numpad-pending/install-disabled/
    /// install-failed/timeout/crashed/failed）。前端按它出**本地化**结论 ——
    /// 后端的 detail 是中文技术描述，英文界面下直接显示会中英混排。
    pub reason: String,
}

impl ImeBootstrapStatus {
    fn from_outcome(outcome: &ime_state::Outcome) -> Self {
        // 同 from_ime：关掉自动安装 + 输入法确实不在 ⇒ 失败，不是成功。
        let missing = outcome.ime.install == "disabled" && !outcome.ime.present;
        // 小键盘有没有生效：只看 core 判好的那一份（ensure 实际用的那份），不重算。
        let reason = reason_of(&outcome.result, &outcome.ime, outcome.numpad.ok);
        Self {
            // 小键盘不可用也不能报「已就绪」：core 保证这种时候结论是 partial
            // （outcome.ok() 已经是 false），这里再兜一层 —— 结论是 ok/noop 而
            // 小键盘确实不可用时，也不能让界面说一切就绪。
            ok: outcome.ok() && outcome.ime.ok() && !missing && outcome.numpad.ok,
            result: if missing {
                "failed".to_string()
            } else {
                outcome.result.as_str().to_string()
            },
            detail: if missing {
                // 与 from_ime 同一句：产物那步可能没问题，但用户手上没有输入法，
                // 这才是界面该说的话。
                "输入法未就绪：自动安装已关闭（RIMESYNC_INSTALL_IME=0），请手动安装或改回自动安装"
                    .to_string()
            } else if reason == "numpad-pending" {
                // 小键盘这回事必须让用户看得见（core 一般已经把提示写进 detail，
                // 这里只做兜底，见 detail_with_numpad）。
                detail_with_numpad(&outcome.detail, &outcome.numpad.hint)
            } else {
                outcome.detail.clone()
            },
            needs_elevation: outcome.ime.needs_elevation,
            ime_install: outcome.ime.install.clone(),
            ime_error: outcome.ime.error.clone(),
            ime_version: outcome.ime.version.clone(),
            ime_installed: outcome.ime.installed,
            seeded_written: outcome.seeded.written.clone(),
            updated_at: now_stamp(),
            running: false,
            reason: reason.to_string(),
        }
    }

    /// 提权修复（--repair-ime）的结论 → 状态快照。
    ///
    /// 与 from_ime 同口径：结果 ok/noop = 产物这一步没问题，但「用户手上有没有
    /// 输入法」要单独判（关掉自动安装 + 输入法不在 ⇒ 失败）。
    fn from_fix_report(report: &FixImeReport) -> Self {
        let missing = report.weasel_install == "disabled" && !report.weasel_present;
        // 小键盘不可用也不能报「已就绪」：core 保证这种时候 result 是 partial，
        // 这里再兜一层（result 是 ok/noop 而小键盘确实不可用时同样算没就绪）。
        let ok = (report.result == "ok" || report.result == "noop") && !missing && report.numpad.ok;
        let reason = if missing {
            // 优先级最高：用户手上没有输入法，比小键盘严重得多。
            "install-disabled"
        } else if report.result == "failed" {
            // 失败比小键盘严重：严重失败不许被小键盘覆盖（原有语义：不是
            // ok/noop/partial 的结论 = 修复/安装这步失败）。
            "install-failed"
        } else if !report.numpad.ok {
            // 小键盘没生效 → 专用原因码（前端据此说人话：重启输入法 / 再修一次）。
            // 放在 install-disabled / install-failed 之后：那两个更严重。
            "numpad-pending"
        } else if ok {
            if report.result == "noop" {
                "noop"
            } else {
                "ok"
            }
        } else if report.result == "partial" {
            "partial"
        } else {
            "install-failed"
        };
        Self {
            ok,
            result: report.result.clone(),
            detail: if reason == "numpad-pending" {
                detail_with_numpad(&report.detail, &report.numpad.hint)
            } else {
                report.detail.clone()
            },
            needs_elevation: report.weasel_needs_elevation || report.needs_elevation,
            ime_install: report.weasel_install.clone(),
            ime_error: report.weasel_error.clone(),
            ime_version: report.weasel_version.clone(),
            ime_installed: report.weasel_installed,
            seeded_written: Vec::new(),
            updated_at: now_stamp(),
            running: false,
            reason: reason.to_string(),
        }
    }

    fn from_ime(report: &ImeReport) -> Self {
        // 判据只有一处：core 的 ImeReport::ready（CLI 的 ensure-ime 同一口径）。
        // 「安装这步没失败」≠「用户手上有输入法」：关掉自动安装（RIMESYNC_INSTALL_IME=0）
        // 而输入法又确实不在时，不能报成功 —— 用户手上没有输入法，必须看得见。
        let missing = report.install == "disabled" && !report.present;
        let ready = report.ready();
        Self {
            ok: ready,
            result: if ready { "ok" } else { "failed" }.to_string(),
            detail: if ready {
                report.summary()
            } else {
                report.not_ready_reason()
            },
            needs_elevation: report.needs_elevation,
            ime_install: report.install.clone(),
            ime_error: report.error.clone(),
            ime_version: report.version.clone(),
            ime_installed: report.installed,
            seeded_written: Vec::new(),
            updated_at: now_stamp(),
            running: false,
            reason: if missing {
                "install-disabled"
            } else if ready {
                "ok"
            } else {
                "install-failed"
            }
            .to_string(),
        }
    }
}

/// 构造一个「自举失败」的状态快照。
///
/// 早期返回路径（读配置失败 / 定位不到 Rime 目录）以前只写日志，界面上什么都
/// 看不到 —— 用户没有输入法、也没有任何提示。失败必须能被看见。
fn failed_status(result: &str, detail: String) -> ImeBootstrapStatus {
    failed_status_with_reason(result, result, detail)
}

/// 同上，但可以单独给失败原因码（前端按它出本地化结论）。
fn failed_status_with_reason(result: &str, reason: &str, detail: String) -> ImeBootstrapStatus {
    ImeBootstrapStatus {
        ok: false,
        result: result.to_string(),
        detail: detail.clone(),
        needs_elevation: false,
        ime_install: "failed".to_string(),
        ime_error: Some(detail),
        ime_version: String::new(),
        ime_installed: false,
        seeded_written: Vec::new(),
        updated_at: now_stamp(),
        running: false,
        reason: reason.to_string(),
    }
}

/// 小键盘缺口的提示追加到 detail 末尾（只在真需要时）。
///
/// core 在小键盘有缺口时已经把 numpad_missing=…（提示）写进 outcome.detail
/// （crates/core/src/ime_state.rs 里往 problems 推的那一条），所以正常路径
/// **不需要**这里再拼一遍。只有 detail 里完全看不到小键盘字样时（例如 detail
/// 被别的内容覆盖）才补上 hint —— 横幅里必须看得出「小键盘这回事」。
fn detail_with_numpad(detail: &str, hint: &str) -> String {
    let text = detail.trim();
    let hint = hint.trim();
    if hint.is_empty() || text.contains("numpad") {
        return text.to_string();
    }
    if text.is_empty() {
        hint.to_string()
    } else {
        format!("{text}；{hint}")
    }
}

/// 失败原因码：结论 + 输入法本体状态 + 小键盘判定 → 稳定的原因码（前端据此本地化）。
///
/// numpad_ok = core 判的「小键盘有没有生效」（ensure 实际用的那一份，不重算）。
fn reason_of(result: &ime_state::OutcomeResult, ime: &ImeReport, numpad_ok: bool) -> &'static str {
    use ime_state::OutcomeResult::{Failed, Noop, Ok, Partial};
    // 顺序要紧：**先**判「用户手上没有输入法」。关掉自动安装（RIMESYNC_INSTALL_IME=0）
    // 而输入法又确实不在时，core 的结论仍是 ok/noop（那是产物那步的结论），但用户
    // 手上没有输入法 —— reason 必须是 install-disabled，不能因为 result=ok 就说
    // 「输入法已就绪」（红条标题会自相矛盾）。
    if ime.install == "disabled" && !ime.present {
        return "install-disabled";
    }
    if !ime.ok() {
        return "install-failed";
    }
    match result {
        Ok => {
            // 结论是 ok 而小键盘不可用：按理不该出现（core 保证小键盘有缺口时
            // 结论降级为 partial）。万一出现，报 "ok" 就是「报 ok 但小键盘其实
            // 不能用」的静默失败 —— 所以给专用的 numpad-pending。
            if numpad_ok {
                "ok"
            } else {
                "numpad-pending"
            }
        }
        Noop => {
            if numpad_ok {
                "noop"
            } else {
                "numpad-pending"
            }
        }
        Partial => {
            // 小键盘没生效时给专用原因码：用户需要知道「重启输入法 / 再修一次」，
            // 而不是看到没法行动的笼统「partial」。
            if numpad_ok {
                "partial"
            } else {
                "numpad-pending"
            }
        }
        // 失败比小键盘严重：严重失败不许被小键盘覆盖。
        Failed => "failed",
    }
}

/// 「自举进行中」的状态快照。
///
/// 自举最长可能占住 Weasel 安装器 300 秒；这段时间里界面必须能看出「在忙」，
/// 否则设置页一片空白，用户以为程序没反应。
fn running_status() -> ImeBootstrapStatus {
    ImeBootstrapStatus {
        ok: false,
        result: "running".to_string(),
        detail: "正在自举输入法（安装 Weasel 可能要几分钟）…".to_string(),
        needs_elevation: false,
        ime_install: "running".to_string(),
        ime_error: None,
        ime_version: String::new(),
        ime_installed: false,
        seeded_written: Vec::new(),
        updated_at: now_stamp(),
        running: true,
        reason: "running".to_string(),
    }
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 把状态写进进程状态并发给前端（设置页红条据此显示）。
fn publish_status(app: &AppHandle, inner: &crate::Inner, status: &ImeBootstrapStatus) {
    if let Ok(mut slot) = inner.ime.lock() {
        *slot = Some(status.clone());
    }
    let _ = app.emit("ime://bootstrap", status);
}

/// 启动自举的输入法报告逐行写进 bootstrap.log。
fn log_ime_report(log: &impl Fn(String), report: &ImeReport) {
    log(format!(
        "ime check present={} installed={} version={} install={} installer={} needs_elevation={}",
        report.present,
        report.installed,
        if report.version.is_empty() {
            "-"
        } else {
            report.version.as_str()
        },
        report.install,
        if report.installer.is_empty() {
            "-"
        } else {
            report.installer.as_str()
        },
        report.needs_elevation
    ));
    for line in report.log.lines() {
        log(format!("ime {line}"));
    }
    if let Some(error) = &report.error {
        log(format!("ime error={error}"));
    }
}

/// Windows 安装版首次启动的 Rime 目录兜底：%APPDATA%\Rime（目录由 ensure 创建）。
///
/// 为什么需要：全新机器上小狼毫还没装，%APPDATA%\Rime 可能不存在，detect 会失败；
/// 但「先装输入法」正是本流程要解决的问题，不能因此中止。
fn bootstrap_fallback_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Rime"))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 首启失败对话框的两个按钮文案（Custom 按钮的原样返回值就是它）。
const DIALOG_FIX: &str = "去修复 / Repair";
const DIALOG_CLOSE: &str = "关闭 / Close";

/// 「去修复」是否被按下。
///
/// tauri-plugin-dialog 2.x 的 OkCancelCustom 返回 MessageDialogResult::Custom(<按钮文案>)
/// （见 plugin 的 desktop.rs：rfd 的 Ok 被映射成 Custom(ok)），**不是** Yes —— 只判 Yes
/// 会让这个按钮变成死代码（B1）。Yes/Ok 一并接受只是兼容旧行为/其它平台。
fn dialog_goes_to_fix(result: &MessageDialogResult) -> bool {
    match result {
        MessageDialogResult::Yes | MessageDialogResult::Ok => true,
        MessageDialogResult::Custom(text) => text == DIALOG_FIX,
        _ => false,
    }
}

/// 输入法自举失败时的阻塞提示。
///
/// 为什么必须阻塞：装不上输入法 = 用户打不了字。只写日志在静默安装/开机自启时
/// 等于静默失败。按钮：去修复（把窗口带到前台，红条上有「修复 / 以管理员重启」）
/// 或关闭。
fn show_ime_failure_dialog(app: &AppHandle, detail: &str, ime_log: &str) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

    // 开机自启（--autostart）时不弹模态框：开机就弹窗 = 每次开机都被打断。
    // 失败信息留在设置页红条里，用户自己去看。
    if std::env::args().any(|arg| arg == "--autostart") {
        push_log(
            &app.state::<AppState>().0,
            "[bootstrap] 自启模式：不弹失败对话框".to_string(),
        );
        return;
    }

    // 措辞：产物没铺好 ≠ 用户打不了字（他可能还装着别的输入法）。
    // 语言跟界面走（原生窗口用不到前端 i18n，由 cmd_set_locale 把语言告诉后端）。
    let locale = ime_text::current();
    let last = ime_log.lines().rev().find(|line| !line.trim().is_empty());
    let message = ime_text::failure_message(locale, detail, last);
    let handle = app.clone();
    let result = app
        .dialog()
        .message(message)
        .title(ime_text::failure_title(locale))
        .kind(tauri_plugin_dialog::MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            DIALOG_FIX.to_string(),
            DIALOG_CLOSE.to_string(),
        ))
        .blocking_show_with_result();
    if dialog_goes_to_fix(&result) {
        // 前端 src/app.ts 订阅同名事件（switchPanel("settings")）：先 show/set_focus，
        // 否则「去修复」只切面板、窗口还留在托盘里。事件名两侧必须一致。
        if let Some(window) = handle.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
        let _ = handle.emit("ime://focus-settings", ());
    }
}

/// 前端把当前界面语言告诉后端。
///
/// 失败对话框是 Tauri 原生窗口，前端那套 i18n 用不上；按钮早就中英双语，正文
/// 之前还是硬编码中文 —— 英文界面下弹的是中文框。现在正文也按界面语言走。
#[tauri::command]
pub fn cmd_set_locale(app: AppHandle, locale: Option<String>) {
    let parsed = ime_text::Locale::parse(locale.as_deref());
    ime_text::set_locale(parsed);
    // 记到磁盘：原生对话框可能在前端加载前就弹出，那时只能靠这份记录。
    if let Ok(dir) = crate::state_dir(&app) {
        ime_text::persist_locale(&dir.join(ime_text::LOCALE_FILE), parsed);
    }
}

/// 设置页红条读取最近一次启动自举结果（还没跑完时返回 None）。
#[tauri::command]
pub fn cmd_ime_status(state: State<'_, AppState>) -> Option<ImeBootstrapStatus> {
    state.0.ime.lock().ok().and_then(|slot| slot.clone())
}

/// 「以管理员重启」：Windows 上用 UAC 重新拉起自己，然后退出当前实例。
///
/// 安装小狼毫需要管理员权限；普通用户双击启动时只能靠这条路径把权限补上。
#[tauri::command]
pub async fn cmd_restart_elevated(app: AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("无法定位程序路径：{error}"))?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // PowerShell 的 Start-Process -Verb RunAs 就是 UAC 提权入口；路径里的单引号
        // 用 PS 的单引号转义（''）。不给 -Wait：提权实例起来后本进程就退出。
        // 新旧两个进程不能同时跑 ensure：等本进程的自举结束（最多 60 秒）。
        // 等不到就明确报错 —— 硬拉起提权实例只会让两个 Weasel 安装器抢同一份
        // 用户目录（build/ 写坏）。
        let idle = tauri::async_runtime::spawn_blocking(|| {
            wait_bootstrap_idle(std::time::Duration::from_secs(60))
        })
        .await
        .map_err(|error| format!("等待自举结束失败：{error}"))?;
        if !idle {
            return Err("输入法正在安装中，请等它结束后再以管理员重启".to_string());
        }
        let quoted = exe.display().to_string().replace('\'', "''");
        // --repair-ime：新实例启动就跑一遍修复（普通启动的自举可能被旧实例的
        // 排他锁挡住，提权这一趟就是来修输入法的，必须真的修）。
        let script =
            format!("Start-Process -FilePath '{quoted}' -ArgumentList '--repair-ime' -Verb RunAs");
        // powershell 会一直等到用户在 UAC 对话框上点确定；这段时间可能好几分钟。
        // 放在 async 命令里同步等 = 占死一个 Tokio 工作线程，必须挪到阻塞线程池。
        let status = tauri::async_runtime::spawn_blocking(move || {
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    &script,
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status()
        })
        .await
        .map_err(|error| format!("提权启动任务异常结束：{error}"))?
        .map_err(|error| format!("启动管理员实例失败：{error}"))?;
        if !status.success() {
            return Err(
                "提权被拒绝（UAC）：请右键 rime-sync 选择「以管理员身份运行」后重试".to_string(),
            );
        }
        // 提权实例已在运行；退出当前实例，避免两份同时操作同一个用户目录。
        app.exit(0);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (app, exe);
        Err("仅 Windows 支持以管理员重启（Linux 用 sudo 或包管理器）".to_string())
    }
}

/// 裁剪结果统一记日志。
///
/// 两条路径共用：正常路径的裁剪由 `ime_state::ensure` 负责（结果在
/// `outcome.prune` 里，单一入口）；开关关闭（RIMESYNC_PREBUILT=0）时不走
/// ensure，由启动自举自己裁一次，行为与以前一致。
fn log_prune(log: &dyn Fn(String), inner: &crate::Inner, pruned: &prune::PruneReport) {
    if pruned.disabled {
        log("skip stage=prune reason=env-disabled".to_string());
        return;
    }
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
        push_log(inner, format!("[bootstrap] {}", pruned.summary()));
    }
}

pub(crate) fn bootstrap(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        // panic 兜底：自举线程崩了，状态不能永远停在 running（界面一直显示「在忙」，
        // 用户以为程序卡死了）。catch_unwind 之后补发一个 failed 状态。
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bootstrap_body(&handle);
        }));
        if outcome.is_err() {
            let state = handle.state::<AppState>();
            publish_failed_if_not_terminal(
                &handle,
                "crashed",
                "输入法自举异常终止（内部错误）".to_string(),
            );
            push_log(
                &state.0,
                "[bootstrap] 自举线程异常终止（panic）".to_string(),
            );
        }
    });
}

/// 自举主体：判据 → 铺产物 → 装输入法 → 评估 → 发布状态（见 bootstrap）。
fn bootstrap_body(handle: &AppHandle) {
    let handle = handle.clone();
    {
        let state = handle.state::<AppState>();
        // 原生对话框可能马上要弹（自举早期失败），那时前端还没上报语言 →
        // 先把上次选的语言读回来。
        if let Ok(dir) = crate::state_dir(&handle) {
            ime_text::load_persisted_locale(&dir.join(ime_text::LOCALE_FILE));
        }
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

        // 自举与「修复输入法」共用同一把排他锁。
        //
        // 自举最长可能占住 Weasel 安装器 300 秒；期间用户点「修复」会并发跑第二
        // 次 ensure → 两个安装器同时写 %APPDATA%\Rime 与 build/（半截文件）。
        //
        // 提权重启（--repair-ime）：旧实例正在退出，稍后会把这把锁放开，
        // 所以这里最多等 60s 再拿锁，而不是直接跳过（否则提权后没人修输入法）。
        let repair_mode = std::env::args().any(|arg| arg == "--repair-ime");
        let guard = if repair_mode {
            wait_acquire(&FIX_IME_BUSY, std::time::Duration::from_secs(60))
        } else {
            try_acquire(&FIX_IME_BUSY)
        };
        // 登记「自举在跑」：提权重启要等它结束（跨进程锁不住）。
        BOOTSTRAP_RUNNING.store(true, Ordering::SeqCst);
        let _bootstrap_guard = BootstrapGuard;
        // 看门狗：自举最长可能占住 Weasel 安装器 300 秒；超过阈值还没结束就把状态
        // 从 running 改成 failed —— 否则界面永远显示「在忙」，用户以为程序死了。
        spawn_bootstrap_watchdog(handle.clone());
        let Some(_guard) = guard else {
            log("skip stage=bootstrap reason=ime-task-in-progress".to_string());
            push_log(
                &state.0,
                "[bootstrap] 已有输入法任务在跑，本次自举跳过".to_string(),
            );
            return;
        };
        if repair_mode {
            log("stage=repair mode=elevated-restart".to_string());
            match fix_ime_blocking(&handle, &state.0, Some(true)) {
                Ok(report) => {
                    log(format!(
                        "repair result={} detail={}",
                        report.result, report.detail
                    ));
                    // 提权修复是「用户点按钮 → 旧实例退出 → 新实例来修」的链路：
                    // 新实例必须把结论说出来，否则失败时用户面对的是一片空白。
                    publish_status(
                        &handle,
                        &state.0,
                        &ImeBootstrapStatus::from_fix_report(&report),
                    );
                }
                Err(error) => {
                    log(format!("repair failed error={error}"));
                    push_log(&state.0, format!("[bootstrap] 提权修复失败：{error}"));
                    // 这里直接发布（不走 publish_failed_if_not_terminal）：提权修复
                    // 是**新的**结论，不是对某个 running 状态的补充。
                    publish_status(
                        &handle,
                        &state.0,
                        &failed_status_with_reason(
                            "failed",
                            "install-failed",
                            format!("提权修复失败：{error}"),
                        ),
                    );
                    show_ime_failure_dialog(&handle, &format!("提权修复失败：{error}"), "");
                }
            }
            log("bootstrap end result=repair".to_string());
            return;
        }

        // 自举开始：先告诉界面「在忙」，最长可能几分钟。
        publish_status(&handle, &state.0, &running_status());

        let exe = exe_dir();
        log(format!(
            "bootstrap begin version={} exe={} RIMESYNC_PREBUILT={} RIMESYNC_INSTALL_IME={}",
            app_version(&handle),
            exe.as_ref()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "<unavailable>".to_string()),
            std::env::var(prebuilt::ENV_MODE).unwrap_or_else(|_| "<unset>".to_string()),
            std::env::var(ime::ENV_INSTALL_IME).unwrap_or_else(|_| "<unset>".to_string())
        ));

        let config = match ensure_config_meta(&handle, &state.0) {
            Ok(config) => config,
            Err(error) => {
                log(format!("config failed error={error}"));
                log("bootstrap end result=failed stage=config".to_string());
                push_log(
                    &state.0,
                    format!("[bootstrap] 读取配置失败，跳过输入法自举：{error}"),
                );
                let status = failed_status("failed", format!("读取配置失败：{error}"));
                publish_status(&handle, &state.0, &status);
                show_ime_failure_dialog(&handle, &status.detail, "");
                return;
            }
        };
        let rime_dir = match detect_rime(&config) {
            Ok(detection) => {
                log(format!(
                    "rime_dir={} source={}",
                    detection.path.display(),
                    detection.source
                ));
                detection.path
            }
            Err(error) => match bootstrap_fallback_dir() {
                // Windows 安装版首次启动：%APPDATA%\Rime 还不在也要能自举（目录由
                // ensure(Mode::Install) 创建）。Linux 没有这个兜底。
                Some(path) => {
                    log(format!(
                        "rime_dir fallback={} reason={error:#}",
                        path.display()
                    ));
                    path
                }
                None => {
                    log(format!("rime_dir failed error={error:#}"));
                    log("bootstrap end result=failed stage=rime-dir".to_string());
                    push_log(
                        &state.0,
                        format!("[bootstrap] 未定位 Rime 用户目录，跳过输入法自举：{error}"),
                    );
                    let status = failed_status("failed", format!("未定位 Rime 用户目录：{error}"));
                    publish_status(&handle, &state.0, &status);
                    show_ime_failure_dialog(&handle, &status.detail, "");
                    return;
                }
            },
        };

        let force = env_mode();
        let data_dirs = data_patch::data_dirs();
        let mut end = "skip";
        match force {
            None => {
                log("skip stage=ensure reason=env-disabled RIMESYNC_PREBUILT=0".to_string());
                push_log(
                    &state.0,
                    "[bootstrap] RIMESYNC_PREBUILT=0，跳过预编译产物自举",
                );
                // 开关只关产物自举；输入法本体仍要装（否则这台机器就没有输入法）。
                let weasel = ensure_ime();
                log_ime_report(&log, &weasel);
                let status = ImeBootstrapStatus::from_ime(&weasel);
                publish_status(&handle, &state.0, &status);
                if weasel.installed || !weasel.ok() {
                    push_log(&state.0, format!("[bootstrap] {}", weasel.summary()));
                }
                if !weasel.ok() {
                    show_ime_failure_dialog(&handle, &weasel.summary(), &weasel.log);
                }
                // 开关关闭时 ensure 不走，裁剪在这里补一次（保持「启动即裁剪」）。
                let pruned = prune::prune_auto();
                log_prune(&log, &state.0, &pruned);
            }
            Some(force) => {
                let payload = prebuilt::resolve_payload(exe.as_deref());
                match &payload {
                    Ok(location) => log(format!(
                        "payload hit path={} source={}",
                        location.path.display(),
                        location.source
                    )),
                    Err(error) => log(format!("payload failed stage=resolve error={error:#}")),
                }
                // B 方案：安装器只交付程序本体；输入法本体、config 模板、
                // prebuilt、data 补丁、部署、assess 全部由这一个状态机负责。
                let outcome = ime_state::ensure(
                    &rime_dir,
                    payload
                        .as_ref()
                        .ok()
                        .map(|location| location.path.as_path()),
                    &data_dirs,
                    // M5：启动自举必须用 bootstrap 语义 —— 部署 deferred（不 taskkill、
                    // 不跑部署器、Linux 不 fcitx5-remote -r），否则每次开机都会打断用户。
                    &ime_state::Options::bootstrap()
                        .with_force(force)
                        .with_version(Some(app_version(&handle)))
                        .with_exe_dir(exe.clone()),
                );
                log(format!(
                    "ensure result={} build_ok={} copied={} kept={} overwritten={} config_copied={} config_kept={} failed={} build_schemas={} repaired={} missing={} data_patched={} deploy_method={} deploy_result={} deploy_ms={} detail={} core_log={}",
                    outcome.result.as_str(),
                    outcome.report.build_ok,
                    outcome.report.copied,
                    outcome.report.kept,
                    outcome.report.overwritten,
                    outcome.report.config_copied,
                    outcome.report.config_kept,
                    outcome.report.failed,
                    outcome.report.build_schemas_line(),
                    outcome.report.build_repaired.join(";"),
                    outcome.report.build_missing.join(";"),
                    outcome.data.patched(),
                    outcome.report.deploy_method,
                    outcome.report.deploy_result,
                    outcome.report.deploy_ms,
                    outcome.detail,
                    outcome.report.log_path.as_deref().unwrap_or("-"),
                ));
                // B-2：config 被 data 门禁挡下 = 用户拿到的输入法没有 schema_list。
                // 这一条必须单独写一行（bootstrap.log 是用户排障时唯一会看的文件）。
                if !outcome.report.config_skipped.is_empty() {
                    log(format!(
                        "ensure config_skipped=[{}] reason=no-schema-in-data",
                        outcome.report.config_skipped.join(",")
                    ));
                    push_log(
                        &state.0,
                        format!(
                            "[bootstrap] config 模板被跳过（{}）：该输入法将没有 schema_list",
                            outcome.report.config_skipped.join(",")
                        ),
                    );
                }
                if !outcome.report.manifest_failed.is_empty() {
                    log(format!(
                        "ensure manifest_failed=[{}]",
                        outcome.report.manifest_failed.join(",")
                    ));
                }
                log_ime_report(&log, &outcome.ime);
                log(format!("seed {}", outcome.seeded.summary()));
                for action in &outcome.actions {
                    log(format!(
                        "ensure action={} changed={} {}",
                        action.kind, action.changed, action.detail
                    ));
                }
                for error in &outcome.data.errors {
                    log(format!("data patch error={error}"));
                }
                for skipped in &outcome.data.schemas_skipped {
                    log(format!("data patch skipped={skipped}"));
                }
                end = match outcome.result {
                    ime_state::OutcomeResult::Ok | ime_state::OutcomeResult::Noop => "ok",
                    ime_state::OutcomeResult::Partial => "degraded",
                    ime_state::OutcomeResult::Failed => "failed stage=ensure",
                };
                let status = ImeBootstrapStatus::from_outcome(&outcome);
                publish_status(&handle, &state.0, &status);
                if outcome.ime.installed || !outcome.ime.ok() {
                    push_log(&state.0, format!("[bootstrap] {}", outcome.ime.summary()));
                }
                if matches!(outcome.result, ime_state::OutcomeResult::Failed) {
                    push_log(
                        &state.0,
                        format!(
                            "[bootstrap] 输入法自举失败（不影响启动）：{}",
                            outcome.detail
                        ),
                    );
                    // 输入法没装好 = 用户打不了字：必须有阻塞提示，不能只写日志。
                    show_ime_failure_dialog(&handle, &outcome.detail, &outcome.ime.log);
                } else if outcome.report.copied + outcome.report.overwritten > 0 {
                    push_log(
                        &state.0,
                        format!("[bootstrap] {}", summary(&outcome.report, &outcome)),
                    );
                }
                if outcome.data.deploy_triggered == data_patch::DEPLOY_DEFERRED {
                    // 自举很可能发生在用户正在打字时：不重载，把「什么时候生效」交给用户。
                    push_log(
                        &state.0,
                        "[bootstrap] 已更新输入法配置，请点「重新部署」或重启输入法",
                    );
                }
                if outcome.data.needs_elevation() {
                    log(
                        "data patch needs elevation: 以管理员身份运行 rime-sync-cli fix-ime"
                            .to_string(),
                    );
                }
                // 裁剪由 ensure 统一做（单一入口），结果随 Outcome 回来。
                log_prune(&log, &state.0, &outcome.prune);
            }
        }

        log(format!("bootstrap end result={end}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_of_prefers_missing_ime_over_a_successful_outcome() {
        use ime_state::OutcomeResult;
        // 关掉自动安装 + 输入法不在：用户手上没有输入法，不能因为结论是 ok/noop
        // 就说「输入法已就绪」（红条标题会自相矛盾）。
        let missing = ImeReport {
            install: "disabled".to_string(),
            present: false,
            ..ImeReport::default()
        };
        assert_eq!(
            reason_of(&OutcomeResult::Ok, &missing, true),
            "install-disabled"
        );
        assert_eq!(
            reason_of(&OutcomeResult::Noop, &missing, true),
            "install-disabled"
        );
        assert_eq!(
            reason_of(&OutcomeResult::Partial, &missing, true),
            "install-disabled"
        );

        // 输入法本体这步失败 → install-failed（不是笼统的 failed）。
        let broken = ImeReport {
            install: "failed".to_string(),
            ..ImeReport::default()
        };
        assert_eq!(
            reason_of(&OutcomeResult::Failed, &broken, true),
            "install-failed"
        );

        // 正常路径：按结论映射。
        let fine = ImeReport {
            install: "installed".to_string(),
            present: true,
            ..ImeReport::default()
        };
        assert_eq!(reason_of(&OutcomeResult::Ok, &fine, true), "ok");
        assert_eq!(reason_of(&OutcomeResult::Noop, &fine, true), "noop");
        assert_eq!(reason_of(&OutcomeResult::Partial, &fine, true), "partial");
        assert_eq!(reason_of(&OutcomeResult::Failed, &fine, true), "failed");
    }

    #[test]
    fn bootstrap_timeout_only_fires_while_still_running() {
        let short = std::time::Duration::from_millis(10);
        // 已经结束：再久也不算超时（正常路径会发布终态，不能被看门狗覆盖）。
        assert!(!bootstrap_timed_out(
            false,
            std::time::Duration::from_secs(3600),
            short
        ));
        // 还在跑但没到阈值：不算超时。
        assert!(!bootstrap_timed_out(
            true,
            std::time::Duration::from_millis(1),
            short
        ));
        // 还在跑且超过阈值：算超时 → 状态从 running 改成 failed。
        assert!(bootstrap_timed_out(
            true,
            std::time::Duration::from_millis(20),
            short
        ));
    }
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 建议项：FixImeReport 里不再重复声明 build_*/deploy_*（它们来自 flatten 的
    /// PrebuiltReport）。本测试锁死「序列化出来的 JSON 里没有同名键」。
    ///
    /// B-2：config_skipped 必须是**一个**键，且值来自显式字段。
    #[test]
    fn fix_ime_report_json_has_no_duplicate_keys() {
        let report = FixImeReport {
            rime_dir: "/tmp/rime".to_string(),
            payload: "/tmp/payload".to_string(),
            payload_source: "test".to_string(),
            probe_before: "none".to_string(),
            message: "m".to_string(),
            data_patched: 0,
            needs_elevation: false,
            data: DataPatchReport::default(),
            weasel_present: false,
            weasel_installed: false,
            weasel_version: String::new(),
            weasel_installer: String::new(),
            weasel_installer_source: String::new(),
            weasel_install: "skipped(platform)".to_string(),
            weasel_error: None,
            weasel_needs_elevation: false,
            weasel_log: String::new(),
            weasel_message: String::new(),
            result: "partial".to_string(),
            detail: "config_skipped=[default.custom.yaml:no-wubi86-in-data]".to_string(),
            actions: Vec::new(),
            numpad: numpad_status(true),
            assessment: ime_state::Assessment {
                schemas: Vec::new(),
                build_ok: true,
                reason: ime_state::Reason::Ready,
            },
            // config_skipped 来自 flatten 的 PrebuiltReport（只有一份键）。
            report: PrebuiltReport {
                config_skipped: vec!["default.custom.yaml:no-wubi86-in-data".to_string()],
                ..PrebuiltReport::empty("test")
            },
        };
        let text = serde_json::to_string(&report).expect("serialize");
        let quote = char::from(34u8);
        let key = |name: &str| format!("{quote}{name}{quote}");
        // 键只能出现一次：flatten 的那份被显式字段覆盖（同名键只写一遍）。
        assert_eq!(
            text.matches(&key("config_skipped")).count(),
            1,
            "config_skipped 出现了重复键：{text}"
        );
        // 显式字段的值必须在里面。
        assert!(
            text.contains("no-wubi86-in-data"),
            "显式的 config_skipped 值没进去：{text}"
        );
        // 重复声明过的 build_*/deploy_* 只出现一次。
        for name in [
            "build_schemas",
            "build_repaired",
            "build_missing",
            "deploy_method",
            "deploy_result",
            "deploy_ok",
            "deploy_ms",
            "deploy_detail",
            "deploy_inproc",
        ] {
            assert_eq!(
                text.matches(&key(name)).count(),
                1,
                "{name} 出现了重复键：{text}"
            );
        }
    }

    /// 第二次调用必须立即 busy，不能排队；第一次结束后标志要复位。
    #[test]
    fn second_fix_ime_call_returns_busy_without_queueing() {
        let flag = Arc::new(AtomicBool::new(false));
        let worker_flag = Arc::clone(&flag);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = run_exclusive(&worker_flag, || {
                started_tx.send(()).expect("signal start");
                release_rx.recv().expect("wait release");
                1
            });
            assert_eq!(result, Ok(1));
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("第一个任务应该已经开始");
        let started = Instant::now();
        let error = run_exclusive(&flag, || 2).expect_err("第二次调用必须返回 busy");
        assert!(error.contains(BUSY_MARKER), "error={error}");
        // 文案不写「修复任务」：这把锁被启动自举和手动修复共用，
        // 用户点修复时可能根本没有修复在跑（是自举在跑）。
        assert!(error.contains("输入法任务已在执行中"), "error={error}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "busy 必须立即返回，实际等了 {:?}",
            started.elapsed()
        );
        release_tx.send(()).expect("release first call");
        worker.join().expect("first task finished");
        // 第一个任务结束后标志必须复位，否则以后再也修不了输入法。
        assert_eq!(run_exclusive(&flag, || 3), Ok(3));
    }

    /// B6：future 被 drop 时标志不能提前释放（任务还在跑就必须仍然 busy）。
    #[test]
    fn dropping_the_future_does_not_release_the_busy_flag() {
        // 'static：生产代码用的是 static FIX_IME_BUSY，这里用 leak 模拟同样的生命周期。
        let flag: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let guard = try_acquire(flag).expect("第一次应该抢到");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        // 模拟 spawn_blocking：守卫被 move 进任务闭包，而不是留在 future 里。
        let worker = std::thread::spawn(move || {
            let _guard = guard;
            started_tx.send(()).expect("signal start");
            let _ = release_rx.recv();
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("任务应该已经开始");
        // 这里相当于 future 被 drop：句柄丢了，但任务还在跑。
        assert!(
            try_acquire(flag).is_none(),
            "任务未结束时第二次调用必须仍然 busy"
        );
        release_tx.send(()).expect("release worker");
        worker.join().expect("worker finished");
        assert!(try_acquire(flag).is_some(), "任务结束后标志必须复位");
    }

    /// B1：OkCancelCustom 的「去修复」返回 Custom("去修复")，不是 Yes。
    #[test]
    fn custom_dialog_button_is_treated_as_fix() {
        assert!(dialog_goes_to_fix(&MessageDialogResult::Custom(
            DIALOG_FIX.to_string()
        )));
        assert!(dialog_goes_to_fix(&MessageDialogResult::Yes));
        assert!(dialog_goes_to_fix(&MessageDialogResult::Ok));
        assert!(!dialog_goes_to_fix(&MessageDialogResult::Custom(
            DIALOG_CLOSE.to_string()
        )));
        assert!(!dialog_goes_to_fix(&MessageDialogResult::Custom(
            "关闭对话框".to_string()
        )));
        assert!(!dialog_goes_to_fix(&MessageDialogResult::Cancel));
        assert!(!dialog_goes_to_fix(&MessageDialogResult::No));
    }

    /// 任务返回 Err 也要复位（否则一次失败就永久 busy）。
    #[test]
    fn failed_task_releases_the_flag() {
        let flag = AtomicBool::new(false);
        let result = run_exclusive(&flag, || Err::<(), &'static str>("boom"));
        assert_eq!(result, Ok(Err::<(), &'static str>("boom")));
        assert!(!flag.load(Ordering::SeqCst));
        assert!(try_acquire(&flag).is_some());
    }

    /// core 的 NumpadStatus 的最小构造（ok / 不 ok 两档）。
    fn numpad_status(ok: bool) -> ime_state::NumpadStatus {
        if ok {
            ime_state::NumpadStatus {
                ok: true,
                missing: Vec::new(),
                detail: vec!["wubi86=ok".to_string()],
                hint: String::new(),
            }
        } else {
            ime_state::NumpadStatus {
                ok: false,
                missing: vec!["wubi86=artifact-stale".to_string()],
                detail: vec!["wubi86=artifact-stale".to_string()],
                hint: "小键盘处理器（numpad）还没编译进产物（wubi86=artifact-stale）：请重启一次输入法。"
                    .to_string(),
            }
        }
    }

    /// 造一份 FixImeReport（只填跟小键盘/结论有关的字段）。
    fn sample_fix_report(result: &str, numpad_ok: bool) -> FixImeReport {
        FixImeReport {
            rime_dir: "/tmp/rime".to_string(),
            payload: "/tmp/payload".to_string(),
            payload_source: "test".to_string(),
            probe_before: "none".to_string(),
            message: "m".to_string(),
            data_patched: 0,
            needs_elevation: false,
            data: DataPatchReport::default(),
            weasel_present: true,
            weasel_installed: true,
            weasel_version: "1.0".to_string(),
            weasel_installer: String::new(),
            weasel_installer_source: String::new(),
            weasel_install: "installed".to_string(),
            weasel_error: None,
            weasel_needs_elevation: false,
            weasel_log: String::new(),
            weasel_message: String::new(),
            result: result.to_string(),
            detail: "numpad_missing=wubi86=artifact-stale (小键盘处理器还没编译进产物)".to_string(),
            actions: Vec::new(),
            numpad: numpad_status(numpad_ok),
            assessment: ime_state::Assessment {
                schemas: Vec::new(),
                build_ok: true,
                reason: ime_state::Reason::Ready,
            },
            report: PrebuiltReport::empty("test"),
        }
    }

    /// 小键盘原因码：只在「结论是 partial 且小键盘确实没生效」时给 numpad-pending，
    /// 而且不许盖掉更严重的原因码（输入法不在 / 安装失败 / 整体失败）。
    #[test]
    fn reason_of_reports_numpad_pending_only_when_the_keypad_is_broken() {
        use ime_state::OutcomeResult;
        let fine = ImeReport {
            install: "installed".to_string(),
            present: true,
            ..ImeReport::default()
        };
        // a. partial + 小键盘没生效 → 专用原因码（不是笼统的 partial）。
        assert_eq!(
            reason_of(&OutcomeResult::Partial, &fine, false),
            "numpad-pending"
        );
        // b. partial + 小键盘好着 → 仍然是 partial（不误判）。
        assert_eq!(reason_of(&OutcomeResult::Partial, &fine, true), "partial");
        // c. failed → 严重失败优先：小键盘不许覆盖它。
        assert_eq!(reason_of(&OutcomeResult::Failed, &fine, false), "failed");
        // d. 输入法不在（install-disabled）→ 优先级最高，小键盘也不许盖。
        let missing = ImeReport {
            install: "disabled".to_string(),
            present: false,
            ..ImeReport::default()
        };
        assert_eq!(
            reason_of(&OutcomeResult::Partial, &missing, false),
            "install-disabled"
        );
        // 输入法本体这步失败 → install-failed（也比小键盘严重）。
        let broken = ImeReport {
            install: "failed".to_string(),
            ..ImeReport::default()
        };
        assert_eq!(
            reason_of(&OutcomeResult::Partial, &broken, false),
            "install-failed"
        );
        // e. result=ok 而小键盘没生效：这种组合按理不该出现（core 保证小键盘
        //    有缺口时结论降级成 partial），但万一出现**不许**报 "ok" ——
        //    那正是缺陷三当初的静默失败（报 ok、小键盘其实用不了）。
        //    所以给专用的 numpad-pending。
        assert_eq!(
            reason_of(&OutcomeResult::Ok, &fine, false),
            "numpad-pending"
        );
        assert_eq!(reason_of(&OutcomeResult::Ok, &fine, true), "ok");
        assert_eq!(reason_of(&OutcomeResult::Noop, &fine, true), "noop");
    }

    /// 提权修复那条路径（from_fix_report）用同一套小键盘原因码。
    #[test]
    fn fix_report_uses_the_numpad_pending_reason_code() {
        // partial + 小键盘没生效 → numpad-pending，红条要亮。
        let status = ImeBootstrapStatus::from_fix_report(&sample_fix_report("partial", false));
        assert_eq!(status.reason, "numpad-pending");
        assert!(!status.ok, "小键盘不可用时不能报「已就绪」");
        // detail 里必须看得出小键盘这回事（core 已经写了 numpad_missing=…）。
        assert!(
            status.detail.contains("numpad"),
            "detail 里看不到小键盘：{}",
            status.detail
        );

        // partial + 小键盘好着 → 仍然是 partial（不误判）。
        let status = ImeBootstrapStatus::from_fix_report(&sample_fix_report("partial", true));
        assert_eq!(status.reason, "partial");

        // failed → 严重失败优先（这条路径的原有语义是 install-failed），小键盘不许盖。
        let status = ImeBootstrapStatus::from_fix_report(&sample_fix_report("failed", false));
        assert_eq!(status.reason, "install-failed");

        // 输入法不在 → 优先级最高。
        let mut report = sample_fix_report("partial", false);
        report.weasel_install = "disabled".to_string();
        report.weasel_present = false;
        let status = ImeBootstrapStatus::from_fix_report(&report);
        assert_eq!(status.reason, "install-disabled");

        // result=ok 而小键盘没生效：按理不该出现，但不许报「已就绪」。
        let status = ImeBootstrapStatus::from_fix_report(&sample_fix_report("ok", false));
        assert_eq!(status.reason, "numpad-pending");
        assert!(!status.ok);
    }

    /// core 忘了往 detail 里写小键盘提示时，GUI 侧兜底把 hint 追加到末尾
    /// （不能把原话改坏，只在末尾追加）。
    #[test]
    fn detail_gets_the_numpad_hint_appended_when_core_did_not_write_it() {
        let mut report = sample_fix_report("partial", false);
        report.detail = "配置有点问题".to_string();
        let status = ImeBootstrapStatus::from_fix_report(&report);
        assert_eq!(status.reason, "numpad-pending");
        assert!(status.detail.starts_with("配置有点问题"));
        assert!(status.detail.contains("numpad"));
        // core 写过的时候不许重复追加。
        let status = ImeBootstrapStatus::from_fix_report(&sample_fix_report("partial", false));
        assert_eq!(
            status.detail.matches("numpad").count(),
            1,
            "detail: {}",
            status.detail
        );
    }

    /// FixImeReport 序列化后必须带 numpad 段（前端按这个名字读）。
    #[test]
    fn fix_ime_report_json_contains_the_numpad_section() {
        let report = sample_fix_report("partial", false);
        let text = serde_json::to_string(&report).expect("serialize");
        assert!(text.contains("\"numpad\":"), "{text}");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        let numpad = value.get("numpad").expect("numpad 段");
        assert_eq!(numpad.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            numpad
                .get("missing")
                .and_then(|v| v.as_array())
                .map(|items| items.len()),
            Some(1)
        );
        assert!(numpad
            .get("hint")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("numpad"));
    }
}
