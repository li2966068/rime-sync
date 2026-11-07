//! Linux 的部署手段优先级（0.6.23）：外部部署器 > 跳过（输入法在跑）> 进程内 dlopen。
//!
//! 起因：rime-sync-cli deploy-ime 过去在 Linux 直接 dlopen librime 进程内部署。
//! 此时 fcitx5-rime 可能正在运行并持有 userdb / 文件锁，librime 部署会写
//! user.yaml / installation.yaml，两边抢同一批文件：轻则阻塞，重则把部署写坏。
//! Windows 侧已经有「先停服务并确认退出」的动作（见 crate::deploy），Linux 没有
//! （也**不能**去杀用户的输入法）。于是按下面顺序降级，每一步都有超时：
//!
//! 1. 有 rime_deployer 且 fcitx5 **没在跑** → 直接跑 rime_deployer --build <user>
//!    <shared>（120 秒超时），exit 0 即 deploy_method=external_ok；失败记
//!    external_failed，**不再**退回 dlopen（部署器存在时它才是官方入口，退回进程内
//!    只会重新引入抢锁问题）；
//! 2. 有部署器但 fcitx5 正在跑（B5）→ 先 `fcitx5-remote -r` **重载**（不杀进程，
//!    不打断正在打字的用户）；重载成功才继续跑 rime_deployer --build，重载失败就
//!    **不执行 build**，返回 skipped reason=ime-running（让用户自己退出输入法后重试）；
//! 3. 没有部署器、但 fcitx5-remote 存在且 fcitx5 正在跑 → 同样 skipped reason=ime-running；
//! 4. 都没有 → 才走进程内 dlopen（120 秒超时；超时后标记失败并等线程自然结束）。
//!
//! 调试开关：RIMESYNC_DEPLOY_FORCE_INPROC=1 强制走进程内；
//! RIMESYNC_DEPLOY_UNSAFE_EXTERNAL=1 允许在 fcitx5 运行时**直接** build（跳过重载，
//! 排障用——会有抢锁风险，别在正常使用时打开）。
//!
//! 本模块跨平台编译（单测在 Windows CI 上也跑），但只有 Linux 的 deploy-ime
//! 会调用它；真正的 fcitx5 探测在 crate::platform::ime_running 里（非 Linux 恒 false）。

use crate::librime::{self, InprocReport};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 调试用：强制走进程内部署，完全跳过外部部署器与「输入法在跑」的判断。
pub const ENV_FORCE_INPROC: &str = "RIMESYNC_DEPLOY_FORCE_INPROC";
/// 调试用（B5）：允许在 fcitx5 运行时直接跑外部部署器，跳过 `fcitx5-remote -r` 重载。
///
/// 正常路径必须先重载再 build；这条开关只用于排障（会有与输入法抢 userdb/文件锁的风险）。
pub const ENV_UNSAFE_EXTERNAL: &str = "RIMESYNC_DEPLOY_UNSAFE_EXTERNAL";
/// 外部部署器（librime-bin 提供）。
pub const DEPLOYER: &str = "rime_deployer";
/// fcitx5 的远程控制命令：它存在且 fcitx5 在跑，就说明用户的输入法占着用户目录。
pub const RELOAD: &str = "fcitx5-remote";
/// 外部部署器最多等多久。
pub const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(120);
/// 进程内部署最多等多久（librime 可能卡在文件锁上）。
pub const INPROC_TIMEOUT: Duration = Duration::from_secs(120);
/// 跳过原因：fcitx5 正在运行。
pub const REASON_IME_RUNNING: &str = "ime-running";
/// 跳过原因：调试开关强制进程内。
pub const REASON_FORCED: &str = "forced-inproc";
/// 外部部署器成功。
pub const REASON_EXTERNAL_OK: &str = "external-ok";
/// 外部部署器失败（非 0 退出 / 跑不起来 / 超时）。
pub const REASON_EXTERNAL_FAILED: &str = "external-failed";
/// 进程内部署超时。
pub const REASON_INPROC_TIMEOUT: &str = "inproc-timeout";
/// deploy_method：外部部署器成功。
pub const METHOD_EXTERNAL_OK: &str = "external_ok";
/// deploy_method：外部部署器失败。
pub const METHOD_EXTERNAL_FAILED: &str = "external_failed";
/// deploy_method：有意跳过（输入法在跑）。
pub const METHOD_SKIPPED: &str = "skipped";
/// 轮询子进程退出的间隔。
const POLL: Duration = Duration::from_millis(100);

