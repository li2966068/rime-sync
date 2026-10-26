//! 0.6.4：每台设备一个配置目录，嵌在该设备自己的 sync 目录下。
//!
//! 布局（远端）：
//!   sync/<installation_id>/config/manifest.json   sha/etag 索引（三向合并的远端基线）
//!   sync/<installation_id>/config/files/<rel>     配置按 Rime 目录内的原相对路径存放
//!   sync/<installation_id>/<schema>.userdb.txt    词库快照（librime 原生位置）
//!
//! 0.6.4 开发期间的旧布局是 profiles/<installation_id>/{manifest.json,files/}：
//! 见 Profile::legacy_remote_root 与 migrate_legacy_local（一次性迁移）。
//!
//! shared/files 只保留为「首次同步的默认模板」：新设备首同步时拉取一次作为初值，
//! 之后不再读写；词库源文件（*.dict.yaml / *.txt）仍走 shared/files 共享。
//!
//! 本地对应物只有一份状态文件：
//!   <state_dir>/sync/<installation_id>/config/manifest.json
//! 它既是本机配置的基线索引，也是「种子已执行」的标记——存在即不再从 shared 拉取。
//! state_dir 其余条目仍是平铺的（index.json / base / backups / tmp ...），只有配置
//! 基线按「远端同构」放进 sync/<id>/config，便于对照远端路径排查。

use anyhow::{ensure, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::FileIndex;

pub const SYNC_DIR: &str = "sync";
/// 设备配置目录名：sync/<id>/config。
pub const CONFIG_DIR: &str = "config";
pub const FILES_DIR: &str = "files";
pub const MANIFEST_NAME: &str = "manifest.json";
pub const SHARED_FILES_DIR: &str = "shared/files";
pub const SHARED_MANIFEST: &str = "shared/manifest.json";
/// 0.6.4 开发期间的旧布局根目录（仅迁移与旧设备列表兼容用）。
pub const LEGACY_PROFILES_DIR: &str = "profiles";

/// 一台设备的配置 profile 标识（即 installation_id，单一路径段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    id: String,
}

