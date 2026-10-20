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
use crate::presets::{self, Preset};

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
                    let retryable = error.is_timeout() || error.is_connect() || error.is_request();
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
                ("Content-Type", "application/xml; charset=utf-8".to_string()),
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
                ("Content-Type", "application/xml; charset=utf-8".to_string()),
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
        let mut headers = vec![("Content-Type", "application/octet-stream".to_string())];
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
                                "getetag" if !value.is_empty() => entry.etag = Some(value),
                                "getlastmodified" if !value.is_empty() => {
                                    entry.last_modified = Some(value)
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
    text.rsplit(':').next().unwrap_or("").to_ascii_lowercase()
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeStep {
    pub step: String,
    pub ok: bool,
    pub status: u16,
    pub detail: String,
}

/// 写权限探测结果：MKCOL → PUT(1 字节) → DELETE 清理。
#[derive(Debug, Clone, Serialize)]
pub struct WriteProbe {
    pub ok: bool,
    pub steps: Vec<ProbeStep>,
    pub server: Option<String>,
    pub preset: Option<String>,
    pub hint: Option<String>,
}

impl DavClient {
    /// 真正走一遍写路径；失败时把 HTTP 状态翻译成中文可执行建议。
    pub fn check_write(&self) -> Result<WriteProbe> {
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        );
        let dir = format!(".rime-sync-probe-{nonce}");
        let object = format!("{dir}/probe.bin");
        let mut steps: Vec<ProbeStep> = Vec::new();
        let mut headers = presets::Headers::new();
        if !self.base_path.is_empty() {
            headers = headers.with_path(&self.base_path);
        }

        // 1) MKCOL 临时目录。
        let url = self.url_for(&dir)?;
        let response = self.send(Method::from_bytes(b"MKCOL")?, &url, None, &[])?;
        let status = response.status().as_u16();
        absorb_headers(&response, &mut headers);
        let created = matches!(status, 200 | 201 | 204 | 405);
        steps.push(ProbeStep {
            step: "MKCOL".to_string(),
            ok: created,
            status,
            detail: format!("create {dir}"),
        });
        let preset = Preset::detect(&headers);
        // RFC 4918：MKCOL 对已存在集合也返回 405，无法区分“已存在”与“不允许”。
        let mkcol_ambiguous = status == 405;
        if !created {
            let hint = Some(write_failure_hint(status, "MKCOL", preset));
            return Ok(finish_probe(steps, headers, preset, hint));
        }

        // 2) PUT 1 字节对象。
        let url = self.url_for(&object)?;
        let response = self.send(
            Method::PUT,
            &url,
            Some(b"x"),
            &[("Content-Type", "application/octet-stream".to_string())],
        )?;
        let status = response.status().as_u16();
        absorb_headers(&response, &mut headers);
        let written = matches!(status, 200 | 201 | 204);
        steps.push(ProbeStep {
            step: "PUT".to_string(),
            ok: written,
            status,
            detail: format!("upload 1 byte to {object}"),
        });
        if !written {
            let hint = Some(if mkcol_ambiguous {
                read_only_hint(status, preset)
            } else {
                write_failure_hint(status, "PUT", preset)
            });
            let _ = self.delete(&dir);
            return Ok(finish_probe(steps, headers, preset, hint));
        }

        // 3) DELETE 对象 + 4) 清理临时目录。
        let url = self.url_for(&object)?;
        let response = self.send(Method::DELETE, &url, None, &[])?;
        let status = response.status().as_u16();
        let removed = matches!(status, 200 | 202 | 204 | 404);
        steps.push(ProbeStep {
            step: "DELETE".to_string(),
            ok: removed,
            status,
            detail: format!("remove {object}"),
        });
        let url = self.url_for(&dir)?;
        let response = self.send(Method::DELETE, &url, None, &[])?;
        let status = response.status().as_u16();
        let cleaned = matches!(status, 200 | 202 | 204 | 404);
        steps.push(ProbeStep {
            step: "CLEANUP".to_string(),
            ok: cleaned,
            status,
            detail: format!("remove {dir}"),
        });
        let hint = if removed && cleaned {
            None
        } else {
            Some(write_failure_hint(status, "DELETE", preset))
        };
        Ok(finish_probe(steps, headers, preset, hint))
    }
}

fn absorb_headers(response: &Response, headers: &mut presets::Headers) {
    for name in ["server", "www-authenticate"] {
        if let Some(value) = response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
        {
            headers.insert(name, value);
        }
    }
}