/// 决策所需的事实（不含任何副作用，单测直接构造）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// RIMESYNC_DEPLOY_FORCE_INPROC=1。
    pub force_inproc: bool,
    /// RIMESYNC_DEPLOY_UNSAFE_EXTERNAL=1（fcitx5 在跑也直接 build）。
    pub unsafe_external: bool,
    /// 找到了 rime_deployer。
    pub deployer: bool,
    /// fcitx5-remote 存在且 fcitx5 正在运行。
    pub ime_running: bool,
}

/// 决策结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// 直接跑外部部署器（输入法没在跑，或调试开关允许硬上）。
    External,
    /// 输入法在跑：先 `fcitx5-remote -r` 重载，成功后才跑外部部署器（B5）。
    ExternalAfterReload,
    /// 输入法在跑且没有部署器：跳过进程内，提示用户。
    Skipped,
    /// 进程内 dlopen。
    Inproc,
}

/// 纯函数决策：调试开关 > 外部部署器（必要时先重载）> 输入法在跑则跳过 > 进程内。
///
/// B5 的关键：**有部署器 + 输入法在跑** 不再直接 build（会和 fcitx5 抢 userdb/文件锁），
/// 而是先重载一次，重载成功才 build。`RIMESYNC_DEPLOY_UNSAFE_EXTERNAL=1` 才允许硬上。
pub fn choose(facts: Facts) -> Choice {
    if facts.force_inproc {
        return Choice::Inproc;
    }
    if facts.deployer {
        if facts.ime_running && !facts.unsafe_external {
            return Choice::ExternalAfterReload;
        }
        return Choice::External;
    }
    if facts.ime_running {
        return Choice::Skipped;
    }
    Choice::Inproc
}

/// 外部命令的结局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRun {
    pub code: i32,
    /// stdout + stderr 的最后一段（写进报告，排障用）。
    pub output: String,
}

/// 部署所需的外部事实与动作；抽成 trait 便于单测断言「没有真的执行外部命令」。
pub trait DeployEnv {
    fn which(&self, program: &str) -> Option<PathBuf>;
    fn ime_running(&self) -> bool;
    /// `fcitx5-remote -r`：重载输入法（不杀进程）。失败就不再跑部署器（B5）。
    fn reload(&self) -> Result<(), String>;
    fn run_external(
        &self,
        deployer: &Path,
        user_dir: &Path,
        shared_dir: &Path,
        timeout: Duration,
    ) -> Result<ExternalRun, String>;
    fn inproc(&self, user_dir: &Path, shared_dir: &Path, timeout: Duration) -> InprocReport;
}

/// 真实环境：PATH 探测 + 进程扫描 + 带超时的子进程/线程。
#[derive(Debug, Clone, Default)]
pub struct SystemEnv {
    /// deploy-ime --library：强制指定 librime 动态库（只影响进程内部署）。
    pub library: Option<PathBuf>,
}

impl SystemEnv {
    pub fn with_library(library: Option<PathBuf>) -> Self {
        Self { library }
    }
}

impl DeployEnv for SystemEnv {
    fn which(&self, program: &str) -> Option<PathBuf> {
        find_program(program)
    }

    fn ime_running(&self) -> bool {
        crate::platform::ime_running()
    }