impl Profile {
    pub fn new(id: &str) -> Result<Self> {
        let id = id.trim();
        ensure!(!id.is_empty(), "profile id is empty");
        ensure!(
            !id.contains('/') && !id.contains('\\'),
            "profile id must be a single path segment: {id}"
        );
        ensure!(
            id != "." && id != ".." && !id.starts_with('.'),
            "invalid profile id: {id}"
        );
        Ok(Self { id: id.to_string() })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// sync/<id>：设备自己的远端目录（owner.json 与词库快照也在这里）。
    pub fn remote_sync(&self) -> String {
        format!("{SYNC_DIR}/{}", self.id)
    }

    /// sync/<id>/config
    pub fn remote_root(&self) -> String {
        format!("{}/{CONFIG_DIR}", self.remote_sync())
    }

    /// sync/<id>/config/files
    pub fn remote_files(&self) -> String {
        format!("{}/{FILES_DIR}", self.remote_root())
    }

    /// sync/<id>/config/manifest.json
    pub fn remote_manifest(&self) -> String {
        format!("{}/{MANIFEST_NAME}", self.remote_root())
    }

    /// sync/<id>/config/files/<rel>
    pub fn remote_file(&self, rel: &str) -> String {
        format!("{}/{rel}", self.remote_files())
    }

    /// 旧布局远端根：profiles/<id>（只在一次性迁移里读取）。
    pub fn legacy_remote_root(&self) -> String {
        format!("{LEGACY_PROFILES_DIR}/{}", self.id)
    }

    /// 本地配置状态目录：<state_dir>/sync/<id>/config。
    pub fn local_root(&self, state_dir: &Path) -> PathBuf {
        state_dir.join(SYNC_DIR).join(&self.id).join(CONFIG_DIR)
    }

    /// 本地配置基线：<state_dir>/sync/<id>/config/manifest.json。
    pub fn local_manifest(&self, state_dir: &Path) -> PathBuf {
        self.local_root(state_dir).join(MANIFEST_NAME)
    }

    /// 旧布局本地状态目录：<state_dir>/profiles/<id>。
    pub fn legacy_local_root(&self, state_dir: &Path) -> PathBuf {
        state_dir.join(LEGACY_PROFILES_DIR).join(&self.id)
    }

    /// 从远端 profile 完整路径反解出 Rime 目录内的相对路径，并过安全闸门。
    pub fn rel_from_remote_path(&self, full_remote_path: &str) -> Result<String> {
        let prefix = format!("{}/", self.remote_files());
        let inner = full_remote_path.strip_prefix(&prefix).ok_or_else(|| {
            anyhow::anyhow!(
                "profile path must start with {}: {full_remote_path}",
                prefix
            )
        })?;
        crate::plan::assert_profile_allowed(inner)?;
        Ok(inner.to_string())
    }
}

/// 读取本地 profile 基线；文件不存在时返回空索引（FileIndex::load 的既有语义）。
pub fn load_manifest(path: &Path) -> Result<FileIndex> {
    FileIndex::load(path)
}

/// 写本地/远端 profile 索引（临时文件 + rename）。
pub fn save_manifest(path: &Path, index: &FileIndex) -> Result<()> {
    index.save(path)
}

/// 本地是否已经建立过 profile（= 种子是否已执行）。
pub fn manifest_exists(path: &Path) -> bool {
    path.is_file()
}

/// 远端设备目录下的配置目录名（快照扫描必须跳过它）。
pub fn is_config_dir_name(name: &str) -> bool {
    name == CONFIG_DIR
}

/// 旧布局 profiles/ 下的目录名是否合法（迁移残留的设备页兼容扫描用）。
pub fn is_legacy_profile_dir_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name != LEGACY_PROFILES_DIR
        && !name.contains('/')
        && !name.contains('\\')
}

/// 本地配置基线的旧布局迁移：<state_dir>/profiles/<id> → <state_dir>/sync/<id>/config。
///
/// 只在「旧目录存在且新 manifest 不存在」时执行；成功后删掉旧目录（连同变空的
/// profiles/ 根）。返回 Some(note) 表示确实发生过迁移，调用方把它写进 notes。
/// 失败时不动旧目录，交由调用方告警——同步本身不因此中断。
pub fn migrate_legacy_local(profile: &Profile, state_dir: &Path) -> Result<Option<String>> {
    let legacy = profile.legacy_local_root(state_dir);
    let target = profile.local_root(state_dir);
    if !legacy.is_dir() || manifest_exists(&profile.local_manifest(state_dir)) {
        return Ok(None);
    }
    fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    copy_missing_files(&legacy, &target)?;
    // 复制完整后才删旧目录；删不掉只是留下残渣，不影响新路径。
    if let Err(error) = fs::remove_dir_all(&legacy) {
        return Ok(Some(format!(
            "已把本机配置基线 {} 迁移到 {}，但删除旧目录失败：{error}",
            legacy.display(),
            target.display()
        )));
    }
    // profiles/ 里只剩空壳时一并清掉，避免 state 目录里留旧根。
    if let Some(parent) = legacy.parent() {
        let empty = fs::read_dir(parent)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if empty {
            fs::remove_dir(parent).ok();
        }
    }
    Ok(Some(format!(
        "已把本机配置基线 {} 迁移到 {}",
        legacy.display(),
        target.display()
    )))
}

