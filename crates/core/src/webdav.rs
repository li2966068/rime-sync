//! 最小 WebDAV 客户端：PROPFIND / GET / PUT / DELETE / MKCOL / HEAD。
//!
//! 不引 webdav crate：这里只需要 RFC 4918 的一个子集，且必须能控制
//! 超时、重试与 ETag 降级策略。

use anyhow::{anyhow, bail, ensure, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::blocking::{Client, Response};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use std::thread::sleep;
use std::time::Duration;

use crate::index::FileMeta;
use crate::plan;

const MAX_RETRIES: usize = 3;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const PROPFIND_BODY: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/><d:getcontentlength/><d:getetag/><d:getlastmodified/></d:prop></d:propfind>";

#[derive(Debug, Clone, Default, Serialize)]
pub struct RemoteEntry {
    pub href: String,
    /// 相对 DavClient base（已通过路径校验）。
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Written,
    PreconditionFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub status: u16,
    pub dav: Vec<String>,
    pub server: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DavClient {
    http: Client,
    root: String,
    base: String,
    base_path: String,
    remote_components: Vec<String>,
    username: String,
    password: String,
}

impl DavClient {
    pub fn new(url: &str, username: &str, password: &str, remote_dir: &str) -> Result<Self> {
        let url = url.trim().trim_end_matches('/');
        ensure!(
            url.starts_with("http://") || url.starts_with("https://"),
            "webdav url must be http(s): {url}"
        );
        let remote = remote_dir.trim().trim_matches('/');
        if !remote.is_empty() {
            plan::validate_rel_path(remote).context("invalid remote_dir")?;
        }
        let base = if remote.is_empty() {
            url.to_string()
        } else {
            format!("{url}/{}", encode_path(remote))
        };
        let base_path = decode_percent(&extract_path(&base));
        let remote_components = remote
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect();
        let http = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("rime-sync/0.1")
            .build()
            .context("build http client")?;
        Ok(Self {
            http,
            root: url.to_string(),
            base,
            base_path,
            remote_components,
            username: username.to_string(),
            password: password.to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn url_for(&self, rel: &str) -> Result<String> {
        if rel.is_empty() {
            return Ok(format!("{}/", self.base));
        }
        plan::validate_rel_path(rel)?;
        Ok(format!("{}/{}", self.base, encode_path(rel)))
    }

    fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, String)],
    ) -> Result<Response> {
        let mut last_error = String::new();
        for attempt in 0..=MAX_RETRIES {
            let mut request = self.http.request(method.clone(), url);
            if !self.username.is_empty() {
                request = request.basic_auth(&self.username, Some(&self.password));
            }
            for (key, value) in headers {
                request = request.header(*key, value);
            }
            if let Some(bytes) = body {
                request = request.body(bytes.to_vec());
            }
            match request.send() {
                Ok(response) => {
                    if response.status().is_server_error() && attempt < MAX_RETRIES {
                        last_error = format!("HTTP {}", response.status());
                        sleep(backoff(attempt));
                        continue;
                    }
                    return Ok(response);
                }
                Err(error) => {
                    let retryable =
                        error.is_timeout() || error.is_connect() || error.is_request();
                    if retryable && attempt < MAX_RETRIES {
                        last_error = error.to_string();
                        sleep(backoff(attempt));
                        continue;
                    }
                    return Err(anyhow::Error::new(error))
                        .with_context(|| format!("{method} {url} failed"));
                }
            }
        }
        Err(anyhow!(
            "{method} {url} failed after {MAX_RETRIES} retries: {last_error}"
        ))
    }

    pub fn propfind(&self, rel: &str, depth: u8) -> Result<Vec<RemoteEntry>> {
        let url = self.url_for(rel)?;
        let response = self.send(
            Method::from_bytes(b"PROPFIND")?,
            &url,
            Some(PROPFIND_BODY.as_bytes()),
            &[
                ("Depth", depth.to_string()),
                (
                    "Content-Type",
                    "application/xml; charset=utf-8".to_string(),
                ),
            ],
        )?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!(
                "PROPFIND {rel} -> {status}: {}",
                body.chars().take(300).collect::<String>()
            );
        }
        let body = response
            .text()
            .with_context(|| format!("read PROPFIND body for {rel}"))?;
        let mut entries = parse_multistatus(&body)?;
        for entry in &mut entries {
            entry.path = self.relative(&entry.href)?;
        }
        entries.retain(|entry| !entry.path.is_empty());
        Ok(entries)
    }

    pub fn probe(&self) -> Result<Probe> {
        let url = self.url_for("")?;
        let response = self.send(
            Method::from_bytes(b"PROPFIND")?,
            &url,
            Some(PROPFIND_BODY.as_bytes()),
            &[
                ("Depth", "0".to_string()),
                (
                    "Content-Type",
                    "application/xml; charset=utf-8".to_string(),
                ),
            ],
        )?;
        let status = response.status().as_u16();
        let dav = response
            .headers()
            .get("dav")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        let server = response
            .headers()
            .get("server")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Ok(Probe {
            status,
            dav,
            server,
        })
    }

    pub fn get(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let url = self.url_for(rel)?;
        let response = self.send(Method::GET, &url, None, &[])?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("GET {rel} -> {}", response.status());
        }
        let bytes = response
            .bytes()
            .with_context(|| format!("read body of {rel}"))?;
        Ok(Some(bytes.to_vec()))
    }

    pub fn head(&self, rel: &str) -> Result<Option<RemoteEntry>> {
        let url = self.url_for(rel)?;
        let response = self.send(Method::HEAD, &url, None, &[])?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("HEAD {rel} -> {}", response.status());
        }
        Ok(Some(entry_from_headers(&response, rel)))
    }

    pub fn put(&self, rel: &str, bytes: &[u8], if_match: Option<&str>) -> Result<PutOutcome> {
        let url = self.url_for(rel)?;
        let mut headers = vec![(
            "Content-Type",
            "application/octet-stream".to_string(),
        )];
        if let Some(etag) = if_match {
            headers.push(("If-Match", etag.to_string()));
        }
        let response = self.send(Method::PUT, &url, Some(bytes), &headers)?;
        match response.status().as_u16() {
            200 | 201 | 204 => Ok(PutOutcome::Written),
            412 => Ok(PutOutcome::PreconditionFailed),
            status => bail!("PUT {rel} -> {status}"),
        }
    }

    pub fn delete(&self, rel: &str) -> Result<()> {
        let url = self.url_for(rel)?;
        let response = self.send(Method::DELETE, &url, None, &[])?;
        match response.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            status => bail!("DELETE {rel} -> {status}"),
        }
    }

    pub fn mkcol(&self, rel: &str) -> Result<()> {
        if rel.is_empty() {
            return Ok(());
        }
        let url = self.url_for(rel)?;
        self.mkcol_url(&url, rel)
    }

    fn mkcol_url(&self, url: &str, display: &str) -> Result<()> {
        let response = self.send(Method::from_bytes(b"MKCOL")?, url, None, &[])?;
        match response.status().as_u16() {
            200 | 201 | 204 | 405 => Ok(()),
            status => bail!("MKCOL {display} -> {status}"),
        }
    }

    /// remote_dir 本身可能还不存在；按段创建（相对服务器根）。
    pub fn ensure_base(&self) -> Result<()> {
        let mut url = self.root.clone();
        for component in &self.remote_components {
            url.push('/');
            url.push_str(&encode_segment(component));
            self.mkcol_url(&url, component)?;
        }
        Ok(())
    }

    /// 逐级 MKCOL，已存在（405）视为成功。
    pub fn ensure_dir(&self, rel: &str) -> Result<()> {
        if rel.is_empty() {
            return Ok(());
        }
        plan::validate_rel_path(rel)?;
        let mut current = String::new();
        for component in rel.split('/') {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(component);
            self.mkcol(&current)?;
        }
        Ok(())
    }

    /// 无 ETag 时按 size + last-modified 降级比对。
    pub fn remote_unchanged(&self, rel: &str, meta: &FileMeta) -> Result<bool> {
        match self.head(rel)? {
            Some(entry) => Ok(entry_unchanged(&entry, meta)),
            None => Ok(false),
        }
    }

    fn relative(&self, href: &str) -> Result<String> {
        let decoded = decode_percent(&href_path(href));
        let base = if self.base_path == "/" {
            ""
        } else {
            self.base_path.trim_end_matches('/')
        };
        let rel = if decoded == base || decoded == format!("{base}/") {
            ""
        } else if base.is_empty() {
            decoded.trim_start_matches('/')
        } else if let Some(rest) = decoded.strip_prefix(&format!("{base}/")) {
            rest
        } else {
            bail!("remote href outside base path: {href}");
        };
        let rel = rel.trim_matches('/');
        if !rel.is_empty() {
            plan::validate_rel_path(rel).with_context(|| format!("unsafe remote href: {href}"))?;
        }
        Ok(rel.to_string())
    }
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(200u64.saturating_mul(1u64 << attempt.min(4)))
}