fn finish_probe(
    steps: Vec<ProbeStep>,
    headers: presets::Headers,
    preset: Option<Preset>,
    hint: Option<String>,
) -> WriteProbe {
    WriteProbe {
        ok: steps.iter().all(|step| step.ok),
        steps,
        server: headers.get("server").map(str::to_string),
        preset: preset.map(|value| value.id().to_string()),
        hint,
    }
}

/// MKCOL 返回 405 后再失败：最可能是只读挂载或权限不足。
pub fn read_only_hint(status: u16, preset: Option<Preset>) -> String {
    let mut text = format!(
        "服务端不允许创建目录或写入(HTTP {status}，PUT)：可能是只读挂载（rclone --read-only / 只读网盘）或账号缺少写权限。"
    );
    match preset {
        Some(Preset::Alist) => text.push_str(
            "AList 请在后台为账号勾选 Webdav Read 与 Webdav Manage，并确认挂载存储允许上传与删除。",
        ),
        _ => text.push_str("请检查账号对该目录的写权限，并确认服务端不是只读模式。"),
    }
    text
}

/// HTTP 状态 → 中文可执行建议（SPEC-2 §2.6）。
pub fn write_failure_hint(status: u16, step: &str, preset: Option<Preset>) -> String {
    match status {
        401 => "认证失败(401)：用户名或密码错误。AList 请用 AList 账号密码，坚果云请用「应用密码」。"
            .to_string(),
        403 => match preset {
            Some(Preset::Alist) => {
                "AList 拒绝写入(403)：请到 AList 后台「用户」为该账号勾选 Webdav Read 与 Webdav Manage 权限，并确认挂载存储本身允许上传与删除。"
                    .to_string()
            }
            Some(other) => format!(
                "{} 拒绝写入(403，{step})：请检查账号的 WebDAV 读写权限与目录归属。",
                other.label()
            ),
            None => format!(
                "服务端拒绝写入(403，{step})：请检查账号是否有该目录的写权限（只读挂载无法同步）。"
            ),
        },
        404 => format!(
            "路径不存在(404，{step})：请确认 WebDAV 地址、端口与远端目录是否正确。"
        ),
        405 => format!(
            "服务端不接受该操作(405，{step})：可能禁用了 MKCOL/PUT/DELETE，请检查 WebDAV 服务器配置。"
        ),
        409 => format!(
            "父目录不存在(409，{step})：请先在服务端创建远端目录，或让其可自动创建。"
        ),
        423 => format!("资源被锁定(423，{step})：稍后重试或解除服务端锁。"),
        507 => "空间不足(507)：清理远端空间后重试。".to_string(),
        code if code >= 500 => format!(
            "服务端错误({code}，{step})：请查看 WebDAV 服务端日志，或稍后重试。"
        ),
        code => format!("写入探测失败(HTTP {code}，{step})：请检查地址、账号与目录权限。"),
    }
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
#[cfg(test)]
mod write_probe_tests {
    use super::*;

    #[test]
    fn alist_403_hint_mentions_permissions() {
        let hint = write_failure_hint(403, "MKCOL", Some(Preset::Alist));
        assert!(hint.contains("Webdav Read"));
        assert!(hint.contains("Webdav Manage"));
    }

    #[test]
    fn auth_and_space_hints_are_actionable() {
        assert!(write_failure_hint(401, "PUT", None).contains("用户名或密码"));
        assert!(write_failure_hint(507, "PUT", None).contains("空间不足"));
        assert!(write_failure_hint(404, "MKCOL", None).contains("远端目录"));
        assert!(write_failure_hint(405, "PUT", None).contains("MKCOL"));
        assert!(write_failure_hint(503, "PUT", None).contains("服务端"));
    }

    #[test]
    fn read_only_hint_is_chinese_and_actionable() {
        let hint = read_only_hint(405, None);
        assert!(hint.contains("只读"));
        assert!(hint
            .chars()
            .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)));
        let alist = read_only_hint(409, Some(Preset::Alist));
        assert!(alist.contains("Webdav Manage"));
    }

    #[test]
    fn probe_result_shape_serializes() {
        let probe = WriteProbe {
            ok: false,
            steps: vec![ProbeStep {
                step: "MKCOL".into(),
                ok: false,
                status: 403,
                detail: "create".into(),
            }],
            server: Some("AList".into()),
            preset: Some("alist".into()),
            hint: Some(write_failure_hint(403, "MKCOL", Some(Preset::Alist))),
        };
        let json = serde_json::to_value(&probe).expect("json");
        assert_eq!(json["ok"], false);
        assert_eq!(json["steps"][0]["status"], 403);
        assert_eq!(json["preset"], "alist");
    }
}
