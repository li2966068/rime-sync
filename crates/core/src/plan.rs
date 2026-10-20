//! 文件分类、红线校验与路径安全。
//!
//! 这些函数是所有上传/下载的唯一闸门：引擎不允许绕过它们直接拼远端路径。

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Rime 原生同步快照，走 sync/<device>/ 树。
    UserdbSnapshot,
    /// *.custom.yaml / user.yaml / *.schema.yaml。
    CustomYaml,
    /// *.dict.yaml / *.txt 词表。
    DictSource,
    /// build 产物、*.bin、*.gram。
    EngineArtifact,
    /// LevelDB 目录/文件（硬红线）。
    LevelDb,
    /// installation.yaml、sync/ 根、.rime-sync/。
    Private,
    /// 不认识的扩展名，不参与同步。
    Unknown,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::UserdbSnapshot => "userdb_snapshot",
            Category::CustomYaml => "custom_yaml",
            Category::DictSource => "dict_source",
            Category::EngineArtifact => "engine_artifact",
            Category::LevelDb => "leveldb",
            Category::Private => "private",
            Category::Unknown => "unknown",
        }
    }

    /// 是否进入 shared 三向合并树。
    pub fn is_shared(&self) -> bool {
        matches!(self, Category::CustomYaml | Category::DictSource)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Classification {
    pub category: Category,
    /// 是否可同步（快照走 sync 树，shared 类走合并树，其余均 false）。
    pub syncable: bool,
    pub reason: &'static str,
}

pub fn classify(relative_path: &str) -> Classification {
    let rel = relative_path.replace('\\', "/");
    let rel = rel.trim_start_matches("./");
    let components: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
    let name = components.last().copied().unwrap_or("");

    let make = |category: Category, reason: &'static str| Classification {
        category,
        syncable: matches!(
            category,
            Category::UserdbSnapshot | Category::CustomYaml | Category::DictSource
        ),
        reason,
    };

    if components.contains(&".rime-sync") {
        return make(Category::Private, "app state directory");
    }
    if components.iter().any(|c| c.ends_with(".userdb")) || name.ends_with(".userdb") {
        return make(Category::LevelDb, "LevelDB userdb is a hard red line");
    }
    if name == "installation.yaml" {
        return make(Category::Private, "per-device private metadata");
    }
    if components.contains(&"build") {
        return make(Category::EngineArtifact, "build output");
    }
    if name.ends_with(".bin") || name.ends_with(".gram") {
        return make(Category::EngineArtifact, "compiled engine artifact");
    }
    if components.first() == Some(&"sync") {
        if name.ends_with(".userdb.txt") {
            return make(Category::UserdbSnapshot, "rime sync snapshot");
        }
        return make(Category::Private, "remote owner/session metadata");
    }
    if name.ends_with(".userdb.txt") {
        return make(Category::UserdbSnapshot, "rime sync snapshot");
    }
    if name == "user.yaml" || name.ends_with(".custom.yaml") || name.ends_with(".schema.yaml") {
        return make(Category::CustomYaml, "rime config");
    }
    if name.ends_with(".dict.yaml") || name.ends_with(".txt") {
        return make(Category::DictSource, "dictionary source");
    }
    if is_conflict_copy(name) {
        return make(Category::Private, "local conflict copy");
    }
    make(Category::Unknown, "not a synced file kind")
}

/// 冲突副本形如 default.custom.yaml.conflict-devA-1700000000。
pub fn is_conflict_copy(file_name: &str) -> bool {
    file_name.contains(".conflict-")
}

fn has_userdb_component(rel: &str) -> bool {
    rel.split('/').any(|c| c.ends_with(".userdb"))
}

fn is_single_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && value != "."
        && value != ".."
}

