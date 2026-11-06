//! 输入法健康检查 + 随包 Weasel 安装器自举（0.6.20）。
//!
//! 227 实测：0.6.19 用 /S 静默安装时 Tauri 的 NSIS 钩子没执行（静默安装下不跑），
//! 旧 Weasel 已被卸载器卸掉，结果机器上没有任何输入法。只靠安装器钩子兜底是不够的：
//! 程序自己必须在启动时（和用户点「修复输入法」时）判断输入法在不在，不在就用随包
//! 安装器装回去。
//!
//! 判定（与 NSIS 钩子的 RimeSyncWeaselReady 同一判据）：
//! 注册表 WeaselRoot/InstallDir 有值 → 目录里真有 WeaselServer.exe（直接在根目录，
//! 或在 weasel-* 版本目录里）→ 同目录的 rime.dll 里能搜到 rimesync_vnum。
//! 任一条不满足都算「输入法缺失 / 不是我们的构建」，触发静默安装。
//!
//! 纪律：安装器参数固定 /S /NOREBOOT（绝不重启机器），最多等 300 秒（与 NSIS 钩子
//! 一致）；需要管理员权限时返回可读错误 + needs_elevation=true，绝不静默失败。

use std::env;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::thread;
#[cfg(any(windows, test))]
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use serde::Serialize;

use crate::data_patch::{self, WeaselRegistryValues};

/// 随包安装器的文件名（与 packaging/windows/vendor/ 及 NSIS 钩子一致）。
pub const INSTALLER_FILE: &str = "weasel-0.17.4.0-installer.exe";
/// 自动安装开关：0 = 只检查不安装。
pub const ENV_INSTALL_IME: &str = "RIMESYNC_INSTALL_IME";
/// 静默安装参数：/S 静默 + /NOREBOOT 绝不允许安装器重启机器。
pub const INSTALLER_ARGS: [&str; 2] = ["/S", "/NOREBOOT"];
/// 等待安装器的最长时间（秒），与 NSIS 钩子的 RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS 一致。
pub const INSTALL_TIMEOUT_SECS: u64 = 300;
/// 输入法服务端可执行文件。
pub const SERVER_EXE: &str = "WeaselServer.exe";
/// 输入法引擎 DLL。
pub const RIME_DLL: &str = "rime.dll";
/// NSIS 安装版写在 exe 同级的卸载器（便携判据用）。
pub const UNINSTALL_FILE: &str = "uninstall.exe";
/// 我们自己的 librime 插件标记（插件合进 rime.dll 后是 ASCII 明文）。
pub const PLUGIN_MARKER: &str = "rimesync_vnum";

/// 安装器退出码：拒绝访问 / 需要重启 / UAC 被拒。只在 Windows 与单测里用。
#[cfg(any(windows, test))]
const EXIT_ACCESS_DENIED: i32 = 5;
#[cfg(any(windows, test))]
const EXIT_REBOOT_REQUIRED: i32 = 3010;
#[cfg(any(windows, test))]
const EXIT_CANCELLED: i32 = 1223;

/// 一次输入法健康检查的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WeaselHealth {
    /// 是否就绪（服务端可执行文件 + 带插件标记的 rime.dll 都在）。
    pub present: bool,
    /// 命中目录的版本号（weasel-0.17.4 → "0.17.4"）；根目录直接命中时为空。
    pub version: String,
    /// 检查的程序目录。
    pub root: String,
    /// 命中的 WeaselServer.exe。
    pub server_exe: String,
    /// 同目录的 rime.dll。
    pub rime_dll: String,
    /// rime.dll 里是否有 rimesync_vnum。
    pub plugin: bool,
    /// exe-present / registry-missing / root-missing / server-missing /
    /// rime-dll-missing / plugin-missing / rime-dll-unreadable。
    pub reason: String,
}

/// 按注册表值判定：WeaselRoot 优先，其次 InstallDir；都缺就是 registry-missing。
pub fn check_values(registry: &WeaselRegistryValues) -> WeaselHealth {
    let mut roots: Vec<&PathBuf> = Vec::new();
    if let Some(root) = &registry.weasel_root {
        roots.push(root);
    }
    if let Some(dir) = &registry.install_dir {
        roots.push(dir);
    }
    if roots.is_empty() {
        return WeaselHealth {
            reason: "registry-missing".to_string(),
            ..WeaselHealth::default()
        };
    }
    let mut last = WeaselHealth::default();
    for root in roots {
        let health = check_root(root);
        if health.present {
            return health;
        }
        last = health;
    }
    last
}

