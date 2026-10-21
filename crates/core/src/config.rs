//! config.json 的加载、0600 权限写回与环境变量覆盖。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::sha256_hex;
use crate::plan::Category;
use crate::rime_dir;
use crate::secret::SecretStore;

/// config.json 当前 schema 版本（SPEC-4 §A4）。
pub const CONFIG_SCHEMA_VERSION: u32 = 2;

/// 首次接入策略：preview=只拉不推并提示；pull-only=只拉不推；normal=双向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirstSyncPolicy {
    #[default]
    Preview,
    PullOnly,
    Normal,
}

impl FirstSyncPolicy {
    pub fn id(&self) -> &'static str {
        match self {
            FirstSyncPolicy::Preview => "preview",
            FirstSyncPolicy::PullOnly => "pull-only",
            FirstSyncPolicy::Normal => "normal",
        }
    }

    /// preview / pull-only 在首次接入时都只拉不推。
    pub fn pull_only(&self) -> bool {
        !matches!(self, FirstSyncPolicy::Normal)
    }

    /// 解析配置/环境里的取值；未知取值返回 None（调用方保持原值）。
    pub fn from_id(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "preview" => Some(Self::Preview),
            "pull-only" | "pull_only" => Some(Self::PullOnly),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }
}

/// 一次 schema 迁移的结果（幂等：已是最新时 changed=false）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub from: u32,
    pub to: u32,
    pub changed: bool,
    pub password_migrated: bool,
}

