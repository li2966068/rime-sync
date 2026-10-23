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

/// 等 fcitx5 退出的上限与轮询间隔：必须先让它放掉 LevelDB 锁。
#[cfg(target_os = "linux")]
const IME_EXIT_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(target_os = "linux")]
const IME_EXIT_POLL: Duration = Duration::from_millis(100);

/// 扫 /proc/<pid>/comm，返回仍在运行的 fcitx5 进程。
#[cfg(target_os = "linux")]
fn ime_pids(name: &str) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
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
        if comm.trim() == name {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

/// 轮询等进程退出；超时返回 false（由调用方转成明确错误）。
#[cfg(target_os = "linux")]
fn wait_for_ime_exit(name: &str, timeout: Duration, poll: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let pids = ime_pids(name);
        if pids.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                "{name} still running after {}ms (pids={pids:?})",
                timeout.as_millis()
            );
            return false;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(target_os = "linux")]
impl RimeTrigger for LinuxTrigger {
    fn sync_userdb(&self, rime_dir: &Path) -> Result<()> {
        self.require(&self.dict_manager)?;
        let running = ime_pids(IME_PROCESS);
        if !running.is_empty() {
            bail!(
                "fcitx5 is still running (pids={running:?}); rime_dict_manager needs exclusive access to the userdb LevelDB"
            );
        }
        let rime_dir = rime_dir.to_str().context("rime_dir is not valid UTF-8")?;
        run(
            &self.dict_manager,
            &["--sync"],
            &[("RIME_USER_DATA_DIR", rime_dir)],
        )
        .context("rime_dict_manager --sync failed")
    }

    fn export_userdb(&self, rime_dir: &Path, schema: &str, dest: &Path) -> Result<()> {
        self.require(&self.dict_manager)?;
        let running = ime_pids(IME_PROCESS);
        if !running.is_empty() {
            bail!(
                "fcitx5 is still running (pids={running:?}); cannot export the userdb while LevelDB is locked"
            );
        }
        let rime_dir = rime_dir.to_str().context("rime_dir is not valid UTF-8")?;
        let dest = dest.to_str().context("export path is not valid UTF-8")?;
        run(
            &self.dict_manager,
            &["-e", schema, dest],
            &[("RIME_USER_DATA_DIR", rime_dir)],
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
        let pids = ime_pids(IME_PROCESS);
        if pids.is_empty() {
            tracing::debug!("fcitx5 is not running; nothing to stop");
            return Ok(());
        }
        tracing::info!("stopping fcitx5 (pids={pids:?}) to release the userdb LevelDB lock");
        if command_exists("fcitx5-remote") {
            // 尽力而为：fcitx5 未运行时返回非零，不应让 apply 失败。
            let _ = command("fcitx5-remote").arg("-e").output();
        } else if command_exists("pkill") {
            let _ = command("pkill").args(["-TERM", "-x", IME_PROCESS]).output();
        }
        if wait_for_ime_exit(IME_PROCESS, IME_EXIT_TIMEOUT, IME_EXIT_POLL) {
            tracing::info!("fcitx5 exited");
            return Ok(());
        }
        bail!(
            "fcitx5 still running after {}s; refusing to run rime_dict_manager while the userdb LevelDB is locked",
            IME_EXIT_TIMEOUT.as_secs()
        )
    }

    fn start_ime(&self) -> Result<()> {
        if !self.stop_fcitx5 {
            return Ok(());
        }
        if command_exists("fcitx5") {
            // fcitx5 会 fork 守护进程并继承管道；用 null stdio + spawn 避免 output() 永久等待。
            let _ = command("fcitx5")
                .args(["-d", "--replace"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        Ok(())
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

    fn deploy(&self, _rime_dir: &Path) -> Result<()> {
        self.run_switch("/deploy")
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
