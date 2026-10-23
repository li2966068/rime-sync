//! 去重与迁移的纯函数：远端设备分组、快照内容分组、本地备份指纹、删除路径安全校验。
//!
//! 这里只做「给定元数据 -> 分组结果」的计算，不碰文件系统与网络，
//! 便于对分组规则与路径闸门做单元测试。

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::index::sha256_hex;
use crate::plan;

/// 分组标签：设备目录（同一 hostname+os）。
pub const TAG_DEVICE: &str = "device";
/// 分组标签：缺失 owner.json 的设备目录（单独成组、默认不勾选）。
pub const TAG_UNKNOWN: &str = "unknown";
/// 分组标签：同一目录内的重复快照。
pub const TAG_SAME_DIR: &str = "same_dir";
/// 分组标签：跨目录的相同内容（默认只提示，需显式勾选）。
pub const TAG_CROSS_DIR: &str = "cross_dir";
/// 分组标签：内容完全相同的本地备份。
pub const TAG_BACKUP: &str = "backup";

/// 远端 sync/<id> 目录的元数据；owner.json 缺失时 hostname/os 为 None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDirEntry {
    pub id: String,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub heartbeat_ts: u64,
    pub size_bytes: u64,
}

/// 去重候选项（UI 直接渲染，id 即删除目标）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupeItem {
    /// 删除目标：设备目录 id / 远端快照相对路径 / 本地备份 id。
    pub id: String,
    /// 展示用补充信息（所在目录、备份时间文案等），可为空。
    pub label: String,
    pub size: u64,
    /// 默认是否勾选。
    pub suggested: bool,
    /// 是否建议保留（保留项不会被默认勾选）。
    pub is_keep: bool,
    /// 设备心跳或快照 last-modified（unix 秒）；不适用时为 0。
    pub heartbeat_ts: u64,
    pub hostname: String,
    pub os: String,
}

/// 一组重复项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupeGroup {
    pub key: String,
    pub tag: String,
    pub items: Vec<DedupeItem>,
}

impl DedupeGroup {
    pub fn suggested_ids(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| item.suggested)
            .map(|item| item.id.clone())
            .collect()
    }

    /// 本组全部非保留项的字节数（显式勾选这些项最多能释放的空间）。
    pub fn reclaimable(&self) -> u64 {
        self.items
            .iter()
            .filter(|item| !item.is_keep)
            .map(|item| item.size)
            .sum()
    }

    /// 本组默认勾选建议项能释放的字节数。
    pub fn suggested_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter(|item| item.suggested)
            .map(|item| item.size)
            .sum()
    }
}

