//! 输入法增强功能检测 + Linux 一键换装。
//!
//! 增强功能（数字小键盘 / 中文标点直出 / v 转数字）是 rime-sync 自研、编进
//! 输入法引擎本体的组件，发行版 Rime 里根本没有。判据全部在 core
//! （ime_state::assess_extra_support），本模块只负责三件事：
//!
//! 1. 把判据翻译成给用户看的中文（不给用户看 librime / processor 这类术语）；
//! 2. Linux 上提供「一键换装」入口：用 pkexec 调换装脚本，脚本由
//!    packaging/linux/install-extra-ime.sh 提供（deb 安装后才有这个路径）；
//! 3. Windows 上不给换装按钮 —— 点一次「修复输入法」程序会自动换 Weasel。

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, State};
// 只有 Linux 分支要用 app.state()，Windows 编译时这个 trait 不能引（否则 unused import）。
#[cfg(target_os = "linux")]
use tauri::Manager;

use rime_sync_core::ime_state;

use crate::AppState;

/// 换装脚本：只有 rime-sync 的 deb 包安装后才有这个路径。
///
/// AppImage 运行时它不存在 —— 这时界面要改成提示用户手动用完整安装包安装，
/// 绝不能直接报错崩掉。
const INSTALL_SCRIPT: &str = "/usr/lib/rime-sync/install-extra-ime.sh";

/// 给前端看的增强功能状态。
#[derive(Serialize)]
pub struct ExtraSupportView {
    /// supported / unsupported / unknown（core 的机器可读值）。
    pub support: String,
    /// 一句中文人话建议，直接给用户看。
    pub advice: String,
    /// 平台：linux / windows / …（前端据此决定要不要给换装按钮）。
    pub platform: String,
    /// 本机能不能一键换装（Linux 且换装脚本在位）。
    pub can_reinstall: bool,
    /// 换装脚本路径（不在位时也报出来，方便排查）。
    pub script_path: String,
}

/// 换装脚本是否在位（AppImage 版本不在位）。
fn script_available() -> bool {
    Path::new(INSTALL_SCRIPT).exists()
}

/// 只有 Linux 且脚本在位时才提供「一键换装」。
fn can_reinstall() -> bool {
    cfg!(target_os = "linux") && script_available()
}

/// 读取本机输入法的增强功能状态。
///
/// 判据在 core 里（引擎二进制 / 编译产物 / 用户配置三路信号），这里不改判据，
/// 只把它翻译成用户看得懂的话。
#[tauri::command]
pub fn cmd_extra_support(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ExtraSupportView, String> {
    let rime_dir = crate::rime_dir_for(&app, &state.0)?;
    let support = ime_state::assess_extra_support(&rime_dir);
    Ok(ExtraSupportView {
        support: support.as_str().to_string(),
        advice: support.advice().to_string(),
        platform: std::env::consts::OS.to_string(),
        can_reinstall: can_reinstall(),
        script_path: INSTALL_SCRIPT.to_string(),
    })
}

/// Linux 一键换装：用 pkexec 调换装脚本，把发行版输入法换成我们的自建版。
///
/// pkexec 会弹系统自己的管理员授权窗口，所以这里「脱离进程、不等结果」：
/// 换装要几分钟，界面不能被冻住（与 diagnostics::cmd_retry_install_deps 同一写法）。
#[cfg(target_os = "linux")]
#[tauri::command]
pub fn cmd_install_extra_ime(app: AppHandle, package_path: String) -> Result<String, String> {
    if !script_available() {
        return Err(format!(
            "当前这个程序里没有换装功能（多半是 AppImage 版本，找不到 {INSTALL_SCRIPT}）：请改用完整安装包手动安装 —— 先卸载原来的输入法，再运行完整安装包。"
        ));
    }
    let path = package_path.trim().to_string();
    if path.is_empty() {
        return Err("请先选一个安装包（完整包和精简包都行，两个都自带输入法）。".to_string());
    }
    let package = Path::new(&path);
    if !package.exists() {
        return Err(format!("找不到你给的安装包：{path}"));
    }
    let mut command = std::process::Command::new("pkexec");
    command
        .arg(INSTALL_SCRIPT)
        .arg(package)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
        .spawn()
        .map_err(|error| format!("启动换装程序失败：{error}"))?;
    crate::push_log(
        &app.state::<AppState>().0,
        format!("已通过 pkexec 启动输入法换装：{path}"),
    );
    Ok(
        "已启动换装：系统会弹出管理员授权窗口，输完密码就会自动替换输入法；换装完成后重启输入法即可。"
            .to_string(),
    )
}

/// Windows 上不提供换装入口：点一次「修复输入法」程序会自动换 Weasel。
///
/// 单独一个 cfg 版本（而不是函数体内分支）：非 Linux 编译时函数体必须自己
/// 结尾有返回值，否则类型对不上。
#[cfg(not(target_os = "linux"))]
#[tauri::command]
pub fn cmd_install_extra_ime(app: AppHandle, package_path: String) -> Result<String, String> {
    let _ = (&app, &package_path);
    Err("Windows 不需要手动换装：点一次「修复输入法」，程序会自动换上我们的输入法。".to_string())
}

/// 让用户挑一个本地的完整安装包（Rust 侧直接调系统文件选择器，不需要前端插件）。
#[tauri::command]
pub async fn cmd_pick_extra_package(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        handle
            .dialog()
            .file()
            .set_title("选择完整安装包".to_string())
            .add_filter("安装包", &["deb", "run", "AppImage", "gz", "zip"])
            .pick_file(move |path| {
                let _ = tx.send(path);
            });
        rx.recv_timeout(std::time::Duration::from_secs(600))
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
    .and_then(|path| path.into_path().ok())
    .map(|path| path.display().to_string())
}