/// 递归复制 src 下的文件到 dst（保留相对结构，已存在的目标文件不覆盖）。
fn copy_missing_files(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry.with_context(|| format!("read entry of {}", src.display()))?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
            copy_missing_files(&path, &target)?;
        } else if !target.exists() {
            fs::copy(&path, &target)
                .with_context(|| format!("copy {} -> {}", path.display(), target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths() {
        let profile = Profile::new("e6169df8-uuid").expect("profile");
        assert_eq!(profile.remote_sync(), "sync/e6169df8-uuid");
        assert_eq!(profile.remote_root(), "sync/e6169df8-uuid/config");
        assert_eq!(profile.remote_files(), "sync/e6169df8-uuid/config/files");
        assert_eq!(
            profile.remote_manifest(),
            "sync/e6169df8-uuid/config/manifest.json"
        );
        assert_eq!(
            profile.remote_file("default.custom.yaml"),
            "sync/e6169df8-uuid/config/files/default.custom.yaml"
        );
        assert_eq!(profile.legacy_remote_root(), "profiles/e6169df8-uuid");
        assert_eq!(
            profile
                .rel_from_remote_path("sync/e6169df8-uuid/config/files/a.custom.yaml")
                .expect("rel"),
            "a.custom.yaml"
        );
        assert!(profile
            .rel_from_remote_path("profiles/e6169df8-uuid/files/a.custom.yaml")
            .is_err());
    }

    #[test]
    fn rejects_bad_ids_and_paths() {
        for bad in ["", " ", "a/b", "a\\b", "..", ".hidden"] {
            assert!(Profile::new(bad).is_err(), "should reject {bad}");
        }
        let profile = Profile::new("dev").expect("profile");
        assert!(profile
            .rel_from_remote_path("sync/dev/config/files/a.txt")
            .is_err());
        assert!(profile
            .rel_from_remote_path("sync/other/config/files/a.custom.yaml")
            .is_err());
        assert!(profile
            .rel_from_remote_path("sync/dev/config/files/../evil.custom.yaml")
            .is_err());
    }

    #[test]
    fn local_paths_live_under_sync_config() {
        let state = Path::new("/tmp/state");
        let profile = Profile::new("dev").expect("profile");
        assert_eq!(
            profile.local_root(state),
            PathBuf::from("/tmp/state/sync/dev/config")
        );
        assert_eq!(
            profile.local_manifest(state),
            PathBuf::from("/tmp/state/sync/dev/config/manifest.json")
        );
        assert_eq!(
            profile.legacy_local_root(state),
            PathBuf::from("/tmp/state/profiles/dev")
        );
    }

    #[test]
    fn manifest_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rime-sync-profile-{}", std::process::id()));
        let path = Profile::new("dev").expect("profile").local_manifest(&dir);
        assert!(!manifest_exists(&path));
        let mut index = FileIndex::default();
        index
            .files
            .insert("a.custom.yaml".into(), crate::index::FileMeta::local(b"x"));
        save_manifest(&path, &index).expect("save");
        assert!(manifest_exists(&path));
        let loaded = load_manifest(&path).expect("load");
        assert_eq!(loaded.files.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrates_legacy_local_baseline_once() {
        let root =
            std::env::temp_dir().join(format!("rime-sync-profile-migrate-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let profile = Profile::new("dev").expect("profile");
        let legacy = profile.legacy_local_root(&root);
        std::fs::create_dir_all(legacy.join("files")).expect("legacy dir");
        std::fs::write(legacy.join(MANIFEST_NAME), b"{}").expect("legacy manifest");
        std::fs::write(legacy.join("files/a.custom.yaml"), b"x").expect("legacy file");

        let note = migrate_legacy_local(&profile, &root)
            .expect("migrate")
            .expect("must migrate");
        assert!(note.contains("迁移"), "{note}");
        assert!(profile.local_manifest(&root).is_file());
        assert_eq!(
            std::fs::read(profile.local_root(&root).join("files/a.custom.yaml"))
                .expect("copied file"),
            b"x"
        );
        assert!(!legacy.exists(), "旧状态目录必须删除");
        assert!(
            !root.join(LEGACY_PROFILES_DIR).exists(),
            "空的 profiles/ 根也要清掉"
        );

        // 第二轮：旧目录已不存在 → 不再迁移。
        assert!(migrate_legacy_local(&profile, &root)
            .expect("second")
            .is_none());

        std::fs::remove_dir_all(&root).ok();
    }
}
