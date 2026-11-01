//! 出站代理：HTTP / HTTPS / SOCKS5（可选认证）。
//!
//! 这里只描述"怎么连代理"，不决定"哪些请求走代理"：本项目不做绕过列表，
//! 启用后所有 WebDAV 请求统一走代理；未启用时保持 reqwest 的默认行为
//! （HTTP_PROXY / HTTPS_PROXY / ALL_PROXY 环境变量仍可作为兜底）。

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::PasswordRef;
use crate::secret::SecretStore;

/// 代理密码在密钥后端里的固定 account（与 WebDAV 用户名的 account 区分开）。
pub const PROXY_PASSWORD_ACCOUNT: &str = "proxy-password";

/// 允许的代理协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl ProxyKind {
    pub fn parse(scheme: &str) -> Option<Self> {
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            "socks5" => Some(Self::Socks5),
            "socks5h" => Some(Self::Socks5h),
            _ => None,
        }
    }

    pub const fn scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
            Self::Socks5h => "socks5h",
        }
    }

    /// 地址里没写端口时的默认值（与 curl/reqwest 的习惯一致）。
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
            Self::Socks5 | Self::Socks5h => 1080,
        }
    }

    pub const fn is_socks(self) -> bool {
        matches!(self, Self::Socks5 | Self::Socks5h)
    }
}

impl std::fmt::Display for ProxyKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.scheme())
    }
}

/// 解析后的代理地址（不含任何凭据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
}

impl ProxySpec {
    /// 只接受 http/https/socks5/socks5h：没有 host、带路径/查询参数、
    /// 或把用户名密码写进 URL 的一律拒绝——错误必须能直接展示给用户。
    pub fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            bail!("proxy.url is empty; set proxy.url or pass --proxy <url>");
        }
        if trimmed.chars().any(char::is_whitespace) {
            bail!("proxy.url contains whitespace: {trimmed}");
        }
        let parsed = reqwest::Url::parse(trimmed)
            .with_context(|| format!("invalid proxy.url: {trimmed}"))?;
        let kind = ProxyKind::parse(parsed.scheme()).ok_or_else(|| {
            anyhow!(
                "unsupported proxy scheme `{}` in proxy.url: use http://, https://, socks5:// or socks5h://",
                parsed.scheme()
            )
        })?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            bail!(
                "proxy.url must not embed credentials; set proxy.username and store the password in the secret backend"
            );
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("proxy.url must not contain a query or fragment: {trimmed}");
        }
        let path = parsed.path();
        if !path.is_empty() && path != "/" {
            bail!("proxy.url must not contain a path: {trimmed}");
        }
        let host = parsed
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| anyhow!("proxy.url is missing a host: {trimmed}"))?;
        let port = parsed.port().unwrap_or_else(|| kind.default_port());
        if port == 0 {
            bail!("proxy.url has an invalid port 0: {trimmed}");
        }
        Ok(Self {
            kind,
            host: host.to_string(),
            port,
        })
    }

    /// 归一化成 reqwest 能接受的形态（scheme://host:port，IPv6 带方括号）。
    pub fn to_url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{}://{}:{}", self.kind, host, self.port)
    }
}

/// config.json 里的 proxy 段。
///
/// 旧配置没有这个段时 serde 落到默认值（disabled），保持向后兼容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub url: String,
    pub username: String,
    /// 只在内存里：绝不序列化（密码走 secret backend，与 WebDAV 密码同一套机制）。
    ///
    /// 反序列化必须接受该字段：GUI 的 cmd_set_config 会把用户刚输入的密码随
    /// Config 一起传进来；写入磁盘时由 skip_serializing 丢弃。
    #[serde(default, skip_serializing)]
    pub password: String,
    /// 密码存放位置（keyring/file/none），语义与 WebDAV 的 PasswordRef 完全一致。
    pub password_ref: PasswordRef,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            password_ref: PasswordRef::None,
        }
    }
}

impl ProxyConfig {
    pub fn is_active(&self) -> bool {
        self.enabled
    }

    pub fn spec(&self) -> Result<ProxySpec> {
        ProxySpec::parse(&self.url)
    }