fn entry_from_headers(response: &Response, rel: &str) -> RemoteEntry {
    RemoteEntry {
        href: rel.to_string(),
        path: rel.to_string(),
        is_dir: false,
        size: response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok()),
        etag: response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        last_modified: response
            .headers()
            .get("last-modified")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    }
}

/// ETag 优先；缺失任一侧时退回 size + mtime。
pub fn entry_unchanged(entry: &RemoteEntry, meta: &FileMeta) -> bool {
    if let (Some(remote), Some(known)) = (entry.etag.as_deref(), meta.etag.as_deref()) {
        if !remote.is_empty() && !known.is_empty() {
            return remote == known;
        }
    }
    if entry.size != Some(meta.size) {
        return false;
    }
    match (entry.last_modified.as_deref(), meta.mtime.is_empty()) {
        (Some(remote), false) => remote == meta.mtime,
        _ => true,
    }
}

fn extract_path(url: &str) -> String {
    match url.find("://") {
        Some(index) => {
            let rest = &url[index + 3..];
            match rest.find('/') {
                Some(slash) => rest[slash..].to_string(),
                None => "/".to_string(),
            }
        }
        None => url.to_string(),
    }
}

fn href_path(href: &str) -> String {
    let path = match href.find("://") {
        Some(index) => {
            let rest = &href[index + 3..];
            match rest.find('/') {
                Some(slash) => rest[slash..].to_string(),
                None => "/".to_string(),
            }
        }
        None => href.to_string(),
    };
    path.split(['?', '#']).next().unwrap_or("").to_string()
}