    fn reload(&self) -> Result<(), String> {
        let Some(program) = find_program(RELOAD) else {
            return Err(format!("PATH 里没有 {RELOAD}"));
        };
        let output = crate::process::command(&program)
            .arg("-r")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("spawn {} -r: {error}", program.display()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} -r 退出 {:?}：{}",
                program.display(),
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn run_external(
        &self,
        deployer: &Path,
        user_dir: &Path,
        shared_dir: &Path,
        timeout: Duration,
    ) -> Result<ExternalRun, String> {
        run_with_timeout(deployer, user_dir, shared_dir, timeout)
    }

    fn inproc(&self, user_dir: &Path, shared_dir: &Path, timeout: Duration) -> InprocReport {
        let shared = shared_dir.to_path_buf();
        let user = user_dir.to_path_buf();
        let library = self.library.clone();
        let work = move || match library {
            Some(path) => {
                librime::deploy_with_library(&path, &librime::Traits::for_dirs(&shared, &user))
            }
            None => librime::deploy_with_dirs(&shared, &user),
        };
        match inproc_with_timeout(timeout, work) {
            InprocOutcome::Done(report) => report,
            InprocOutcome::TimedOut { waited_ms, late } => report(
                librime::METHOD_FAILED,
                false,
                user_dir,
                shared_dir,
                format!(
                    "reason={REASON_INPROC_TIMEOUT} 进程内部署超时（{} 秒），按失败处理；已等待后台线程自然结束（+{waited_ms} ms，late={}）。注意：超时后 librime 仍可能在后台继续写 user.yaml/installation.yaml，等它结束才退出；若结果与本次报告不一致，以引擎下次启动的部署为准",
                    timeout.as_secs(),
                    if late.is_some() { "done" } else { "none" }
                ),
            ),
            InprocOutcome::SpawnFailed(error) => report(
                librime::METHOD_FAILED,
                false,
                user_dir,
                shared_dir,
                format!("reason={REASON_INPROC_TIMEOUT} 无法启动部署线程：{error}"),
            ),
        }
    }
}

/// 按优先级跑一次部署，返回与 CLI deploy-ime 共用的报告。
pub fn deploy_with_env(
    user_dir: &Path,
    shared_dir: &Path,
    force_inproc: bool,
    unsafe_external: bool,
    env: &dyn DeployEnv,
) -> InprocReport {
    let started = Instant::now();
    let deployer = if force_inproc {
        None
    } else {
        env.which(DEPLOYER)
    };
    // 与 0.6.22 一致的判据：fcitx5-remote 存在 + fcitx5 在跑，才认为用户目录被占用。
    let ime_running = env.which(RELOAD).is_some() && env.ime_running();
    let choice = choose(Facts {
        force_inproc,
        unsafe_external,
        deployer: deployer.is_some(),
        ime_running,
    });
    let mut out = match choice {
        Choice::External | Choice::ExternalAfterReload => {
            let Some(path) = deployer else {
                // choose 已经把「没有部署器」排除掉了，这里只在逻辑被改坏时兜底。
                return report(
                    librime::METHOD_FAILED,
                    false,
                    user_dir,
                    shared_dir,
                    format!("reason={REASON_EXTERNAL_FAILED} 没有可用的 {DEPLOYER}"),
                );
            };
            // B5：输入法在跑时先重载（不杀进程）；重载失败就**不执行 build**。
            let mut reload_note = String::new();
            if choice == Choice::ExternalAfterReload {
                match env.reload() {
                    Ok(()) => reload_note = format!(" reload={RELOAD} -r ok"),
                    Err(error) => {
                        let mut out = report(
                            METHOD_SKIPPED,
                            false,
                            user_dir,
                            shared_dir,
                            format!(
                                "reason={REASON_IME_RUNNING} fcitx5 正在运行，{RELOAD} -r 重载失败（{error}）；未执行 {DEPLOYER} --build。请先退出输入法（{RELOAD} -e）后重试，或设 {ENV_UNSAFE_EXTERNAL}=1 强制外部部署"
                            ),
                        );
                        out.library = path.display().to_string();
                        out.deploy_ms = started.elapsed().as_millis() as u64;
                        return out;
                    }
                }
            }
            match env.run_external(&path, user_dir, shared_dir, EXTERNAL_TIMEOUT) {
                Ok(run) if run.code == 0 => report(
                    METHOD_EXTERNAL_OK,
                    true,
                    user_dir,
                    shared_dir,
                    format!(
                        "reason={REASON_EXTERNAL_OK}{reload_note} {} --build {} {} exit=0{}",
                        path.display(),
                        user_dir.display(),
                        shared_dir.display(),
                        suffix(&run.output)
                    ),
                ),
                Ok(run) => report(
                    METHOD_EXTERNAL_FAILED,
                    false,
                    user_dir,
                    shared_dir,
                    format!(
                        "reason={REASON_EXTERNAL_FAILED}{reload_note} {} --build {} {} exit={}{}",
                        path.display(),
                        user_dir.display(),
                        shared_dir.display(),
                        run.code,
                        suffix(&run.output)
                    ),
                ),
                Err(error) => report(
                    METHOD_EXTERNAL_FAILED,
                    false,
                    user_dir,
                    shared_dir,
                    format!("reason={REASON_EXTERNAL_FAILED}{reload_note} {error}"),
                ),
            }
        }
        Choice::Skipped => report(
            METHOD_SKIPPED,
            false,
            user_dir,
            shared_dir,
            format!(
                "reason={REASON_IME_RUNNING} fcitx5 正在运行（{RELOAD} 可用）：进程内部署会与输入法抢 userdb/文件锁，已跳过；请先退出输入法（{RELOAD} -e）后重试，或在托盘菜单点「重新部署」"
            ),
        ),
        Choice::Inproc => {
            let mut report = env.inproc(user_dir, shared_dir, INPROC_TIMEOUT);
            if force_inproc {
                report.detail = format!("reason={REASON_FORCED} {}", report.detail);
            }
            report
        }
    };
    out.deploy_ms = started.elapsed().as_millis() as u64;
    out
}

/// 真实环境入口：环境变量 + 系统探测。
pub fn deploy(user_dir: &Path, shared_dir: &Path, library: Option<&Path>) -> InprocReport {
    let env = SystemEnv::with_library(library.map(Path::to_path_buf));
    deploy_with_env(
        user_dir,
        shared_dir,
        force_inproc(),
        unsafe_external(),
        &env,
    )
}

/// RIMESYNC_DEPLOY_FORCE_INPROC=1 时强制进程内（调试用）。
pub fn force_inproc() -> bool {
    std::env::var(ENV_FORCE_INPROC)
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

/// RIMESYNC_DEPLOY_UNSAFE_EXTERNAL=1：fcitx5 在跑也直接 build（排障用，B5）。
pub fn unsafe_external() -> bool {
    std::env::var(ENV_UNSAFE_EXTERNAL)
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

/// 组一份报告（不含耗时：调用方补）。
fn report(
    method: &str,
    ok: bool,
    user_dir: &Path,
    shared_dir: &Path,
    detail: String,
) -> InprocReport {
    InprocReport {
        deploy_method: method.to_string(),
        deploy_ok: ok,
        deploy_ms: 0,
        library: String::new(),
        detail,
        shared_data_dir: shared_dir.display().to_string(),
        user_data_dir: user_dir.display().to_string(),
    }
}

/// 子进程输出只留最后 400 个字符，避免把报告撑爆。
fn suffix(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let text: String = trimmed
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    format!(" output={text}")
}

/// 找可执行程序：PATH，再补 /usr/bin、/usr/local/bin（spec 里点名的两个位置）。
pub fn find_program(program: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.push(PathBuf::from("/usr/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    find_in(&dirs, program)
}

/// 在给定目录里按顺序找可执行文件（单测与 find_program 共用）。
fn find_in(dirs: &[PathBuf], program: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// 跑外部部署器；超时就 kill **并 wait 回收**（不留僵尸），返回可读错误（绝不 panic）。
///
/// B7：输出重定向到临时文件，**不用 piped**。之前 piped 之后只在退出时读，子进程写满
/// 管道缓冲区（64 KiB）就会阻塞在 write 上、永远不退出，于是「输出多」被误判成
/// 「部署超时」。改成文件后子进程随便写，我们退出后再读尾部。
fn run_with_timeout(
    deployer: &Path,
    user_dir: &Path,
    shared_dir: &Path,
    timeout: Duration,
) -> Result<ExternalRun, String> {
    let log_path = temp_output_path();
    let log = std::fs::File::create(&log_path)
        .map_err(|error| format!("创建临时输出文件 {} 失败：{error}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|error| format!("复制临时输出文件句柄失败：{error}"))?;
    let mut child = crate::process::command(deployer)
        .arg("--build")
        .arg(user_dir)
        .arg(shared_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", deployer.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&log_path);
                return Err(format!("等待 {} 失败：{error}", deployer.display()));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            // kill 之后必须 wait 回收，否则子进程变成僵尸挂在我们名下。
            let _ = child.wait();
            let _ = std::fs::remove_file(&log_path);
            return Err(format!(
                "{} --build 超时（{} 秒），已结束并回收该进程",
                deployer.display(),
                timeout.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
    // try_wait 已经回收过；这里再 wait 一次拿缓存的状态（std 保证幂等）。
    let status = child
        .wait()
        .map_err(|error| format!("回收 {} 失败：{error}", deployer.display()))?;
    let output = read_tail(&log_path, 8192);
    let _ = std::fs::remove_file(&log_path);
    Ok(ExternalRun {
        code: status.code().unwrap_or(-1),
        output,
    })
}

/// 临时文件名：pid + 纳秒，避免并发部署互相覆盖。
fn temp_output_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "rime-sync-deploy-{}-{stamp}.log",
        std::process::id()
    ))
}

/// 只读文件尾部 max_bytes（部署器可能输出很多，没必要全读进内存）。
fn read_tail(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if len > max_bytes {
        let _ = file.seek(SeekFrom::Start(len - max_bytes));
    }
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    text
}

/// 进程内部署的超时结局。
#[derive(Debug)]
enum InprocOutcome {
    /// 线程在超时内跑完。
    Done(InprocReport),
    /// 超时：已经标记失败，并**等线程自然结束时**补上了它最终有没有跑完的证据。
    TimedOut {
        /// 超时后 join 等了多久。
        waited_ms: u64,
        /// 线程最终是否产出了报告（产出了也不用：结果按失败处理）。
        late: Option<InprocReport>,
    },
    /// 线程根本起不来（资源耗尽）：当失败处理，不 panic。
    SpawnFailed(String),
}

/// 进程内部署的超时包装：起线程 + recv_timeout + **超时后 join**。
///
/// B7：旧实现超时就直接返回，把线程丢在后台——librime 仍在写 user.yaml，
/// 而我们已经在打印「失败」；进程退出时线程被硬杀，可能留下写了一半的文件。
/// 现在超时后：① 仍然标记失败（deploy_method=inproc_failed）；② 等线程自然结束，
/// 把「实际又等了多久」写进日志；③ CLI 退出前线程一定已经 join（不会有后台线程
/// 比进程活得久）。librime 若真卡在文件锁上，这一步会一直等到它自己解开——所以
/// 日志里必须写明「超时后仍在后台完成」的风险，别让用户以为已经立刻放弃了。
fn inproc_with_timeout<F>(timeout: Duration, work: F) -> InprocOutcome
where
    F: FnOnce() -> InprocReport + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let handle = match std::thread::Builder::new()
        .name("rime-sync-inproc-deploy".to_string())
        .spawn(move || {
            let _ = sender.send(work());
        }) {
        Ok(handle) => handle,
        Err(error) => return InprocOutcome::SpawnFailed(error.to_string()),
    };
    match receiver.recv_timeout(timeout) {
        Ok(report) => InprocOutcome::Done(report),
        Err(_) => {
            let waited = Instant::now();
            let _ = handle.join();
            let waited_ms = waited.elapsed().as_millis() as u64;
            let late = receiver.try_recv().ok();
            InprocOutcome::TimedOut { waited_ms, late }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 假环境：只记录调用，绝不执行外部命令。
    struct FakeEnv {
        deployer: Option<PathBuf>,
        reload: bool,
        /// fcitx5-remote -r 是否成功（B5）。
        reload_ok: bool,
        ime: bool,
        exit: i32,
        calls: RefCell<Vec<String>>,
    }

    impl FakeEnv {
        fn new(deployer: bool, ime: bool) -> Self {
            Self {
                deployer: deployer.then(|| PathBuf::from("/usr/bin").join(DEPLOYER)),
                reload: true,
                reload_ok: true,
                ime,
                exit: 0,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_reload_ok(mut self, ok: bool) -> Self {
            self.reload_ok = ok;
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn build_ran(&self) -> bool {
            self.calls().iter().any(|call| call.starts_with("external"))
        }
    }

    impl DeployEnv for FakeEnv {
        fn which(&self, program: &str) -> Option<PathBuf> {
            match program {
                DEPLOYER => self.deployer.clone(),
                RELOAD if self.reload => Some(PathBuf::from("/usr/bin").join(RELOAD)),
                _ => None,
            }
        }

        fn ime_running(&self) -> bool {
            self.ime
        }

        fn reload(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("reload".to_string());
            if self.reload_ok {
                Ok(())
            } else {
                Err("fcitx5-remote -r exit=1".to_string())
            }
        }

        fn run_external(
            &self,
            _deployer: &Path,
            user_dir: &Path,
            shared_dir: &Path,
            _timeout: Duration,
        ) -> Result<ExternalRun, String> {
            self.calls.borrow_mut().push(format!(
                "external user={} shared={}",
                user_dir.display(),
                shared_dir.display()
            ));
            Ok(ExternalRun {
                code: self.exit,
                output: String::new(),
            })
        }

        fn inproc(&self, user_dir: &Path, shared_dir: &Path, _timeout: Duration) -> InprocReport {
            self.calls.borrow_mut().push("inproc".to_string());
            let mut report = report(
                librime::METHOD_OK,
                true,
                user_dir,
                shared_dir,
                "inproc ok".to_string(),
            );
            report.library = "fake-librime.so".to_string();
            report
        }
    }

    fn dirs() -> (PathBuf, PathBuf) {
        (
            PathBuf::from("/tmp/rime-user"),
            PathBuf::from("/usr/share/rime-data"),
        )
    }

    #[test]
    fn deployer_present_runs_external_and_never_dlopens() {
        // 输入法没在跑：直接跑部署器，不该出现 reload。
        let env = FakeEnv::new(true, false);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, METHOD_EXTERNAL_OK);
        assert!(report.deploy_ok);
        let calls = env.calls();
        assert_eq!(calls.len(), 1, "只该跑一次外部部署器：{calls:?}");
        assert!(calls[0].starts_with("external"), "calls={calls:?}");
        assert!(
            !calls.iter().any(|call| call == "inproc"),
            "有 rime_deployer 时绝不能 dlopen：{calls:?}"
        );
        assert!(
            report.detail.starts_with("reason=external-ok"),
            "{}",
            report.detail
        );
    }

    #[test]
    fn ime_running_without_deployer_skips_inproc() {
        let env = FakeEnv::new(false, true);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, METHOD_SKIPPED);
        assert!(!report.deploy_ok);
        assert!(
            report.detail.contains("reason=ime-running"),
            "detail={}",
            report.detail
        );
        assert!(
            env.calls().is_empty(),
            "跳过时不得执行任何命令：{:?}",
            env.calls()
        );
    }

    #[test]
    fn no_deployer_and_no_ime_runs_inproc() {
        let env = FakeEnv::new(false, false);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, librime::METHOD_OK);
        assert!(report.deploy_ok);
        assert_eq!(env.calls(), vec!["inproc".to_string()]);
        assert!(!report.detail.contains("forced-inproc"));
    }

    #[test]
    fn force_inproc_ignores_external_deployer() {
        let env = FakeEnv::new(true, true);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, true, false, &env);
        assert_eq!(report.deploy_method, librime::METHOD_OK);
        assert_eq!(env.calls(), vec!["inproc".to_string()]);
        assert!(
            report.detail.starts_with("reason=forced-inproc"),
            "{}",
            report.detail
        );
    }

    #[test]
    fn external_failure_is_reported_instead_of_falling_back() {
        let mut env = FakeEnv::new(true, false);
        env.exit = 2;
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, METHOD_EXTERNAL_FAILED);
        assert!(!report.deploy_ok);
        assert!(report.detail.contains("exit=2"), "{}", report.detail);
        assert!(!env.calls().iter().any(|call| call == "inproc"));
    }

    /// B5：输入法在跑 → 先 reload，**reload 成功之后**才 build（顺序也要对）。
    #[test]
    fn ime_running_reloads_before_the_external_build() {
        let env = FakeEnv::new(true, true);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, METHOD_EXTERNAL_OK);
        let calls = env.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0], "reload", "必须先重载：{calls:?}");
        assert!(calls[1].starts_with("external"), "{calls:?}");
        assert!(
            report.detail.contains("reload=fcitx5-remote -r ok"),
            "{}",
            report.detail
        );
    }

    /// B5：reload 失败 → 不执行 build，返回 skipped reason=ime-running。
    #[test]
    fn ime_running_with_broken_reload_skips_the_build() {
        let env = FakeEnv::new(true, true).with_reload_ok(false);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, METHOD_SKIPPED, "{}", report.detail);
        assert!(!report.deploy_ok);
        assert_eq!(
            env.calls(),
            vec!["reload".to_string()],
            "重载失败时绝不 build"
        );
        assert!(!env.build_ran());
        assert!(
            report.detail.contains("reason=ime-running"),
            "{}",
            report.detail
        );
        assert!(report.detail.contains("未执行"), "{}", report.detail);
    }

    /// 输入法没在跑：不重载，直接 build。
    #[test]
    fn idle_ime_skips_the_reload() {
        let env = FakeEnv::new(true, false).with_reload_ok(false);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, false, &env);
        assert_eq!(report.deploy_method, METHOD_EXTERNAL_OK);
        assert!(!env.calls().iter().any(|call| call == "reload"));
        assert!(env.build_ran());
    }

    /// RIMESYNC_DEPLOY_UNSAFE_EXTERNAL=1：输入法在跑也直接 build（不 reload）。
    #[test]
    fn unsafe_external_builds_while_the_ime_is_running() {
        let env = FakeEnv::new(true, true).with_reload_ok(false);
        let (user, shared) = dirs();
        let report = deploy_with_env(&user, &shared, false, true, &env);
        assert_eq!(report.deploy_method, METHOD_EXTERNAL_OK);
        assert!(!env.calls().iter().any(|call| call == "reload"));
        assert!(env.build_ran());
    }

    /// B7：超时后按失败处理，但线程必须已经 join（不会留后台线程）。
    #[test]
    fn inproc_timeout_is_a_failure_and_the_thread_is_joined() {
        let started = Instant::now();
        let outcome = inproc_with_timeout(Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_millis(200));
            report(
                librime::METHOD_OK,
                true,
                Path::new("/tmp/rime-user"),
                Path::new("/usr/share/rime-data"),
                "late".to_string(),
            )
        });
        match outcome {
            InprocOutcome::TimedOut { waited_ms, late } => {
                assert!(late.is_some(), "超时后 join 应拿到迟到结果");
                assert!(waited_ms >= 100, "join 真的等了线程：{waited_ms} ms");
            }
            other => panic!("超时必须返回 TimedOut，实际 {other:?}"),
        }
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "函数返回时线程必须已经结束"
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_timeout_kills_the_child() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rime-sync-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp dir");
        let script = root.join("slow_deployer");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\n").expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        let started = Instant::now();
        let error = run_with_timeout(
            &script,
            Path::new("/tmp/rime-user"),
            Path::new("/usr/share/rime-data"),
            Duration::from_millis(50),
        )
        .expect_err("超时必须返回错误");
        assert!(error.contains("超时"), "error={error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "超时后应立即返回：{:?}",
            started.elapsed()
        );
    }

