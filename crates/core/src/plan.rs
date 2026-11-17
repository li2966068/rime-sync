//! 文件分类、红线校验与路径安全。
//!
//! 这些函数是所有上传/下载的唯一闸门：引擎不允许绕过它们直接拼远端路径。

use std::collections::{BTreeMap, BTreeSet};

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

    /// 是否是设备配置：进 profiles/<id>/files 三向合并树。
    pub fn is_device_config(&self) -> bool {
        matches!(self, Category::CustomYaml)
    }

    /// 是否是词库源：仍走 shared/files 共享树。
    pub fn is_shared_source(&self) -> bool {
        matches!(self, Category::DictSource)
    }

    /// 是否可进入某一侧的同步树（配置进 profile，词库源进 shared）。
    pub fn is_shared(&self) -> bool {
        self.is_device_config() || self.is_shared_source()
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
    classify_with(relative_path, None)
}

/// 带「哪些 .txt 词表被 *.dict.yaml 引用过」上下文的分类。
///
/// 为什么需要上下文：Rime 用户目录里任何 .txt 都不一定是词表 ——
/// 随手放个 notes.txt / 备份片段也会被当成词库源上传到 shared/。
/// 真正的判据是「有没有被某个 *.dict.yaml 的 import_tables 引用」。
/// 传 None（没有上下文，例如校验远端清单里的路径）时保持旧行为：
/// .txt 一律当作词库源，允许通过。
pub fn classify_with(
    relative_path: &str,
    dict_sources: Option<&DictSourceIndex>,
) -> Classification {
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
    if name.ends_with(".dict.yaml") {
        return make(Category::DictSource, "dictionary source");
    }
    if name.ends_with(".txt") {
        // 有上下文时只认被 import_tables 引用过的 .txt，其余按未知文件处理（不同步）。
        // 比较用相对路径优先、basename 兜底：同名但不同目录的文件不算被引用。
        if let Some(index) = dict_sources {
            if !index.contains_rel(rel) {
                return make(Category::Unknown, "not referenced by any *.dict.yaml");
            }
        }
        return make(Category::DictSource, "dictionary source");
    }
    if is_conflict_copy(name) {
        return make(Category::Private, "local conflict copy");
    }
    make(Category::Unknown, "not a synced file kind")
}

/// 作用域目录的「未知」标记：只知道文件名、不知道在哪个目录（宽松匹配）。
const UNKNOWN_DIR: &str = "*";

/// 被某个 *.dict.yaml 的 import_tables 引用过的 .txt 词表索引。
///
/// 索引同时记两样东西：
///
/// 1. **候选相对路径**（相对 rime 用户目录根，`/` 分隔）。import_tables 里写
///    `dict/words.txt` 时就是它本身；写纯文件名 `words.txt` 时按 librime 的解析
///    基准（用户目录根）记成 `words.txt`，同时把引用它的 `*.dict.yaml` 所在目录
///    也算一份候选（很多用户把词典和词表放在同一个 `dict/` 子目录下）。
/// 2. **basename → 作用域目录集合**。比较时优先用相对路径，basename 只作兜底，
///    并且能表达「同名但不同目录 ⇒ 不算被引用」：引用 `dict/a.dict.yaml` 里的
///    `a.txt` 不会让 `extra/a.txt` 变成被引用的词表。
#[derive(Debug, Clone, Default)]
pub struct DictSourceIndex {
    paths: BTreeSet<String>,
    by_basename: BTreeMap<String, BTreeSet<String>>,
}

