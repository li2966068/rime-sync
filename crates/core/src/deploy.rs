//! 部署手段决策与「自动部署」前置条件（0.6.23）。
//!
//! 227 实测（2026-11-06）：WeaselDeployer.exe /deploy 恒返回 0，却不产出任何
//! %APPDATA%\Rime\build 文件——它只是通过 IPC 请求 WeaselServer 部署，非交互会话下
//! 静默失败（官方安装包同样）。引擎启动时按时间戳判断 build 是否最新，我们铺进去的
//! prebuilt 永远比 data 新，于是「本机部署」这条链永远不生效。
//!
//! 0.6.23 的出路：
//! * 显式重新部署 → 进程内直接调 librime API（见 crate::librime，deploy_method=inproc）；
//! * 自动部署 → 只刷新 data 下相关方案文件的 mtime（mtime=现在），让引擎的
//!   「配置文件有改动」判定成立（见 touch_time 里的 librime 源码依据）；然后结束
//!   WeaselServer.exe，由系统在用户下次输入时
//!   自动拉起并触发引擎启动部署。绝不在会话 0 里 Start-Process——那会得到会话 0 的
//!   僵尸实例（我们踩过）。
//! * 竞态（0.6.23）：进程内部署会写 user.yaml / installation.yaml，必须先停服务再跑，
//!   否则和正在运行的输入法抢同一个文件。
//!
//! user.yaml 属于跨设备同步的文件，**绝不能**为了让本机重新部署去改它：那会让其它
//! 设备下次启动也多跑一次部署。所以触发条件只剩「data 比 build 新」这一条。
//!
//! Linux 保持 0.6.22 的行为（rime_deployer / fcitx5-remote 在 data_patch 里），
//! 本模块在非 Windows 平台只做决策记录，不执行任何外部命令。

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 结束输入法服务用（Windows）。
pub const TASKKILL: &str = "taskkill";
/// 确认服务是否真的退出了（Windows）。
pub const TASKLIST: &str = "tasklist";
/// 输入法服务进程名：结束它之后系统会在用户下次输入时自动拉起。
pub const SERVER_IMAGE: &str = "WeaselServer.exe";
/// 调试用：完全不碰 WeaselServer（单测/排障）。
pub const ENV_NO_RESTART: &str = "RIMESYNC_DEPLOY_NO_RESTART";
/// 停服务后最多等多久确认退出。
pub const SERVER_STOP_WAIT_MS: u64 = 3000;
/// 确认退出时的轮询间隔。
pub const SERVER_STOP_POLL_MS: u64 = 250;
/// taskkill 的「没有这个进程」退出码：进程本来就不在 = 成功。
const TASKKILL_NOT_FOUND: i32 = 128;
/// 停服务失败（进程仍存活）时，跳过进程内部署的决策理由。
pub const REASON_SERVER_ALIVE: &str = "server-alive";

pub const RESULT_OK: &str = "ok";
/// 部署完成了，但有降级/告警（进程内部署失败、touch 失败、taskkill 失败……）。
pub const RESULT_PARTIAL: &str = "partial";
pub const RESULT_FAILED: &str = "failed";
pub const RESULT_SKIPPED: &str = "skipped";
/// 启动自举：本次一个外部命令都没跑，留给用户手动「重新部署」或引擎下次启动自己重建。
pub const RESULT_DEFERRED: &str = "deferred";
/// 启动自举把部署降级成 deferred 时的决策理由。
pub const REASON_BOOTSTRAP_DEFERRED: &str = "bootstrap-deferred";

/// 部署手段，写进日志的 deploy_method=（serde 用 kebab-case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployMethod {
    /// 什么都不做（build 已经可用，也没有任何东西需要刷新）。
    None,
    /// 铺了随包 prebuilt 产物，并结束 WeaselServer 让引擎重读。
    Prebuilt,
    /// 依赖引擎下次启动时的自动部署（touch data + 结束服务）。
    Auto,
    /// 进程内 librime 部署已成功（见 crate::librime）。
    Inproc,
    /// 启动自举：只刷 mtime，绝不结束服务、绝不跑部署器（引擎下次启动自己重建）。
    Deferred,
}

impl DeployMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Prebuilt => "prebuilt",
            Self::Auto => "auto",
            Self::Inproc => "inproc",
            Self::Deferred => "deferred",
        }
    }
}

impl std::fmt::Display for DeployMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 决策输入（全部是事实，不含任何副作用）。
#[derive(Debug, Clone, Copy, Default)]
pub struct DeployInput {
    /// 当前平台是否 Windows。非 Windows 一律保持现状。
    pub windows: bool,
    /// 是否用户显式来源（设置页「修复输入法」、CLI fix-ime）。
    pub explicit: bool,
    /// 随包 prebuilt 产物是否可用（payload/build 里有文件）。
    pub prebuilt_available: bool,
    /// 收尾时 build/ 是否可用。
    pub build_usable: bool,
    /// 本次是否真的写了 build/ 文件。
    pub wrote_build: bool,
    /// 进程内 librime 部署是否已成功。
    pub inproc_ok: bool,
}

/// 决策结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployPlan {
    pub method: DeployMethod,
    /// 收尾时结束 WeaselServer.exe（部署前已停过则为 false）。
    ///
    /// 我们**从不**在收尾时拉起服务：由系统在用户下次输入时自动拉起。
    pub stop_server: bool,
    /// 先做「touch data」，让引擎下次启动自动部署。
    pub prepare_auto: bool,
    /// 决策理由（写日志，便于排障）。
    pub reason: &'static str,
}