    /// 启用时才校验；禁用的配置可以留着不完整的地址（用户可能刚清空）。
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.spec()?;
        if self.username.trim().is_empty() && !self.password.is_empty() {
            bail!("proxy.username is required when a proxy password is set");
        }
        Ok(())
    }

    /// 构造 reqwest 代理；未启用时返回 None（此时 reqwest 仍会读环境变量）。
    pub fn proxy(&self) -> Result<Option<reqwest::Proxy>> {
        if !self.enabled {
            return Ok(None);
        }
        let spec = self.spec()?;
        let url = spec.to_url();
        let mut proxy =
            reqwest::Proxy::all(&url).with_context(|| format!("build proxy for {url} failed"))?;
        if !self.username.trim().is_empty() {
            proxy = proxy.basic_auth(self.username.trim(), &self.password);
        }
        Ok(Some(proxy))
    }

    /// RFC 7617 认证头（HTTP 代理的非隧道请求使用）。
    ///
    /// reqwest 内部会生成同样的头；这里保留一个纯函数用于单测与诊断展示，
    /// 密码永远不落盘、也不出现在日志里。
    pub fn auth_header(&self) -> Option<String> {
        let username = self.username.trim();
        if username.is_empty() {
            return None;
        }
        let raw = format!("{username}:{}", self.password);
        Some(format!("Basic {}", base64_encode(raw.as_bytes())))
    }

    /// 不含凭据的展示形式（日志/CLI 输出用）。
    pub fn redacted_url(&self) -> String {
        match self.spec() {
            Ok(spec) => spec.to_url(),
            Err(_) => "<invalid proxy url>".to_string(),
        }
    }

    /// 一行描述：禁用 / 地址 + 是否带认证。
    pub fn describe(&self) -> String {
        if !self.enabled {
            return "disabled".to_string();
        }
        let auth = if self.username.trim().is_empty() {
            "no-auth"
        } else {
            "auth"
        };
        format!("{} ({auth})", self.redacted_url())
    }

    /// 把密码写进密钥后端，并记录 password_ref。
    pub fn store_password(&mut self, store: &dyn SecretStore, password: &str) -> Result<()> {
        store
            .set(PROXY_PASSWORD_ACCOUNT, password)
            .with_context(|| "store proxy password in secret backend")?;
        self.password_ref = store.backend().password_ref();
        self.password = password.to_string();
        Ok(())
    }

    /// 内存里已有密码（--proxy-pass 之类）时优先；否则按 password_ref 从后端读。
    pub fn load_password(&mut self, store: &dyn SecretStore) -> Result<Option<String>> {
        if !self.password.is_empty() {
            return Ok(Some(self.password.clone()));
        }
        if self.password_ref == PasswordRef::None {
            return Ok(None);
        }
        match store.get(PROXY_PASSWORD_ACCOUNT)? {
            Some(password) => {
                self.password = password.clone();
                Ok(Some(password))
            }
            None => Ok(None),
        }
    }
}

