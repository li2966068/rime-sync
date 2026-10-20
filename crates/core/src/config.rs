//! config.json 的加载、0600 权限写回与环境变量覆盖。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::sha256_hex;
use crate::plan::Category;
use crate::rime_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub remote_dir: String,
}

impl Default for WebDavConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            username: String::new(),
            password: String::new(),
            remote_dir: "rime-sync".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncKinds {
    pub userdict: bool,
    pub custom_yaml: bool,
    pub dict_sources: bool,
}

impl Default for SyncKinds {
    fn default() -> Self {
        Self {
            userdict: true,
            custom_yaml: true,
            dict_sources: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub webdav: WebDavConfig,
    pub rime_dir: Option<String>,
    pub device_id: String,
    pub transport_interval_minutes: u32,
    pub sync_kinds: SyncKinds,
    pub exclude_globs: Vec<String>,
    pub stop_fcitx5_during_apply: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            webdav: WebDavConfig::default(),
            rime_dir: None,
            device_id: String::new(),
            transport_interval_minutes: 10,
            sync_kinds: SyncKinds::default(),
            exclude_globs: Vec::new(),
            stop_fcitx5_during_apply: true,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parse config {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read config {}", path.display())),
        }
    }

    /// 含密码，落盘必须是 0600。
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let text = serde_json::to_string_pretty(self)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.json");
        let tmp = path.with_file_name(format!(".{name}.tmp"));
        fs::write(&tmp, format!("{text}\n"))
            .with_context(|| format!("write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
        }
        fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
        Ok(())
    }

    pub fn apply_env(&mut self) {
        if let Some(v) = non_empty(env::var("RIME_SYNC_DAV_URL").ok()) {
            self.webdav.url = v;
        }
        if let Some(v) = env::var("RIME_SYNC_DAV_USER").ok() {
            self.webdav.username = v;
        }
        if let Some(v) = env::var("RIME_SYNC_DAV_PASS").ok() {
            self.webdav.password = v;
        }
        if let Some(v) = non_empty(env::var("RIME_SYNC_RIME_DIR").ok()) {
            self.rime_dir = Some(v);
        }
    }

    pub fn apply_overrides(
        &mut self,
        url: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
    ) {
        if let Some(v) = non_empty(url.map(str::to_string)) {
            self.webdav.url = v;
        }
        if let Some(v) = username {
            self.webdav.username = v.to_string();
        }
        if let Some(v) = password {
            self.webdav.password = v.to_string();
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.webdav.url.trim().is_empty() {
            bail!("webdav.url is empty; set it in config or pass --dav-url");
        }
        if !(self.webdav.url.starts_with("http://") || self.webdav.url.starts_with("https://")) {
            bail!("webdav.url must start with http:// or https://");
        }
        if self.device_id.trim().is_empty() {
            bail!("device_id is empty; cannot identify sync subdirectory");
        }
        Ok(())
    }

    pub fn wants(&self, category: Category) -> bool {
        match category {
            Category::UserdbSnapshot => self.sync_kinds.userdict,
            Category::CustomYaml => self.sync_kinds.custom_yaml,
            Category::DictSource => self.sync_kinds.dict_sources,
            _ => false,
        }
    }

    pub fn excluded(&self, rel: &str) -> bool {
        self.exclude_globs
            .iter()
            .any(|pattern| crate::plan::glob_match(pattern, rel))
    }

    /// 优先级：config.device_id > installation.yaml > 生成并写回。
    pub fn resolve_device_id(&mut self, rime_dir_path: &Path) -> String {
        if let Some(existing) = non_empty(Some(self.device_id.clone())) {
            return existing;
        }
        if let Some(id) = rime_dir::installation_id(rime_dir_path) {
            self.device_id = id.clone();
            return id;
        }
        let id = generate_device_id();
        self.device_id = id.clone();
        id
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

pub fn generate_device_id() -> String {
    format!("{}-{}", hostname(), random_suffix())
}

fn hostname() -> String {
    let raw = env::var("HOSTNAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            fs::read_to_string("/etc/hostname")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "device".to_string());
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    sanitized.trim_matches('-').to_string()
}

fn random_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}:{}:{}", nanos, std::process::id(), hostname());
    sha256_hex(seed.as_bytes())[..6].to_string()
}

/// 默认配置路径（Tauri 端会覆盖为 app_config_dir）。
pub fn default_config_path() -> PathBuf {
    if let Some(path) = non_empty(env::var("RIME_SYNC_CONFIG").ok()) {
        return PathBuf::from(path);
    }
    if let Some(dir) = non_empty(env::var("XDG_CONFIG_HOME").ok()) {
        return PathBuf::from(dir).join("rime-sync").join("config.json");
    }
    if let Some(home) = non_empty(env::var("HOME").ok()) {
        return PathBuf::from(home)
            .join(".config")
            .join("rime-sync")
            .join("config.json");
    }
    PathBuf::from("rime-sync-config.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn save_sets_0600_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("rime-sync-cfg-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let mut config = Config::default();
        config.webdav.url = "http://127.0.0.1:18080".into();
        config.webdav.password = "secret".into();
        config.save(&path).expect("save");
        let loaded = Config::load(&path).expect("load");
        assert_eq!(loaded.webdav.url, "http://127.0.0.1:18080");
        assert_eq!(loaded.webdav.password, "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn device_id_falls_back_to_installation_yaml() {
        let dir = std::env::temp_dir().join(format!("rime-sync-dev-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let mut file = fs::File::create(dir.join("installation.yaml")).expect("create");
        writeln!(file, "installation_id: devA").expect("write");
        let mut config = Config::default();
        let id = config.resolve_device_id(&dir);
        assert_eq!(id, "devA");
        assert_eq!(config.device_id, "devA");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generated_device_id_is_single_segment() {
        let id = generate_device_id();
        assert!(!id.contains('/'));
        assert!(id.len() > 7);
    }
}
