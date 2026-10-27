//! 开机自启（0.6.7）：Linux 写 XDG Autostart，Windows 写 HKCU Run。
//!
//! 不用 tauri-plugin-autostart 的原因：自启条目必须固定带 --autostart，
//! desktop 文件的 Name/Icon/Exec 也要可控，并且要把「实际写入的可执行路径」
//! 回报给设置页（AppImage 移动后要能提示用户重新开启）。
//!
//! 平台实现拆成两个可独立测试的模块：
//! - xdg：desktop 文件内容生成 / 写入 / 删除（Linux 产物，任何平台都能单测）；
//! - registry：HKCU Run 的读改写，走 RegistryStore trait 注入，
//!   Windows 用 reg.exe 实现，单测用内存 mock（因此不引入新 crate）。

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::{push_log, AppState};

/// 自启条目固定附加的参数：带它启动时不弹主窗口，只驻留托盘。
pub(crate) const AUTOSTART_ARG: &str = "--autostart";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutostartView {
    pub supported: bool,
    pub enabled: bool,
    /// "xdg-desktop" | "registry" | "launch-agent"
    pub mechanism: &'static str,
    /// 实际写入自启条目的可执行路径。
    pub target: String,
    pub error: Option<String>,
}

/// 自启要用的可执行路径。
///
/// AppImage 运行时 $APPIMAGE 才是稳定路径：current_exe() 指向 /tmp 下的
/// 只读挂载点，重启后就失效了。deb 安装没有这个变量，退回 current_exe()
/// （即 /usr/bin/rime-sync-app）。Windows 上 $APPIMAGE 永远不存在。
pub(crate) fn target_executable() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("APPIMAGE") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    std::env::current_exe().map_err(|error| format!("无法定位当前可执行文件：{error}"))
}

/* ---------- Linux：XDG Autostart ---------- */

#[cfg(any(target_os = "linux", test))]
pub(crate) mod xdg {
    use std::path::{Path, PathBuf};

    use super::AUTOSTART_ARG;

    pub(crate) const DESKTOP_NAME: &str = "rime-sync.desktop";
    /// 旧版本（tauri-plugin-autostart）可能留下的条目；开启时顺手清掉，
    /// 否则登录后会同时跑两份进程。
    pub(crate) const LEGACY_DESKTOP_NAMES: [&str; 2] =
        ["rime-sync-app.desktop", "rime_sync_app.desktop"];

    fn config_home() -> Result<PathBuf, String> {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            if !dir.is_empty() {
                return Ok(PathBuf::from(dir));
            }
        }
        match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => Ok(PathBuf::from(home).join(".config")),
            _ => Err("无法解析用户配置目录（HOME / XDG_CONFIG_HOME 均为空）".to_string()),
        }
    }

    /// ~/.config/autostart（或 $XDG_CONFIG_HOME/autostart）。
    pub(crate) fn autostart_dir() -> Result<PathBuf, String> {
        Ok(config_home()?.join("autostart"))
    }

    /// Desktop Entry 规范：参数含空格时必须用双引号包住；引号内的
    /// 双引号、反引号、美元符、反斜杠用反斜杠转义。
    pub(crate) fn quote_exec_arg(value: &str) -> String {
        let mut quoted = String::with_capacity(value.len() + 2);
        quoted.push('"');
        for ch in value.chars() {
            if matches!(ch, '"' | '\u{60}' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(ch);
        }
        quoted.push('"');
        quoted
    }

    fn exec_line(exe: &Path) -> String {
        format!(
            "Exec={} {}",
            quote_exec_arg(&exe.to_string_lossy()),
            AUTOSTART_ARG
        )
    }

    pub(crate) fn desktop_entry(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\n\
Type=Application\n\
Name=Rime 同步\n\
Comment=Rime 词库与配置的 WebDAV 同步\n\
{}\n\
Icon=rime-sync\n\
Terminal=false\n\
X-GNOME-Autostart-enabled=true\n\
StartupNotify=false\n\
Categories=Utility;\n",
            exec_line(exe)
        )
    }

    fn write_error(path: &Path, error: &std::io::Error) -> String {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            format!("没有写入权限：{}（{error}）", path.display())
        } else {
            format!("写入 {} 失败：{error}", path.display())
        }
    }

    #[cfg(unix)]
    fn set_file_mode(path: &Path) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
            .map_err(|error| format!("设置 {} 权限为 0644 失败：{error}", path.display()))
    }

    #[cfg(not(unix))]
    fn set_file_mode(_path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn remove_file(path: &Path) -> Result<(), String> {
        std::fs::remove_file(path)
            .map_err(|error| format!("删除自启条目 {} 失败：{error}", path.display()))
    }

    /// 写 ~/.config/autostart/rime-sync.desktop（0644），并清理旧文件名。
    pub(crate) fn write_desktop_entry(dir: &Path, exe: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("创建自启目录 {} 失败：{error}", dir.display()))?;
        let path = dir.join(DESKTOP_NAME);
        std::fs::write(&path, desktop_entry(exe)).map_err(|error| write_error(&path, &error))?;
        set_file_mode(&path)?;
        for name in LEGACY_DESKTOP_NAMES {
            let legacy = dir.join(name);
            if legacy.exists() {
                remove_file(&legacy)?;
            }
        }
        Ok(path)
    }

    /// 删除自启条目（含旧文件名）。返回是否真的删掉了东西。
    pub(crate) fn remove_desktop_entry(dir: &Path) -> Result<bool, String> {
        let mut removed = false;
        let mut names = vec![DESKTOP_NAME];
        names.extend(LEGACY_DESKTOP_NAMES);
        for name in names {
            let path = dir.join(name);
            if path.exists() {
                remove_file(&path)?;
                removed = true;
            }
        }
        Ok(removed)
    }

    pub(crate) fn desktop_entry_enabled(dir: &Path) -> bool {
        dir.join(DESKTOP_NAME).is_file()
    }

    /// 条目还在但 Exec 指向别的路径（AppImage 被移动/升级）时给出可读提示。
    pub(crate) fn stale_entry_note(dir: &Path, exe: &Path) -> Option<String> {
        let text = std::fs::read_to_string(dir.join(DESKTOP_NAME)).ok()?;
        let expected = exec_line(exe);
        if text.lines().any(|line| line.trim_end() == expected) {
            return None;
        }
        Some(format!(
            "自启条目指向的可执行文件已变化（当前运行路径 {}），请关闭后重新开启一次。",
            exe.display()
        ))
    }
}

