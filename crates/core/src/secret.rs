//! 密码存储：系统钥匙串优先，无 Secret Service 的 headless 环境回落 0600 文件。
//!
//! 明文密码不再写入 config.json；config 只记录 password_ref（Keyring / File / None）。

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::PasswordRef;

pub const SERVICE: &str = "rime-sync";
pub const SECRETS_FILE: &str = "secrets.json";
/// 测试/运维钩子：RIME_SYNC_SECRET_BACKEND=file 可强制走文件后端。
pub const BACKEND_ENV: &str = "RIME_SYNC_SECRET_BACKEND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBackend {
    Keyring,
    File,
}

impl SecretBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretBackend::Keyring => "keyring",
            SecretBackend::File => "file",
        }
    }

    pub fn password_ref(&self) -> PasswordRef {
        match self {
            SecretBackend::Keyring => PasswordRef::Keyring,
            SecretBackend::File => PasswordRef::File,
        }
    }
}

pub trait SecretStore: Send + Sync {
    fn backend(&self) -> SecretBackend;
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

/// 系统钥匙串（service = "rime-sync"，account = WebDAV 用户名）。
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self {
            service: SERVICE.to_string(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, account)
            .with_context(|| format!("open keyring entry for account {account}"))
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for KeyringStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyringStore")
            .field("service", &self.service)
            .finish()
    }
}

impl SecretStore for KeyringStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::Keyring
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(anyhow::anyhow!("keyring read failed: {error}")),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.entry(account)?
            .set_password(secret)
            .map_err(|error| anyhow::anyhow!("keyring write failed: {error}"))
    }

    fn delete(&self, account: &str) -> Result<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(anyhow::anyhow!("keyring delete failed: {error}")),
        }
    }
}

/// 0600 文件兜底：headless / 无 Secret Service 时使用。
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<BTreeMap<String, String>> {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parse secret file {}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => {
                Err(error).with_context(|| format!("read secret file {}", self.path.display()))
            }
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let text = serde_json::to_string_pretty(map)?;
        fs::write(&self.path, format!("{text}\n"))
            .with_context(|| format!("write {}", self.path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", self.path.display()))?;
        }
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn backend(&self) -> SecretBackend {
        SecretBackend::File
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.load()?.remove(account))
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        let mut map = self.load()?;
        map.insert(account.to_string(), secret.to_string());
        self.save(&map)
    }

    fn delete(&self, account: &str) -> Result<()> {
        let mut map = self.load()?;
        if map.remove(account).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }
}

/// detect() 的结果：实际使用的后端 + 回落说明（用于 CLI 提示，不含任何密码）。
pub struct SecretSelection {
    pub store: Box<dyn SecretStore>,
    pub backend: SecretBackend,
    pub note: Option<String>,
}

impl SecretSelection {
    pub fn describe(&self) -> String {
        match &self.note {
            Some(note) => format!("{}: {note}", self.backend.as_str()),
            None => self.backend.as_str().to_string(),
        }
    }
}

impl std::fmt::Debug for SecretSelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretSelection")
            .field("backend", &self.backend)
            .field("note", &self.note)
            .finish()
    }
}

/// 优先系统钥匙串；探测失败（无 Secret Service / 无会话总线）则回落 0600 文件。
pub fn detect(app_data_dir: &Path) -> SecretSelection {
    detect_with(app_data_dir, Box::new(KeyringStore::new()))
}

fn detect_with(app_data_dir: &Path, primary: Box<dyn SecretStore>) -> SecretSelection {
    let fallback = || FileStore::new(&app_data_dir.join(SECRETS_FILE));
    if std::env::var(BACKEND_ENV)
        .map(|value| value.eq_ignore_ascii_case("file"))
        .unwrap_or(false)
    {
        return SecretSelection {
            backend: SecretBackend::File,
            note: Some(format!("forced by {BACKEND_ENV}=file")),
            store: Box::new(fallback()),
        };
    }
    match probe(primary.as_ref()) {
        Ok(()) => SecretSelection {
            backend: primary.backend(),
            note: None,
            store: primary,
        },
        Err(error) => {
            tracing::warn!("keyring unavailable, falling back to 0600 file store: {error}");
            SecretSelection {
                backend: SecretBackend::File,
                note: Some(format!("keyring unavailable ({error}); using 0600 file")),
                store: Box::new(fallback()),
            }
        }
    }
}