pub fn encode_path(rel: &str) -> String {
    rel.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub fn decode_percent(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_multistatus(xml: &str) -> Result<Vec<RemoteEntry>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut entries = Vec::new();
    let mut current: Option<RemoteEntry> = None;
    let mut in_resource_type = false;
    let mut text_field: Option<String> = None;
    let mut text_buffer = String::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "response" => current = Some(RemoteEntry::default()),
                    "resourcetype" => in_resource_type = true,
                    "collection" if in_resource_type => {
                        if let Some(entry) = current.as_mut() {
                            entry.is_dir = true;
                        }
                    }
                    "href" | "getcontentlength" | "getetag" | "getlastmodified" => {
                        text_field = Some(name);
                        text_buffer.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "collection" && in_resource_type {
                    if let Some(entry) = current.as_mut() {
                        entry.is_dir = true;
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if text_field.is_some() {
                    if let Ok(value) = text.xml10_content() {
                        text_buffer.push_str(value.as_ref());
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                match name.as_str() {
                    "resourcetype" => in_resource_type = false,
                    "href" | "getcontentlength" | "getetag" | "getlastmodified" => {
                        if let Some(entry) = current.as_mut() {
                            let value = text_buffer.trim().to_string();
                            match name.as_str() {
                                "href" => entry.href = value,
                                "getcontentlength" => entry.size = value.parse::<u64>().ok(),
                                "getetag" => {
                                    if !value.is_empty() {
                                        entry.etag = Some(value)
                                    }
                                }
                                "getlastmodified" => {
                                    if !value.is_empty() {
                                        entry.last_modified = Some(value)
                                    }
                                }
                                _ => {}
                            }
                        }
                        text_field = None;
                        text_buffer.clear();
                    }
                    "response" => {
                        if let Some(entry) = current.take() {
                            entries.push(entry);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(anyhow!("parse PROPFIND XML: {error}")),
        }
        buffer.clear();
    }
    Ok(entries)
}

fn local_name(qname: &[u8]) -> String {
    let text = String::from_utf8_lossy(qname);
    text.rsplit(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> DavClient {
        DavClient::new("http://127.0.0.1:18080", "u", "p", "rime-sync").expect("client")
    }

    #[test]
    fn percent_roundtrip() {
        assert_eq!(encode_path("a b/c+d.yaml"), "a%20b/c%2Bd.yaml");
        assert_eq!(decode_percent("a%20b/c%2Bd.yaml"), "a b/c+d.yaml");
        assert_eq!(decode_percent("%E4%B8%AD"), "中");
    }

    #[test]
    fn parses_multistatus_with_namespaces() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/rime-sync/shared/</D:href>
    <D:propstat><D:prop>
      <D:resourcetype><D:collection/></D:resourcetype>
      <D:getcontentlength>0</D:getcontentlength>
      <D:getetag>"dir"</D:getetag>
      <D:getlastmodified>Mon, 01 Jan 2024 00:00:00 GMT</D:getlastmodified>
    </D:prop></D:propstat>
  </D:response>
  <D:response>
    <D:href>/rime-sync/shared/files/a%20b.custom.yaml</D:href>
    <D:propstat><D:prop>
      <D:resourcetype/>
      <D:getcontentlength>12</D:getcontentlength>
      <D:getetag>"abc"</D:getetag>
    </D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let entries = parse_multistatus(xml).expect("parse");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].path, "");
        assert_eq!(entries[1].size, Some(12));
        assert_eq!(entries[1].etag.as_deref(), Some("\"abc\""));
        let client = client();
        assert_eq!(
            client
                .relative("/rime-sync/shared/files/a%20b.custom.yaml")
                .expect("relative"),
            "shared/files/a b.custom.yaml"
        );
    }

    #[test]
    fn rejects_traversal_in_href_and_remote_dir() {
        let client = client();
        assert!(client.relative("/rime-sync/../etc/passwd").is_err());
        assert!(DavClient::new("http://x", "", "", "../evil").is_err());
    }

    #[test]
    fn etag_takes_priority_then_size_and_mtime() {
        let meta = FileMeta {
            sha256: "h".into(),
            size: 5,
            mtime: "Mon, 01 Jan 2024 00:00:00 GMT".into(),
            etag: Some("\"e1\"".into()),
        };
        let mut entry = RemoteEntry {
            size: Some(5),
            etag: Some("\"e1\"".into()),
            last_modified: Some("ignored".into()),
            ..RemoteEntry::default()
        };
        assert!(entry_unchanged(&entry, &meta));
        entry.etag = Some("\"e2\"".into());
        assert!(!entry_unchanged(&entry, &meta));

        let meta_without_etag = FileMeta {
            etag: None,
            ..meta.clone()
        };
        let mut entry_without_etag = RemoteEntry {
            size: Some(5),
            etag: None,
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            ..RemoteEntry::default()
        };
        assert!(entry_unchanged(&entry_without_etag, &meta_without_etag));
        entry_without_etag.last_modified = Some("Tue, 02 Jan 2024 00:00:00 GMT".into());
        assert!(!entry_unchanged(&entry_without_etag, &meta_without_etag));
    }

    #[test]
    fn base_url_join_encodes_segments() {
        let client = DavClient::new("http://x/dav/", "u", "p", "my sync").expect("client");
        assert_eq!(
            client.url_for("a b/c.yaml").expect("url"),
            "http://x/dav/my%20sync/a%20b/c.yaml"
        );
    }
}