/* ---------- Windows：HKCU Run ---------- */

#[cfg(any(windows, test))]
pub(crate) mod registry {
    use std::path::Path;

    use super::AUTOSTART_ARG;

    /// 用户级 Run 键：不需要管理员权限，也不用写 HKLM。
    pub(crate) const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    pub(crate) const RUN_VALUE_NAME: &str = "RimeSync";

    /// 注册表读改写的注入点：Windows 用 reg.exe 实现，单测用内存 mock。
    pub(crate) trait RegistryStore {
        /// 读字符串值；不存在返回 Ok(None)。
        fn get(&self, key: &str, name: &str) -> Result<Option<String>, String>;
        fn set(&self, key: &str, name: &str, value: &str) -> Result<(), String>;
        fn delete(&self, key: &str, name: &str) -> Result<(), String>;
    }

    /// 写入 Run 的数据：双引号包住的 exe 路径 + --autostart（兼容路径含空格）。
    pub(crate) fn run_value_data(exe: &Path) -> String {
        format!("\"{}\" {}", exe.display(), AUTOSTART_ARG)
    }

    pub(crate) fn registry_enable<S: RegistryStore + ?Sized>(
        store: &S,
        exe: &Path,
    ) -> Result<(), String> {
        store.set(RUN_KEY, RUN_VALUE_NAME, &run_value_data(exe))
    }

    pub(crate) fn registry_disable<S: RegistryStore + ?Sized>(store: &S) -> Result<(), String> {
        store.delete(RUN_KEY, RUN_VALUE_NAME)
    }

    pub(crate) fn registry_enabled<S: RegistryStore + ?Sized>(store: &S) -> Result<bool, String> {
        Ok(store
            .get(RUN_KEY, RUN_VALUE_NAME)?
            .is_some_and(|value| !value.trim().is_empty()))
    }

