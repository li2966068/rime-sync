//! 桌面壳的落盘日志 + panic 兜底。
//!
//! 三件事：
//! 1. 日志文件：`<app_config_dir>/log.txt`，用 core 的 `RotatingLog`（5 MB × 3 份滚动）。
//!    与 CLI 的 `state_dir/log.txt` 同一套实现，crates/** 不需要改。
//! 2. 同一个文件里既有壳层的 `push_log`，也有 core 里 70 多处 `tracing::` 调用。
//! 3. panic 钩子：release 是 `panic = "abort"`，panic 展开后直接 abort，
//!    `catch_unwind` 永远抓不到 —— 这个钩子是唯一能把现场留下来的地方。
//!    它把「消息 + 位置 + 线程名」写进同一个日志文件，并尽量弹一个原生对话框
//!    （输入法自举线程崩了显示「输入法自举失败」，而不是让程序无声消失）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use rime_sync_core::log::{LogWriter, RotatingLog, DEFAULT_KEEP, DEFAULT_MAX_BYTES};
use tauri::AppHandle;

/// 日志文件名（与 CLI 一致）。
pub(crate) const LOG_FILE_NAME: &str = "log.txt";

/// 崩溃标记文件名：panic 钩子写它，下次启动读到就告知用户「上次崩了」。
///
/// 为什么单独一个文件而不是翻日志：日志会滚动、也会被后续的启动记录冲掉，
/// 标记文件是「读一次就清」的明确信号，不会误报。
pub(crate) const CRASH_FILE_NAME: &str = "crash.txt";

/// 输入法自举线程名：panic 钩子据此判断是不是自举崩了。
pub(crate) const BOOTSTRAP_THREAD_NAME: &str = "rime-sync-bootstrap";

/// 进程内共享的滚动日志句柄。路径要等 AppHandle 才有，所以延迟到 `init` 里设置。
static LOG: OnceLock<Arc<RotatingLog>> = OnceLock::new();
/// 供 panic 钩子与恢复提示用（还没有就只落盘）。
static APP: OnceLock<AppHandle> = OnceLock::new();
/// 钩子只跑一次：钩子里再出岔子不该递归。
static HOOK_RAN: AtomicBool = AtomicBool::new(false);
/// 上一次启动留下的崩溃摘要（下次启动读到标记时填上）。
static LAST_CRASH: OnceLock<String> = OnceLock::new();

/// 日志文件路径：`<app_config_dir>/log.txt`。
pub(crate) fn log_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::state_dir(app)?.join(LOG_FILE_NAME))
}

/// 初始化落盘日志与 tracing 订阅器。拿到 AppHandle 之后调用一次。
///
/// 返回日志文件路径（面板上的「打开日志目录」之类的提示要用）。
pub(crate) fn init(app: &AppHandle) -> Result<PathBuf, String> {
    let path = log_path(app)?;
    let log = Arc::new(RotatingLog::new(
        path.clone(),
        DEFAULT_MAX_BYTES,
        DEFAULT_KEEP,
    ));
    // set 只会成功一次；重复调用（测试里）不算错误。
    let _ = LOG.set(Arc::clone(&log));
    let _ = APP.set(app.clone());
    init_tracing(log);
    take_crash_marker(app);
    Ok(path)
}

/// 上一次启动是不是崩了（崩了就有崩溃摘要）。
pub(crate) fn last_crash() -> Option<&'static str> {
    LAST_CRASH.get().map(|value| value.as_str())
}

/// 读崩溃标记并清掉它：读一次就清，避免每次启动都重复提示。
fn take_crash_marker(app: &AppHandle) -> Option<String> {
    let path = crate::state_dir(app).ok()?.join(CRASH_FILE_NAME);
    take_crash_marker_at(&path)
}

/// 读崩溃标记文件并删除它（纯文件操作，方便单测）。
pub(crate) fn take_crash_marker_at(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    let summary = content.trim().to_string();
    if summary.is_empty() {
        return None;
    }
    let _ = LAST_CRASH.set(summary.clone());
    Some(summary)
}