impl DictSourceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次引用：只知道文件名、不知道在哪个目录（宽松，命中任何同名文件）。
    ///
    /// import_tables 里既可能写全名（`words.txt`），也可能只写表名
    /// （`words`，librime 自己去补 .txt）—— 两种写法都要认。
    pub fn insert(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.paths.insert(name.to_string());
        if !name.ends_with(".txt") {
            self.paths.insert(format!("{name}.txt"));
        }
        self.by_basename
            .entry(name.to_string())
            .or_default()
            .insert(UNKNOWN_DIR.to_string());
        if !name.ends_with(".txt") {
            self.by_basename
                .entry(format!("{name}.txt"))
                .or_default()
                .insert(UNKNOWN_DIR.to_string());
        }
    }

    /// 记录一次引用：`dir` 是引用它的 `*.dict.yaml` 所在目录
    /// （相对 rime 用户目录根，`""` 表示根目录）。
    pub fn insert_ref(&mut self, dir: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let dir = dir.trim_matches('/');
        let (own_dir, base) = match name.rsplit_once('/') {
            Some((dir, base)) => (dir.trim_matches('/').to_string(), base.to_string()),
            None => (String::new(), name.to_string()),
        };
        let mut candidates: Vec<String> = Vec::new();
        let mut scopes: Vec<String> = Vec::new();
        if !own_dir.is_empty() {
            // import_tables 自带目录 ⇒ 相对 rime 根，librime 也按用户目录根解析。
            candidates.push(format!("{own_dir}/{base}"));
            scopes.push(own_dir);
        } else {
            // 纯文件名：librime 按用户目录根找；同时把词典所在目录算一份候选。
            candidates.push(base.clone());
            scopes.push(String::new());
            if !dir.is_empty() {
                candidates.push(format!("{dir}/{base}"));
                scopes.push(dir.to_string());
            }
        }
        for candidate in &candidates {
            self.paths.insert(candidate.clone());
            if !candidate.ends_with(".txt") {
                self.paths.insert(format!("{candidate}.txt"));
            }
        }
        for scope in &scopes {
            self.by_basename
                .entry(base.clone())
                .or_default()
                .insert(scope.clone());
        }
        if !base.ends_with(".txt") {
            let variant = format!("{base}.txt");
            for scope in &scopes {
                self.by_basename
                    .entry(variant.clone())
                    .or_default()
                    .insert(scope.clone());
            }
        }
    }

    /// 把远端（其它设备）上某个 `*.dict.yaml` 的内容并进索引。
    ///
    /// 远端清单里的 `.txt` 不能只看本机有没有引用：别的设备引用的词表，
    /// 本机没引用它时若判成 Unknown，那个文件就既不同步也拉不下来（半迁移）。
    /// `rel_path` 是该 dict.yaml 相对 rime 根的路径，用来定位它引用的词表目录。
    pub fn insert_remote_dict_yaml(&mut self, rel_path: &str, text: &str) {
        let dir = rel_path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default();
        for table in parse_import_tables(text) {
            self.insert_ref(&dir, &table);
        }
    }

    /// 合并另一份索引（远端字典源）。
    pub fn merge(&mut self, other: &DictSourceIndex) {
        self.paths.extend(other.paths.iter().cloned());
        for (base, scopes) in &other.by_basename {
            self.by_basename
                .entry(base.clone())
                .or_default()
                .extend(scopes.iter().cloned());
        }
    }

    /// 宽松查询：这个**文件名**（不含目录）是否被引用过。
    pub fn contains(&self, file_name: &str) -> bool {
        let base = file_name.rsplit('/').next().unwrap_or(file_name).trim();
        self.by_basename.contains_key(base)
    }

    /// 严格查询：这个**相对路径**指向的 `.txt` 是否被引用过。
    ///
    /// 相对路径优先；basename 只作兜底，而且同名不同目录不算命中。
    pub fn contains_rel(&self, relative_path: &str) -> bool {
        let rel = relative_path.replace('\\', "/");
        let rel = rel.trim_start_matches("./");
        if rel.is_empty() {
            return false;
        }
        if self.paths.contains(rel) {
            return true;
        }
        let (dir, base) = match rel.rsplit_once('/') {
            Some((dir, base)) => (dir.to_string(), base.to_string()),
            None => (String::new(), rel.to_string()),
        };
        match self.by_basename.get(&base) {
            Some(scopes) => scopes.contains(&dir) || scopes.contains(UNKNOWN_DIR),
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// 从一个 *.dict.yaml 的文本里抽出 import_tables 列出的词表名。
///
/// 认得三种写法（词表名可以带引号，引号会被剥掉）：
///
/// ```yaml
/// import_tables: [words.txt, "quoted.txt"]   # 内联数组
/// import_tables:                             # 缩进列表
///   - words.txt
///   - 'quoted.txt'
/// ```
///
/// 词典头格式是 librime 自己定的（头几行就结束），不需要完整 YAML 解析器；
/// 解析失败返回空集合，绝不 panic。
pub fn parse_import_tables(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line == "---" {
            continue;
        }
        if line == "..." {
            break;
        }
        let Some(rest) = line.strip_prefix("import_tables") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            // 缩进列表：下面连续的 `- xxx` 都算引用。
            for item in lines.by_ref() {
                let item = item.trim();
                if item.is_empty() || item.starts_with('#') {
                    continue;
                }
                let Some(value) = item.strip_prefix('-') else {
                    break;
                };
                push_table_name(&mut out, value);
            }
            continue;
        }
        if rest.starts_with('[') {
            // 内联数组：允许跨行（`[` 之后没 `]` 就接着读下一行）。
            let mut buffer = rest.to_string();
            while !buffer.contains(']') {
                let Some(next) = lines.next() else {
                    break;
                };
                buffer.push(' ');
                buffer.push_str(next.trim());
            }
            for item in split_flow_items(&buffer) {
                push_table_name(&mut out, &item);
            }
        }
    }
    out
}

