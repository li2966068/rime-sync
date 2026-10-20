//! 基线索引 / 远端 manifest 共用的文件元数据表。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub mtime: String,
    #[serde(default)]
    pub etag: Option<String>,
}

impl FileMeta {
    pub fn local(bytes: &[u8]) -> Self {
        Self {
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
            mtime: unix_ts_string(),
            etag: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIndex {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub files: BTreeMap<String, FileMeta>,
    /// 已推送到远端 sync/<device>/ 的快照，避免重复上传。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub snapshots: BTreeMap<String, FileMeta>,
}

fn default_version() -> u32 {
    INDEX_VERSION
}

impl Default for FileIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            files: BTreeMap::new(),
            snapshots: BTreeMap::new(),
        }
    }
}

impl FileIndex {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parse index {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read index {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, format!("{text}\n"))
            .with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
        Ok(())
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn unix_ts_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn index_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rime-sync-index-{}", std::process::id()));
        let path = dir.join("index.json");
        let mut index = FileIndex::default();
        index
            .files
            .insert("a.custom.yaml".into(), FileMeta::local(b"x"));
        index.snapshots.insert(
            "sync/dev/a.userdb.txt".into(),
            FileMeta::local(b"y"),
        );
        index.save(&path).expect("save");
        let loaded = FileIndex::load(&path).expect("load");
        assert_eq!(loaded.files["a.custom.yaml"].sha256, sha256_hex(b"x"));
        assert_eq!(loaded.snapshots.len(), 1);
        fs::remove_dir_all(&dir).ok();
    }
}