impl DeployPlan {
    fn idle(reason: &'static str) -> Self {
        Self {
            method: DeployMethod::None,
            stop_server: false,
            prepare_auto: false,
            reason,
        }
    }

    /// 停服务失败（进程仍存活）→ 跳过进程内部署，退回 prebuilt。
    ///
    /// 日志因此是 deploy_method=prebuilt reason=server-alive。
    pub fn server_alive_degraded() -> Self {
        Self {
            method: DeployMethod::Prebuilt,
            stop_server: false,
            prepare_auto: true,
            reason: REASON_SERVER_ALIVE,
        }
    }
}

/// 决策函数（纯函数：单测直接断言，不碰进程也不碰文件系统）。
///
/// 规则（Windows）：
/// 1. 进程内部署成功 → inproc，且**不**再碰服务；
/// 2. prebuilt 可用而 build 不可用 → prebuilt（铺完再让引擎重读）；
/// 3. 否则只要需要刷新（显式来源 / build 不可用 / 本次写过文件）→ auto；
/// 4. 其余 → none。
pub fn decide(input: &DeployInput) -> DeployPlan {
    if !input.windows {
        // Linux：部署触发留在 data_patch（rime_deployer / fcitx5-remote），这里不插手。
        return DeployPlan::idle("platform:linux-keeps-existing-deploy");
    }
    if input.inproc_ok {
        return DeployPlan {
            method: DeployMethod::Inproc,
            stop_server: false,
            prepare_auto: false,
            reason: "inproc-deployed",
        };
    }
    if input.prebuilt_available && !input.build_usable {
        return DeployPlan {
            method: DeployMethod::Prebuilt,
            stop_server: true,
            prepare_auto: true,
            reason: "prebuilt-seeded",
        };
    }
    if input.explicit {
        return DeployPlan {
            method: DeployMethod::Auto,
            stop_server: true,
            prepare_auto: true,
            reason: "explicit-inproc-failed",
        };
    }
    if !input.build_usable || input.wrote_build {
        return DeployPlan {
            method: DeployMethod::Auto,
            stop_server: true,
            prepare_auto: true,
            reason: if input.build_usable {
                "build-files-changed"
            } else {
                "no-usable-build"
            },
        };
    }
    DeployPlan::idle("build-already-usable")
}

/// 执行决策的结果（写日志用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    /// ok / failed / skipped。
    pub result: &'static str,
    /// 明细（每一步的退出码/原因），拼在日志一行里。
    pub detail: String,
}

impl DeployOutcome {
    fn skipped(detail: impl Into<String>) -> Self {
        Self {
            result: RESULT_SKIPPED,
            detail: detail.into(),
        }
    }
}

impl DeployOutcome {
    pub fn ok(detail: impl Into<String>) -> Self {
        Self {
            result: RESULT_OK,
            detail: detail.into(),
        }
    }

    /// 成功但有告警（例如 taskkill 非 0/128）：结果仍是 ok，细节带 warning=。
    ///
    /// B3：结束输入法服务是 best-effort，失败只记一行 warning，不能把整次 apply 判死。
    pub fn ok_with_warning(detail: impl Into<String>) -> Self {
        Self {
            result: RESULT_OK,
            detail: format!("warning={}", detail.into()),
        }
    }

    /// 明细里是否带 warning=（调用方据此写 tracing::warn 而不是 info）。
    pub fn has_warning(&self) -> bool {
        self.detail.contains("warning=")
    }

    /// 追加一段明细（保留原有 warning 标记在最前）。
    pub fn push_detail(&mut self, extra: impl AsRef<str>) {
        let extra = extra.as_ref();
        if extra.is_empty() {
            return;
        }
        if self.detail.is_empty() {
            self.detail.push_str(extra);
        } else {
            self.detail.push(' ');
            self.detail.push_str(extra);
        }
    }
}

/// 当前平台的决策输入。
pub fn platform_input(
    explicit: bool,
    prebuilt_available: bool,
    build_usable: bool,
    wrote_build: bool,
    inproc_ok: bool,
) -> DeployInput {
    DeployInput {
        windows: cfg!(windows),
        explicit,
        prebuilt_available,
        build_usable,
        wrote_build,
        inproc_ok,
    }
}

/// C8：本次没有任何方案被改动 → 取消「刷新 mtime」，免得引擎白重建一遍。
///
/// 返回是否真的取消了（调用方据此写日志）。抽成纯函数才能在非 Windows 上单测。
pub fn skip_prepare_auto(plan: &mut DeployPlan) -> bool {
    if !plan.prepare_auto {
        return false;
    }
    plan.prepare_auto = false;
    true
}

/// 启动自举的部署决策：只保留「刷 mtime 让引擎下次启动自己重建」。
///
/// 绝不结束服务（taskkill）、绝不跑部署器 —— 自举很可能发生在用户正在打字时。
/// 与 decide() 分开是因为它必须**无条件**压掉 stop_server（decide 的 prebuilt 分支
/// 会给出 stop_server=true）。
pub fn defer_plan(plan: DeployPlan) -> DeployPlan {
    DeployPlan {
        method: DeployMethod::Deferred,
        stop_server: false,
        prepare_auto: plan.prepare_auto,
        reason: REASON_BOOTSTRAP_DEFERRED,
    }
}