/// 极小的 base64 编码器：只服务 Basic 认证头，避免为一行编码引入依赖。
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (a << 16) | (b << 8) | c;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::{FileStore, SecretBackend, SecretStore};

    fn enabled(url: &str) -> ProxyConfig {
        ProxyConfig {
            enabled: true,
            url: url.to_string(),
            ..ProxyConfig::default()
        }
    }

    #[test]
    fn parses_all_supported_schemes() {
        for (url, kind) in [
            ("http://127.0.0.1:7890", ProxyKind::Http),
            ("https://proxy.example:8443", ProxyKind::Https),
            ("socks5://127.0.0.1:1080", ProxyKind::Socks5),
            ("socks5h://127.0.0.1:1080", ProxyKind::Socks5h),
            ("SOCKS5://127.0.0.1:1080", ProxyKind::Socks5),
            ("  socks5h://proxy.local:1081  ", ProxyKind::Socks5h),
        ] {
            let spec = ProxySpec::parse(url).expect(url);
            assert_eq!(spec.kind, kind, "{url}");
        }
    }

    #[test]
    fn rejects_unknown_schemes_with_readable_error() {
        for url in [
            "ftp://127.0.0.1:21",
            "socks4://127.0.0.1:1080",
            "127.0.0.1:7890",
        ] {
            let error = ProxySpec::parse(url).expect_err(url);
            let text = format!("{error:#}");
            assert!(
                text.contains("unsupported proxy scheme")
                    || text.contains("relative URL")
                    || text.contains("invalid proxy.url"),
                "{url} -> {text}"
            );
        }
    }

    #[test]
    fn rejects_missing_host_bad_port_and_paths() {
        for (url, needle) in [
            ("http://", "empty host"),
            ("http://:7890", "empty host"),
            ("http://127.0.0.1:0", "port 0"),
            ("http://127.0.0.1:70000", "invalid port"),
            ("http://127.0.0.1:7890/dav", "must not contain a path"),
            ("socks5://127.0.0.1:1080/?x=1", "query"),
            ("http://u:p@127.0.0.1:7890", "must not embed credentials"),
            ("   ", "proxy.url is empty"),
        ] {
            let error = ProxySpec::parse(url).expect_err(url);
            let text = format!("{error:#}");
            assert!(text.contains(needle), "{url} -> {text}");
        }
    }

    #[test]
    fn default_ports_follow_scheme() {
        assert_eq!(ProxySpec::parse("http://127.0.0.1").expect("http").port, 80);
        assert_eq!(
            ProxySpec::parse("https://127.0.0.1").expect("https").port,
            443
        );
        assert_eq!(
            ProxySpec::parse("socks5://127.0.0.1").expect("socks5").port,
            1080
        );
        assert_eq!(
            ProxySpec::parse("socks5h://127.0.0.1")
                .expect("socks5h")
                .port,
            1080
        );
        assert_eq!(
            ProxySpec::parse("http://[::1]:7890")
                .expect("ipv6")
                .to_url(),
            "http://[::1]:7890"
        );
    }

    #[test]
    fn default_config_is_disabled_and_builds_no_proxy() {
        let config = ProxyConfig::default();
        assert!(!config.is_active());
        assert!(config.proxy().expect("no proxy").is_none());
        assert!(config.validate().is_ok(), "disabled config must not block");
        assert_eq!(config.describe(), "disabled");
        assert!(config.auth_header().is_none());
    }

    #[test]
    fn disabled_config_ignores_invalid_url() {
        // 用户可能刚清空地址但忘了取消勾选：禁用状态不应让保存/同步失败。
        let config = ProxyConfig {
            enabled: false,
            url: "not a url".to_string(),
            ..ProxyConfig::default()
        };
        assert!(config.validate().is_ok());
        assert!(config.proxy().expect("none").is_none());
    }

    #[test]
    fn enabled_builds_proxy_for_every_scheme() {
        for url in [
            "http://127.0.0.1:7890",
            "https://127.0.0.1:8443",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            let config = enabled(url);
            config.validate().expect(url);
            let proxy = config.proxy().expect(url).expect("proxy built");
            drop(proxy);
        }
    }

    #[test]
    fn enabled_rejects_invalid_url_at_build_time() {
        let config = enabled("http://127.0.0.1:7890/some/path");
        assert!(config.proxy().is_err());
        assert!(config.validate().is_err());
    }

    #[test]
    fn password_without_username_is_rejected() {
        let mut config = enabled("http://127.0.0.1:7890");
        config.password = "secret".to_string();
        let error = config.validate().expect_err("must fail");
        assert!(format!("{error}").contains("proxy.username is required"));
    }

    #[test]
    fn auth_header_is_rfc7617_basic() {
        let mut config = enabled("http://127.0.0.1:7890");
        assert!(config.auth_header().is_none(), "no username = no header");
        config.username = "alice".to_string();
        config.password = "s3cret".to_string();
        assert_eq!(
            config.auth_header().as_deref(),
            Some("Basic YWxpY2U6czNjcmV0")
        );
        config.username = "user".to_string();
        config.password = "pass".to_string();
        assert_eq!(config.auth_header().as_deref(), Some("Basic dXNlcjpwYXNz"));
        // 空密码也要生成头（有些代理只校验用户名）。
        config.password.clear();
        assert_eq!(config.auth_header().as_deref(), Some("Basic dXNlcjo="));
        // 用户名两侧空白被忽略。
        config.username = "  user  ".to_string();
        assert_eq!(config.auth_header().as_deref(), Some("Basic dXNlcjo="));
    }

    #[test]
    fn password_never_serializes_but_is_accepted_for_ipc() {
        let mut config = enabled("http://127.0.0.1:7890");
        config.username = "alice".to_string();
        config.password = "top-secret".to_string();
        let json = serde_json::to_value(&config).expect("serialize");
        assert!(json.get("password").is_none(), "{json}");
        assert_eq!(json["password_ref"], serde_json::json!("none"));
        assert_eq!(json["enabled"], serde_json::json!(true));

        let parsed: ProxyConfig = serde_json::from_str(
            r#"{"enabled":true,"url":"http://127.0.0.1:7890","password":"plaintext"}"#,
        )
        .expect("deserialize");
        // GUI 的 set_config 需要接收刚输入的密码；但再保存时绝不会写回磁盘。
        assert_eq!(parsed.password, "plaintext");
        let roundtrip = serde_json::to_value(&parsed).expect("serialize");
        assert!(roundtrip.get("password").is_none(), "{roundtrip}");
        assert!(parsed.enabled);
    }

    #[test]
    fn missing_proxy_section_defaults_to_disabled() {
        let json = r#"{"enabled":true,"url":"socks5://127.0.0.1:1080"}"#;
        let config: ProxyConfig = serde_json::from_str("{}").expect("empty");
        assert!(!config.enabled);
        assert_eq!(config.url, "");
        assert_eq!(config.password_ref, PasswordRef::None);
        let explicit: ProxyConfig = serde_json::from_str(json).expect("explicit");
        assert!(explicit.enabled);
        assert!(ProxySpec::parse(&explicit.url).is_ok());
    }

    #[test]
    fn password_roundtrips_through_secret_backend() {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-proxy-secret-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = FileStore::new(&dir.join("secrets.json"));
        let mut config = enabled("socks5://127.0.0.1:1080");
        config.username = "alice".to_string();
        config.store_password(&store, "hunter2").expect("store");
        assert_eq!(config.password_ref, PasswordRef::File);
        assert_eq!(config.password, "hunter2");

        let mut reloaded = enabled("socks5://127.0.0.1:1080");
        reloaded.username = "alice".to_string();
        reloaded.password_ref = config.password_ref;
        assert_eq!(
            reloaded.load_password(&store).expect("load"),
            Some("hunter2".to_string())
        );
        assert_eq!(reloaded.password, "hunter2");
        assert_eq!(
            store.get(PROXY_PASSWORD_ACCOUNT).expect("get"),
            Some("hunter2".to_string())
        );
        assert_eq!(store.backend(), SecretBackend::File);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
