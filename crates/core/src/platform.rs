//! 平台触发：让 librime 真正合并 userdb 并重新部署。
//!
//! 这里只做进程调用，不链接 librime；headless 测试用 FakeTrigger。

use crate::process::command;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

pub trait RimeTrigger: Send + Sync {
    fn sync_userdb(&self, rime_dir: &Path) -> Result<()>;
    fn deploy(&self, rime_dir: &Path) -> Result<()>;

    /// 用户显式要求「重新部署」时用（保存并重新部署 / 修复输入法）。
    ///
    /// 0.6.23 起 Windows 走进程内 librime API（见 crate::librime），不再依赖
    /// WeaselDeployer——它只用 IPC 请求 WeaselServer，非交互会话下返回 0 却不产出。
    fn manual_deploy(&self, rime_dir: &Path) -> Result<()> {
        self.deploy(rime_dir)
    }

    /// apply 期间 Linux 需要让 fcitx5 释放 LevelDB 锁。
    fn stop_ime(&self) -> Result<()> {
        Ok(())
    }

    fn start_ime(&self) -> Result<()> {
        Ok(())
    }

    /// 导出本地词库文本（rime_dict_manager -e <schema> <dest>），要求输入法已停止。
    /// 平台不支持时返回错误；调用方只 warn，不阻断 apply。
    fn export_userdb(&self, _rime_dir: &Path, _schema: &str, _dest: &Path) -> Result<()> {
        bail!("userdb export is not supported on this platform")
    }

    fn name(&self) -> &'static str {
        "unknown"
    }
}

