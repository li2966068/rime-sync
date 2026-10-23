//! 备份与回滚：apply 前快照、rollback 前再快照、按份数清理。
//!
//! 备份目录结构：<app_data>/backups/<YYYYmmdd-HHMMSS>/files/<相对路径>
//! 每条备份都带 manifest.json（相对路径 + sha256 + size），restore 先校验再写回。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::index::sha256_hex;
use crate::plan;
use crate::rime_dir;

/// 默认保留份数（SPEC-2 §2.1）。
pub const DEFAULT_KEEP: usize = 10;
const MANIFEST_NAME: &str = "manifest.json";
/// sync/ 树下只镜像 *.userdb.txt，深度上限防止异常深目录拖垮备份。
const SYNC_SCAN_DEPTH: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFile {
    pub rel: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub id: String,
    pub created_at: u64,
    pub created_at_text: String,
    pub reason: String,
    pub app_version: String,
    pub files: Vec<BackupFile>,
}

impl BackupManifest {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupInfo {
    pub id: String,
    pub path: String,
    pub created_at: u64,
    pub created_at_text: String,
    pub reason: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreReport {
    pub backup_id: String,
    pub pre_restore_id: Option<String>,
    pub restored: usize,
}

#[derive(Debug, Clone)]
pub struct BackupManager {
    rime_dir: PathBuf,
    root: PathBuf,
    sync_dir: PathBuf,
}

impl BackupManager {
    pub fn new(rime_dir: &Path, root: &Path) -> Self {
        Self {
            rime_dir: rime_dir.to_path_buf(),
            root: root.to_path_buf(),
            sync_dir: rime_dir::sync_dir(rime_dir),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 在任何写操作之前调用；返回备份 id。失败即视为不可继续（宁可不合并）。
    pub fn create(&self, reason: &str) -> Result<String> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create backup root {}", self.root.display()))?;
        let now = now_secs();
        let id = self.unique_id(now);
        let dir = self.root.join(&id);
        let files_dir = dir.join("files");
        fs::create_dir_all(&files_dir)
            .with_context(|| format!("create {}", files_dir.display()))?;

        let result = self.populate(&files_dir, &id, now, reason);
        if let Err(error) = &result {
            // 半成品备份会污染 list()，直接清掉。
            let _ = fs::remove_dir_all(&dir);
            tracing::error!("backup failed: {error:#}");
        }
        result
    }

    fn populate(&self, files_dir: &Path, id: &str, now: u64, reason: &str) -> Result<String> {
        let mut entries = Vec::new();
        self.collect_root_files(&mut entries)?;
        self.collect_snapshots(&self.sync_dir, 0, &mut entries)?;
        entries.sort();

        let mut files = Vec::new();
        for rel in entries {
            let source = self.rime_dir.join(&rel);
            let bytes = match fs::read(&source) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("read {}", source.display()))
                }
            };
            let target = files_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            fs::write(&target, &bytes).with_context(|| format!("write {}", target.display()))?;
            files.push(BackupFile {
                sha256: sha256_hex(&bytes),
                size: bytes.len() as u64,
                rel,
            });
        }