/// 按 owner.json 的 hostname+os 分组：组内保留本机 device_id，否则保留
/// heartbeat_ts 最新的一份；缺失 owner.json 的目录单独成组。
pub fn group_device_dirs(dirs: &[DeviceDirEntry], self_id: &str) -> Vec<DedupeGroup> {
    let mut groups: Vec<DedupeGroup> = Vec::new();
    let mut known: BTreeMap<(String, String), Vec<&DeviceDirEntry>> = BTreeMap::new();
    for dir in dirs {
        match (&dir.hostname, &dir.os) {
            (Some(hostname), Some(os)) => known
                .entry((hostname.clone(), os.clone()))
                .or_default()
                .push(dir),
            _ => groups.push(DedupeGroup {
                key: format!("unknown:{}", dir.id),
                tag: TAG_UNKNOWN.to_string(),
                // 没有 owner.json 就无法判断归属：单独列出、默认不勾选，但仍是可删候选。
                items: vec![device_item(dir, false, false)],
            }),
        }
    }
    for ((_, _), mut list) in known {
        list.sort_by(|left, right| {
            right
                .heartbeat_ts
                .cmp(&left.heartbeat_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        let keep_id = list
            .iter()
            .find(|dir| dir.id == self_id)
            .map(|dir| dir.id.clone())
            .unwrap_or_else(|| list[0].id.clone());
        let items = list
            .iter()
            .map(|dir| {
                let is_keep = dir.id == keep_id;
                device_item(dir, !is_keep, is_keep)
            })
            .collect();
        groups.push(DedupeGroup {
            key: format!("device:{}:{}", list[0].id, keep_id),
            tag: TAG_DEVICE.to_string(),
            items,
        });
    }
    groups.sort_by(|left, right| {
        left.tag
            .cmp(&right.tag)
            .then_with(|| left.key.cmp(&right.key))
    });
    groups
}

fn device_item(dir: &DeviceDirEntry, suggested: bool, is_keep: bool) -> DedupeItem {
    DedupeItem {
        id: dir.id.clone(),
        label: String::new(),
        size: dir.size_bytes,
        suggested,
        is_keep,
        heartbeat_ts: dir.heartbeat_ts,
        hostname: dir.hostname.clone().unwrap_or_default(),
        os: dir.os.clone().unwrap_or_default(),
    }
}

/// 远端快照文件（path 为相对远端的完整路径）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub modified_ts: u64,
}

/// 快照去重：同一目录内保留最新（last-modified，其次文件名），其余建议删除；
/// 跨目录相同内容保留最新，其余仅提示（suggested=false）。
pub fn group_snapshots(files: &[SnapshotFile]) -> Vec<DedupeGroup> {
    let mut by_dir: BTreeMap<String, BTreeMap<String, Vec<&SnapshotFile>>> = BTreeMap::new();
    for file in files {
        by_dir
            .entry(parent_dir(&file.path).to_string())
            .or_default()
            .entry(file.sha256.clone())
            .or_default()
            .push(file);
    }

    let mut removed: HashSet<String> = HashSet::new();
    let mut groups: Vec<DedupeGroup> = Vec::new();
    for (dir, by_sha) in &by_dir {
        for (sha, list) in by_sha {
            if list.len() < 2 {
                continue;
            }
            let mut sorted = list.clone();
            sorted.sort_by(newest_first);
            let keep = sorted[0].path.clone();
            let items = sorted
                .iter()
                .map(|file| {
                    let is_keep = file.path == keep;
                    if !is_keep {
                        removed.insert(file.path.clone());
                    }
                    snapshot_item(file, dir, !is_keep, is_keep)
                })
                .collect();
            groups.push(DedupeGroup {
                key: format!("same-dir:{dir}:{}", short(sha)),
                tag: TAG_SAME_DIR.to_string(),
                items,
            });
        }
    }

    let mut by_sha: BTreeMap<&str, Vec<&SnapshotFile>> = BTreeMap::new();
    for file in files {
        if removed.contains(&file.path) {
            continue;
        }
        by_sha.entry(file.sha256.as_str()).or_default().push(file);
    }
    for (sha, list) in by_sha {
        let dirs: BTreeSet<&str> = list.iter().map(|file| parent_dir(&file.path)).collect();
        if dirs.len() < 2 {
            continue;
        }
        let mut sorted = list.clone();
        sorted.sort_by(newest_first);
        let keep = sorted[0].path.clone();
        let items = sorted
            .iter()
            .map(|file| snapshot_item(file, parent_dir(&file.path), false, file.path == keep))
            .collect();
        groups.push(DedupeGroup {
            key: format!("cross-dir:{}", short(sha)),
            tag: TAG_CROSS_DIR.to_string(),
            items,
        });
    }

    groups.sort_by(|left, right| {
        left.tag
            .cmp(&right.tag)
            .then_with(|| left.key.cmp(&right.key))
    });
    groups
}

fn snapshot_item(file: &SnapshotFile, dir: &str, suggested: bool, is_keep: bool) -> DedupeItem {
    DedupeItem {
        id: file.path.clone(),
        label: dir.to_string(),
        size: file.size,
        suggested,
        is_keep,
        heartbeat_ts: file.modified_ts,
        hostname: String::new(),
        os: String::new(),
    }
}

fn newest_first(left: &&SnapshotFile, right: &&SnapshotFile) -> std::cmp::Ordering {
    right
        .modified_ts
        .cmp(&left.modified_ts)
        .then_with(|| right.path.cmp(&left.path))
}

pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

/// 本地备份的内容指纹：排序后的「相对路径 + 文件 sha256」列表再哈希。
pub fn backup_content_hash(files: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = files.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let mut buffer = String::new();
    for (rel, sha) in sorted {
        buffer.push_str(rel);
        buffer.push('\0');
        buffer.push_str(sha);
        buffer.push('\n');
    }
    sha256_hex(buffer.as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupFingerprint {
    pub id: String,
    pub content_hash: String,
    pub bytes: u64,
    pub created_at: u64,
    pub label: String,
}

/// 按内容指纹分组：保留最新（created_at，其次 id）的一份，其余建议删除。
pub fn group_backups(backups: &[BackupFingerprint]) -> Vec<DedupeGroup> {
    let mut by_hash: BTreeMap<&str, Vec<&BackupFingerprint>> = BTreeMap::new();
    for backup in backups {
        by_hash
            .entry(backup.content_hash.as_str())
            .or_default()
            .push(backup);
    }
    let mut groups = Vec::new();
    for (hash, list) in by_hash {
        if list.len() < 2 {
            continue;
        }
        let mut sorted = list.clone();
        sorted.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let keep = sorted[0].id.clone();
        let items = sorted
            .iter()
            .map(|backup| DedupeItem {
                id: backup.id.clone(),
                label: backup.label.clone(),
                size: backup.bytes,
                suggested: backup.id != keep,
                is_keep: backup.id == keep,
                heartbeat_ts: backup.created_at,
                hostname: String::new(),
                os: String::new(),
            })
            .collect();
        groups.push(DedupeGroup {
            key: format!("content:{}", short(hash)),
            tag: TAG_BACKUP.to_string(),
            items,
        });
    }
    groups.sort_by(|left, right| left.key.cmp(&right.key));
    groups
}

fn short(value: &str) -> &str {
    let end = value.len().min(12);
    &value[..end]
}

/// 远端删除闸门：必须落在 sync/ 下、不得指向 shared/ 内容，
/// 删除整个设备目录时不得指向本机 device_id。
pub fn validate_remote_delete_path(rel: &str, self_id: &str) -> Result<()> {
    plan::validate_rel_path(rel)?;
    ensure!(
        rel.starts_with("sync/"),
        "refusing to delete outside sync/: {rel}"
    );
    let parts: Vec<&str> = rel.split('/').collect();
    ensure!(parts.len() >= 2, "remote path has no device segment: {rel}");
    ensure!(
        parts[1] != "shared",
        "refusing to delete shared/ content: {rel}"
    );
    if parts.len() == 2 {
        ensure!(
            parts[1] != self_id,
            "refusing to delete the local device directory: {rel}"
        );
    }
    Ok(())
}

/// 快照删除闸门：在 validate_remote_delete_path 之上再限定 *.userdb.txt 且位于设备目录内。
pub fn validate_snapshot_delete_path(rel: &str, self_id: &str) -> Result<()> {
    validate_remote_delete_path(rel, self_id)?;
    ensure!(rel.ends_with(".userdb.txt"), "not a userdb snapshot: {rel}");
    ensure!(
        rel.split('/').count() >= 3,
        "snapshot must live inside a device directory: {rel}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(id: &str, host: &str, os: &str, heartbeat: u64, size: u64) -> DeviceDirEntry {
        DeviceDirEntry {
            id: id.to_string(),
            hostname: Some(host.to_string()),
            os: Some(os.to_string()),
            heartbeat_ts: heartbeat,
            size_bytes: size,
        }
    }

    #[test]
    fn device_groups_keep_self_or_newest_heartbeat() {
        let dirs = vec![
            dir("old-a", "pc", "linux", 100, 10),
            dir("new-b", "pc", "linux", 300, 20),
            dir("mid-c", "pc", "linux", 200, 30),
            dir("mac-1", "mac", "macos", 50, 40),
            dir("mac-2", "mac", "macos", 60, 50),
        ];
        let groups = group_device_dirs(&dirs, "self-x");
        assert_eq!(groups.len(), 2);
        let pc = groups
            .iter()
            .find(|group| {
                group.tag == TAG_DEVICE && group.items.iter().any(|item| item.id == "new-b")
            })
            .expect("pc group");
        assert_eq!(pc.items.len(), 3);
        let kept: Vec<&str> = pc
            .items
            .iter()
            .filter(|item| item.is_keep)
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(kept, vec!["new-b"]);
        assert_eq!(pc.suggested_ids(), vec!["mid-c", "old-a"]);
        assert_eq!(pc.reclaimable(), 40);
        assert_eq!(pc.suggested_bytes(), 40);

        // 本机在组内时优先保留本机，即使心跳更旧。
        let groups = group_device_dirs(&dirs, "old-a");
        let pc = groups
            .iter()
            .find(|group| {
                group.tag == TAG_DEVICE && group.items.iter().any(|item| item.id == "old-a")
            })
            .expect("pc group");
        assert!(pc
            .items
            .iter()
            .any(|item| item.id == "old-a" && item.is_keep));
        assert!(!pc.suggested_ids().contains(&"old-a".to_string()));
    }

    #[test]
    fn missing_owner_json_is_its_own_unchecked_group() {
        let dirs = vec![
            dir("pc-a", "pc", "linux", 10, 1),
            DeviceDirEntry {
                id: "ghost".to_string(),
                hostname: None,
                os: None,
                heartbeat_ts: 0,
                size_bytes: 7,
            },
        ];
        let groups = group_device_dirs(&dirs, "pc-a");
        let unknown = groups
            .iter()
            .find(|group| group.tag == TAG_UNKNOWN)
            .expect("unknown group");
        assert_eq!(unknown.items.len(), 1);
        assert_eq!(unknown.items[0].id, "ghost");
        assert!(!unknown.items[0].suggested);
        assert!(!unknown.items[0].is_keep);
        assert!(unknown.suggested_ids().is_empty());
        assert_eq!(unknown.reclaimable(), 7);
    }

    fn snapshot(path: &str, sha: &str, size: u64, modified: u64) -> SnapshotFile {
        SnapshotFile {
            path: path.to_string(),
            sha256: sha.to_string(),
            size,
            modified_ts: modified,
        }
    }

    #[test]
    fn same_directory_duplicates_keep_newest() {
        let files = vec![
            snapshot("sync/devA/main.userdb.txt", "same", 10, 100),
            snapshot("sync/devA/old.userdb.txt", "same", 10, 300),
            snapshot("sync/devA/other.userdb.txt", "diff", 5, 50),
        ];
        let groups = group_snapshots(&files);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tag, TAG_SAME_DIR);
        let keep: Vec<&str> = groups[0]
            .items
            .iter()
            .filter(|item| item.is_keep)
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(keep, vec!["sync/devA/old.userdb.txt"]);
        assert_eq!(groups[0].suggested_ids(), vec!["sync/devA/main.userdb.txt"]);
        assert_eq!(groups[0].reclaimable(), 10);
    }

    #[test]
    fn cross_directory_duplicates_are_prompt_only() {
        let files = vec![
            snapshot("sync/devA/main.userdb.txt", "same", 10, 100),
            snapshot("sync/devB/main.userdb.txt", "same", 10, 200),
        ];
        let groups = group_snapshots(&files);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tag, TAG_CROSS_DIR);
        assert!(groups[0].suggested_ids().is_empty());
        assert_eq!(groups[0].reclaimable(), 10);
        // 保留最新的一份，但不默认勾选任何项。
        assert!(groups[0]
            .items
            .iter()
            .any(|item| item.id == "sync/devB/main.userdb.txt" && item.is_keep));
    }

    #[test]
    fn backup_content_hash_is_order_independent_and_groups_by_content() {
        let left = vec![
            ("a.txt".to_string(), "h1".to_string()),
            ("b.txt".to_string(), "h2".to_string()),
        ];
        let mut right = left.clone();
        right.reverse();
        assert_eq!(backup_content_hash(&left), backup_content_hash(&right));
        let other = vec![("a.txt".to_string(), "h9".to_string())];
        assert_ne!(backup_content_hash(&left), backup_content_hash(&other));

        let backups = vec![
            BackupFingerprint {
                id: "20240101-000000".to_string(),
                content_hash: backup_content_hash(&left),
                bytes: 100,
                created_at: 100,
                label: "old".to_string(),
            },
            BackupFingerprint {
                id: "20240102-000000".to_string(),
                content_hash: backup_content_hash(&left),
                bytes: 100,
                created_at: 200,
                label: "new".to_string(),
            },
            BackupFingerprint {
                id: "20240103-000000".to_string(),
                content_hash: backup_content_hash(&other),
                bytes: 10,
                created_at: 300,
                label: "unique".to_string(),
            },
        ];
        let groups = group_backups(&backups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tag, TAG_BACKUP);
        assert_eq!(groups[0].suggested_ids(), vec!["20240101-000000"]);
        assert_eq!(groups[0].reclaimable(), 100);
    }

    #[test]
    fn delete_gate_rejects_out_of_bounds_targets() {
        for rel in [
            "shared/files/a.txt",
            "sync",
            "sync/",
            "sync/shared/owner.json",
            "sync/self-dev",
            "sync/self-dev/../other",
            "../sync/self-dev",
            "/sync/self-dev/main.userdb.txt",
            "sync/self-dev//x.userdb.txt",
        ] {
            assert!(
                validate_remote_delete_path(rel, "self-dev").is_err(),
                "must reject {rel}"
            );
        }
        assert!(validate_remote_delete_path("sync/other-dev", "self-dev").is_ok());
        assert!(validate_remote_delete_path("sync/other-dev/a.userdb.txt", "self-dev").is_ok());
        assert!(validate_remote_delete_path("sync/self-dev/a.userdb.txt", "self-dev").is_ok());
        assert!(validate_snapshot_delete_path("sync/other-dev/a.userdb.txt", "self-dev").is_ok());
        assert!(validate_snapshot_delete_path("sync/other-dev/a.txt", "self-dev").is_err());
        assert!(validate_snapshot_delete_path("sync/other-dev", "self-dev").is_err());
    }
}
