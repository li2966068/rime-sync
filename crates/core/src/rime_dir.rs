//! Rime 用户目录探测与 installation.yaml 解析。

use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Detection {
    pub path: PathBuf,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct Installation {
    pub installation_id: Option<String>,
    pub sync_dir: Option<PathBuf>,
}

/// explicit 来自 --rime-dir 或 config.rime_dir；其次环境变量；最后已知平台路径。
pub fn detect(explicit: Option<&Path>) -> Result<Detection> {
    if let Some(path) = explicit {
        if !path.is_dir() {
            bail!(
                "rime dir does not exist or is not a directory: {}",
                path.display()
            );
        }
        return Ok(Detection {
            path: path.to_path_buf(),
            source: "argument".to_string(),
        });
    }
    if let Some(raw) = env::var_os("RIME_USER_DATA_DIR") {
        let path = PathBuf::from(raw);
        if path.is_dir() {
            return Ok(Detection {
                path,
                source: "env:RIME_USER_DATA_DIR".to_string(),
            });
        }
    }
    // fcitx5-rime 的自定义用户目录（vendored 版新增的 UserDataDirPath）。
    if let Some(path) = fcitx5_user_data_dir() {
        if path.is_dir() {
            return Ok(Detection {
                path,
                source: "fcitx5-rime:UserDataDirPath".to_string(),
            });
        }
    }
    for candidate in candidates() {
        if candidate.is_dir() {
            let source = format!("known:{}", candidate.display());
            return Ok(Detection {
                path: candidate,
                source,
            });
        }
    }
    bail!("no Rime user directory found; pass --rime-dir or set RIME_USER_DATA_DIR")
}

pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(home) = home_dir() else {
        return out;
    };
    #[cfg(target_os = "linux")]
    {
        out.push(home.join(".local/share/fcitx5/rime"));
        out.push(home.join(".config/fcitx/rime"));
        out.push(home.join(".config/ibus/rime"));
        out.push(home.join(".local/share/ibus/rime"));
    }
    #[cfg(target_os = "macos")]
    {
        out.push(home.join("Library/Rime"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = env::var_os("APPDATA") {
            out.push(PathBuf::from(appdata).join("Rime"));
        }
        out.push(home.join("AppData/Roaming/Rime"));
    }
    out
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// fcitx5-rime 的配置文件（相对配置根）。
pub const FCITX5_RIME_CONF: &str = "fcitx5/conf/rime.conf";

/// 配置根：$XDG_CONFIG_HOME，缺省 ~/.config。
fn config_home() -> Option<PathBuf> {
    if let Some(raw) = env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    home_dir().map(|home| home.join(".config"))
}

/// 纯函数：rime.conf 文本 → UserDataDirPath（支持 `key=value` / `key: value`、引号、~）。
pub fn parse_fcitx5_user_data_dir(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let split = line.split_once('=').or_else(|| line.split_once(':'));
        let Some((key, value)) = split else {
            continue;
        };
        if key.trim() != "UserDataDirPath" {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\u{27}').trim();
        if value.is_empty() {
            return None;
        }
        return Some(expand_tilde(value, home));
    }
    None
}

fn expand_tilde(value: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if value == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = value.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

/// 读 fcitx5 的 rime.conf 并解析 UserDataDirPath（读不到/为空都返回 None）。
fn fcitx5_user_data_dir() -> Option<PathBuf> {
    let conf = config_home()?.join(FCITX5_RIME_CONF);
    let text = fs::read_to_string(conf).ok()?;
    parse_fcitx5_user_data_dir(&text, home_dir().as_deref())
}

pub fn installation(rime_dir: &Path) -> Installation {
    let Ok(text) = fs::read_to_string(rime_dir.join("installation.yaml")) else {
        return Installation::default();
    };
    parse_installation(&text)
}

pub fn parse_installation(text: &str) -> Installation {
    let mut out = Installation::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "installation_id" => out.installation_id = Some(value.to_string()),
            "sync_dir" => out.sync_dir = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    out
}

pub fn installation_id(rime_dir: &Path) -> Option<String> {
    installation(rime_dir).installation_id
}

/// 纯函数：installation.yaml 文本（None = 文件缺失/读不到）→ sync/ 下的本机目录名。
///
/// librime / 小狼毫按 installation_id 建快照目录；取不到、空值或不是合法单段
/// 目录名时回退 device_id。
pub fn local_sync_id(installation_yaml: Option<&str>, device_id: &str) -> String {
    let candidate = installation_yaml
        .and_then(|text| parse_installation(text).installation_id)
        .map(|value| value.trim().to_string())
        .filter(|value| is_valid_segment(value));
    candidate.unwrap_or_else(|| device_id.to_string())
}

/// 读 <rime_dir>/installation.yaml 后解析（缺失文件按 None 处理）。
pub fn local_sync_id_in(rime_dir: &Path, device_id: &str) -> String {
    let text = fs::read_to_string(rime_dir.join("installation.yaml")).ok();
    local_sync_id(text.as_deref(), device_id)
}

/// 目录名必须是单段、非隐藏、无空白：installation.yaml 是外部输入，不能让它逃出 sync/。
fn is_valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
        && !value.chars().any(char::is_whitespace)
}

/// Rime 的 sync_dir 可以是绝对路径，也可以是相对 rime 目录的路径。
pub fn sync_dir(rime_dir: &Path) -> PathBuf {
    match installation(rime_dir).sync_dir {
        Some(path) if path.is_absolute() => path,
        Some(path) => rime_dir.join(path),
        None => rime_dir.join("sync"),
    }
}

pub fn ensure_sync_dir(rime_dir: &Path) -> Result<PathBuf> {
    let dir = sync_dir(rime_dir);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installation_yaml() {
        let text = "# comment\ndistribution_code_name: fcitx-rime\ninstallation_id: devA\nsync_dir: \"my-sync\"\n";
        let parsed = parse_installation(text);
        assert_eq!(parsed.installation_id.as_deref(), Some("devA"));
        assert_eq!(parsed.sync_dir, Some(PathBuf::from("my-sync")));
    }

    #[test]
    fn explicit_detection_wins() {
        let dir = std::env::temp_dir().join(format!("rime-sync-det-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let det = detect(Some(&dir)).expect("detect");
        assert_eq!(det.source, "argument");
        assert_eq!(det.path, dir);
        fs::remove_dir_all(&det.path).ok();
    }

    #[test]
    fn missing_explicit_dir_is_an_error() {
        let dir = std::env::temp_dir().join("rime-sync-definitely-missing-dir");
        assert!(detect(Some(&dir)).is_err());
    }

    #[test]
    fn local_sync_id_falls_back_when_file_missing() {
        assert_eq!(
            local_sync_id(None, "debian-528996"),
            "debian-528996",
            "缺失 installation.yaml 时必须回退 device_id"
        );
    }

    #[test]
    fn local_sync_id_prefers_quoted_installation_id() {
        let text = "distribution_code_name: fcitx-rime\ninstallation_id: \"5d0d8b1e-0f7a-4c2a-9a3f-1f2e\"\n";
        assert_eq!(
            local_sync_id(Some(text), "debian-528996"),
            "5d0d8b1e-0f7a-4c2a-9a3f-1f2e"
        );
    }

    #[test]
    fn local_sync_id_tolerates_single_quotes_and_whitespace() {
        let text = "installation_id:   'abc-123'   \n";
        assert_eq!(local_sync_id(Some(text), "debian-528996"), "abc-123");
    }

    #[test]
    fn local_sync_id_falls_back_on_empty_value() {
        for text in [
            "installation_id: \"\"\n",
            "installation_id:\n",
            "other: value\n",
        ] {
            assert_eq!(
                local_sync_id(Some(text), "debian-528996"),
                "debian-528996",
                "空值或缺失键必须回退 device_id: {text:?}"
            );
        }
    }

    #[test]
    fn local_sync_id_rejects_path_escape() {
        for text in [
            "installation_id: ../evil\n",
            "installation_id: .hidden\n",
            "installation_id: a/b\n",
            "installation_id: a b\n",
        ] {
            assert_eq!(
                local_sync_id(Some(text), "debian-528996"),
                "debian-528996",
                "非法目录名必须回退 device_id: {text:?}"
            );
        }
    }

    #[test]
    fn local_sync_id_in_reads_real_file() {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-install-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        assert_eq!(local_sync_id_in(&dir, "debian-528996"), "debian-528996");
        fs::write(
            dir.join("installation.yaml"),
            "installation_id: \"uuid-from-disk\"\n",
        )
        .expect("write");
        assert_eq!(local_sync_id_in(&dir, "debian-528996"), "uuid-from-disk");
        fs::remove_dir_all(&dir).ok();
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 临时覆盖 HOME / XDG_CONFIG_HOME / RIME_USER_DATA_DIR，Drop 时还原。
    struct EnvScope {
        home: Option<std::ffi::OsString>,
        xdg: Option<std::ffi::OsString>,
        rime_env: Option<std::ffi::OsString>,
    }

    impl EnvScope {
        fn new(home: &Path, xdg: &Path) -> Self {
            let scope = Self {
                home: std::env::var_os("HOME"),
                xdg: std::env::var_os("XDG_CONFIG_HOME"),
                rime_env: std::env::var_os("RIME_USER_DATA_DIR"),
            };
            std::env::set_var("HOME", home);
            std::env::set_var("XDG_CONFIG_HOME", xdg);
            std::env::remove_var("RIME_USER_DATA_DIR");
            scope
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            restore_env("HOME", self.home.as_ref());
            restore_env("XDG_CONFIG_HOME", self.xdg.as_ref());
            restore_env("RIME_USER_DATA_DIR", self.rime_env.as_ref());
        }
    }

    fn restore_env(key: &str, value: Option<&std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn env_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rime-sync-env-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("mkdir");
        root
    }

    fn write_rime_conf(xdg: &Path, text: &str) {
        let conf = xdg.join(FCITX5_RIME_CONF);
        fs::create_dir_all(conf.parent().expect("parent")).expect("mkdir conf");
        fs::write(conf, text).expect("write conf");
    }

    #[test]
    fn parses_fcitx5_conf_variants() {
        let home = std::env::temp_dir().join("rime-sync-fake-home");
        assert_eq!(
            parse_fcitx5_user_data_dir("UserDataDirPath = \"/tmp/rime-custom\"\n", Some(&home)),
            Some(PathBuf::from("/tmp/rime-custom"))
        );
        assert_eq!(
            parse_fcitx5_user_data_dir("UserDataDirPath: ~/rime-custom\n", Some(&home)),
            Some(home.join("rime-custom"))
        );
        assert_eq!(
            parse_fcitx5_user_data_dir("UserDataDirPath=\n", Some(&home)),
            None
        );
        assert_eq!(
            parse_fcitx5_user_data_dir("OtherKey=/tmp/x\n", Some(&home)),
            None
        );
    }

    #[test]
    fn detects_fcitx5_user_data_dir_from_conf() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let root = env_root("conf-key");
        let home = root.join("home");
        let xdg = root.join("xdg");
        let custom = root.join("custom-rime");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&custom).expect("custom");
        write_rime_conf(
            &xdg,
            &format!("[Profile]\nUserDataDirPath=\"{}\"\n", custom.display()),
        );
        let _scope = EnvScope::new(&home, &xdg);

        let det = detect(None).expect("detect");
        assert_eq!(det.path, custom);
        assert_eq!(det.source, "fcitx5-rime:UserDataDirPath");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_fcitx5_user_data_dir_falls_back_to_candidates() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let root = env_root("conf-empty");
        let home = root.join("home");
        let xdg = root.join("xdg");
        let known = home.join(".local/share/fcitx5/rime");
        fs::create_dir_all(&known).expect("known rime dir");
        write_rime_conf(&xdg, "UserDataDirPath=\n");
        let _scope = EnvScope::new(&home, &xdg);

        let det = detect(None).expect("detect");
        assert_eq!(det.path, known);
        assert!(det.source.starts_with("known:"), "{}", det.source);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_fcitx5_conf_falls_back_to_candidates() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let root = env_root("conf-missing");
        let home = root.join("home");
        let xdg = root.join("xdg");
        let known = home.join(".config/fcitx/rime");
        fs::create_dir_all(&known).expect("known rime dir");
        fs::create_dir_all(&xdg).expect("xdg");
        let _scope = EnvScope::new(&home, &xdg);

        let det = detect(None).expect("detect");
        assert_eq!(det.path, known);
        assert!(det.source.starts_with("known:"), "{}", det.source);

        fs::remove_dir_all(&root).ok();
    }
}