/// 引擎 apply 之后让输入法重新加载：只结束服务，不碰部署器。
pub fn reload_plan(reason: &'static str) -> DeployPlan {
    DeployPlan {
        method: DeployMethod::Auto,
        stop_server: true,
        prepare_auto: false,
        reason,
    }
}

/// 一条外部命令的输出（只保留判断需要的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
}

/// 可注入的命令执行器：单测用它断言「先停服务、再部署」的顺序与次数。
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String>;
    /// 轮询之间的等待（单测实现成空操作，不必真等 3 秒）。
    fn sleep(&self, ms: u64);
}

/// 真实实现：走 crate::process::command（Windows 上不带控制台窗口）。
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
        let output = crate::process::command(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| format!("spawn {program}: {error}"))?;
        Ok(CommandOutput {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }

    fn sleep(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// 「停服务」的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStop {
    /// 结束成功，并且已确认进程退出。
    Stopped,
    /// 本来就不在运行（taskkill 退出码 128）。
    NotRunning,
    /// taskkill 成功，但等待期内仍能看到进程。
    StillAlive,
    /// 命令跑不起来或退出码异常。
    Failed,
    /// ENV_NO_RESTART=1：按开关要求完全不碰服务。
    Disabled,
}

impl ServerStop {
    /// 服务已不在（或按开关有意不动）：可以安全地跑进程内部署。
    pub fn ok(self) -> bool {
        matches!(self, Self::Stopped | Self::NotRunning | Self::Disabled)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::NotRunning => "not-running",
            Self::StillAlive => "still-alive",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
        }
    }
}

/// 一次「停服务」的结果 + 明细。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStopOutcome {
    pub state: ServerStop,
    pub detail: String,
}

impl ServerStopOutcome {
    fn new(state: ServerStop, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
        }
    }

    pub fn ok(&self) -> bool {
        self.state.ok()
    }
}

/// 部署**之前**结束 WeaselServer.exe，并确认它真的退出了（最多等 3 秒）。
///
/// 只应在 Windows 上调用（调用方按 cfg!(windows) 判断）。
/// 进程本来就不在（taskkill 退出码 128）= 成功：没有什么可停的。
pub fn stop_before_deploy(runner: &dyn CommandRunner) -> ServerStopOutcome {
    let killed = kill_server_once(runner);
    if killed.state != ServerStop::Stopped {
        // 本来就不在 / 命令失败 / 调试开关关闭：都不必再确认。
        return killed;
    }
    let polls = (SERVER_STOP_WAIT_MS / SERVER_STOP_POLL_MS).max(1);
    for poll in 0..polls {
        if !server_alive(runner) {
            return ServerStopOutcome::new(
                ServerStop::Stopped,
                format!("{} {TASKLIST}=gone poll={poll}", killed.detail),
            );
        }
        runner.sleep(SERVER_STOP_POLL_MS);
    }
    ServerStopOutcome::new(
        ServerStop::StillAlive,
        format!(
            "{} but {SERVER_IMAGE} alive after {SERVER_STOP_WAIT_MS}ms",
            killed.detail
        ),
    )
}

/// 收尾（引擎 apply 之后的重载）用：只发一次 taskkill，不轮询确认。
///
/// 保持 0.6.23 的语义——重载是 best-effort，不值得为「确认退出」卡 3 秒；需要
/// 「确认服务已退出再写 user.yaml」的是进程内部署，用 stop_before_deploy。
pub fn kill_server_once(runner: &dyn CommandRunner) -> ServerStopOutcome {
    if no_restart_switch() {
        return ServerStopOutcome::new(ServerStop::Disabled, format!("{ENV_NO_RESTART}=1"));
    }
    match runner.run(TASKKILL, &["/IM", SERVER_IMAGE, "/F"]) {
        Err(error) => ServerStopOutcome::new(ServerStop::Failed, error),
        Ok(output) if output.code == TASKKILL_NOT_FOUND => ServerStopOutcome::new(
            ServerStop::NotRunning,
            format!("{TASKKILL}=exit{}(not-running)", output.code),
        ),
        Ok(output) if output.code != 0 => ServerStopOutcome::new(
            ServerStop::Failed,
            format!("{TASKKILL}=exit{}", output.code),
        ),
        Ok(output) => ServerStopOutcome::new(
            ServerStop::Stopped,
            format!("{TASKKILL}=exit{}", output.code),
        ),
    }
}

/// RIMESYNC_DEPLOY_NO_RESTART=1：调试开关，完全不碰 WeaselServer。
fn no_restart_switch() -> bool {
    std::env::var(ENV_NO_RESTART)
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

/// 服务是否还活着：能查到进程名就算活着。
///
/// 查不出来（tasklist 跑不起来 / 输出看不懂）也按「活着」处理：宁可退回 prebuilt，
/// 也不要和正在运行的输入法抢 user.yaml / installation.yaml。
fn server_alive(runner: &dyn CommandRunner) -> bool {
    let filter = format!("IMAGENAME eq {SERVER_IMAGE}");
    match runner.run(TASKLIST, &["/FI", filter.as_str(), "/NH"]) {
        Ok(output) => {
            let needle = SERVER_IMAGE.to_ascii_lowercase();
            output.stdout.to_ascii_lowercase().contains(needle.as_str())
        }
        Err(_) => true,
    }
}

/// 服务确实停掉之后才跑进程内部署；停不下来返回 None。
///
/// 调用方拿到 None 时必须降级 prebuilt（见 DeployPlan::server_alive_degraded）：
/// 输入法还在写 user.yaml，此时再跑 librime 部署就是两个进程抢同一个文件。
pub fn inproc_after_stop<F>(
    stop: &ServerStopOutcome,
    deploy: F,
) -> Option<crate::librime::InprocReport>
where
    F: FnOnce() -> crate::librime::InprocReport,
{
    if !stop.ok() {
        return None;
    }
    Some(deploy())
}

/// 自动部署的前置条件（只刷 data 的 mtime）。
#[derive(Debug, Clone, Serialize, Default)]
pub struct AutoDeployReport {
    /// mtime 被刷新成「现在」（且严格晚于 build）的 data 方案文件。
    pub touched: Vec<String>,
    /// 共享 data 目录写不进去、改刷新用户目录同名方案的情况（B2）。
    pub fallbacks: Vec<String>,
    /// 刷新失败的文件与原因（含尝试过的全部路径）。
    pub failed: Vec<String>,
}

impl AutoDeployReport {
    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }
}