/// 检查一个程序目录（<root>\WeaselServer.exe 或 <root>\weasel-*\WeaselServer.exe）。
pub fn check_root(root: &Path) -> WeaselHealth {
    let mut health = WeaselHealth {
        root: root.display().to_string(),
        reason: "root-missing".to_string(),
        ..WeaselHealth::default()
    };
    if !root.is_dir() {
        return health;
    }
    health.reason = "server-missing".to_string();
    let Some(server) = find_server(root) else {
        return health;
    };
    health.server_exe = server.display().to_string();
    health.version = version_of(&server);
    let Some(parent) = server.parent() else {
        return health;
    };
    let dll = parent.join(RIME_DLL);
    if !dll.is_file() {
        health.reason = "rime-dll-missing".to_string();
        return health;
    }
    health.rime_dll = dll.display().to_string();
    match file_contains(&dll, PLUGIN_MARKER.as_bytes()) {
        Ok(true) => {
            health.plugin = true;
            health.present = true;
            health.reason = "exe-present".to_string();
        }
        Ok(false) => health.reason = "plugin-missing".to_string(),
        Err(_) => health.reason = "rime-dll-unreadable".to_string(),
    }
    health
}

/// <root>\WeaselServer.exe 优先；没有再扫 weasel-*\WeaselServer.exe（版本高的先用）。
fn find_server(root: &Path) -> Option<PathBuf> {
    let direct = root.join(SERVER_EXE);
    if direct.is_file() {
        return Some(direct);
    }
    for dir in data_patch::weasel_version_dirs(root) {
        let candidate = dir.join(SERVER_EXE);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 从 WeaselServer.exe 的父目录名取版本号（weasel-0.17.4 → 0.17.4）。
fn version_of(server: &Path) -> String {
    let name = server
        .parent()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if data_patch::weasel_version(&name).is_some() {
        name.trim_start_matches("weasel-").to_string()
    } else {
        String::new()
    }
}

/// 分块搜索文件内容（rime.dll 一二十 MB，不整块读进内存）。
fn file_contains(path: &Path, needle: &[u8]) -> std::io::Result<bool> {
    if needle.is_empty() {
        return Ok(true);
    }
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(false);
        }
        let mut window = Vec::with_capacity(carry.len() + read);
        window.extend_from_slice(&carry);
        window.extend_from_slice(&buffer[..read]);
        if window.windows(needle.len()).any(|chunk| chunk == needle) {
            return Ok(true);
        }
        let keep = needle.len().saturating_sub(1).min(window.len());
        carry = window[window.len() - keep..].to_vec();
    }
}

/// 自动安装开关（RIMESYNC_INSTALL_IME=0 关闭）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    Auto,
    Disabled,
}

/// 读环境变量决定是否允许自动安装。
pub fn install_mode() -> InstallMode {
    match env::var(ENV_INSTALL_IME) {
        Ok(value) if value.trim() == "0" => InstallMode::Disabled,
        _ => InstallMode::Auto,
    }
}

/// 安装器执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallExit {
    /// None = 超时被杀（没有退出码）。
    pub code: Option<i32>,
    pub timed_out: bool,
}

/// 安装器执行失败：连进程都起不来，或权限不足。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// 需要管理员权限（spawn 740 / 退出码 5 / UAC 被拒 1223）。
    NeedsElevation(String),
    Other(String),
}

/// 一次 fix-ime 的输入法报告。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImeReport {
    /// windows / linux / macos …
    pub platform: String,
    /// 检查（安装后复查）是否就绪。
    pub present: bool,
    /// 本次是否真的把输入法装上（安装器成功 + 复查通过）。
    pub installed: bool,
    pub version: String,
    /// 随包安装器路径；空 = 没找到。
    pub installer: String,
    /// 安装器来源（exe:resources/weasel / dev:…）。
    pub installer_source: String,
    /// installed / already-present / skipped(platform) / disabled / failed。
    pub install: String,
    pub error: Option<String>,
    /// 是否需要提示用户以管理员身份重启。
    pub needs_elevation: bool,
    /// 多行过程日志（bootstrap.log / 设置页排障用）。
    pub log: String,
}