/// 去掉行尾注释与引号后，把一个表名塞进结果。
fn push_table_name(out: &mut Vec<String>, raw: &str) {
    let value = strip_comment(raw);
    let value = value.trim();
    let value = unquote(value);
    if !value.is_empty() {
        out.push(value.to_string());
    }
}

/// 去掉 YAML 行尾注释（`#` 必须在引号外）。
fn strip_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut quote: Option<u8> = None;
    for (index, byte) in bytes.iter().enumerate() {
        match quote {
            Some(open) => {
                if *byte == open {
                    quote = None;
                }
            }
            None => {
                if *byte == b'"' || *byte == b'\'' {
                    quote = Some(*byte);
                } else if *byte == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
                    return &raw[..index];
                }
            }
        }
    }
    raw
}

/// 剥掉成对的单/双引号。
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// 拆内联数组 `[a.txt, "b.txt"]` 里的元素。
fn split_flow_items(buffer: &str) -> Vec<String> {
    let inner = buffer.trim();
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    inner
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// 扫描 Rime 用户目录，收集所有 *.dict.yaml 的 import_tables 里引用过的 .txt。
///
/// 解析失败的 .dict.yaml 一律当作「没有引用」（不 panic、不影响其它文件）。
///
/// 软链处理：`*.dict.yaml` 常用软链指向共享词典目录，所以判定「是不是文件」
/// 用 `fs::metadata`（跟随软链）而不是 `symlink_metadata`。为防软链成环：
/// 递归深度照旧封顶 4 层，另外按 canonical 路径去重目录与文件，并限制
/// 跟随软链的总跳数。
pub fn scan_dict_source_index(root: &std::path::Path) -> DictSourceIndex {
    const MAX_SYMLINK_HOPS: usize = 32;
    let mut index = DictSourceIndex::new();
    let mut visited_dirs: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut parsed_files: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut symlink_hops = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 4 {
            continue;
        }
        let Ok(canonical) = dir.canonicalize() else {
            continue;
        };
        if !visited_dirs.insert(canonical) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            // metadata 会跟随软链：软链指向的 *.dict.yaml 也要认。
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let is_symlink = entry
                .file_type()
                .map(|kind| kind.is_symlink())
                .unwrap_or(false);
            if meta.is_dir() {
                if is_excluded_dir(&name) {
                    continue;
                }
                if is_symlink {
                    // 软链指向目录：不递归进去（可能是环，也可能是同一份目录的另一个入口）。
                    symlink_hops += 1;
                    continue;
                }
                stack.push((path, depth + 1));
                continue;
            }
            if !meta.is_file() || !name.ends_with(".dict.yaml") {
                continue;
            }
            if is_symlink {
                symlink_hops += 1;
                if symlink_hops > MAX_SYMLINK_HOPS {
                    continue;
                }
            }
            let Ok(canonical_file) = path.canonicalize() else {
                continue;
            };
            if !parsed_files.insert(canonical_file) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let dir_rel = relative_dir(root, &path);
            for table in parse_import_tables(&text) {
                index.insert_ref(&dir_rel, &table);
            }
        }
    }
    index
}

/// 文件所在目录相对 rime 根的路径（根目录返回空串）。
fn relative_dir(root: &std::path::Path, path: &std::path::Path) -> String {
    let parent = match path.parent() {
        Some(parent) => parent,
        None => return String::new(),
    };
    parent
        .strip_prefix(root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
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

/// profile 树（profiles/<id>/files）只允许配置类文件。
pub fn assert_profile_allowed(rel: &str) -> Result<()> {
    validate_rel_path(rel)?;
    assert_no_userdb_dir(rel)?;
    let class = classify(rel);
    ensure!(
        class.category.is_device_config(),
        "refusing non-config file in profile tree: {rel} ({})",
        class.reason
    );
    Ok(())
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
        // 没有上下文时 .txt 沿用旧行为（当作词库源），保证远端清单校验不受影响。
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
    fn import_tables_are_parsed_minimally() {
        let text = "name: a\nimport_tables:\n  - words.txt\n  - 'quoted'\n...\n";
        assert_eq!(
            parse_import_tables(text),
            vec!["words.txt".to_string(), "quoted".to_string()]
        );
        // 解析不了就当没有引用，绝不 panic。
        assert!(parse_import_tables("broken: [").is_empty());
        assert!(parse_import_tables("").is_empty());
    }

    #[test]
    fn dict_source_index_accepts_bare_table_names() {
        let mut index = DictSourceIndex::new();
        // librime 的词典头里常常写表名（不带 .txt）。
        index.insert("cangjie5.base");
        assert!(index.contains("cangjie5.base"));
        assert!(index.contains("cangjie5.base.txt"));
        index.insert("words.txt");
        assert!(index.contains("words.txt"));
        assert!(!index.contains("notes.txt"));
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