/// 远端返回或配置里来的相对路径都要过这一关（防 ../ 穿越）。
pub fn validate_rel_path(rel: &str) -> Result<()> {
    ensure!(!rel.is_empty(), "empty path");
    ensure!(!rel.contains('\0'), "path contains NUL");
    ensure!(
        !rel.starts_with('/') && !rel.starts_with('\\'),
        "absolute path rejected: {rel}"
    );
    ensure!(!rel.starts_with('~'), "home-relative path rejected: {rel}");
    for component in rel.split('/') {
        ensure!(!component.is_empty(), "empty path component: {rel}");
        ensure!(component != ".", "current-directory component: {rel}");
        ensure!(component != "..", "path traversal rejected: {rel}");
        ensure!(
            !component.contains('\\'),
            "backslash component rejected: {rel}"
        );
    }
    if let Some(first) = rel.split('/').next() {
        let bytes = first.as_bytes();
        ensure!(
            !(bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'),
            "windows drive prefix rejected: {rel}"
        );
    }
    Ok(())
}

pub fn assert_no_userdb_dir(rel: &str) -> Result<()> {
    ensure!(
        !has_userdb_component(rel),
        "refusing to touch *.userdb directory: {rel}"
    );
    Ok(())
}

/// shared 树里只允许配置/词典源；*.userdb.txt、installation.yaml、build 全部拒绝。
pub fn assert_shared_allowed(rel: &str) -> Result<()> {
    validate_rel_path(rel)?;
    assert_no_userdb_dir(rel)?;
    let class = classify(rel);
    ensure!(
        class.category.is_shared(),
        "refusing non-shared file in shared tree: {rel} ({})",
        class.reason
    );
    Ok(())
}

/// full remote path 形如 shared/files/<rel>。
pub fn assert_remote_shared_path(full_remote_path: &str) -> Result<String> {
    validate_rel_path(full_remote_path)?;
    let inner = full_remote_path
        .strip_prefix("shared/files/")
        .ok_or_else(|| {
            anyhow::anyhow!("shared path must start with shared/files/: {full_remote_path}")
        })?;
    assert_shared_allowed(inner)?;
    Ok(inner.to_string())
}

/// full remote path 形如 sync/<device>/<file>.userdb.txt。
pub fn assert_remote_snapshot_path(full_remote_path: &str) -> Result<()> {
    validate_rel_path(full_remote_path)?;
    assert_no_userdb_dir(full_remote_path)?;
    let parts: Vec<&str> = full_remote_path.split('/').collect();
    ensure!(
        parts.len() == 3,
        "unexpected snapshot path: {full_remote_path}"
    );
    ensure!(
        parts[0] == "sync",
        "snapshot must live under sync/: {full_remote_path}"
    );
    ensure!(
        is_single_segment(parts[1]),
        "invalid device id in remote path: {full_remote_path}"
    );
    ensure!(
        parts[2].ends_with(".userdb.txt"),
        "only *.userdb.txt may be mirrored: {full_remote_path}"
    );
    Ok(())
}

/// 本地上传前最后一道闸门。
pub fn assert_upload_allowed(local_rel: &str) -> Result<()> {
    validate_rel_path(local_rel)?;
    assert_no_userdb_dir(local_rel)?;
    if local_rel.split('/').any(|c| c == ".rime-sync") {
        bail!("refusing to upload app state: {local_rel}");
    }
    if local_rel.split('/').any(|c| c == "installation.yaml") {
        bail!("refusing to upload installation.yaml: {local_rel}");
    }
    Ok(())
}

/// 扫描 rime 目录时要跳过的目录名。
pub fn is_excluded_dir(name: &str) -> bool {
    name == "build"
        || name == "sync"
        || name == ".rime-sync"
        || name.starts_with('.')
        || name.ends_with(".userdb")
}

/// 简易 glob：单个星号不跨目录，双星号跨目录，问号匹配单字符。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(pattern: &[char], text: &[char]) -> bool {
        match pattern.first() {
            None => text.is_empty(),
            Some('*') => {
                let double = pattern.get(1) == Some(&'*');
                let rest = if double { &pattern[2..] } else { &pattern[1..] };
                if go(rest, text) {
                    return true;
                }
                let mut i = 0;
                while i < text.len() && (double || text[i] != '/') {
                    i += 1;
                    if go(rest, &text[i..]) {
                        return true;
                    }
                }
                false
            }
            Some('?') => !text.is_empty() && text[0] != '/' && go(&pattern[1..], &text[1..]),
            Some(expected) => {
                !text.is_empty() && text[0] == *expected && go(&pattern[1..], &text[1..])
            }
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    go(&pattern, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matrix() {
        assert_eq!(
            classify("sync/devA/main.userdb.txt").category,
            Category::UserdbSnapshot
        );
        assert_eq!(
            classify("default.custom.yaml").category,
            Category::CustomYaml
        );
        assert_eq!(classify("user.yaml").category, Category::CustomYaml);
        assert_eq!(
            classify("rime_ice.schema.yaml").category,
            Category::CustomYaml
        );
        assert_eq!(classify("words.dict.yaml").category, Category::DictSource);
        assert_eq!(classify("words.txt").category, Category::DictSource);
        assert_eq!(
            classify("build/main.bin").category,
            Category::EngineArtifact
        );
        assert_eq!(
            classify("main.prism.bin").category,
            Category::EngineArtifact
        );
        assert_eq!(
            classify("main.table.bin").category,
            Category::EngineArtifact
        );
        assert_eq!(classify("main.userdb/CURRENT").category, Category::LevelDb);
        assert_eq!(classify("installation.yaml").category, Category::Private);
        assert_eq!(classify("sync/devA/owner.json").category, Category::Private);
        assert_eq!(
            classify(".rime-sync/index.json").category,
            Category::Private
        );
        assert_eq!(classify("README.md").category, Category::Unknown);
    }

    #[test]
    fn userdb_dir_is_hard_red_line() {
        assert!(assert_no_userdb_dir("main.userdb/CURRENT").is_err());
        assert!(assert_no_userdb_dir("nested/foo.userdb/data/LOG").is_err());
        assert!(assert_upload_allowed("main.userdb/CURRENT").is_err());
        assert!(assert_shared_allowed("main.userdb/CURRENT").is_err());
        assert!(assert_no_userdb_dir("main.userdb.txt").is_ok());
    }

    #[test]
    fn userdb_snapshot_must_not_enter_shared() {
        assert!(assert_shared_allowed("main.userdb.txt").is_err());
        assert!(assert_remote_shared_path("shared/files/main.userdb.txt").is_err());
        assert!(assert_upload_allowed("main.userdb.txt").is_ok());
    }

    #[test]
    fn installation_yaml_never_syncs() {
        assert!(assert_shared_allowed("installation.yaml").is_err());
        assert!(assert_upload_allowed("installation.yaml").is_err());
        assert!(!classify("installation.yaml").syncable);
    }

    #[test]
    fn traversal_is_rejected() {
        for bad in [
            "../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "C:/Windows/win.ini",
            "a\\b",
            "a//b",
            "./a",
            "~/secret",
        ] {
            assert!(validate_rel_path(bad).is_err(), "should reject {bad}");
        }
        assert!(validate_rel_path("a/b.custom.yaml").is_ok());
        assert!(assert_remote_shared_path("shared/files/../../etc/passwd").is_err());
        assert!(assert_remote_snapshot_path("sync/dev/../../etc/passwd").is_err());
        assert!(assert_remote_snapshot_path("sync/dev/evil.userdb.txt").is_ok());
    }

    #[test]
    fn remote_snapshot_path_shape() {
        assert!(assert_remote_snapshot_path("sync/devA/main.userdb.txt").is_ok());
        assert!(assert_remote_snapshot_path("sync/devA/main.yaml").is_err());
        assert!(assert_remote_snapshot_path("sync/devA/x.userdb/CURRENT").is_err());
        assert!(assert_remote_snapshot_path("shared/x.userdb.txt").is_err());
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.tmp", "a.tmp"));
        assert!(!glob_match("*.tmp", "dir/a.tmp"));
        assert!(glob_match("build/**", "build/a/b.bin"));
        assert!(glob_match("**/*.bak", "a/b/c.bak"));
        assert!(glob_match("secret?:.yaml", "secret1:.yaml"));
        assert!(!glob_match("*.yaml", "a.yamlx"));
    }
}
