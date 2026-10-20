//! 服务商预设：默认路径、URL 归一化、按响应头/路径识别服务商。

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Alist,
    Jianguoyun,
    Nextcloud,
    Synology,
    Custom,
}

impl Preset {
    pub fn all() -> [Preset; 5] {
        [
            Preset::Alist,
            Preset::Jianguoyun,
            Preset::Nextcloud,
            Preset::Synology,
            Preset::Custom,
        ]
    }

    pub fn id(&self) -> &'static str {
        match self {
            Preset::Alist => "alist",
            Preset::Jianguoyun => "jianguoyun",
            Preset::Nextcloud => "nextcloud",
            Preset::Synology => "synology",
            Preset::Custom => "custom",
        }
    }

    pub fn from_id(value: &str) -> Option<Preset> {
        let value = value.trim().to_ascii_lowercase();
        Self::all().into_iter().find(|preset| preset.id() == value)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Preset::Alist => "AList",
            Preset::Jianguoyun => "坚果云",
            Preset::Nextcloud => "Nextcloud",
            Preset::Synology => "群晖 Synology",
            Preset::Custom => "自定义",
        }
    }

    /// WebDAV 根路径（SPEC-2 §2.6）。
    pub fn default_path(&self) -> &'static str {
        match self {
            Preset::Alist => "/dav",
            Preset::Jianguoyun => "/dav",
            Preset::Nextcloud => "/remote.php/dav/",
            Preset::Synology => "/webdav",
            Preset::Custom => "",
        }
    }

    /// 补全 scheme / 去掉尾斜杠 / AList 补默认端口 5244 / 补默认路径。
    pub fn normalize(&self, raw: &str) -> String {
        let raw = raw.trim();
        if raw.is_empty() {
            return String::new();
        }
        let (scheme, rest) = match raw.split_once("://") {
            Some((scheme, rest)) if !scheme.is_empty() => {
                (scheme.to_ascii_lowercase(), rest.to_string())
            }
            Some(_) | None => (default_scheme(raw).to_string(), raw.to_string()),
        };
        let rest = rest.trim_start_matches('/');
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return String::new();
        }
        let mut authority = authority.to_string();
        if *self == Preset::Alist && !authority_has_port(&authority) {
            authority.push_str(":5244");
        }
        let mut path = path.trim_end_matches('/').to_string();
        if path.is_empty() {
            path = self.default_path().trim_end_matches('/').to_string();
        }
        format!("{scheme}://{authority}{path}")
    }

    /// 识别依据（SPEC-2 §2.6）：Server 头含 alist，或 /dav + Basic 认证。
    pub fn detect(headers: &Headers) -> Option<Preset> {
        let server = headers.get("server").unwrap_or("").to_ascii_lowercase();
        if server.contains("alist") {
            return Some(Preset::Alist);
        }
        let path = headers.path().unwrap_or("").to_ascii_lowercase();
        if (path == "/dav" || path.ends_with("/dav"))
            && headers
                .get("www-authenticate")
                .map(|value| value.to_ascii_lowercase().contains("basic"))
                .unwrap_or(false)
        {
            return Some(Preset::Alist);
        }
        if server.contains("jianguoyun") || server.contains("nutstore") {
            return Some(Preset::Jianguoyun);
        }
        if server.contains("nextcloud") || server.contains("owncloud") {
            return Some(Preset::Nextcloud);
        }
        if server.contains("synology") || server.contains("diskstation") {
            return Some(Preset::Synology);
        }
        None
    }

    /// 给 CLI/诊断用的可读中文建议。
    pub fn hint(&self) -> &'static str {
        match self {
            Preset::Alist => {
                "AList：地址形如 http://主机:5244/dav，需在后台为账号勾选 Webdav Read 与 Webdav Manage 权限"
            }
            Preset::Jianguoyun => "坚果云：地址形如 https://dav.jianguoyun.com/dav，使用应用密码而非登录密码",
            Preset::Nextcloud => "Nextcloud：地址形如 https://主机/remote.php/dav",
            Preset::Synology => "群晖：先在套件中心启用 WebDAV Server，地址形如 https://主机:5006/webdav",
            Preset::Custom => "自定义：请确保地址包含完整的 WebDAV 根路径",
        }
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