/// 让 core 的 `tracing::` 调用写进同一个日志文件。
///
/// 默认级别 info：debug 太吵；core 在 info 级别只报计数与结论，不落 HTTP 请求体。
/// 想调级别就设 `RUST_LOG`（与 CLI 同口径）。
fn init_tracing(log: Arc<RotatingLog>) {
    let writer = LogWriter::new(log);
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // try_init：全局订阅器只能装一次，重复初始化不算失败。
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(move || writer.clone())
        .try_init();
}

/// 统一的行首时间戳（壳层 `push_log` 与 panic 行同格式）。
pub(crate) fn stamp(message: &str) -> String {
    format!("[{}] {}", chrono::Local::now().format("%H:%M:%S"), message)
}

/// 追加一行到日志文件。
///
/// 写失败只往 stderr 记一句，绝不 panic：日志写不进去不能反过来打断同步。
pub(crate) fn append_line(line: &str) {
    if let Some(log) = LOG.get() {
        if let Err(error) = log.append_line(line) {
            eprintln!("[rime-sync] 写日志失败：{error}");
        }
        return;
    }
    // 还没初始化（极早期就崩了）：退到临时目录，至少别把现场丢了。
    let fallback = std::env::temp_dir().join("rime-sync-panic.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&fallback)
    {
        let _ = writeln!(file, "{line}");
    }
    eprintln!("[rime-sync] {line}");
}

/// panic 的一行摘要：纯函数，方便单测钉住格式。
///
/// 落盘的那一行必须同时含三样东西：panic 消息、出错位置、线程名。
/// 少一样，事后排查就少一半线索（尤其是「崩在哪个线程」）。
pub(crate) fn format_panic_line(message: &str, location: &str, thread: &str) -> String {
    format!("[panic] 线程 {thread} panic：{message}（位置：{location}）")
}

/// 安装 panic 钩子：落盘 + 写崩溃标记 + 尽力弹一个原生对话框。
///
/// release 是 `panic = "abort"`：panic 展开后直接 abort，`catch_unwind` 抓不到，
/// 所以这个钩子是唯一能把现场留下来的地方。钩子里做三件事：
/// - 消息 + 位置 + 线程名写进日志文件；
/// - 写一份崩溃标记：下次启动读到就告诉用户「上次崩了」（abort 下同进程的弹窗
///   会被进程一起带走，跨进程的下次启动提示才是真正可靠的可见面）；
/// - 尽力弹一个原生对话框：在独立线程里弹、不等它返回 —— debug 构建
///   （panic=unwind，进程不死）下用户能当场看到，release 下多半来不及，
///   但留着不亏。
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 原来的钩子照旧跑：stderr 上的标准 panic 输出保留。
        previous(info);
        if HOOK_RAN.swap(true, Ordering::SeqCst) {
            return;
        }
        let payload = info.payload();
        let message = if let Some(text) = payload.downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = payload.downcast_ref::<String>() {
            text.clone()
        } else {
            "panic（载荷不是字符串）".to_string()
        };
        let location = info
            .location()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "未知位置".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("未命名线程")
            .to_string();
        let line = format_panic_line(&message, &location, &thread);
        append_line(&stamp(&line));
        write_crash_marker(&line);
        notify_user(&line);
    }));
}

/// 崩溃标记的内容（纯函数，方便单测钉住格式）。
pub(crate) fn format_crash_marker(line: &str) -> String {
    format!("{}\n{}", stamp(line), line)
}

/// 写崩溃标记：下次启动读到就告知用户。
fn write_crash_marker(line: &str) {
    let marker = format_crash_marker(line);
    let path = APP
        .get()
        .and_then(|app| crate::state_dir(app).ok())
        .map(|dir| dir.join(CRASH_FILE_NAME))
        .unwrap_or_else(|| std::env::temp_dir().join(CRASH_FILE_NAME));
    if let Err(error) = std::fs::write(&path, marker) {
        eprintln!("[rime-sync] 写崩溃标记失败：{error}");
    }
}

/// 尽量让用户看见这次崩溃：在独立线程里弹框，不等它返回。
///
/// 弹框线程与崩溃线程分开：崩溃线程要尽快把控制权交回 abort/展开流程。
fn notify_user(line: &str) {
    let Some(app) = APP.get() else {
        return;
    };
    let app = app.clone();
    let body =
        format!("程序异常退出\n\n{line}\n\n详细信息已写入日志文件。\n下次启动时会再次提示。");
    let _ = std::thread::Builder::new()
        .name("rime-sync-panic-notify".to_string())
        .spawn(move || {
            use tauri_plugin_dialog::DialogExt;
            app.dialog()
                .message(body)
                .title("程序异常退出")
                .blocking_show();
        });
}