/// 让「引擎下次启动时重新部署」的条件成立。
///
/// 只把 data 下相关方案的 *.schema.yaml 的 mtime 刷成现在（时间点见 touch_time）；
/// **绝不写 user.yaml**——它是跨设备同步的文件，归零 last_build_time 会让其它设备
/// 下次启动也多做一次部署。
///
/// 写不进去只记失败，不 panic（权限问题时调用方还有 prebuilt 兜底）。共享 data 目录
/// 不可写时退一步刷新 user_dir 下的同名方案（B2），两边都失败才记 failed。
pub fn prepare_auto_deploy(
    data_dirs: &[PathBuf],
    schema_ids: &[String],
    build_dir: Option<&Path>,
    user_dir: Option<&Path>,
) -> AutoDeployReport {
    let when = touch_time(build_dir);
    let mut report = AutoDeployReport::default();
    for id in schema_ids {
        let name = format!("{id}.schema.yaml");
        let shared: Vec<PathBuf> = data_dirs
            .iter()
            .map(|dir| dir.join(&name))
            .filter(|path| path.is_file())
            .collect();
        let mut tried: Vec<String> = Vec::new();
        if !shared.is_empty() {
            let (touched, failed) = touch_files(&shared, when);
            let all_failed = touched.is_empty();
            report.touched.extend(touched);
            if !all_failed {
                continue;
            }
            tried.extend(failed);
        }
        // B2：共享 data 目录写不进去（Windows 上很常见：Program Files 需要管理员），
        // 就退一步刷用户目录下的同名方案——引擎同样会看用户目录的 mtime。
        // 两边都不成功才记失败，并把「尝试过哪些路径」写进日志。
        let Some(candidate) = user_dir.map(|dir| dir.join(&name)) else {
            if !tried.is_empty() {
                report
                    .failed
                    .push(format!("{id}: 刷新失败（尝试过：{}）", tried.join("；")));
            }
            continue;
        };
        if candidate.is_file() {
            let (mut touched, mut failed) = touch_files(std::slice::from_ref(&candidate), when);
            if !touched.is_empty() {
                if !shared.is_empty() {
                    report.fallbacks.push(format!(
                        "{} -> {}",
                        shared
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(" | "),
                        candidate.display()
                    ));
                }
                report.touched.append(&mut touched);
                continue;
            }
            tried.append(&mut failed);
        } else if !tried.is_empty() {
            tried.push(format!("{}（不存在）", candidate.display()));
        }
        if tried.is_empty() {
            // 两边都没有这个文件：不是失败，本次只是没有可刷新对象。
            continue;
        }
        report
            .failed
            .push(format!("{id}: 刷新失败（尝试过：{}）", tried.join("；")));
    }
    report
}

/// 刷新 mtime 用的时间：现在，且不早于 build 里最新的文件。
///
/// 引擎的真实判据（third_party/librime/src/rime/lever/deployment_tasks.cc:45-78）：
/// `max(data 目录自身与其下 *.yaml 的 mtime, 排除 user.yaml) > user.yaml 的
/// var/last_build_time` → 需要部署；build/ 的 mtime 不参与判定，last_build_time 由
/// 上一次成功部署写成。所以「把 data 刷成现在」本身就够用（现在 > 上次部署时间）。
///
/// build 的最新 mtime 只做下限：产物里若出现比现在更晚的时间戳（时钟回拨、预置/拷贝
/// 保留的时间），取「build + 1s」，保证 data 不会比 build 旧（粗粒度时间戳上两边也
/// 不会相等）。
pub fn touch_time(build_dir: Option<&Path>) -> SystemTime {
    let now = SystemTime::now();
    let Some(newest) = build_dir.and_then(newest_mtime) else {
        return now;
    };
    let floor = newest + Duration::from_secs(1);
    if floor > now {
        floor
    } else {
        now
    }
}

/// 目录里平铺文件的最新 mtime（build/ 是平铺的）。
fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .filter_map(|meta| meta.modified().ok())
        .max()
}

/// 把一批文件的 mtime 刷成 when。返回（成功，失败+原因）。
pub fn touch_files(files: &[PathBuf], when: SystemTime) -> (Vec<String>, Vec<String>) {
    let mut touched = Vec::new();
    let mut failed = Vec::new();
    for path in files {
        match fs::OpenOptions::new().write(true).open(path) {
            Ok(file) => match file.set_modified(when) {
                Ok(()) => touched.push(path.display().to_string()),
                Err(error) => failed.push(format!("{}: {error}", path.display())),
            },
            Err(error) => failed.push(format!("{}: {error}", path.display())),
        }
    }
    (touched, failed)
}