        let manifest = BackupManifest {
            id: id.to_string(),
            created_at: now,
            created_at_text: format_timestamp(now),
            reason: reason.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            files,
        };
        let text = serde_json::to_string_pretty(&manifest)?;
        let path = files_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("backup dir has no parent"))?
            .join(MANIFEST_NAME);
        fs::write(&path, format!("{text}\n"))
            .with_context(|| format!("write {}", path.display()))?;
        Ok(id.to_string())
    }

    fn unique_id(&self, now: u64) -> String {
        let base = format_timestamp(now);
        let mut candidate = base.clone();
        let mut suffix = 1u32;
        while self.root.join(&candidate).exists() {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        candidate
    }

    /// 根目录下：*.custom.yaml / *.dict.yaml / *.txt / user.yaml（只取一层）。
    fn collect_root_files(&self, out: &mut Vec<String>) -> Result<()> {
        let entries = match fs::read_dir(&self.rime_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", self.rime_dir.display()))
            }
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if is_backed_up_root_file(&name) {
                out.push(name);
            }
        }
        Ok(())
    }

    fn collect_snapshots(&self, dir: &Path, depth: usize, out: &mut Vec<String>) -> Result<()> {
        if depth > SYNC_SCAN_DEPTH || !dir.is_dir() {
            return Ok(());
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).with_context(|| format!("read {}", dir.display())),
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".userdb") {
                    continue;
                }
                self.collect_snapshots(&path, depth + 1, out)?;
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".userdb.txt") {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&self.rime_dir) else {
                continue;
            };
            let rel = relative.to_string_lossy().replace('\\', "/");
            if plan::validate_rel_path(&rel).is_ok() {
                out.push(rel);
            }
        }
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<BackupInfo>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", self.root.display()))
            }
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            let Ok(manifest) = read_manifest(&path) else {
                continue;
            };
            let file_count = manifest.files.len();
            let total_bytes = manifest.total_bytes();
            out.push(BackupInfo {
                id: manifest.id,
                path: path.display().to_string(),
                created_at: manifest.created_at,
                created_at_text: manifest.created_at_text,
                reason: manifest.reason,
                file_count,
                total_bytes,
            });
        }
        out.sort_by(|left, right| right.id.cmp(&left.id));
        Ok(out)
    }

    /// 先校验 manifest 中每个文件的 sha256，全部通过才写回；写回前自动建 pre-restore 备份。
    pub fn restore(&self, id: &str) -> Result<RestoreReport> {
        validate_backup_id(id)?;
        let dir = self.root.join(id);
        let manifest = read_manifest(&dir)?;
        let files_dir = dir.join("files");

        let mut verified = Vec::with_capacity(manifest.files.len());
        for file in &manifest.files {
            plan::validate_rel_path(&file.rel)
                .with_context(|| format!("unsafe path in manifest: {}", file.rel))?;
            plan::assert_no_userdb_dir(&file.rel)?;
            let source = files_dir.join(file.rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            let bytes = fs::read(&source)
                .with_context(|| format!("read backup file {}", source.display()))?;
            let actual = sha256_hex(&bytes);
            if actual != file.sha256 || bytes.len() as u64 != file.size {
                bail!("backup {} is corrupted: {} sha256 mismatch", id, file.rel);
            }
            verified.push((file.rel.clone(), bytes));
        }

        let pre_restore_id = self.create("pre-restore").ok();
        let mut restored = 0usize;
        for (rel, bytes) in &verified {
            let target = self
                .rime_dir
                .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            fs::write(&target, bytes).with_context(|| format!("restore {}", target.display()))?;
            restored += 1;
        }
        Ok(RestoreReport {
            backup_id: id.to_string(),
            pre_restore_id,
            restored,
        })
    }

    /// 删除单份备份。id 只允许数字与连字符，且目标必须落在备份根目录内。
    pub fn delete(&self, id: &str) -> Result<()> {
        if !is_valid_backup_id(id) {
            bail!("invalid backup id: {id}");
        }
        let root = fs::canonicalize(&self.root)
            .with_context(|| format!("backup root {} is not readable", self.root.display()))?;
        let dir = self.root.join(id);
        if !dir.exists() {
            bail!("backup {id} does not exist");
        }
        // canonicalize 会解析符号链接：指向根目录之外的条目一律拒绝。
        let target = fs::canonicalize(&dir)
            .with_context(|| format!("resolve backup dir {}", dir.display()))?;
        if target == root || !target.starts_with(&root) {
            bail!("backup {id} is outside the backup root, refusing to delete");
        }
        fs::remove_dir_all(&target).with_context(|| format!("remove {}", target.display()))?;
        Ok(())
    }

    /// 按 id 倒序保留最新 keep 份，返回被删除的 id。
    pub fn prune(&self, keep: usize) -> Result<Vec<String>> {
        let infos = self.list()?;
        let mut removed = Vec::new();
        for info in infos.into_iter().skip(keep) {
            let dir = self.root.join(&info.id);
            if dir.starts_with(&self.root) && dir.is_dir() {
                fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
                removed.push(info.id);
            }
        }
        Ok(removed)
    }
}

fn is_backed_up_root_file(name: &str) -> bool {
    name == "user.yaml"
        || name.ends_with(".custom.yaml")
        || name.ends_with(".dict.yaml")
        || name.ends_with(".txt")
}

fn read_manifest(dir: &Path) -> Result<BackupManifest> {
    let path = dir.join(MANIFEST_NAME);
    let text =
        fs::read_to_string(&path).with_context(|| format!("read manifest {}", path.display()))?;
    let manifest: BackupManifest =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(manifest)
}