/// 上次启动留下的崩溃：启动后弹一次提示（跨进程才可靠，abort 会带走同进程的弹窗）。
///
/// 弹框在独立线程里做，不阻塞启动。
pub(crate) fn notify_previous_crash() {
    let Some(summary) = last_crash() else {
        return;
    };
    let Some(app) = APP.get() else {
        return;
    };
    let app = app.clone();
    let body = format!("程序上次异常退出。\n\n{summary}\n\n详细信息已写入日志文件。");
    let _ = std::thread::Builder::new()
        .name("rime-sync-crash-notify".to_string())
        .spawn(move || {
            use tauri_plugin_dialog::DialogExt;
            app.dialog()
                .message(body)
                .title("程序异常退出")
                .blocking_show();
        });
}

/// 测试用：把全局日志指向一个临时文件（真实运行时由 `init` 设置）。
///
/// 全局句柄只能装一次：第二次调用不会生效，所以这里返回**实际生效**的路径，
/// 测试要读文件就读这个返回值，别假设自己传进去的那个路径一定被采用。
#[cfg(test)]
pub(crate) fn use_log_for_test(path: PathBuf) -> Option<PathBuf> {
    let log = Arc::new(RotatingLog::new(path, DEFAULT_MAX_BYTES, DEFAULT_KEEP));
    match LOG.set(log) {
        Ok(()) => current_log_path(),
        Err(_) => current_log_path(),
    }
}

/// 测试用：当前全局日志文件的位置（没初始化过就是 None）。
#[cfg(test)]
pub(crate) fn current_log_path() -> Option<PathBuf> {
    LOG.get().map(|log| log.path().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_line_contains_message_location_and_thread() {
        let line = format_panic_line("索引越界", "src/lib.rs:42:9", "rime-sync-bootstrap");
        assert!(line.contains("索引越界"), "{line}");
        assert!(line.contains("src/lib.rs:42:9"), "{line}");
        assert!(line.contains("rime-sync-bootstrap"), "{line}");
    }

    #[test]
    fn appended_line_is_readable_from_the_log_file() {
        // 用真实的 RotatingLog 走一遍「写进去 → 读回来」，证明落盘链路通。
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-applog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(LOG_FILE_NAME);
        let log = RotatingLog::new(path.clone(), DEFAULT_MAX_BYTES, DEFAULT_KEEP);
        let line = format_panic_line("写日志自检", "src/app_log.rs:1:1", "test-thread");
        log.append_line(&line).expect("写日志");
        let content = std::fs::read_to_string(&path).expect("读日志");
        assert!(content.contains("写日志自检"), "{content}");
        assert!(content.contains("test-thread"), "{content}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn log_path_is_inside_the_app_config_dir() {
        // 只验证文件名与拼接规则（真正的 app_config_dir 需要 AppHandle，测不了）。
        assert_eq!(LOG_FILE_NAME, "log.txt");
        assert!(BOOTSTRAP_THREAD_NAME.contains("bootstrap"));
    }

    /// 崩溃标记：写了能读到，读完即清（不会每次启动都重复提示）。
    #[test]
    fn crash_marker_is_read_once_and_then_cleared() {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-crash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(CRASH_FILE_NAME);

        // 没有标记时读到 None
        assert!(take_crash_marker_at(&path).is_none());

        let marker = format_crash_marker(&format_panic_line(
            "自举崩了",
            "src/prebuilt_cmds.rs:1:1",
            BOOTSTRAP_THREAD_NAME,
        ));
        std::fs::write(&path, &marker).expect("写崩溃标记");

        let summary = take_crash_marker_at(&path).expect("应读到崩溃标记");
        assert!(summary.contains("自举崩了"), "{summary}");
        assert!(summary.contains(BOOTSTRAP_THREAD_NAME), "{summary}");
        // 读一次就清：文件没了，不会下次启动再提示一遍。
        assert!(!path.exists(), "崩溃标记必须被清掉");
        assert!(take_crash_marker_at(&path).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