    /// B7：子进程输出 200 KiB 不能因为管道写满被误判成超时。
    #[cfg(unix)]
    #[test]
    fn large_child_output_is_not_mistaken_for_a_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rime-sync-loud-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp dir");
        let script = root.join("loud_deployer");
        // 200 KiB 输出：远大于管道缓冲区（64 KiB），旧实现会卡死到超时。
        std::fs::write(
            &script,
            "#!/bin/sh\ni=0\nwhile [ $i -lt 200 ]; do \n  printf '%1024s' '' | tr ' ' 'x'\n  echo\n  i=$((i+1))\ndone\nexit 0\n",
        )
        .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        let run = run_with_timeout(
            &script,
            Path::new("/tmp/rime-user"),
            Path::new("/usr/share/rime-data"),
            Duration::from_secs(20),
        )
        .expect("输出多不是超时");
        assert_eq!(run.code, 0);
        assert!(!run.output.trim().is_empty(), "尾部输出必须被保留");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B7：超时后子进程被 kill 并且被 wait 回收（/proc 里不留僵尸）。
    #[cfg(target_os = "linux")]
    #[test]
    fn timed_out_child_is_killed_and_reaped() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("rime-sync-slowpid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp dir");
        let pid_file = root.join("pid");
        let script = root.join("slow_deployer");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho $$ > {}\nsleep 10\n", pid_file.display()),
        )
        .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        let error = run_with_timeout(
            &script,
            Path::new("/tmp/rime-user"),
            Path::new("/usr/share/rime-data"),
            Duration::from_millis(300),
        )
        .expect_err("必须超时");
        assert!(error.contains("超时"), "error={error}");
        let pid = std::fs::read_to_string(&pid_file)
            .expect("子进程应写下了自己的 pid")
            .trim()
            .to_string();
        // 回收之后 /proc/<pid> 必须消失（僵尸会留在 /proc 里直到被 wait）。
        let proc_dir = PathBuf::from(format!("/proc/{pid}"));
        assert!(
            !proc_dir.exists(),
            "超时的子进程必须被 kill + wait 回收，/proc/{pid} 仍然存在"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn find_in_skips_non_executable_files() {
        let root = std::env::temp_dir().join(format!("rime-sync-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create temp dir");
        let plain = root.join("rime_deployer");
        std::fs::write(&plain, "#!/bin/sh\n").expect("write script");
        assert!(find_in(std::slice::from_ref(&root), DEPLOYER).is_none());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        assert_eq!(find_in(std::slice::from_ref(&root), DEPLOYER), Some(plain));
        let _ = std::fs::remove_dir_all(&root);
    }
}