fn authority_has_port(authority: &str) -> bool {
    // IPv6 字面量 [::1]:5244 只认括号之后的冒号。
    match authority.rfind(']') {
        Some(index) => authority[index..].contains(':'),
        None => authority.contains(':'),
    }
}

fn default_scheme(raw: &str) -> &'static str {
    let lower = raw.to_ascii_lowercase();
    let host = lower.trim_start_matches('/');
    let local = host.starts_with("localhost")
        || host.starts_with("127.")
        || host.starts_with("192.168.")
        || host.starts_with("10.")
        || host.starts_with("[::1]");
    if local {
        "http"
    } else {
        "https"
    }
}

/// 轻量响应头集合（不引 http crate，便于单测与序列化）。
#[derive(Debug, Clone, Default)]
pub struct Headers {
    entries: Vec<(String, String)>,
    path: Option<String>,
}

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: &str, value: &str) {
        self.entries
            .push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.insert(name, value);
        self
    }

    pub fn with_path(mut self, path: &str) -> Self {
        self.path = Some(path.to_string());
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.entries
            .iter()
            .rev()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.path.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_match_spec() {
        assert_eq!(Preset::Alist.default_path(), "/dav");
        assert_eq!(Preset::Jianguoyun.default_path(), "/dav");
        assert_eq!(Preset::Nextcloud.default_path(), "/remote.php/dav/");
        assert_eq!(Preset::Synology.default_path(), "/webdav");
        assert_eq!(Preset::Custom.default_path(), "");
    }

    #[test]
    fn normalize_completes_scheme_and_alist_port() {
        assert_eq!(
            Preset::Alist.normalize("192.168.1.10"),
            "http://192.168.1.10:5244/dav"
        );
        assert_eq!(
            Preset::Alist.normalize("http://192.168.1.10:5244/dav/"),
            "http://192.168.1.10:5244/dav"
        );
        assert_eq!(
            Preset::Alist.normalize("https://dav.example.com"),
            "https://dav.example.com:5244/dav"
        );
        assert_eq!(
            Preset::Alist.normalize("http://[::1]"),
            "http://[::1]:5244/dav"
        );
    }

    #[test]
    fn normalize_strips_trailing_slash_and_adds_path() {
        assert_eq!(
            Preset::Jianguoyun.normalize("https://dav.jianguoyun.com/"),
            "https://dav.jianguoyun.com/dav"
        );
        assert_eq!(
            Preset::Nextcloud.normalize("https://cloud.example.com"),
            "https://cloud.example.com/remote.php/dav"
        );
        assert_eq!(
            Preset::Custom.normalize("http://127.0.0.1:18080/"),
            "http://127.0.0.1:18080"
        );
        assert_eq!(Preset::Custom.normalize("   "), "");
    }

    #[test]
    fn detect_alist_by_server_header() {
        let headers = Headers::new().with("Server", "AList/3.28.0");
        assert_eq!(Preset::detect(&headers), Some(Preset::Alist));
    }

    #[test]
    fn detect_alist_by_dav_path_and_basic_auth() {
        let headers = Headers::new()
            .with("WWW-Authenticate", "Basic realm=\"alist\"")
            .with_path("/dav");
        assert_eq!(Preset::detect(&headers), Some(Preset::Alist));
        let no_auth = Headers::new().with_path("/dav");
        assert_eq!(Preset::detect(&no_auth), None);
    }

    #[test]
    fn detect_other_providers() {
        assert_eq!(
            Preset::detect(&Headers::new().with("server", "nginx + nextcloud")),
            Some(Preset::Nextcloud)
        );
        assert_eq!(
            Preset::detect(&Headers::new().with("server", "DiskStation")),
            Some(Preset::Synology)
        );
        assert_eq!(
            Preset::detect(&Headers::new().with("server", "Nutstore")),
            Some(Preset::Jianguoyun)
        );
        assert_eq!(
            Preset::detect(&Headers::new().with("server", "rclone")),
            None
        );
    }

    #[test]
    fn preset_ids_roundtrip() {
        for preset in Preset::all() {
            assert_eq!(Preset::from_id(preset.id()), Some(preset));
        }
        assert_eq!(Preset::from_id("ALIST"), Some(Preset::Alist));
        assert_eq!(Preset::from_id("unknown"), None);
    }
}