/// 密码存放位置。None = 尚未设置（或旧配置还没迁移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordRef {
    Keyring,
    File,
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavConfig {
    pub url: String,
    pub username: String,
    /// 只在内存里：绝不序列化（SPEC-2 §2.2 不允许明文密码落盘）。
    #[serde(default, skip_serializing)]
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
    /// 密码保存在钥匙串还是 0600 文件里。
    pub password_ref: PasswordRef,
    pub rime_dir: Option<String>,
    pub device_id: String,
    pub transport_interval_minutes: u32,
    pub sync_kinds: SyncKinds,
    pub exclude_globs: Vec<String>,
    pub stop_fcitx5_during_apply: bool,
    /// 配置 schema 版本（写入文件；缺失按 v1 处理）。
    pub schema_version: u32,
    /// 首次接入策略，默认 preview（只拉不推）。
    pub first_sync_policy: FirstSyncPolicy,
    /// 旧版配置里读到明文 password：迁移后清掉。
    #[serde(skip)]
    pub legacy_password: bool,
    /// 从原始 JSON 探测到的文件版本（不落盘）。
    #[serde(skip)]
    pub schema_detected: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            webdav: WebDavConfig::default(),
            password_ref: PasswordRef::None,
            rime_dir: None,
            device_id: String::new(),
            transport_interval_minutes: 10,
            sync_kinds: SyncKinds::default(),
            exclude_globs: Vec::new(),
            stop_fcitx5_during_apply: true,
            schema_version: CONFIG_SCHEMA_VERSION,
            first_sync_policy: FirstSyncPolicy::default(),
            legacy_password: false,
            schema_detected: CONFIG_SCHEMA_VERSION,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mut config: Self = serde_json::from_str(&text)
                    .with_context(|| format!("parse config {}", path.display()))?;
                config.legacy_password = has_plaintext_password(&text);
                config.schema_detected = detected_schema_version(&text);
                config.schema_version = config.schema_detected;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read config {}", path.display())),
        }
    }

    /// 钥匙串/文件后端的 key：WebDAV 用户名。
    pub fn password_account(&self) -> String {
        self.webdav.username.trim().to_string()
    }

    /// 内存里的密码优先（--dav-pass / 环境变量），其次从后端读。
    pub fn load_password(&mut self, store: &dyn SecretStore) -> Result<Option<String>> {
        if !self.webdav.password.is_empty() {
            return Ok(Some(self.webdav.password.clone()));
        }
        if self.password_ref == PasswordRef::None {
            return Ok(None);
        }
        match store.get(&self.password_account())? {
            Some(password) => {
                self.webdav.password = password.clone();
                Ok(Some(password))
            }
            None => Ok(None),
        }
    }

    /// 旧配置的明文 password → 后端，然后重写不含 password 的 JSON。
    pub fn migrate_legacy_password(
        &mut self,
        store: &dyn SecretStore,
        path: &Path,
    ) -> Result<bool> {
        if !self.legacy_password {
            return Ok(false);
        }
        let password = self.webdav.password.clone();
        self.legacy_password = false;
        if !password.is_empty() {
            store
                .set(&self.password_account(), &password)
                .with_context(|| "store webdav password in secret backend")?;
            self.password_ref = store.backend().password_ref();
        }
        self.save(path)?;
        Ok(true)
    }

    /// SPEC-4 §A4：v1（无 schema_version / 明文密码）→ v2。
    ///
    /// 幂等：已是最新版本且无明文密码时不写盘。迁移失败时原文件保持不变，
    /// 错误信息里明确说明这一点（绝不静默丢配置）。
    pub fn migrate(&mut self, store: &dyn SecretStore, path: &Path) -> Result<MigrationReport> {
        let from = self.schema_version;
        if from > CONFIG_SCHEMA_VERSION {
            bail!(
                "config schema_version {from} is newer than supported {CONFIG_SCHEMA_VERSION}; original file kept: {}",
                path.display()
            );
        }
        let mut report = MigrationReport {
            from,
            to: CONFIG_SCHEMA_VERSION,
            changed: false,
            password_migrated: false,
        };
        if self.legacy_password {
            let password = self.webdav.password.clone();
            if !password.is_empty() {
                store
                    .set(&self.password_account(), &password)
                    .with_context(|| {
                        format!(
                            "migrate {} to schema v{CONFIG_SCHEMA_VERSION}: store password in {} failed; original config kept",
                            path.display(),
                            store.backend().as_str()
                        )
                    })?;
                self.password_ref = store.backend().password_ref();
                report.password_migrated = true;
            }
            report.changed = true;
        }
        if from < CONFIG_SCHEMA_VERSION {
            self.schema_version = CONFIG_SCHEMA_VERSION;
            report.changed = true;
        }
        if !report.changed {
            return Ok(report);
        }
        self.save(path).with_context(|| {
            format!(
                "write migrated config {} (schema v{from} -> v{CONFIG_SCHEMA_VERSION}); original config kept",
                path.display()
            )
        })?;
        self.legacy_password = false;
        self.schema_detected = CONFIG_SCHEMA_VERSION;
        Ok(report)
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
        fs::write(&tmp, format!("{text}\n")).with_context(|| format!("write {}", tmp.display()))?;
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
        if let Ok(v) = env::var("RIME_SYNC_DAV_USER") {
            self.webdav.username = v;
        }
        if let Ok(v) = env::var("RIME_SYNC_DAV_PASS") {
            self.webdav.password = v;
        }
        if let Some(v) = non_empty(env::var("RIME_SYNC_RIME_DIR").ok()) {
            self.rime_dir = Some(v);
        }
        if let Ok(v) = env::var("RIME_SYNC_DAV_DIR") {
            self.webdav.remote_dir = v.trim().to_string();
        }
        if let Some(v) = non_empty(env::var("RIME_SYNC_FIRST_SYNC_POLICY").ok()) {
            if let Some(policy) = FirstSyncPolicy::from_id(&v) {
                self.first_sync_policy = policy;
            }
        }
    }

    pub fn apply_overrides(
        &mut self,
        url: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
        remote_dir: Option<&str>,
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
        // SPEC-3 §A4：允许显式传空串 = 服务器根，所以这里不做 non_empty 过滤。
        if let Some(v) = remote_dir {
            self.webdav.remote_dir = v.trim().to_string();
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

/// 旧配置里 webdav.password 是非空字符串 → 需要迁移进钥匙串。
fn has_plaintext_password(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("webdav")
        .and_then(|webdav| webdav.get("password"))
        .and_then(|password| password.as_str())
        .map(|password| !password.is_empty())
        .unwrap_or(false)
}

/// 原始 JSON 里的 schema_version；缺失 = v1。
fn detected_schema_version(text: &str) -> u32 {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("schema_version")
                .and_then(|version| version.as_u64())
        })
        .map(|version| version as u32)
        .unwrap_or(1)
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
    use crate::secret::SecretBackend;
    use std::io::Write;

    struct FailingStore;

    impl SecretStore for FailingStore {
        fn backend(&self) -> SecretBackend {
            SecretBackend::File
        }

        fn get(&self, _account: &str) -> Result<Option<String>> {
            Ok(None)
        }

        fn set(&self, _account: &str, _secret: &str) -> Result<()> {
            bail!("secret backend is locked")
        }

        fn delete(&self, _account: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn v1_config_migrates_to_v2_idempotently() {
        let dir = std::env::temp_dir().join(format!("rime-sync-schema-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice","password":"legacy"}}"#,
        )
        .expect("write");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        let mut config = Config::load(&path).expect("load");
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.first_sync_policy, FirstSyncPolicy::Preview);

        let report = config.migrate(&store, &path).expect("migrate");
        assert_eq!(report.from, 1);
        assert_eq!(report.to, CONFIG_SCHEMA_VERSION);
        assert!(report.changed);
        assert!(report.password_migrated);
        let migrated = fs::read_to_string(&path).expect("read");
        let value: serde_json::Value = serde_json::from_str(&migrated).expect("json");
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["first_sync_policy"], "preview");
        assert!(value["webdav"].get("password").is_none());
        assert!(!migrated.contains("legacy"));

        let mut reloaded = Config::load(&path).expect("reload");
        assert_eq!(reloaded.schema_version, 2);
        let again = reloaded.migrate(&store, &path).expect("migrate twice");
        assert!(!again.changed);
        assert!(!again.password_migrated);
        assert_eq!(fs::read_to_string(&path).expect("read again"), migrated);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migration_failure_keeps_original_file() {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-schema-fail-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let original =
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice","password":"legacy"}}"#;
        fs::write(&path, original).expect("write");
        let mut config = Config::load(&path).expect("load");
        let error = config.migrate(&FailingStore, &path).expect_err("must fail");
        assert!(format!("{error:#}").contains("original config kept"));
        assert_eq!(fs::read_to_string(&path).expect("read"), original);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newer_schema_is_rejected_without_writing() {
        let dir = std::env::temp_dir().join(format!("rime-sync-schema-new-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let original = r#"{"schema_version":9,"webdav":{"url":"http://127.0.0.1:1"}}"#;
        fs::write(&path, original).expect("write");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        let mut config = Config::load(&path).expect("load");
        assert_eq!(config.schema_version, 9);
        assert!(config.migrate(&store, &path).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), original);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_sync_policy_from_id_roundtrips() {
        assert_eq!(
            FirstSyncPolicy::from_id("preview"),
            Some(FirstSyncPolicy::Preview)
        );
        assert_eq!(
            FirstSyncPolicy::from_id(" Pull-Only "),
            Some(FirstSyncPolicy::PullOnly)
        );
        assert_eq!(
            FirstSyncPolicy::from_id("normal"),
            Some(FirstSyncPolicy::Normal)
        );
        assert_eq!(FirstSyncPolicy::from_id("bogus"), None);
    }

    #[test]
    fn missing_schema_version_is_v1() {
        assert_eq!(detected_schema_version(r#"{"a":1}"#), 1);
        assert_eq!(detected_schema_version(r#"{"schema_version":2}"#), 2);
        assert_eq!(detected_schema_version("not json"), 1);
    }

    #[test]
    fn save_sets_0600_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("rime-sync-cfg-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        let mut config = Config::default();
        config.webdav.url = "http://127.0.0.1:18080".into();
        config.webdav.password = "secret".into();
        config.password_ref = PasswordRef::File;
        config.save(&path).expect("save");
        let loaded = Config::load(&path).expect("load");
        assert_eq!(loaded.webdav.url, "http://127.0.0.1:18080");
        assert_eq!(loaded.password_ref, PasswordRef::File);
        // SPEC-2 §2.2：明文密码不再落盘，只在内存里。
        let raw = fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("\"password\""), "{raw}");
        assert_eq!(config.webdav.password, "secret");
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

    #[test]
    fn legacy_plaintext_password_is_migrated_and_removed() {
        let dir = std::env::temp_dir().join(format!("rime-sync-mig-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"webdav":{"url":"http://127.0.0.1:1","username":"alice","password":"legacy"}}"#,
        )
        .expect("write");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        let mut config = Config::load(&path).expect("load");
        assert!(config.legacy_password);
        assert_eq!(config.webdav.password, "legacy");
        assert!(config
            .migrate_legacy_password(&store, &path)
            .expect("migrate"));
        assert_eq!(config.password_ref, PasswordRef::File);
        let raw = fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("legacy"), "{raw}");
        assert!(!raw.contains("\"password\""));
        let reloaded = Config::load(&path).expect("reload");
        assert!(!reloaded.legacy_password);
        assert_eq!(reloaded.webdav.password, "");
        assert_eq!(store.get("alice").expect("get"), Some("legacy".to_string()));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_password_prefers_memory_then_backend() {
        let dir = std::env::temp_dir().join(format!("rime-sync-pw-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let store = crate::secret::FileStore::new(&dir.join("secrets.json"));
        store.set("alice", "stored").expect("set");
        let mut config = Config::default();
        config.webdav.username = "alice".into();
        config.password_ref = PasswordRef::File;
        assert_eq!(
            config.load_password(&store).expect("load"),
            Some("stored".into())
        );
        config.webdav.password = "override".into();
        assert_eq!(
            config.load_password(&store).expect("load"),
            Some("override".into())
        );
        config.password_ref = PasswordRef::None;
        config.webdav.password.clear();
        assert_eq!(config.load_password(&store).expect("load"), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plaintext_detection_only_looks_at_webdav_password() {
        assert!(has_plaintext_password(r#"{"webdav":{"password":"x"}}"#));
        assert!(!has_plaintext_password(
            r#"{"webdav":{"password_ref":"file"}}"#
        ));
        assert!(!has_plaintext_password(r#"{"webdav":{"password":""}}"#));
        assert!(!has_plaintext_password("not json"));
    }
}