impl ImeReport {
    /// 是否算「输入法这块没问题」（用户显式关掉自动安装也算）。
    pub fn ok(&self) -> bool {
        matches!(
            self.install.as_str(),
            "installed" | "already-present" | "skipped(platform)" | "disabled"
        )
    }

    /// 一句话结论（界面 / 日志共用）。
    pub fn summary(&self) -> String {
        let version = if self.version.is_empty() {
            "版本未知".to_string()
        } else {
            format!("Weasel {}", self.version)
        };
        match self.install.as_str() {
            "already-present" => format!("输入法已就绪（{version}）"),
            "installed" => format!("已用随包安装器装回输入法（{version}）"),
            "skipped(platform)" => "输入法安装：跳过（Linux 由包管理器负责）".to_string(),
            "disabled" => format!("输入法未就绪，自动安装已关闭（{ENV_INSTALL_IME}=0）"),
            _ => format!(
                "输入法未就绪且自动安装失败：{}",
                self.error.as_deref().unwrap_or("未知原因")
            ),
        }
    }
}

/// 便携版判据（只用来选提示文案，不影响任何真实逻辑）。
///
/// 便携 = 下面三样都没有：
/// 1. exe 同级的 resources/（Windows 安装版由 Tauri / NSIS 铺在这里）；
/// 2. ../lib/rime-sync/resources（Linux deb / AppImage 的安装布局）；
/// 3. exe 同级的 uninstall.exe（NSIS 安装版一定会写卸载器）。
///
/// 0.6.22 起便携 zip 也会带 resources/prebuilt 与 resources/weasel；资源被手工删掉
/// 的便携版同样要判成便携，所以这里只做「像不像安装版」的弱判据。
pub fn looks_portable(exe_dir: Option<&Path>) -> bool {
    let Some(dir) = exe_dir else { return false };
    if dir.join("resources").is_dir() || dir.join(UNINSTALL_FILE).is_file() {
        return false;
    }
    if let Some(prefix) = dir.parent() {
        if prefix
            .join("lib")
            .join("rime-sync")
            .join("resources")
            .is_dir()
        {
            return false;
        }
    }
    true
}

/// 随包安装器的候选路径（按优先级；单测据此断言「安装器缺失」的错误文案）。
pub fn installer_candidates(exe_dir: Option<&Path>) -> Vec<(PathBuf, String)> {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    if let Some(dir) = exe_dir {
        // Windows NSIS：<安装目录>/resources/weasel；Tauri 在 Linux 是 ../lib/rime-sync/…
        candidates.push((
            dir.join("resources").join("weasel").join(INSTALLER_FILE),
            "exe:resources/weasel".to_string(),
        ));
        if let Some(prefix) = dir.parent() {
            candidates.push((
                prefix
                    .join("lib")
                    .join("rime-sync")
                    .join("resources")
                    .join("weasel")
                    .join(INSTALLER_FILE),
                "exe:../lib/rime-sync/resources/weasel".to_string(),
            ));
        }
    }
    // 开发/本地构建：仓库里的 packaging/windows/vendor（fetch-weasel.ps1 的产物，
    // 也是 NSIS 钩子内嵌的同一个文件）。单测里排除它：否则「安装器缺失」的错误
    // 路径会随仓库里有没有这个文件而变。
    #[cfg(all(debug_assertions, not(test)))]
    {
        candidates.push((
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("packaging")
                .join("windows")
                .join("vendor")
                .join(INSTALLER_FILE),
            "dev:packaging/windows/vendor".to_string(),
        ));
    }
    candidates
}

/// 找到随包安装器（返回路径 + 来源）。
pub fn resolve_installer(exe_dir: Option<&Path>) -> Option<(PathBuf, String)> {
    installer_candidates(exe_dir)
        .into_iter()
        .find(|(path, _)| path.is_file())
}

/// 安装器命令行（参数固定，单测直接断言 get_args）。
pub fn installer_command(installer: &Path) -> Command {
    let mut command = Command::new(installer);
    command.args(INSTALLER_ARGS);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    command
}