/// 执行决策：满足条件才动外部进程；非 Windows 平台只记录 skipped。
pub fn apply(plan: &DeployPlan) -> DeployOutcome {
    if plan.method == DeployMethod::None {
        return DeployOutcome::skipped(plan.reason.to_string());
    }
    if plan.method == DeployMethod::Deferred {
        // 启动自举：一个外部命令都不跑（连 taskkill 也不跑）。
        return DeployOutcome {
            result: RESULT_DEFERRED,
            detail: plan.reason.to_string(),
        };
    }
    #[cfg(windows)]
    {
        apply_with(plan, &SystemRunner)
    }
    #[cfg(not(windows))]
    {
        DeployOutcome::skipped("platform:linux")
    }
}

/// 决策的执行体（平台无关，单测直接注入假 runner）。
///
/// B3：结束输入法服务是 **best-effort**。taskkill 非 0/128（例如进程拒绝终止、权限不足）
/// 只记一行 warning、结果仍是 ok——重载没做成不影响已经铺好的产物，也不该让
/// 「同步 + 部署」整体判失败。真正能判失败的是「build 不可用且随包产物也补不齐」，
/// 那由 finish_deploy 汇总（见 crate::prebuilt::finish_deploy）。
pub fn apply_with(plan: &DeployPlan, runner: &dyn CommandRunner) -> DeployOutcome {
    if plan.method == DeployMethod::None {
        return DeployOutcome::skipped(plan.reason.to_string());
    }
    if plan.method == DeployMethod::Deferred {
        // deferred 的契约：runner 一次都不许被调用（bootstrap 不打断用户）。
        return DeployOutcome {
            result: RESULT_DEFERRED,
            detail: plan.reason.to_string(),
        };
    }
    if !plan.stop_server {
        return DeployOutcome::ok(plan.reason.to_string());
    }
    if no_restart_switch() {
        return DeployOutcome::skipped(format!("{ENV_NO_RESTART}=1"));
    }
    // 收尾只发一次 taskkill（不轮询）：重载是 best-effort，别让同步多等 3 秒。
    let stop = kill_server_once(runner);
    let detail = format!(
        "{} server={} {}",
        plan.reason,
        stop.state.as_str(),
        stop.detail
    );
    if stop.ok() {
        DeployOutcome::ok(detail)
    } else {
        // 非 0/128（含 taskkill 跑不起来）：告警 + 继续，绝不 bail。
        DeployOutcome::ok_with_warning(format!("server-stop-failed {detail}"))
    }
}