/// 真的写一次再读一次，避免“Entry 能创建但 Secret Service 不存在”的假可用。
fn probe(store: &dyn SecretStore) -> Result<()> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let account = format!("probe-{}-{nanos}", std::process::id());
    store
        .set(&account, "probe")
        .with_context(|| format!("{} probe write failed", store.backend().as_str()))?;
    let read = store.get(&account).unwrap_or(None);
    let _ = store.delete(&account);
    if read.as_deref() != Some("probe") {
        anyhow::bail!("{} probe read-back mismatch", store.backend().as_str());
    }
    Ok(())
}

/// 生成 stderr 可用的一行状态（永不包含密码）。
pub fn describe_selection(selection: &SecretSelection) -> String {
    selection.describe()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-secret-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[allow(dead_code)]
    struct FailingStore;

    impl SecretStore for FailingStore {
        fn backend(&self) -> SecretBackend {
            SecretBackend::Keyring
        }

        fn get(&self, _account: &str) -> Result<Option<String>> {
            bail!("no session bus")
        }

        fn set(&self, _account: &str, _secret: &str) -> Result<()> {
            bail!("no session bus")
        }

        fn delete(&self, _account: &str) -> Result<()> {
            bail!("no session bus")
        }
    }

    /// 读得到但读回不一致的“假可用”后端也必须被识破。
    struct LyingStore;

    impl SecretStore for LyingStore {
        fn backend(&self) -> SecretBackend {
            SecretBackend::Keyring
        }

        fn get(&self, _account: &str) -> Result<Option<String>> {
            Ok(Some("wrong".to_string()))
        }

        fn set(&self, _account: &str, _secret: &str) -> Result<()> {
            Ok(())
        }

        fn delete(&self, _account: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn file_store_roundtrip_and_0600() {
        let dir = temp_dir("file");
        let path = dir.join(SECRETS_FILE);
        let store = FileStore::new(&path);
        assert_eq!(store.get("alice").expect("get"), None);
        store.set("alice", "s3cret").expect("set");
        assert_eq!(store.get("alice").expect("get"), Some("s3cret".into()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        store.delete("alice").expect("delete");
        assert_eq!(store.get("alice").expect("get"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_store_keeps_accounts_separate() {
        let dir = temp_dir("accounts");
        let store = FileStore::new(&dir.join(SECRETS_FILE));
        store.set("alice", "a").expect("set a");
        store.set("bob", "b").expect("set b");
        assert_eq!(store.get("alice").expect("get"), Some("a".into()));
        assert_eq!(store.get("bob").expect("get"), Some("b".into()));
        store.delete("alice").expect("delete");
        assert_eq!(store.get("bob").expect("get"), Some("b".into()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_falls_back_to_file_when_keyring_fails() {
        let dir = temp_dir("fallback");
        let selection = detect_with(&dir, Box::new(FailingStore));
        assert_eq!(selection.backend, SecretBackend::File);
        assert!(selection.note.is_some());
        selection.store.set("u", "p").expect("set through fallback");
        assert!(dir.join(SECRETS_FILE).is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_rejects_backend_that_lies_about_writes() {
        let dir = temp_dir("lying");
        let selection = detect_with(&dir, Box::new(LyingStore));
        assert_eq!(selection.backend, SecretBackend::File);
        assert!(selection
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("keyring unavailable"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn forced_file_backend_skips_keyring() {
        let dir = temp_dir("forced");
        std::env::set_var(BACKEND_ENV, "file");
        let selection = detect_with(&dir, Box::new(FailingStore));
        std::env::remove_var(BACKEND_ENV);
        assert_eq!(selection.backend, SecretBackend::File);
        assert!(selection.note.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backend_maps_to_password_ref() {
        assert_eq!(SecretBackend::Keyring.password_ref(), PasswordRef::Keyring);
        assert_eq!(SecretBackend::File.password_ref(), PasswordRef::File);
        assert_eq!(SecretBackend::File.as_str(), "file");
    }
}