/// 备份 id（时间戳形态）只含 ASCII 数字与 '-'，长度受限以防异常路径。
const MAX_BACKUP_ID_LEN: usize = 64;

fn is_valid_backup_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_BACKUP_ID_LEN
        && id.chars().all(|c| c.is_ascii_digit() || c == '-')
        && id.chars().any(|c| c.is_ascii_digit())
}

fn validate_backup_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("backup id is empty");
    }
    let path = Path::new(id);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => bail!("invalid backup id: {id}"),
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 固定 UTC 的 YYYYmmdd-HHMMSS，不引 chrono。
pub fn format_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 days -> (y, m, d)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-backup-{}-{}-{}",
            tag,
            std::process::id(),
            now_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn seed_rime(dir: &Path) {
        fs::create_dir_all(dir.join("sync/devA")).expect("sync");
        fs::create_dir_all(dir.join("main.userdb")).expect("userdb");
        fs::write(dir.join("default.custom.yaml"), b"patch:\n  a: 1\n").expect("custom");
        fs::write(dir.join("words.txt"), b"word\n").expect("txt");
        fs::write(dir.join("words.dict.yaml"), b"---\nname: words\n").expect("dict");
        fs::write(dir.join("user.yaml"), b"var: 1\n").expect("user");
        fs::write(dir.join("README.md"), b"ignored\n").expect("readme");
        fs::write(dir.join("main.userdb/CURRENT"), b"leveldb\n").expect("leveldb");
        fs::write(dir.join("sync/devA/main.userdb.txt"), b"snapshot\n").expect("snapshot");
        fs::create_dir_all(dir.join("sync/devA/backup")).expect("nested");
        fs::write(dir.join("sync/devA/backup/other.userdb.txt"), b"old\n").expect("old");
    }

    #[test]
    fn create_snapshots_expected_files_and_skips_red_lines() {
        let root = temp_root("create");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        seed_rime(&rime);
        let manager = BackupManager::new(&rime, &root.join("backups"));
        let id = manager.create("pre-apply").expect("create");
        let dir = manager.root().join(&id);
        assert!(dir.join("files/default.custom.yaml").is_file());
        assert!(dir.join("files/words.txt").is_file());
        assert!(dir.join("files/user.yaml").is_file());
        assert!(dir.join("files/sync/devA/main.userdb.txt").is_file());
        assert!(dir
            .join("files/sync/devA/backup/other.userdb.txt")
            .is_file());
        assert!(!dir.join("files/README.md").exists());
        assert!(!dir.join("files/main.userdb/CURRENT").exists());
        let manifest = read_manifest(&dir).expect("manifest");
        assert_eq!(manifest.reason, "pre-apply");
        assert!(!manifest.files.is_empty());
        assert!(manifest.files.iter().all(|file| file.sha256.len() == 64));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_verifies_manifest_and_rewrites_file() {
        let root = temp_root("restore");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        seed_rime(&rime);
        let manager = BackupManager::new(&rime, &root.join("backups"));
        let id = manager.create("pre-apply").expect("create");
        fs::write(rime.join("default.custom.yaml"), b"patch:\n  a: 2\n").expect("mutate");
        let report = manager.restore(&id).expect("restore");
        assert_eq!(report.restored, 6);
        assert!(report.pre_restore_id.is_some());
        let text = fs::read_to_string(rime.join("default.custom.yaml")).expect("read");
        assert_eq!(text, "patch:\n  a: 1\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_rejects_corrupted_backup_without_writing() {
        let root = temp_root("corrupt");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        seed_rime(&rime);
        let manager = BackupManager::new(&rime, &root.join("backups"));
        let id = manager.create("pre-apply").expect("create");
        fs::write(
            manager.root().join(&id).join("files/default.custom.yaml"),
            b"tampered",
        )
        .expect("tamper");
        fs::write(rime.join("default.custom.yaml"), b"patch:\n  a: 9\n").expect("mutate");
        assert!(manager.restore(&id).is_err());
        let text = fs::read_to_string(rime.join("default.custom.yaml")).expect("read");
        assert_eq!(text, "patch:\n  a: 9\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_keeps_newest_entries() {
        let root = temp_root("prune");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        seed_rime(&rime);
        let manager = BackupManager::new(&rime, &root.join("backups"));
        for index in 0..3 {
            fs::create_dir_all(manager.root().join(format!("2024010{index}-000000"))).expect("dir");
            fs::write(
                manager
                    .root()
                    .join(format!("2024010{index}-000000"))
                    .join("manifest.json"),
                format!(
                    r#"{{"id":"2024010{index}-000000","created_at":{},"created_at_text":"x","reason":"r","app_version":"0","files":[]}}"#,
                    index
                ),
            )
            .expect("manifest");
        }
        let removed = manager.prune(2).expect("prune");
        assert_eq!(removed, vec!["20240100-000000".to_string()]);
        assert_eq!(manager.list().expect("list").len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    fn seed_backup(manager: &BackupManager, id: &str) -> PathBuf {
        let dir = manager.root().join(id);
        fs::create_dir_all(dir.join("files")).expect("backup files dir");
        fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{id}","created_at":0,"created_at_text":"x","reason":"r","app_version":"0","files":[]}}"#
            ),
        )
        .expect("manifest");
        dir
    }

    #[test]
    fn delete_removes_backup_dir() {
        let root = temp_root("delete");
        let manager = BackupManager::new(&root, &root.join("backups"));
        let dir = seed_backup(&manager, "20240101-000000");
        manager.delete("20240101-000000").expect("delete");
        assert!(!dir.exists());
        assert!(manager.list().expect("list").is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_rejects_invalid_ids() {
        let root = temp_root("delete-badid");
        let manager = BackupManager::new(&root, &root.join("backups"));
        let long_id = "1".repeat(200);
        for id in [
            "",
            ".",
            "..",
            "../escape",
            "20240101-000000/..",
            "2024/01",
            "abc",
            "2024_01_01",
            "20240101-000000a",
            " 20240101-000000",
            "-",
            long_id.as_str(),
        ] {
            assert!(manager.delete(id).is_err(), "id should be rejected: {id:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_reports_missing_backup() {
        let root = temp_root("delete-missing");
        let manager = BackupManager::new(&root, &root.join("backups"));
        fs::create_dir_all(manager.root()).expect("root");
        let error = manager.delete("20240101-000000").expect_err("must fail");
        assert!(format!("{error:#}").contains("does not exist"), "{error:#}");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn delete_rejects_symlink_escape() {
        let root = temp_root("delete-symlink");
        let manager = BackupManager::new(&root, &root.join("backups"));
        let outside = root.join("outside");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(outside.join("keep.txt"), b"keep").expect("keep");
        fs::create_dir_all(manager.root()).expect("root");
        std::os::unix::fs::symlink(&outside, manager.root().join("20240101-000000"))
            .expect("symlink");
        let error = manager.delete("20240101-000000").expect_err("must fail");
        assert!(
            format!("{error:#}").contains("refusing to delete"),
            "{error:#}"
        );
        assert!(outside.join("keep.txt").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_with_keep_one_leaves_newest() {
        let root = temp_root("prune-one");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        let manager = BackupManager::new(&rime, &root.join("backups"));
        for index in 0..3 {
            seed_backup(&manager, &format!("2024010{index}-000000"));
        }
        let removed = manager.prune(1).expect("prune");
        assert_eq!(
            removed,
            vec!["20240101-000000".to_string(), "20240100-000000".to_string()]
        );
        let left = manager.list().expect("list");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "20240102-000000");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_with_keep_above_count_removes_nothing() {
        let root = temp_root("prune-none");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        let manager = BackupManager::new(&rime, &root.join("backups"));
        seed_backup(&manager, "20240100-000000");
        seed_backup(&manager, "20240101-000000");
        let removed = manager.prune(50).expect("prune");
        assert!(removed.is_empty());
        assert_eq!(manager.list().expect("list").len(), 2);
        assert!(manager.delete("20240100-000000").is_ok());
        assert_eq!(manager.list().expect("list").len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ids_are_unique_within_the_same_second() {
        let root = temp_root("unique");
        let rime = root.join("rime");
        fs::create_dir_all(&rime).expect("rime");
        seed_rime(&rime);
        let manager = BackupManager::new(&rime, &root.join("backups"));
        let first = manager.create("a").expect("first");
        let second = manager.create("b").expect("second");
        assert_ne!(first, second);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_id_is_rejected() {
        let root = temp_root("badid");
        let manager = BackupManager::new(&root, &root.join("backups"));
        assert!(manager.restore("../escape").is_err());
        assert!(manager.restore("").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn timestamp_is_stable() {
        assert_eq!(format_timestamp(0), "19700101-000000");
        assert_eq!(format_timestamp(1_704_067_200), "20240101-000000");
    }
}