/// 真正启动安装器并等待（唯一的进程执行点；单测不调用它）。
#[cfg(windows)]
fn run_installer(
    installer: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<InstallExit, InstallError> {
    let mut command = Command::new(installer);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| classify_spawn_error(installer, &error))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(InstallExit {
                    code: status.code(),
                    timed_out: false,
                })
            }
            Ok(None) => {}
            Err(error) => {
                return Err(InstallError::Other(format!(
                    "等待安装器 {} 失败：{error}",
                    installer.display()
                )))
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(InstallExit {
                code: None,
                timed_out: true,
            });
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// 740 = ERROR_ELEVATION_REQUIRED：安装器带 admin manifest，非管理员进程直接起不来。
#[cfg(windows)]
fn classify_spawn_error(installer: &Path, error: &std::io::Error) -> InstallError {
    if error.raw_os_error() == Some(740) || error.kind() == std::io::ErrorKind::PermissionDenied {
        return InstallError::NeedsElevation(elevation_hint(installer));
    }
    InstallError::Other(format!("启动安装器 {} 失败：{error}", installer.display()))
}

#[cfg(any(windows, test))]
fn elevation_hint(installer: &Path) -> String {
    format!(
        "安装输入法需要管理员权限（安装器退出码/UAC 表示权限不足或被拒绝）。请以管理员身份重新启动 rime-sync 后再点「修复输入法」，或手动运行 \"{}\" {} {}",
        installer.display(),
        INSTALLER_ARGS[0],
        INSTALLER_ARGS[1]
    )
}

/// 平台分派：Windows 才检查/安装 Weasel；Linux 由包管理器负责，明确跳过。
#[cfg(windows)]
pub fn ensure(exe_dir: Option<&Path>) -> ImeReport {
    ensure_impl(
        exe_dir,
        env::consts::OS,
        install_mode(),
        &check_installed,
        &run_installer,
    )
}

#[cfg(not(windows))]
pub fn ensure(_exe_dir: Option<&Path>) -> ImeReport {
    ImeReport {
        platform: env::consts::OS.to_string(),
        install: "skipped(platform)".to_string(),
        log:
            "skip: Linux 输入法由包管理器（rime-inputmethod / fcitx5-rime）负责，不跑 Weasel 安装器"
                .to_string(),
        ..ImeReport::default()
    }
}

/// Windows 的检查入口：从注册表拿 WeaselRoot/InstallDir。
#[cfg(windows)]
pub fn check_installed() -> WeaselHealth {
    check_values(&data_patch::weasel_registry())
}

/// 健康检查闭包（可注入，单测据此走完 Windows 分支）。
#[cfg(any(windows, test))]
type HealthFn<'a> = &'a dyn Fn() -> WeaselHealth;
/// 安装器执行闭包（单测用它验证参数，不会真的起进程）。
#[cfg(any(windows, test))]
type RunnerFn<'a> = &'a dyn Fn(&Path, &[&str], Duration) -> Result<InstallExit, InstallError>;

/// 检查 → 必要时安装 → 复查。health/runner 可注入，单测据此走完 Windows 分支。
#[cfg(any(windows, test))]
fn ensure_impl(
    exe_dir: Option<&Path>,
    platform: &str,
    mode: InstallMode,
    health: HealthFn<'_>,
    runner: RunnerFn<'_>,
) -> ImeReport {
    let mut log: Vec<String> = Vec::new();
    let before = health();
    let mut report = ImeReport {
        platform: platform.to_string(),
        present: before.present,
        version: before.version.clone(),
        ..ImeReport::default()
    };
    log.push(format!(
        "check present={} reason={} root={} server={} dll={} plugin={}",
        before.present,
        before.reason,
        before.root,
        before.server_exe,
        before.rime_dll,
        before.plugin
    ));

    if before.present {
        report.install = "already-present".to_string();
        report.log = log.join("\n");
        return report;
    }

    if mode == InstallMode::Disabled {
        report.install = "disabled".to_string();
        report.error = Some(format!(
            "输入法未就绪（{}），已由 {ENV_INSTALL_IME}=0 关闭自动安装",
            before.reason
        ));
        log.push("install skipped: env-disabled".to_string());
        report.log = log.join("\n");
        return report;
    }

    let Some((installer, source)) = resolve_installer(exe_dir) else {
        let probed: Vec<String> = installer_candidates(exe_dir)
            .iter()
            .map(|(path, source)| format!("{}（{source}）", path.display()))
            .collect();
        report.install = "failed".to_string();
        let mut message = format!(
            "输入法未就绪（{}），且没有随包安装器 {INSTALLER_FILE}；已探测：{}",
            before.reason,
            probed.join("；")
        );
        if looks_portable(exe_dir) {
            message.push_str(
                "；检测到便携版（exe 同级没有 resources/）：便携版不内置输入法安装器，请手动安装小狼毫（Weasel）后再重试，或改用 NSIS 安装版",
            );
        }
        report.error = Some(message);
        log.push("installer missing".to_string());
        report.log = log.join("\n");
        return report;
    };
    report.installer = installer.display().to_string();
    report.installer_source = source.clone();
    log.push(format!(
        "installer path={} source={source} args={} timeout={INSTALL_TIMEOUT_SECS}s",
        installer.display(),
        INSTALLER_ARGS.join(" ")
    ));

    match runner(
        &installer,
        &INSTALLER_ARGS,
        Duration::from_secs(INSTALL_TIMEOUT_SECS),
    ) {
        Ok(exit) if exit.timed_out => {
            report.install = "failed".to_string();
            report.error = Some(format!(
                "输入法安装器 {INSTALL_TIMEOUT_SECS} 秒未结束，已结束进程；请以管理员身份重试或手动运行 \"{}\" {} {}",
                installer.display(),
                INSTALLER_ARGS[0],
                INSTALLER_ARGS[1]
            ));
            log.push("installer timed out".to_string());
        }
        Ok(exit) => {
            let code = exit.code.unwrap_or(-1);
            log.push(format!("installer exit={code}"));
            if code == 0 || code == EXIT_REBOOT_REQUIRED {
                let after = health();
                report.present = after.present;
                report.version = after.version.clone();
                log.push(format!(
                    "recheck present={} reason={} version={}",
                    after.present, after.reason, after.version
                ));
                if after.present {
                    report.install = "installed".to_string();
                    report.installed = true;
                } else {
                    report.install = "failed".to_string();
                    report.error = Some(format!(
                        "安装器退出码 {code}，但复查仍不就绪（{}）：{}",
                        after.reason,
                        elevation_hint(&installer)
                    ));
                }
            } else if code == EXIT_ACCESS_DENIED || code == EXIT_CANCELLED {
                report.install = "failed".to_string();
                report.needs_elevation = true;
                report.error = Some(format!(
                    "安装器退出码 {code}：{}",
                    elevation_hint(&installer)
                ));
                log.push("installer needs elevation".to_string());
            } else {
                report.install = "failed".to_string();
                report.error = Some(format!(
                    "安装器退出码 {code}（见 %TEMP% 下的输入法安装日志）：请以管理员身份重试或联系维护者"
                ));
            }
        }
        Err(InstallError::NeedsElevation(message)) => {
            report.install = "failed".to_string();
            report.needs_elevation = true;
            report.error = Some(message);
            log.push("installer needs elevation".to_string());
        }
        Err(InstallError::Other(message)) => {
            report.install = "failed".to_string();
            report.error = Some(message);
        }
    }
    report.log = log.join("\n");
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path =
                env::temp_dir().join(format!("rime-sync-ime-{}-{tag}-{seq}", std::process::id()));
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

    /// 伪造 <root>\weasel-<version>\WeaselServer.exe（+ 可选带插件标记的 rime.dll）。
    fn fake_weasel(root: &Path, version: &str, marker: bool) -> PathBuf {
        let dir = root.join(format!("weasel-{version}"));
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join(SERVER_EXE), b"MZ fake server").expect("write server");
        let dll: &[u8] = if marker {
            b"MZ .... rimesync_vnum / rimesync_punct ...."
        } else {
            b"MZ stock librime without our components"
        };
        fs::write(dir.join(RIME_DLL), dll).expect("write dll");
        dir
    }

    fn missing_health() -> WeaselHealth {
        WeaselHealth {
            present: false,
            reason: "registry-missing".to_string(),
            ..WeaselHealth::default()
        }
    }

    #[test]
    fn health_detects_version_dir() {
        let tmp = TempDir::new("version-dir");
        let version_dir = fake_weasel(tmp.path(), "9.9.9", true);
        let health = check_root(tmp.path());

        assert!(health.present, "{health:?}");
        assert_eq!(health.version, "9.9.9");
        assert_eq!(health.reason, "exe-present");
        assert!(health.plugin);
        assert_eq!(
            health.server_exe,
            version_dir.join(SERVER_EXE).display().to_string()
        );
        assert_eq!(
            health.rime_dll,
            version_dir.join(RIME_DLL).display().to_string()
        );
    }

    #[test]
    fn health_detects_root_level_install() {
        let tmp = TempDir::new("root-level");
        fs::write(tmp.path().join(SERVER_EXE), b"MZ server").expect("write server");
        fs::write(tmp.path().join(RIME_DLL), b"MZ rimesync_vnum").expect("write dll");

        let health = check_root(tmp.path());

        assert!(health.present, "{health:?}");
        assert_eq!(health.version, "", "根目录直接命中时没有版本号");
        assert_eq!(health.reason, "exe-present");
    }

    #[test]
    fn health_reports_missing_server() {
        let tmp = TempDir::new("no-server");
        let health = check_root(tmp.path());

        assert!(!health.present);
        assert_eq!(health.reason, "server-missing");
        assert!(health.server_exe.is_empty());
    }

    #[test]
    fn health_reports_missing_plugin() {
        let tmp = TempDir::new("no-plugin");
        fake_weasel(tmp.path(), "9.9.9", false);
        let health = check_root(tmp.path());

        assert!(!health.present, "没有 rimesync_vnum 就不是我们的构建");
        assert_eq!(health.reason, "plugin-missing");
        assert!(!health.plugin);
        assert_eq!(health.version, "9.9.9");
    }

    #[test]
    fn health_reports_registry_missing() {
        let health = check_values(&WeaselRegistryValues::default());
        assert!(!health.present);
        assert_eq!(health.reason, "registry-missing");
    }

    #[test]
    fn health_uses_install_dir_when_root_is_stale() {
        let tmp = TempDir::new("install-dir");
        let version_dir = fake_weasel(&tmp.path().join("Rime"), "9.9.9", true);
        let registry = WeaselRegistryValues {
            weasel_root: Some(tmp.path().join("Rime-removed")),
            install_dir: Some(version_dir),
        };

        let health = check_values(&registry);

        assert!(health.present, "{health:?}");
        assert_eq!(health.version, "9.9.9");
    }

    #[test]
    fn installer_command_is_silent_and_never_reboots() {
        let installer = Path::new(r"C:\Program Files\Rime\weasel-0.17.4.0-installer.exe");
        let command = installer_command(installer);

        assert_eq!(
            command.get_program().to_string_lossy(),
            installer.display().to_string()
        );
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["/S", "/NOREBOOT"]);
    }

    #[test]
    fn installer_missing_is_a_readable_error() {
        let tmp = TempDir::new("no-installer");
        let health = missing_health;
        let runner = |_path: &Path, _args: &[&str], _timeout: Duration| {
            panic!("安装器缺失时不该执行任何进程")
        };

        let report = ensure_impl(
            Some(tmp.path()),
            "windows",
            InstallMode::Auto,
            &health,
            &runner,
        );

        assert_eq!(report.install, "failed");
        assert!(!report.installed);
        assert!(!report.needs_elevation);
        let error = report.error.expect("必须有可读错误");
        assert!(error.contains("没有随包安装器"), "{error}");
        assert!(error.contains(INSTALLER_FILE), "{error}");
        assert!(
            error.contains("resources/weasel"),
            "错误里要带探测过的路径：{error}"
        );
        assert!(
            error.contains("便携版") && error.contains("手动安装"),
            "便携版要给出明确的手动安装提示：{error}"
        );
    }

    #[test]
    fn install_uses_silent_args_and_rechecks() {
        let tmp = TempDir::new("install-ok");
        let exe_dir = tmp.path().join("install");
        let resource_dir = exe_dir.join("resources").join("weasel");
        fs::create_dir_all(&resource_dir).expect("mkdir");
        let installer = resource_dir.join(INSTALLER_FILE);
        fs::write(&installer, b"MZ fake installer").expect("write installer");

        let calls = Cell::new(0usize);
        let health = || {
            let seen = calls.get();
            calls.set(seen + 1);
            if seen == 0 {
                missing_health()
            } else {
                WeaselHealth {
                    present: true,
                    version: "0.17.4".to_string(),
                    reason: "exe-present".to_string(),
                    ..WeaselHealth::default()
                }
            }
        };
        let runs = RefCell::new(Vec::new());
        let runner = |path: &Path, args: &[&str], timeout: Duration| {
            runs.borrow_mut()
                .push((path.to_path_buf(), args.join(" "), timeout.as_secs()));
            Ok(InstallExit {
                code: Some(0),
                timed_out: false,
            })
        };

        let report = ensure_impl(
            Some(&exe_dir),
            "windows",
            InstallMode::Auto,
            &health,
            &runner,
        );

        assert_eq!(report.install, "installed", "{report:?}");
        assert!(report.installed);
        assert!(report.present);
        assert_eq!(report.version, "0.17.4");
        assert!(!report.needs_elevation);
        assert!(report.error.is_none());
        assert_eq!(report.installer, installer.display().to_string());
        assert_eq!(report.installer_source, "exe:resources/weasel");

        let runs = runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, installer);
        assert_eq!(runs[0].1, "/S /NOREBOOT");
        assert_eq!(runs[0].2, INSTALL_TIMEOUT_SECS);
    }

    #[test]
    fn install_reports_needs_elevation() {
        let tmp = TempDir::new("elevation");
        let resource_dir = tmp.path().join("resources").join("weasel");
        fs::create_dir_all(&resource_dir).expect("mkdir");
        fs::write(resource_dir.join(INSTALLER_FILE), b"MZ fake").expect("write installer");

        let health = missing_health;
        let runner = |_path: &Path, _args: &[&str], _timeout: Duration| {
            Err(InstallError::NeedsElevation(
                "安装输入法需要管理员权限".to_string(),
            ))
        };

        let report = ensure_impl(
            Some(tmp.path()),
            "windows",
            InstallMode::Auto,
            &health,
            &runner,
        );

        assert_eq!(report.install, "failed");
        assert!(report.needs_elevation);
        assert!(report.error.expect("错误").contains("管理员"));
    }

    #[test]
    fn install_skips_when_already_present() {
        let health = || WeaselHealth {
            present: true,
            version: "0.17.4".to_string(),
            reason: "exe-present".to_string(),
            ..WeaselHealth::default()
        };
        let runner =
            |_path: &Path, _args: &[&str], _timeout: Duration| panic!("已就绪时不该跑安装器");

        let report = ensure_impl(None, "windows", InstallMode::Auto, &health, &runner);

        assert_eq!(report.install, "already-present");
        assert!(!report.installed);
        assert!(report.present);
        assert_eq!(report.version, "0.17.4");
    }

    #[test]
    fn install_respects_env_switch() {
        let health = missing_health;
        let runner = |_path: &Path, _args: &[&str], _timeout: Duration| {
            panic!("RIMESYNC_INSTALL_IME=0 时不该跑安装器")
        };

        let report = ensure_impl(None, "windows", InstallMode::Disabled, &health, &runner);

        assert_eq!(report.install, "disabled");
        assert!(!report.installed);
        assert!(report.ok(), "用户显式关掉自动安装不算失败");
        assert!(report.error.expect("错误").contains(ENV_INSTALL_IME));
    }

    /// 便携判据（只影响提示文案）：三种真实布局都要判对。
    #[test]
    fn portable_layout_detection() {
        let tmp = TempDir::new("portable");
        // 1. Windows 安装版：exe 同级有 resources/。
        let installed = tmp.path().join("program-files");
        fs::create_dir_all(installed.join("resources")).expect("mkdir");
        assert!(!looks_portable(Some(&installed)));

        // 2. Linux 安装版：资源在 ../lib/rime-sync/resources。
        let bin = tmp.path().join("usr").join("bin");
        fs::create_dir_all(&bin).expect("mkdir");
        fs::create_dir_all(
            tmp.path()
                .join("usr")
                .join("lib")
                .join("rime-sync")
                .join("resources"),
        )
        .expect("mkdir");
        assert!(!looks_portable(Some(&bin)));

        // 3. 便携：只有一个 exe，既没有 resources/ 也没有安装版标记。
        let portable = tmp.path().join("portable");
        fs::create_dir_all(&portable).expect("mkdir");
        assert!(looks_portable(Some(&portable)));

        // 4. 便携目录里有 NSIS 卸载器 => 仍按安装版处理（提示文案别误导）。
        fs::write(portable.join(UNINSTALL_FILE), b"MZ stub").expect("write");
        assert!(!looks_portable(Some(&portable)));

        // 不知道 exe 目录时按安装版处理（保持旧行为，不乱提示便携）。
        assert!(!looks_portable(None));
    }
}