/// 毫秒时间戳（日志用）。
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    static SEQ: AtomicUsize = AtomicUsize::new(0);
    /// 停服务相关测试串行执行：ENV_NO_RESTART 是进程级环境变量，并发会互相干扰。
    static STOP_SERIAL: Mutex<()> = Mutex::new(());

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path =
                std::env::temp_dir().join(format!("rime-sync-deploy-{}-{seq}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn input(
        windows: bool,
        explicit: bool,
        prebuilt: bool,
        usable: bool,
        inproc: bool,
    ) -> DeployInput {
        DeployInput {
            windows,
            explicit,
            prebuilt_available: prebuilt,
            build_usable: usable,
            wrote_build: false,
            inproc_ok: inproc,
        }
    }

    fn fake_inproc() -> crate::librime::InprocReport {
        crate::librime::InprocReport {
            deploy_method: crate::librime::METHOD_OK.to_string(),
            deploy_ok: true,
            deploy_ms: 1,
            library: "-".to_string(),
            detail: "test".to_string(),
            shared_data_dir: "-".to_string(),
            user_data_dir: "-".to_string(),
        }
    }

    /// 可注入的执行器：记录每条命令，用来断言顺序与次数。
    struct Recorder {
        taskkill_code: i32,
        alive_polls: AtomicUsize,
        calls: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn new(taskkill_code: i32, alive_polls: usize) -> Self {
            Self {
                taskkill_code,
                alive_polls: AtomicUsize::new(alive_polls),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn record(&self, line: impl Into<String>) {
            self.calls.lock().expect("lock").push(line.into());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock").clone()
        }

        fn count(&self, program: &str) -> usize {
            let prefix = format!("{program} ");
            self.calls()
                .iter()
                .filter(|line| line.starts_with(&prefix))
                .count()
        }

        fn deploy_count(&self) -> usize {
            self.calls().iter().filter(|line| *line == "deploy").count()
        }
    }

    impl CommandRunner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
            self.record(format!("{program} {}", args.join(" ")));
            match program {
                TASKKILL => Ok(CommandOutput {
                    code: self.taskkill_code,
                    stdout: String::new(),
                }),
                TASKLIST => {
                    let alive = self.alive_polls.load(Ordering::SeqCst) > 0;
                    if alive {
                        self.alive_polls.fetch_sub(1, Ordering::SeqCst);
                    }
                    Ok(CommandOutput {
                        code: 0,
                        stdout: if alive {
                            "WeaselServer.exe          4242 Console                    1     12,345 K\n"
                                .to_string()
                        } else {
                            "INFO: No tasks are running which match the specified criteria.\n"
                                .to_string()
                        },
                    })
                }
                other => Err(format!("unexpected program {other}")),
            }
        }

        fn sleep(&self, ms: u64) {
            self.record(format!("sleep {ms}"));
        }
    }

    #[test]
    fn inproc_success_skips_the_server_stop() {
        let plan = decide(&input(true, true, true, false, true));
        assert_eq!(plan.method, DeployMethod::Inproc);
        assert!(!plan.stop_server, "进程内已部署，不需要再停服务");
        assert!(!plan.prepare_auto);
        assert_eq!(plan.method.as_str(), "inproc");
    }

    /// M5：启动自举（Options::bootstrap）的部署决策一个外部命令都不许跑。
    ///
    /// 这里取 decide() 最激进的那条分支（payload 可用、build 不可用 →
    /// stop_server=true 会 taskkill），defer_plan 必须把它压平，且 apply_with
    /// 复用可注入 runner 证明 runner 一次都没被调用。
    #[test]
    fn deferred_plan_runs_no_external_command() {
        let host = Recorder::new(0, 0);
        let aggressive = decide(&input(true, false, true, false, false));
        assert!(aggressive.stop_server, "前提：decide 本会结束服务");
        let plan = defer_plan(aggressive);
        assert_eq!(plan.method, DeployMethod::Deferred);
        assert_eq!(plan.method.as_str(), "deferred");
        assert!(!plan.stop_server, "自举绝不允许 taskkill");
        let outcome = apply_with(&plan, &host);
        assert_eq!(outcome.result, RESULT_DEFERRED);
        assert_eq!(outcome.detail, REASON_BOOTSTRAP_DEFERRED);
        assert!(
            host.calls().is_empty(),
            "deferred 不许调用任何外部命令：{:?}",
            host.calls()
        );
    }

    #[test]
    fn prebuilt_seeding_stops_the_server_and_prepares_auto_deploy() {
        let plan = decide(&input(true, false, true, false, false));
        assert_eq!(plan.method, DeployMethod::Prebuilt);
        assert!(plan.stop_server);
        assert!(plan.prepare_auto);
        assert_eq!(plan.method.as_str(), "prebuilt");
    }

    #[test]
    fn explicit_without_inproc_falls_back_to_auto() {
        let plan = decide(&input(true, true, false, false, false));
        assert_eq!(plan.method, DeployMethod::Auto);
        assert!(plan.stop_server && plan.prepare_auto);
        assert_eq!(plan.method.as_str(), "auto");
    }

    #[test]
    fn repaired_build_uses_auto_without_extra_change() {
        let mut case = input(true, false, true, true, false);
        case.wrote_build = true;
        let plan = decide(&case);
        assert_eq!(plan.method, DeployMethod::Auto);
        assert_eq!(plan.reason, "build-files-changed");
    }

    #[test]
    fn healthy_build_needs_nothing() {
        let plan = decide(&input(true, false, true, true, false));
        assert_eq!(plan.method, DeployMethod::None);
        assert!(!plan.stop_server && !plan.prepare_auto);
        assert_eq!(plan.method.as_str(), "none");
    }

    #[test]
    fn linux_never_touches_the_input_method() {
        for explicit in [true, false] {
            for inproc in [true, false] {
                let plan = decide(&input(false, explicit, true, false, inproc));
                assert_eq!(plan.method, DeployMethod::None);
                assert!(!plan.stop_server && !plan.prepare_auto);
            }
        }
    }

    #[test]
    fn method_strings_match_the_log_contract() {
        assert_eq!(DeployMethod::None.as_str(), "none");
        assert_eq!(DeployMethod::Prebuilt.as_str(), "prebuilt");
        assert_eq!(DeployMethod::Auto.as_str(), "auto");
        assert_eq!(DeployMethod::Inproc.as_str(), "inproc");
    }

    #[test]
    fn server_alive_degradation_logs_prebuilt() {
        let plan = DeployPlan::server_alive_degraded();
        assert_eq!(plan.method, DeployMethod::Prebuilt);
        assert_eq!(plan.reason, REASON_SERVER_ALIVE);
        assert!(!plan.stop_server, "停不下来就别再重复 taskkill");
        assert!(plan.prepare_auto, "仍要让引擎下次启动自己部署");
        assert_eq!(
            format!("deploy_method={} reason={}", plan.method, plan.reason),
            "deploy_method=prebuilt reason=server-alive"
        );
    }

    #[test]
    fn touch_files_updates_mtime() {
        let tmp = TempDir::new();
        let path = tmp.path().join("luna_pinyin_simp.schema.yaml");
        fs::write(&path, "name: x\n").expect("write");
        let old = SystemTime::now() - Duration::from_secs(3600);
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(old)
            .expect("set mtime");
        let before = fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");

        let when = SystemTime::now();
        let (touched, failed) = touch_files(std::slice::from_ref(&path), when);

        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(touched.len(), 1);
        let after = fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert!(after > before, "mtime 必须前进");
    }

    /// 0.6.23 c：只刷 data 的 mtime；user.yaml 一个字节都不许动，
    /// 且 data 不旧于 build 里最新的文件。
    #[test]
    fn prepare_auto_deploy_touches_only_data_and_never_writes_user_yaml() {
        let tmp = TempDir::new();
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).expect("data dir");
        let schema = data.join("wubi86.schema.yaml");
        fs::write(&schema, "name: wubi86\n").expect("write schema");
        let build = tmp.path().join("build");
        fs::create_dir_all(&build).expect("build dir");
        let built = build.join("wubi86.schema.yaml");
        fs::write(&built, "name: wubi86\n").expect("write build schema");
        let prism = build.join("wubi86.prism.bin");
        fs::write(&prism, "prism").expect("write prism");
        let old = SystemTime::now() - Duration::from_secs(3600);
        for path in [&built, &prism] {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("open build file")
                .set_modified(old)
                .expect("set build mtime");
        }
        let newest_build = [&built, &prism]
            .iter()
            .filter_map(|path| fs::metadata(path).ok()?.modified().ok())
            .max()
            .expect("build mtime");

        // user.yaml 属于会跨设备同步的文件：内容与 mtime 都必须原样。
        let user = tmp.path().join("rime");
        fs::create_dir_all(&user).expect("user dir");
        let user_yaml = user.join("user.yaml");
        let original = b"var:\r\n  last_build_time: 1712345678\r\n".to_vec();
        fs::write(&user_yaml, &original).expect("write user.yaml");
        let user_mtime = fs::metadata(&user_yaml)
            .expect("meta")
            .modified()
            .expect("mtime");

        let report = prepare_auto_deploy(
            std::slice::from_ref(&data),
            &["wubi86".to_string(), "missing".to_string()],
            Some(&build),
            Some(&user),
        );

        assert!(report.ok(), "{report:?}");
        assert_eq!(report.touched.len(), 1);
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(
            fs::read(&user_yaml).expect("read user.yaml"),
            original,
            "user.yaml 必须逐字节不变"
        );
        assert_eq!(
            fs::metadata(&user_yaml)
                .expect("meta")
                .modified()
                .expect("mtime"),
            user_mtime,
            "user.yaml 的 mtime 也不能被碰"
        );
        let schema_mtime = fs::metadata(&schema)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert!(
            schema_mtime > newest_build,
            "data 不应旧于 build：data={schema_mtime:?} build={newest_build:?}"
        );
    }

    #[test]
    fn prepare_auto_deploy_without_data_files_is_a_noop() {
        let tmp = TempDir::new();
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).expect("data dir");
        let report = prepare_auto_deploy(
            std::slice::from_ref(&data),
            &["absent".to_string()],
            None,
            None,
        );
        assert!(report.ok());
        assert!(report.touched.is_empty());
        assert!(
            fs::read_dir(&data).expect("read dir").next().is_none(),
            "没有可 touch 的文件时不得凭空创建"
        );
    }

    #[test]
    fn reload_kill_is_single_shot_without_polling() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let host = Recorder::new(0, 1_000_000);
        let stop = kill_server_once(&host);
        assert_eq!(stop.state, ServerStop::Stopped, "{stop:?}");
        assert!(stop.ok());
        assert_eq!(host.count(TASKKILL), 1);
        assert_eq!(host.count(TASKLIST), 0, "收尾不再轮询确认");
    }

    #[test]
    fn stop_kills_the_server_then_the_inproc_deploy_runs_once() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        // 第一次 tasklist 还能看到进程，之后消失。
        let host = Recorder::new(0, 1);
        let stop = stop_before_deploy(&host);
        assert_eq!(stop.state, ServerStop::Stopped, "{stop:?}");
        assert!(stop.ok());
        let inproc = inproc_after_stop(&stop, || {
            host.record("deploy");
            fake_inproc()
        });
        assert!(inproc.is_some());
        assert_eq!(
            host.calls(),
            vec![
                format!("{TASKKILL} /IM {SERVER_IMAGE} /F"),
                format!("{TASKLIST} /FI IMAGENAME eq {SERVER_IMAGE} /NH"),
                format!("sleep {SERVER_STOP_POLL_MS}"),
                format!("{TASKLIST} /FI IMAGENAME eq {SERVER_IMAGE} /NH"),
                "deploy".to_string(),
            ],
            "顺序必须是先停服务、再部署"
        );
        assert_eq!(host.count(TASKKILL), 1);
        assert_eq!(host.count(TASKLIST), 2);
        assert_eq!(host.deploy_count(), 1);
    }

    #[test]
    fn absent_server_counts_as_stopped() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let host = Recorder::new(TASKKILL_NOT_FOUND, 0);
        let stop = stop_before_deploy(&host);
        assert_eq!(stop.state, ServerStop::NotRunning, "{stop:?}");
        assert!(stop.ok());
        assert_eq!(host.count(TASKLIST), 0, "进程本来就不在，不必再查");
        assert!(inproc_after_stop(&stop, fake_inproc).is_some());
    }

    #[test]
    fn taskkill_failure_skips_the_inproc_deploy() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let host = Recorder::new(1, 0);
        let stop = stop_before_deploy(&host);
        assert_eq!(stop.state, ServerStop::Failed, "{stop:?}");
        assert!(!stop.ok());
        assert_eq!(host.count(TASKLIST), 0);
        let inproc = inproc_after_stop(&stop, || panic!("服务还活着时不得跑进程内部署"));
        assert!(inproc.is_none());
    }

    #[test]
    fn still_alive_server_waits_then_degrades() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let host = Recorder::new(0, 1_000_000);
        let stop = stop_before_deploy(&host);
        assert_eq!(stop.state, ServerStop::StillAlive, "{stop:?}");
        assert!(!stop.ok());
        let polls = (SERVER_STOP_WAIT_MS / SERVER_STOP_POLL_MS) as usize;
        assert_eq!(host.count(TASKLIST), polls, "最多等 3 秒");
        assert_eq!(
            host.calls()
                .iter()
                .filter(|line| line.starts_with("sleep "))
                .count(),
            polls
        );
        assert_eq!(host.count(TASKKILL), 1, "只杀一次，不反复杀");
        assert!(inproc_after_stop(&stop, || panic!("服务还活着时不得跑进程内部署")).is_none());
        assert_eq!(
            DeployPlan::server_alive_degraded().method,
            DeployMethod::Prebuilt
        );
    }

    #[test]
    fn no_restart_switch_never_touches_the_server() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let previous = std::env::var_os(ENV_NO_RESTART);
        std::env::set_var(ENV_NO_RESTART, "1");
        let host = Recorder::new(0, 0);
        let stop = stop_before_deploy(&host);
        match previous {
            Some(value) => std::env::set_var(ENV_NO_RESTART, value),
            None => std::env::remove_var(ENV_NO_RESTART),
        }
        assert_eq!(stop.state, ServerStop::Disabled, "{stop:?}");
        assert!(stop.ok(), "调试开关不是失败：进程内部署照跑");
        assert!(host.calls().is_empty(), "开关打开时不得启动任何外部命令");
    }

    /// C8：没有方案被改动时取消刷 mtime；本来就不需要刷时返回 false。
    #[test]
    fn skip_prepare_auto_only_cancels_a_pending_touch() {
        let mut plan = decide(&input(true, true, false, false, false));
        assert!(plan.prepare_auto, "显式来源本来要 touch");
        assert!(skip_prepare_auto(&mut plan));
        assert!(!plan.prepare_auto);
        assert!(!skip_prepare_auto(&mut plan), "已经不打算 touch 了");
    }

    /// B3：taskkill 返回非 0/128 只告警，结果仍是 ok（不 bail、不中断后续步骤）。
    #[test]
    fn taskkill_failure_is_a_warning_and_keeps_the_apply_running() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let host = Recorder::new(1, 0);
        let plan = reload_plan("engine-apply");
        let outcome = apply_with(&plan, &host);
        assert_eq!(outcome.result, RESULT_OK, "{outcome:?}");
        assert!(outcome.has_warning(), "{outcome:?}");
        assert!(outcome.detail.contains("server=failed"), "{outcome:?}");
        assert_eq!(host.count(TASKKILL), 1, "只尝试一次，不重试、不 bail");
    }

    /// 进程本来就不在（128）是正常情况：ok 且不带 warning。
    #[test]
    fn absent_server_is_ok_without_warning() {
        let _serial = STOP_SERIAL.lock().expect("serial");
        let host = Recorder::new(TASKKILL_NOT_FOUND, 0);
        let outcome = apply_with(&reload_plan("engine-apply"), &host);
        assert_eq!(outcome.result, RESULT_OK, "{outcome:?}");
        assert!(!outcome.has_warning(), "{outcome:?}");
        assert_eq!(host.count(TASKKILL), 1);
    }

    /// B2：共享 data 目录写不进去时改刷用户目录同名方案，成功且记 fallback。
    #[cfg(unix)]
    #[test]
    fn readonly_shared_schema_falls_back_to_the_user_dir() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new();
        let data = tmp.path().join("data");
        let user = tmp.path().join("rime");
        fs::create_dir_all(&data).expect("data dir");
        fs::create_dir_all(&user).expect("user dir");
        let shared = data.join("wubi86.schema.yaml");
        let local = user.join("wubi86.schema.yaml");
        fs::write(&shared, "name: wubi86\n").expect("write shared");
        fs::write(&local, "name: wubi86\n").expect("write local");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o444)).expect("chmod shared");
        if fs::OpenOptions::new().write(true).open(&shared).is_ok() {
            eprintln!("skip: 当前用户（可能是 root）不受只读权限限制");
            return;
        }

        let report = prepare_auto_deploy(
            std::slice::from_ref(&data),
            &["wubi86".to_string()],
            None,
            Some(&user),
        );

        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(report.touched.len(), 1, "{report:?}");
        assert_eq!(report.touched[0], local.display().to_string());
        assert_eq!(report.fallbacks.len(), 1, "{report:?}");
        assert!(
            report.fallbacks[0].contains("wubi86.schema.yaml"),
            "{report:?}"
        );
        assert!(report.ok());
    }

    /// B2：共享目录与用户目录都写不进去 → failed（绝不能报 ok）。
    #[cfg(unix)]
    #[test]
    fn readonly_shared_and_user_schema_is_a_failure() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new();
        let data = tmp.path().join("data");
        let user = tmp.path().join("rime");
        fs::create_dir_all(&data).expect("data dir");
        fs::create_dir_all(&user).expect("user dir");
        let shared = data.join("wubi86.schema.yaml");
        let local = user.join("wubi86.schema.yaml");
        fs::write(&shared, "name: wubi86\n").expect("write shared");
        fs::write(&local, "name: wubi86\n").expect("write local");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o444)).expect("chmod shared");
        fs::set_permissions(&local, fs::Permissions::from_mode(0o444)).expect("chmod local");
        if fs::OpenOptions::new().write(true).open(&shared).is_ok() {
            eprintln!("skip: 当前用户（可能是 root）不受只读权限限制");
            return;
        }

        let report = prepare_auto_deploy(
            std::slice::from_ref(&data),
            &["wubi86".to_string()],
            None,
            Some(&user),
        );

        assert!(!report.ok(), "两边都失败时必须失败：{report:?}");
        assert!(report.touched.is_empty(), "{report:?}");
        assert_eq!(report.failed.len(), 1, "{report:?}");
        // 日志要写清尝试过的路径。
        assert!(
            report.failed[0].contains(&shared.display().to_string()),
            "{report:?}"
        );
        assert!(
            report.failed[0].contains(&local.display().to_string()),
            "{report:?}"
        );
    }
}