pub fn command_exists(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// 在指定目录里运行外部命令（rime_dict_manager 依赖 CWD 定位用户词库）。
fn run_in(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<()> {
    let output = command(program)
        .current_dir(dir)
        .args(args)
        .envs(envs.iter().map(|(key, value)| (*key, *value)))
        .output()
        .with_context(|| format!("spawn {program} in {}", dir.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "{program} {:?} exited {:?}: {}{}",
            args,
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        );
    }
    Ok(())
}

fn run(program: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<()> {
    let output = command(program)
        .args(args)
        .envs(envs.iter().map(|(key, value)| (*key, *value)))
        .output()
        .with_context(|| format!("spawn {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "{program} {:?} exited {:?}: {}{}",
            args,
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct LinuxTrigger {
    pub dict_manager: String,
    pub deployer: String,
    pub shared_data_dir: PathBuf,
    pub stop_fcitx5: bool,
}

#[cfg(target_os = "linux")]
impl LinuxTrigger {
    pub fn new(stop_fcitx5: bool) -> Self {
        Self {
            dict_manager: "rime_dict_manager".to_string(),
            deployer: "rime_deployer".to_string(),
            shared_data_dir: detect_shared_data_dir(),
            stop_fcitx5,
        }
    }

    fn require(&self, program: &str) -> Result<()> {
        if command_exists(program) {
            return Ok(());
        }
        bail!(
            "{program} not found in PATH; install librime-bin (Debian/Ubuntu: sudo apt install librime-bin)"
        )
    }
}

/// fcitx5 主进程名（对应 /proc/<pid>/comm）。
#[cfg(target_os = "linux")]
const IME_PROCESS: &str = "fcitx5";

/// 僵尸/已死状态（Z=zombie、X/x=dead）：它们已释放所有 fd，不能算「还在运行」。
#[cfg(target_os = "linux")]
const DEAD_STATES: [char; 3] = ['Z', 'X', 'x'];

/// 停输入法的升级节奏：fcitx5-remote -e → SIGTERM → SIGKILL。
#[cfg(target_os = "linux")]
const IME_SOFT_STOP_GRACE: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const IME_TERM_GRACE: Duration = Duration::from_secs(3);
#[cfg(target_os = "linux")]
const IME_KILL_GRACE: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const IME_EXIT_POLL: Duration = Duration::from_millis(100);

/// 等启动器退出的上限；超时就把回收交给后台线程，apply 不阻塞。
#[cfg(target_os = "linux")]
const IME_LAUNCHER_WAIT: Duration = Duration::from_secs(5);

/// rime_dict_manager --sync 的重试次数与间隔（只对 LevelDB 锁错误重试）。
#[cfg(target_os = "linux")]
const DICT_SYNC_ATTEMPTS: u32 = 3;
#[cfg(target_os = "linux")]
const DICT_SYNC_LOCK_RETRY: Duration = Duration::from_secs(1);

/// 解析 /proc/<pid>/stat 的 state 字段。
///
/// 格式是 `<pid> (<comm>) <state> ...`；comm 由用户态决定，可含空格甚至 `)`，
/// 内核不转义，所以只能用最后一个 `)` 定位字段边界，不能按空白切分。
#[cfg(target_os = "linux")]
fn parse_proc_state(stat: &str) -> Option<char> {
    let close = stat.rfind(')')?;
    stat[close + 1..].trim_start().chars().next()
}

/// 读 /proc/<pid>/stat 的进程状态；进程已退出或读不到时返回 None。
#[cfg(target_os = "linux")]
fn proc_state(proc_root: &Path, pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    parse_proc_state(&stat)
}

/// 扫 proc_root 下 comm == name 的进程，返回 (pid, state)，跳过僵尸/已死进程。
#[cfg(target_os = "linux")]
fn ime_procs_in(proc_root: &Path, name: &str) -> Vec<(u32, char)> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut procs = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        if comm.trim() != name {
            continue;
        }
        // 读不到 stat：两次读之间进程已退出。僵尸虽然名字还在 /proc，但锁早已释放。
        let Some(state) = proc_state(proc_root, pid) else {
            tracing::debug!("{name} pid {pid} vanished before its state could be read");
            continue;
        };
        if DEAD_STATES.contains(&state) {
            tracing::debug!("ignoring {name} pid {pid} in state {state}: not running");
            continue;
        }
        procs.push((pid, state));
    }
    procs.sort_unstable_by_key(|(pid, _)| *pid);
    procs
}

/// 仍在运行的 fcitx5 进程（不含僵尸）。
#[cfg(target_os = "linux")]
fn live_ime_procs() -> Vec<(u32, char)> {
    ime_procs_in(Path::new("/proc"), IME_PROCESS)
}

/// fcitx5 是否正在运行（不含僵尸）。非 Linux 恒 false。
///
/// Linux 的部署编排（crate::linux_deploy）用它决定「要不要跳过进程内部署」：
/// 输入法在跑的时候，librime 部署会和它抢 userdb / 文件锁。
pub fn ime_running() -> bool {
    #[cfg(target_os = "linux")]
    {
        !live_ime_procs().is_empty()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// 日志里打印 pid(state)，直接说明「为什么它看起来还在跑」。
#[cfg(target_os = "linux")]
fn describe_procs(procs: &[(u32, char)]) -> String {
    procs
        .iter()
        .map(|(pid, state)| format!("{pid}({state})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 停输入法所需的最小原语；抽成 trait 便于测试断言升级顺序。
#[cfg(target_os = "linux")]
trait ImeOps {
    fn alive(&mut self) -> Vec<(u32, char)>;
    /// fcitx5-remote -e 是否已执行（命令不存在时返回 false）。
    fn soft_stop(&mut self) -> bool;
    fn term(&mut self) -> bool;
    fn kill(&mut self) -> bool;

    /// 轮询等进程退出；超时返回 false。
    fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.alive().is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(IME_EXIT_POLL);
        }
    }
}

/// 先礼后兵：fcitx5-remote -e → SIGTERM → SIGKILL，每一步都等进程真正退出。
#[cfg(target_os = "linux")]
fn escalate_stop(ime: &mut dyn ImeOps) -> Result<()> {
    let running = ime.alive();
    if running.is_empty() {
        tracing::debug!("{IME_PROCESS} is not running; nothing to stop");
        return Ok(());
    }
    tracing::info!(
        "stopping {IME_PROCESS} ({}) to release the userdb LevelDB lock",
        describe_procs(&running)
    );
    if ime.soft_stop() && ime.wait_exit(IME_SOFT_STOP_GRACE) {
        tracing::info!("{IME_PROCESS} exited after fcitx5-remote -e");
        return Ok(());
    }
    if !ime.term() {
        bail!(
            "{IME_PROCESS} is still running ({}) and pkill is unavailable; cannot release the userdb LevelDB lock",
            describe_procs(&ime.alive())
        );
    }
    tracing::warn!(
        "{IME_PROCESS} is still running ({}); sent SIGTERM and waiting {}s",
        describe_procs(&ime.alive()),
        IME_TERM_GRACE.as_secs()
    );
    if ime.wait_exit(IME_TERM_GRACE) {
        tracing::info!("{IME_PROCESS} exited after SIGTERM");
        return Ok(());
    }
    if !ime.kill() {
        bail!(
            "{IME_PROCESS} ignored SIGTERM ({}) and pkill is unavailable; cannot release the userdb LevelDB lock",
            describe_procs(&ime.alive())
        );
    }
    tracing::warn!(
        "{IME_PROCESS} survived SIGTERM ({}); sent SIGKILL and waiting {}s",
        describe_procs(&ime.alive()),
        IME_KILL_GRACE.as_secs()
    );
    if ime.wait_exit(IME_KILL_GRACE) {
        tracing::info!("{IME_PROCESS} exited after SIGKILL");
        return Ok(());
    }
    let residual = ime.alive();
    bail!(
        "{IME_PROCESS} still running {}s after SIGKILL ({}); refusing to run rime_dict_manager while the userdb LevelDB is locked",
        IME_KILL_GRACE.as_secs(),
        describe_procs(&residual)
    )
}

/// 真实的进程操作。
#[cfg(target_os = "linux")]
struct SystemIme;

#[cfg(target_os = "linux")]
impl SystemIme {
    fn signal(signal: &str) -> bool {
        if !command_exists("pkill") {
            return false;
        }
        let flag = format!("-{signal}");
        match command("pkill")
            .args([flag.as_str(), "-x", IME_PROCESS])
            .output()
        {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!("pkill {flag} -x {IME_PROCESS}: {error}");
                false
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl ImeOps for SystemIme {
    fn alive(&mut self) -> Vec<(u32, char)> {
        live_ime_procs()
    }

    fn soft_stop(&mut self) -> bool {
        if !command_exists("fcitx5-remote") {
            tracing::debug!("fcitx5-remote not found; falling back to signals");
            return false;
        }
        // 尽力而为：fcitx5 未运行时返回非零，不应让 apply 失败。
        let _ = command("fcitx5-remote").arg("-e").output();
        true
    }

    fn term(&mut self) -> bool {
        Self::signal("TERM")
    }

    fn kill(&mut self) -> bool {
        Self::signal("KILL")
    }
}

/// LevelDB 被别的进程占用时的典型报错。
#[cfg(target_os = "linux")]
fn is_lock_error(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    ["read-only", "error opening db", "lock"]
        .iter()
        .any(|pattern| text.contains(pattern))
}

#[cfg(target_os = "linux")]
impl RimeTrigger for LinuxTrigger {
    fn sync_userdb(&self, rime_dir: &Path) -> Result<()> {
        self.require(&self.dict_manager)?;
        let running = live_ime_procs();
        if !running.is_empty() {
            bail!(
                "fcitx5 is still running ({}); rime_dict_manager needs exclusive access to the userdb LevelDB",
                describe_procs(&running)
            );
        }
        let rime_dir = rime_dir.to_str().context("rime_dir is not valid UTF-8")?;
        for attempt in 1..=DICT_SYNC_ATTEMPTS {
            // rime_dict_manager 相对「当前目录」解析 installation.yaml 和
            // <dict>.userdb；只设 RIME_USER_DATA_DIR 不够（会打开错误的 LevelDB，
            // 报 'Error opening db ... read-only'）。必须在 rime_dir 里运行。
            let output = command(&self.dict_manager)
                .arg("--sync")
                .current_dir(rime_dir)
                .env("RIME_USER_DATA_DIR", rime_dir)
                .output()
                .with_context(|| format!("spawn {}", self.dict_manager))?;
            if output.status.success() {
                tracing::info!(
                    "{} --sync ok (attempt {attempt}/{DICT_SYNC_ATTEMPTS})",
                    self.dict_manager
                );
                return Ok(());
            }
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            tracing::warn!(
                "{} --sync attempt {attempt}/{DICT_SYNC_ATTEMPTS} exited {:?}: {stdout}{stderr}",
                self.dict_manager,
                output.status.code()
            );
            // 只对 LevelDB 锁错误重试：fcitx5 退出后内核回收锁需要一点时间。
            // 其它错误（schema 缺失等）重试也不会好。
            if attempt < DICT_SYNC_ATTEMPTS && is_lock_error(&format!("{stderr}{stdout}")) {
                tracing::info!(
                    "userdb LevelDB is still locked; retrying in {}s",
                    DICT_SYNC_LOCK_RETRY.as_secs()
                );
                std::thread::sleep(DICT_SYNC_LOCK_RETRY);
                continue;
            }
            bail!(
                "{} --sync exited {:?}: {stdout}{stderr}",
                self.dict_manager,
                output.status.code()
            );
        }
        unreachable!("the sync loop returns on success or bails on the last attempt")
    }

    fn export_userdb(&self, rime_dir: &Path, schema: &str, dest: &Path) -> Result<()> {
        self.require(&self.dict_manager)?;
        let running = live_ime_procs();
        if !running.is_empty() {
            bail!(
                "fcitx5 is still running ({}); cannot export the userdb while LevelDB is locked",
                describe_procs(&running)
            );
        }
        let dest = dest.to_str().context("export path is not valid UTF-8")?;
        // 同 sync：必须在该用户目录里运行，否则工具会去 CWD 找 <dict>.userdb。
        run_in(
            rime_dir,
            &self.dict_manager,
            &["-e", schema, dest],
            &[(
                "RIME_USER_DATA_DIR",
                rime_dir.to_str().context("rime_dir is not valid UTF-8")?,
            )],
        )
        .context("rime_dict_manager -e failed")
    }

    fn deploy(&self, rime_dir: &Path) -> Result<()> {
        self.require(&self.deployer)?;
        let rime_dir = rime_dir.to_str().context("rime_dir is not valid UTF-8")?;
        let shared = self
            .shared_data_dir
            .to_str()
            .context("shared_data_dir is not valid UTF-8")?;
        run(&self.deployer, &["--build", rime_dir, shared], &[])
            .context("rime_deployer --build failed")
    }

    fn stop_ime(&self) -> Result<()> {
        if !self.stop_fcitx5 {
            tracing::debug!("stop_fcitx5_during_apply=false; leaving fcitx5 alone");
            return Ok(());
        }
        escalate_stop(&mut SystemIme)
    }

    fn start_ime(&self) -> Result<()> {
        if !self.stop_fcitx5 {
            return Ok(());
        }
        if !command_exists(IME_PROCESS) {
            tracing::warn!(
                "{IME_PROCESS} not found in PATH; the input method was stopped and stays stopped"
            );
            return Ok(());
        }
        // fcitx5 -d 会 fork 守护进程后自己立即退出；必须 wait 回收，否则留下
        // comm=fcitx5 的僵尸，下一次 apply 又会把它当成「仍在运行」。
        let mut child = match command(IME_PROCESS)
            .args(["-d", "--replace"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!("spawn {IME_PROCESS} -d --replace: {error}");
                return Ok(());
            }
        };
        let deadline = Instant::now() + IME_LAUNCHER_WAIT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::info!("{IME_PROCESS} launcher exited ({status}); child reaped");
                    return Ok(());
                }
                Ok(None) if Instant::now() < deadline => std::thread::sleep(IME_EXIT_POLL),
                Ok(None) => {
                    // 启动器迟迟不退（例如 fcitx5 在前台跑）：交给后台线程回收，
                    // 既不阻塞 apply，也不会在退出时留下僵尸。
                    tracing::warn!(
                        "{IME_PROCESS} launcher still running after {}s; reaping it in the background",
                        IME_LAUNCHER_WAIT.as_secs()
                    );
                    std::thread::spawn(move || {
                        if let Err(error) = child.wait() {
                            tracing::warn!("wait for {IME_PROCESS} launcher: {error}");
                        }
                    });
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!("wait for {IME_PROCESS} launcher: {error}");
                    return Ok(());
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "linux"
    }
}

#[cfg(target_os = "linux")]
fn detect_shared_data_dir() -> PathBuf {
    for candidate in ["/usr/share/rime-data", "/usr/local/share/rime-data"] {
        let path = PathBuf::from(candidate);
        if path.is_dir() {
            return path;
        }
    }
    PathBuf::from("/usr/share/rime-data")
}

#[cfg(windows)]
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct WeaselTrigger {
    pub deployer: Option<PathBuf>,
}

#[cfg(windows)]
#[allow(dead_code)]
impl WeaselTrigger {
    pub fn locate() -> Self {
        Self {
            deployer: find_weasel_deployer(),
        }
    }

    fn exe(&self) -> Result<&Path> {
        match self.deployer.as_deref() {
            Some(path) => Ok(path),
            None => {
                bail!("WeaselDeployer.exe not found; install Weasel or keep transport-only mode")
            }
        }
    }

    fn run_switch(&self, switch: &str) -> Result<()> {
        let exe = self.exe()?;
        let output = command(exe)
            .arg(switch)
            .output()
            .with_context(|| format!("spawn {}", exe.display()))?;
        if !output.status.success() {
            bail!(
                "{} {} exited {:?}",
                exe.display(),
                switch,
                output.status.code()
            );
        }
        Ok(())
    }
}

#[cfg(windows)]
#[allow(dead_code)]
fn find_weasel_deployer() -> Option<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles")?;
    let root = PathBuf::from(program_files).join("Rime");
    let entries = std::fs::read_dir(&root).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("WeaselDeployer.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(windows)]
#[allow(dead_code)]
impl RimeTrigger for WeaselTrigger {
    fn sync_userdb(&self, _rime_dir: &Path) -> Result<()> {
        self.run_switch("/sync")
    }

    /// 引擎 apply 之后的重新加载：**不调部署器**，只结束 WeaselServer。
    ///
    /// 227 证据：WeaselDeployer.exe /deploy 恒返回 0 却不产出任何 build 文件；
    /// 而结束服务后系统会在用户下次输入时自动拉起（绝不在会话 0 里 Start-Process，
    /// 那会得到会话 0 的僵尸实例）。
    fn deploy(&self, _rime_dir: &Path) -> Result<()> {
        let outcome = crate::deploy::apply(&crate::deploy::reload_plan("engine-apply"));
        // B3：taskkill 非 0/128（进程拒绝终止、权限不足……）只告警。重载没做成不影响
        // 已经铺好的产物；整次 apply 只有在「build 不可用且随包产物也补不齐」时才失败，
        // 那条判据在 prebuilt::finish_deploy 里。这里**绝不** bail。
        if outcome.result == crate::deploy::RESULT_FAILED {
            tracing::warn!(
                "weasel reload after apply failed (best-effort): result={} {}",
                outcome.result,
                outcome.detail
            );
            return Ok(());
        }
        if outcome.has_warning() || outcome.result == crate::deploy::RESULT_PARTIAL {
            tracing::warn!(
                "weasel reload after apply degraded: method={} result={} {}",
                crate::deploy::DeployMethod::Auto,
                outcome.result,
                outcome.detail
            );
        } else {
            tracing::info!(
                "weasel reload after apply: method={} result={} {}",
                crate::deploy::DeployMethod::Auto,
                outcome.result,
                outcome.detail
            );
        }
        Ok(())
    }

    /// 用户显式「重新部署」：**先停服务**，再在进程内直接调 librime API。
    ///
    /// 0.6.23：librime 部署会写 user.yaml / installation.yaml；服务还在跑的时候
    /// 两边抢同一个文件。停不下来就跳过 inproc（绝不硬上）。
    fn manual_deploy(&self, rime_dir: &Path) -> Result<()> {
        let stop = crate::deploy::stop_before_deploy(&crate::deploy::SystemRunner);
        let Some(report) =
            crate::deploy::inproc_after_stop(&stop, || crate::librime::deploy(rime_dir))
        else {
            bail!(
                "结束 WeaselServer 失败（server={} {}），已跳过进程内部署",
                stop.state.as_str(),
                stop.detail
            );
        };
        if report.deploy_ok {
            tracing::info!("in-process deploy ok: {}", report.detail);
            Ok(())
        } else {
            bail!("进程内部署失败：{}", report.detail)
        }
    }

    fn name(&self) -> &'static str {
        "weasel"
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedTrigger;

#[cfg(not(any(target_os = "linux", windows)))]
impl RimeTrigger for UnsupportedTrigger {
    fn sync_userdb(&self, _rime_dir: &Path) -> Result<()> {
        bail!("no RimeTrigger implementation for this platform")
    }

    fn deploy(&self, _rime_dir: &Path) -> Result<()> {
        bail!("no RimeTrigger implementation for this platform")
    }

    fn name(&self) -> &'static str {
        "unsupported"
    }
}

pub fn default_trigger(stop_fcitx5: bool) -> Box<dyn RimeTrigger> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxTrigger::new(stop_fcitx5))
    }
    #[cfg(windows)]
    {
        Box::new(WeaselTrigger::locate())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = stop_fcitx5;
        Box::new(UnsupportedTrigger)
    }
}

#[derive(Debug, Default)]
pub struct FakeTrigger {
    calls: Arc<Mutex<Vec<String>>>,
    fail_on: Mutex<Option<String>>,
    userdb_entries: Mutex<Vec<String>>,
}

impl FakeTrigger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn failing(step: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail_on: Mutex::new(Some(step.to_string())),
            userdb_entries: Mutex::new(Vec::new()),
        }
    }

    /// 跨线程共享的调用记录：Engine 会拿走 Box<dyn RimeTrigger>，测试仍能事后断言顺序。
    pub fn calls_shared(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.calls)
    }

    /// 让假的 userdb 导出写出这些词条（用于断言合并后条目数）。
    pub fn with_userdb_entries(self, entries: &[&str]) -> Self {
        if let Ok(mut guard) = self.userdb_entries.lock() {
            *guard = entries.iter().map(|value| value.to_string()).collect();
        }
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn record(&self, step: &str) -> Result<()> {
        if let Ok(mut guard) = self.calls.lock() {
            guard.push(step.to_string());
        }
        let fail = self.fail_on.lock().ok().and_then(|guard| guard.clone());
        if fail.as_deref() == Some(step) {
            bail!("fake trigger failure at {step}");
        }
        Ok(())
    }
}

impl RimeTrigger for FakeTrigger {
    fn sync_userdb(&self, _rime_dir: &Path) -> Result<()> {
        self.record("sync_userdb")
    }

    fn deploy(&self, _rime_dir: &Path) -> Result<()> {
        self.record("deploy")
    }

    fn stop_ime(&self) -> Result<()> {
        self.record("stop_ime")
    }

    fn start_ime(&self) -> Result<()> {
        self.record("start_ime")
    }

    fn export_userdb(&self, _rime_dir: &Path, _schema: &str, dest: &Path) -> Result<()> {
        self.record("export_userdb")?;
        let entries = self
            .userdb_entries
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let mut text = String::new();
        for entry in entries {
            text.push_str(&entry);
            text.push_str("\t1\n");
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, text)?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_trigger_records_order() {
        let trigger = FakeTrigger::new();
        trigger.stop_ime().expect("stop");
        trigger.sync_userdb(Path::new("/tmp/rime")).expect("sync");
        trigger.deploy(Path::new("/tmp/rime")).expect("deploy");
        trigger.start_ime().expect("start");
        assert_eq!(
            trigger.calls(),
            vec!["stop_ime", "sync_userdb", "deploy", "start_ime"]
        );
    }

    #[test]
    fn fake_trigger_can_fail_a_step() {
        let trigger = FakeTrigger::failing("deploy");
        assert!(trigger.sync_userdb(Path::new("/tmp/rime")).is_ok());
        assert!(trigger.deploy(Path::new("/tmp/rime")).is_err());
    }

    #[test]
    fn command_exists_finds_shell() {
        assert!(command_exists("sh"));
        assert!(!command_exists("definitely-not-a-real-binary-xyz"));
    }

    #[cfg(target_os = "linux")]
    fn temp_proc_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rime-sync-proc-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).expect("mkdir proc root");
        root
    }

    #[cfg(target_os = "linux")]
    fn write_proc(root: &Path, pid: u32, comm: &str, stat: &str) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).expect("mkdir pid dir");
        std::fs::write(dir.join("comm"), format!("{comm}\n")).expect("write comm");
        std::fs::write(dir.join("stat"), stat).expect("write stat");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parses_state_from_ordinary_stat_line() {
        assert_eq!(
            parse_proc_state("1234 (fcitx5) S 1 1234 1234 0 -1 4194560\n"),
            Some('S')
        );
        assert_eq!(parse_proc_state("1234 (fcitx5) Z 1 1234"), Some('Z'));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parses_state_when_comm_contains_spaces() {
        assert_eq!(
            parse_proc_state("77 (fcitx5 --replace) R 1 77 77 0 -1"),
            Some('R')
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parses_state_when_comm_contains_close_paren() {
        // comm 里的 ')' 不转义，只能按最后一个 ')' 定位。
        assert_eq!(parse_proc_state("88 (rime)(x) S 1 88"), Some('S'));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn rejects_malformed_stat_lines() {
        assert_eq!(parse_proc_state(""), None);
        assert_eq!(parse_proc_state("1234 fcitx5 S 1"), None);
        assert_eq!(parse_proc_state("1234 (fcitx5) "), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn proc_state_reads_the_injected_proc_root() {
        let root = temp_proc_root("state");
        write_proc(&root, 42, "fcitx5", "42 (fcitx5) S 1 42");
        assert_eq!(proc_state(&root, 42), Some('S'));
        assert_eq!(proc_state(&root, 43), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn ime_procs_ignores_zombies_and_other_names() {
        let root = temp_proc_root("zombie");
        write_proc(&root, 100, "fcitx5", "100 (fcitx5) Z 1 100");
        write_proc(&root, 101, "fcitx5", "101 (fcitx5) S 1 101");
        write_proc(&root, 102, "bash", "102 (bash) S 1 102");
        // comm 可读但 stat 缺失 == 扫描期间已退出。
        std::fs::create_dir_all(root.join("103")).expect("mkdir");
        std::fs::write(root.join("103").join("comm"), "fcitx5\n").expect("write comm");

        let procs = ime_procs_in(&root, "fcitx5");
        assert_eq!(procs, vec![(101, 'S')]);
        assert_eq!(describe_procs(&procs), "101(S)");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn classifies_only_leveldb_lock_failures_as_retryable() {
        assert!(is_lock_error(
            "Error opening db luna_pinyin read-only: IO error"
        ));
        assert!(is_lock_error("ERROR OPENING DB /home/u/luna_pinyin.userdb"));
        assert!(is_lock_error("IO error: lock hold by current process"));
        assert!(!is_lock_error("schema luna_pinyin not found"));
    }

    /// 只记录升级顺序的假实现：wait_exit 不真睡，退出时机由 exit_after 脚本决定。
    #[cfg(target_os = "linux")]
    struct FakeIme {
        calls: Vec<&'static str>,
        alive: Vec<(u32, char)>,
        soft_available: bool,
        term_available: bool,
        kill_available: bool,
        exit_after: Option<&'static str>,
        waits: Vec<Duration>,
    }

    #[cfg(target_os = "linux")]
    impl FakeIme {
        fn new(exit_after: Option<&'static str>) -> Self {
            Self {
                calls: Vec::new(),
                alive: vec![(1234, 'S')],
                soft_available: true,
                term_available: true,
                kill_available: true,
                exit_after,
                waits: Vec::new(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl ImeOps for FakeIme {
        fn alive(&mut self) -> Vec<(u32, char)> {
            self.alive.clone()
        }

        fn soft_stop(&mut self) -> bool {
            self.calls.push("soft_stop");
            if self.exit_after == Some("soft_stop") {
                self.alive.clear();
            }
            self.soft_available
        }

        fn term(&mut self) -> bool {
            self.calls.push("term");
            if self.exit_after == Some("term") {
                self.alive.clear();
            }
            self.term_available
        }

        fn kill(&mut self) -> bool {
            self.calls.push("kill");
            if self.exit_after == Some("kill") {
                self.alive.clear();
            }
            self.kill_available
        }

        fn wait_exit(&mut self, timeout: Duration) -> bool {
            self.waits.push(timeout);
            self.alive.is_empty()
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_uses_remote_first() {
        let mut ime = FakeIme::new(Some("soft_stop"));
        escalate_stop(&mut ime).expect("fcitx5 stopped");
        assert_eq!(ime.calls, vec!["soft_stop"]);
        assert_eq!(ime.waits, vec![IME_SOFT_STOP_GRACE]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_escalates_to_sigterm() {
        let mut ime = FakeIme::new(Some("term"));
        escalate_stop(&mut ime).expect("fcitx5 stopped");
        assert_eq!(ime.calls, vec!["soft_stop", "term"]);
        assert_eq!(ime.waits, vec![IME_SOFT_STOP_GRACE, IME_TERM_GRACE]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_escalates_to_sigkill() {
        let mut ime = FakeIme::new(Some("kill"));
        escalate_stop(&mut ime).expect("fcitx5 stopped");
        assert_eq!(ime.calls, vec!["soft_stop", "term", "kill"]);
        assert_eq!(
            ime.waits,
            vec![IME_SOFT_STOP_GRACE, IME_TERM_GRACE, IME_KILL_GRACE]
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_skips_remote_when_it_is_missing() {
        let mut ime = FakeIme::new(Some("term"));
        ime.soft_available = false;
        escalate_stop(&mut ime).expect("fcitx5 stopped");
        assert_eq!(ime.calls, vec!["soft_stop", "term"]);
        assert_eq!(ime.waits, vec![IME_TERM_GRACE]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_reports_residual_pids_with_state() {
        let mut ime = FakeIme::new(None);
        let error = format!("{:#}", escalate_stop(&mut ime).expect_err("still running"));
        assert!(error.contains("1234(S)"), "{error}");
        assert_eq!(ime.calls, vec!["soft_stop", "term", "kill"]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_fails_when_pkill_is_missing() {
        let mut ime = FakeIme::new(None);
        ime.soft_available = false;
        ime.term_available = false;
        let error = format!("{:#}", escalate_stop(&mut ime).expect_err("no pkill"));
        assert!(error.contains("pkill"), "{error}");
        assert_eq!(ime.calls, vec!["soft_stop", "term"]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_sequence_is_a_noop_when_ime_is_gone() {
        let mut ime = FakeIme::new(None);
        ime.alive.clear();
        escalate_stop(&mut ime).expect("nothing to stop");
        assert!(ime.calls.is_empty());
        assert!(ime.waits.is_empty());
    }

    #[test]
    fn fake_trigger_exports_configured_entries() {
        let trigger = FakeTrigger::new().with_userdb_entries(&["词条一", "词条二"]);
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-fake-export-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let dest = dir.join("out.txt");
        trigger
            .export_userdb(Path::new("/tmp/rime"), "luna_pinyin", &dest)
            .expect("export");
        let text = std::fs::read_to_string(&dest).expect("read");
        assert_eq!(text.lines().count(), 2);
        assert_eq!(trigger.calls(), vec!["export_userdb"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