    /// 解析 reg query 输出的一行：
    /// 「    RimeSync    REG_SZ    "C:\\...\\rime-sync-app.exe" --autostart」。
    /// 路径里有空格，所以按 REG_SZ 之后的整段取，不能按空白切词。
    pub(crate) fn parse_reg_query(stdout: &str, name: &str) -> Option<String> {
        for line in stdout.lines() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix(name) else {
                continue;
            };
            let rest = rest.trim_start();
            if !rest.starts_with("REG_") {
                continue;
            }
            let index = rest.find("REG_SZ")?;
            return Some(
                rest[index + "REG_SZ".len()..]
                    .trim_start()
                    .trim_end()
                    .to_string(),
            );
        }
        None
    }

    #[cfg(windows)]
    pub(crate) struct RegExe;

    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[cfg(windows)]
    fn reg_output(args: &[&str]) -> Result<std::process::Output, String> {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("reg.exe")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|error| format!("调用 reg.exe 失败：{error}"))
    }

    #[cfg(windows)]
    impl RegistryStore for RegExe {
        fn get(&self, key: &str, name: &str) -> Result<Option<String>, String> {
            let full = format!("HKCU\\{key}");
            let output = reg_output(&["query", &full, "/v", name])?;
            if !output.status.success() {
                // 值不存在时 reg.exe 返回 1：按「未开启」处理，不是错误。
                return Ok(None);
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(parse_reg_query(&stdout, name))
        }

        fn set(&self, key: &str, name: &str, value: &str) -> Result<(), String> {
            let full = format!("HKCU\\{key}");
            let output =
                reg_output(&["add", &full, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])?;
            if output.status.success() {
                return Ok(());
            }
            Err(format!(
                "写入注册表失败（HKCU\\{key}）：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }

        fn delete(&self, key: &str, name: &str) -> Result<(), String> {
            let full = format!("HKCU\\{key}");
            let output = reg_output(&["delete", &full, "/v", name, "/f"])?;
            if output.status.success() {
                return Ok(());
            }
            // 值本来就不存在时 reg.exe 也返回非 0：再 query 一次确认，确认不存在即成功。
            let query = reg_output(&["query", &full, "/v", name])?;
            if query.status.success() {
                Err(format!(
                    "删除注册表值失败（HKCU\\{key}\\{name}）：{}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            } else {
                Ok(())
            }
        }
    }
}

/* ---------- 平台实现 ---------- */

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub(super) fn status() -> AutostartView {
        let dir = match xdg::autostart_dir() {
            Ok(dir) => dir,
            Err(error) => return unsupported(error),
        };
        let target = match target_executable() {
            Ok(path) => path,
            Err(error) => return unsupported(error),
        };
        let enabled = xdg::desktop_entry_enabled(&dir);
        AutostartView {
            supported: true,
            enabled,
            mechanism: "xdg-desktop",
            target: target.to_string_lossy().into_owned(),
            error: if enabled {
                xdg::stale_entry_note(&dir, &target)
            } else {
                None
            },
        }
    }

    pub(super) fn set(enabled: bool) -> Result<AutostartView, String> {
        let dir = xdg::autostart_dir()?;
        let target = target_executable()?;
        if enabled {
            xdg::write_desktop_entry(&dir, &target)?;
        } else {
            xdg::remove_desktop_entry(&dir)?;
        }
        Ok(status())
    }

    fn unsupported(error: String) -> AutostartView {
        AutostartView {
            supported: false,
            enabled: false,
            mechanism: "xdg-desktop",
            target: String::new(),
            error: Some(error),
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::registry::{self, RegExe};
    use super::*;

    pub(super) fn status() -> AutostartView {
        let target = target_executable()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        match registry::registry_enabled(&RegExe) {
            Ok(enabled) => AutostartView {
                supported: true,
                enabled,
                mechanism: "registry",
                target,
                error: None,
            },
            Err(error) => AutostartView {
                supported: true,
                enabled: false,
                mechanism: "registry",
                target,
                error: Some(error),
            },
        }
    }

    pub(super) fn set(enabled: bool) -> Result<AutostartView, String> {
        let target = target_executable()?;
        if enabled {
            registry::registry_enable(&RegExe, &target)?;
        } else {
            registry::registry_disable(&RegExe)?;
        }
        Ok(status())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::*;

    pub(super) fn status() -> AutostartView {
        AutostartView {
            supported: false,
            enabled: false,
            mechanism: "launch-agent",
            target: String::new(),
            error: Some("当前平台暂不支持开机自启".to_string()),
        }
    }

    pub(super) fn set(_enabled: bool) -> Result<AutostartView, String> {
        Err("当前平台暂不支持开机自启".to_string())
    }
}

/* ---------- 命令 ---------- */

#[tauri::command]
pub fn cmd_autostart_status() -> AutostartView {
    imp::status()
}

#[tauri::command]
pub fn cmd_set_autostart(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<AutostartView, String> {
    let view = imp::set(enabled)?;
    push_log(
        &state.0,
        format!(
            "开机自启已{}（机制 {}，目标 {}）",
            if enabled { "开启" } else { "关闭" },
            view.mechanism,
            if view.target.is_empty() {
                "未知"
            } else {
                view.target.as_str()
            }
        ),
    );
    Ok(view)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    // 直接调用 mock 的 get/set 需要 trait 在作用域内。
    use super::registry::RegistryStore as _;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-autostart-{tag}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn desktop_entry_has_required_fields() {
        let exe = Path::new("/opt/Rime 同步/rime-sync-app");
        let text = xdg::desktop_entry(exe);
        assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
        for line in [
            "Type=Application",
            "Name=Rime 同步",
            "Icon=rime-sync",
            "Terminal=false",
            "X-GNOME-Autostart-enabled=true",
        ] {
            assert!(
                text.lines().any(|item| item == line),
                "缺少 {line}：\n{text}"
            );
        }
        assert!(
            text.lines()
                .any(|item| item == "Exec=\"/opt/Rime 同步/rime-sync-app\" --autostart"),
            "Exec 行不对：\n{text}"
        );
    }

    #[test]
    fn quote_exec_arg_escapes_specials() {
        assert_eq!(xdg::quote_exec_arg("plain"), "\"plain\"");
        assert_eq!(
            xdg::quote_exec_arg("/opt/Rime Sync/app"),
            "\"/opt/Rime Sync/app\""
        );
        let quote = '"';
        assert_eq!(
            xdg::quote_exec_arg(&format!("a{quote}b")),
            format!("\"a\\{quote}b\"")
        );
        assert_eq!(xdg::quote_exec_arg("a$b"), "\"a\\$b\"");
        let backslash = char::from(92);
        assert_eq!(
            xdg::quote_exec_arg(&format!("a{backslash}b")),
            format!("\"a\\{backslash}b\"")
        );
        let backtick = char::from(96);
        assert_eq!(
            xdg::quote_exec_arg(&format!("a{backtick}b")),
            format!("\"a\\{backtick}b\"")
        );
    }

    #[test]
    fn write_entry_sets_mode_cleans_legacy_and_detects_stale_target() {
        let dir = temp_dir("write");
        let exe = Path::new("/home/u/Applications/Rime Sync_0.6.7_amd64.AppImage");
        let legacy = dir.join("rime-sync-app.desktop");
        std::fs::write(&legacy, "old").unwrap();

        let path = xdg::write_desktop_entry(&dir, exe).unwrap();
        assert_eq!(path, dir.join("rime-sync.desktop"));
        assert!(xdg::desktop_entry_enabled(&dir));
        assert!(!legacy.exists(), "旧条目应被清理");
        assert!(xdg::stale_entry_note(&dir, exe).is_none());

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("--autostart"));
        assert!(text.contains("Exec=\"/home/u/Applications/Rime Sync_0.6.7_amd64.AppImage\""));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644);
        }

        let moved = Path::new("/home/u/Applications/moved.AppImage");
        assert!(xdg::stale_entry_note(&dir, moved).is_some());

        assert!(xdg::remove_desktop_entry(&dir).unwrap());
        assert!(!xdg::desktop_entry_enabled(&dir));
        assert!(!xdg::remove_desktop_entry(&dir).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    struct MockRegistry {
        values: RefCell<BTreeMap<(String, String), String>>,
    }

    impl MockRegistry {
        fn new() -> Self {
            Self {
                values: RefCell::new(BTreeMap::new()),
            }
        }
    }

    impl registry::RegistryStore for MockRegistry {
        fn get(&self, key: &str, name: &str) -> Result<Option<String>, String> {
            Ok(self
                .values
                .borrow()
                .get(&(key.to_string(), name.to_string()))
                .cloned())
        }

        fn set(&self, key: &str, name: &str, value: &str) -> Result<(), String> {
            self.values
                .borrow_mut()
                .insert((key.to_string(), name.to_string()), value.to_string());
            Ok(())
        }

        fn delete(&self, key: &str, name: &str) -> Result<(), String> {
            self.values
                .borrow_mut()
                .remove(&(key.to_string(), name.to_string()));
            Ok(())
        }
    }

    #[test]
    fn registry_enable_writes_quoted_value_and_disable_removes_it() {
        assert_eq!(
            registry::RUN_KEY,
            r"Software\Microsoft\Windows\CurrentVersion\Run"
        );
        assert_eq!(registry::RUN_VALUE_NAME, "RimeSync");

        let store = MockRegistry::new();
        let exe = Path::new(r"C:\Program Files\Rime 同步\rime-sync-app.exe");
        assert!(!registry::registry_enabled(&store).unwrap());

        registry::registry_enable(&store, exe).unwrap();
        assert_eq!(
            store
                .get(registry::RUN_KEY, registry::RUN_VALUE_NAME)
                .unwrap()
                .as_deref(),
            Some(r#""C:\Program Files\Rime 同步\rime-sync-app.exe" --autostart"#)
        );
        assert!(registry::registry_enabled(&store).unwrap());

        registry::registry_disable(&store).unwrap();
        assert!(store
            .get(registry::RUN_KEY, registry::RUN_VALUE_NAME)
            .unwrap()
            .is_none());
        assert!(!registry::registry_enabled(&store).unwrap());
    }

    #[test]
    fn parse_reg_query_reads_value_data() {
        let stdout = "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\r\n    RimeSync    REG_SZ    \"C:\\Program Files\\Rime 同步\\rime-sync-app.exe\" --autostart\r\n";
        assert_eq!(
            registry::parse_reg_query(stdout, "RimeSync").as_deref(),
            Some(r#""C:\Program Files\Rime 同步\rime-sync-app.exe" --autostart"#)
        );
        assert_eq!(registry::parse_reg_query(stdout, "Other"), None);
    }
}
