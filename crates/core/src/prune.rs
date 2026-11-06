//! 把 Rime 方案裁剪到随包保留清单（0.6.21）。
//!
//! 随包只保留 3 个方案（朙月拼音·简化字 / 五笔拼音 / 五笔86）。升级安装、自举，
//! 或者用户在用发行版数据时，data 目录、用户目录与 build 目录里仍会留着其余方案的
//! 文件；这里把它们清掉，并保证可回滚：
//!
//! * 保留清单的唯一来源是 packaging/rime/retained-schemas.txt（用 include_str!
//!   编译进来，Python 生成器与安装脚本读同一个文件）；
//! * 依赖闭包自动解析 schema 的 __include: <base>.schema:/ 与 dependencies:，
//!   不把 luna_pinyin / pinyin_simp / stroke 这类依赖当垃圾删掉；
//! * 删除前一律同目录改名备份为 <原文件名>.rimesync-removed-<时间戳>，
//!   不覆盖已有备份（同一秒重跑会追加 .1/.2）；
//! * RIMESYNC_PRUNE_SCHEMAS=0 关闭；重复执行是幂等的。

use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data_patch;
use crate::rime_dir;

/// 保留清单（编译期嵌入，与 Python/安装脚本共用同一份文件）。
pub const RETAINED_SCHEMAS_TXT: &str = include_str!("../../../packaging/rime/retained-schemas.txt");
/// 关闭开关：0 / no / off / false。
pub const ENV_PRUNE: &str = "RIMESYNC_PRUNE_SCHEMAS";
/// 追加白名单（0.6.22）：逗号分隔的方案 id，视为保留；非法项只记 warning。
pub const ENV_KEEP: &str = "RIMESYNC_PRUNE_KEEP";
/// 备份后缀；实际文件名是 <原文件名>.rimesync-removed-<时间戳>。
pub const BACKUP_SUFFIX: &str = ".rimesync-removed-";
/// 部署器在 data/ 与 build/ 里产出的方案产物后缀。
pub const PRODUCT_SUFFIXES: [&str; 3] = [".prism.bin", ".table.bin", ".reverse.bin"];
/// 用户目录里不是「某方案模板」的 *.custom.yaml：绝不能按方案清单删。
pub const NON_SCHEMA_CUSTOM: [&str; 5] =
    ["default", "weasel", "squirrel", "ibus_rime", "fcitx5_rime"];
/// 用户默认配置（schema_list 里会引用已删方案的条目，需要一并清掉）。
pub const USER_DEFAULT_CUSTOM: &str = "default.custom.yaml";

/// 保留清单里的方案 id（顺序与 schema_list 一致）。
pub fn retained_schemas() -> Vec<&'static str> {
    RETAINED_SCHEMAS_TXT
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .collect()
}

/// 是否在保留清单里（不含依赖闭包）。
pub fn is_retained(id: &str) -> bool {
    retained_schemas().contains(&id)
}

/// RIMESYNC_PRUNE_SCHEMAS 的取值判定（纯函数，便于单测）。
pub fn disabled_value(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|value| value.trim().to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "0" | "no" | "off" | "false")
    )
}

/// 环境开关是否关闭清理。
pub fn disabled_by_env() -> bool {
    disabled_value(env::var(ENV_PRUNE).ok().as_deref())
}

/// 方案 id 的合法形状：仅 [a-zA-Z0-9_-.]+（0.6.22 的追加白名单用它校验）。
pub fn valid_schema_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// 解析 RIMESYNC_PRUNE_KEEP：逗号分隔；非法项不生效，进 warnings。
pub fn parse_keep_env(raw: Option<&str>) -> (Vec<String>, Vec<String>) {
    let mut keep = Vec::new();
    let mut warnings = Vec::new();
    let Some(raw) = raw else {
        return (keep, warnings);
    };
    for item in raw.split(',') {
        let id = item.trim();
        if id.is_empty() {
            continue;
        }
        if !valid_schema_id(id) {
            warnings.push(format!(
                "{ENV_KEEP} 里的 {id:?} 不是合法方案 id（只允许 [a-zA-Z0-9_-.]+），已忽略"
            ));
            continue;
        }
        if !keep.iter().any(|known| known == id) {
            keep.push(id.to_string());
        }
    }
    (keep, warnings)
}

/// 当前进程环境里的 RIMESYNC_PRUNE_KEEP。
pub fn env_keep() -> (Vec<String>, Vec<String>) {
    parse_keep_env(env::var(ENV_KEEP).ok().as_deref())
}

/// 清理选项。
#[derive(Debug, Clone, Copy, Default)]
pub struct PruneOptions {
    /// 只报告，不落盘（安装器演练 / 排错用）。
    pub dry_run: bool,
}

/// 一次清理的结果（CLI --json / 安装日志共用）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct PruneReport {
    /// 被 RIMESYNC_PRUNE_SCHEMAS=0 关闭。
    pub disabled: bool,
    /// 演练模式：removed/backups 只是「将会发生」。
    pub dry_run: bool,
    /// 本次备份/删除使用的时间戳。
    pub timestamp: String,
    /// 保留清单（不含依赖）。
    pub retained: Vec<String>,
    /// RIMESYNC_PRUNE_KEEP 追加的 id（已校验；非法项只在 warnings 里）。
    pub kept_by_env: Vec<String>,
    /// 实际保留集合 = 保留清单 + kept_by_env + 依赖闭包。
    pub keep: Vec<String>,
    /// 非致命问题（非法的 RIMESYNC_PRUNE_KEEP 项等）。
    pub warnings: Vec<String>,
    /// 扫描过的 data 目录。
    pub data_dirs: Vec<String>,
    /// 扫描过的用户目录。
    pub user_dir: String,
    /// 已删除（或演练时将会删除）的文件。
    pub removed: Vec<String>,
    /// 备份文件（与 removed 一一对应；schema_list 改动记一条）。
    pub backups: Vec<String>,
    /// 失败原因（权限不足等）；失败不影响其余文件。
    pub failures: Vec<String>,
}

impl PruneReport {
    /// 删除的文件数。
    pub fn removed(&self) -> usize {
        self.removed.len()
    }

    /// 是否需要提示以管理员身份重试。
    pub fn needs_elevation(&self) -> bool {
        self.failures.iter().any(|item| item.contains("管理员"))
    }

    /// 一句话结论（日志 / 界面共用）。
    pub fn summary(&self) -> String {
        if self.disabled {
            return format!("方案清理：已按 {ENV_PRUNE}=0 关闭");
        }
        let mut text = format!(
            "方案清理：删除 {} 个文件，保留 {}（清单 {}），备份 {}，失败 {}",
            self.removed.len(),
            self.keep.len(),
            self.retained.join("/"),
            self.backups.len(),
            self.failures.len()
        );
        if self.dry_run {
            text.push_str("（演练，未落盘）");
        }
        if let Some(first) = self.failures.first() {
            text.push_str(&format!("；首个错误：{first}"));
        }
        text
    }
}

/// 解析 data 目录并清理（CLI / 设置页 / 启动自举统一入口）。
pub fn prune_auto() -> PruneReport {
    prune_auto_with(None, None, PruneOptions::default())
}

/// 同 prune_auto，但允许显式指定目录（安装器 / 测试 / 排错）。
pub fn prune_auto_with(
    data_dirs: Option<&[PathBuf]>,
    user_dir: Option<&Path>,
    options: PruneOptions,
) -> PruneReport {
    let (env_keep, mut warnings) = env_keep();
    let retained = retained_schemas();
    if disabled_by_env() {
        return PruneReport {
            disabled: true,
            dry_run: options.dry_run,
            retained: retained.into_iter().map(str::to_string).collect(),
            kept_by_env: env_keep,
            warnings,
            ..PruneReport::default()
        };
    }
    let timestamp = unix_timestamp();
    let dirs = match data_dirs {
        Some(dirs) if !dirs.is_empty() => dirs.to_vec(),
        _ => data_patch::resolve_data_dirs().dirs,
    };
    let user = match user_dir {
        Some(dir) => Some(dir.to_path_buf()),
        None => rime_dir::detect(None).ok().map(|detection| detection.path),
    };
    let mut report = prune_paths_with_keep(&dirs, user.as_deref(), &timestamp, options, &env_keep);
    report.warnings.append(&mut warnings);
    report
}

/// 在指定目录上执行清理（测试与安装器用；不读环境开关）。
pub fn prune_paths(
    data_dirs: &[PathBuf],
    user_dir: Option<&Path>,
    timestamp: &str,
    options: PruneOptions,
) -> PruneReport {
    // 不读环境变量：测试与安装器演练要的是确定性结果。env 白名单由
    // prune_auto_with 读取后走这里。
    prune_paths_with_keep(data_dirs, user_dir, timestamp, options, &[])
}

/// 同 prune_paths，但允许追加保留 id（RIMESYNC_PRUNE_KEEP 的解析结果）。
pub fn prune_paths_with_keep(
    data_dirs: &[PathBuf],
    user_dir: Option<&Path>,
    timestamp: &str,
    options: PruneOptions,
    extra_keep: &[String],
) -> PruneReport {
    let retained = retained_schemas();
    let mut report = PruneReport {
        disabled: false,
        dry_run: options.dry_run,
        timestamp: timestamp.to_string(),
        retained: retained.iter().map(|id| (*id).to_string()).collect(),
        kept_by_env: extra_keep.to_vec(),
        data_dirs: data_dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect(),
        user_dir: user_dir
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
        ..PruneReport::default()
    };

    // data 目录里现存的 schema 文件：既是依赖闭包的输入，也是「这个 id 确实是方案」
    // 的证据（产物文件只有在能确认归属时才删，避免误删码表 table）。
    let known = known_schemas(data_dirs);
    let keep = keep_set(&known, extra_keep);
    report.keep = keep.iter().cloned().collect();

    for dir in data_dirs {
        prune_schema_files(dir, &keep, &known, timestamp, options, &mut report);
        // data/default.yaml 仍列着上游的 16 个方案时，删掉 schema 文件会让下一次
        // 部署整次失败（librime 把缺失 schema 计为 failure），所以 schema_list 也要裁。
        prune_schema_list_file(
            &dir.join("default.yaml"),
            &keep,
            timestamp,
            options,
            &mut report,
        );
    }

    if let Some(user) = user_dir {
        let build = user.join("build");
        let mut build_known = known.clone();
        build_known.extend(known_schemas(std::slice::from_ref(&build)));
        prune_schema_files(&build, &keep, &build_known, timestamp, options, &mut report);
        prune_user_customs(user, &keep, timestamp, options, &mut report);
        prune_schema_list_file(
            &user.join(USER_DEFAULT_CUSTOM),
            &keep,
            timestamp,
            options,
            &mut report,
        );
    }
    report
}

fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// 目录里的 <id>.schema.yaml 到 id（只读文件名，不解析内容）。
///
/// 公开是为了让 data_patch 的 custom 降级算同一份保留集合（见 keep_set）。
pub fn known_schemas(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(id) = name.strip_suffix(".schema.yaml") {
                out.push((id.to_string(), path));
            }
        }
    }
    out
}

/// 保留集合 = retained-schemas.txt ∪ extra_keep，再展开依赖闭包。
///
/// prune 与 data_patch 的 Linux custom 降级共用这一份计算：两边各算一份必然漂移
/// （custom 给已裁掉的方案生成 custom.yaml，prune 下一次启动又把它删掉）。
pub fn keep_set(known: &[(String, PathBuf)], extra_keep: &[String]) -> BTreeSet<String> {
    let mut keep: BTreeSet<String> = retained_schemas()
        .iter()
        .map(|id| (*id).to_string())
        .collect();
    keep.extend(extra_keep.iter().cloned());
    expand_keep(known, &mut keep);
    keep
}

/// 展开依赖闭包：__include: <x>.schema:/、dependencies: 与 dictionary:。
fn expand_keep(known: &[(String, PathBuf)], keep: &mut BTreeSet<String>) {
    let mut queue: Vec<String> = keep.iter().cloned().collect();
    while let Some(id) = queue.pop() {
        let Some((_, path)) = known.iter().find(|(name, _)| *name == id) else {
            continue;
        };
        for dep in schema_refs(path) {
            if !dep.is_empty() && keep.insert(dep.clone()) {
                queue.push(dep);
            }
        }
    }
}

fn schema_refs(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(include) = doc.get("__include").and_then(|value| value.as_str()) {
        if let Some(base) = include.strip_suffix(".schema:/") {
            out.push(base.trim().to_string());
        }
    }
    if let Some(list) = doc
        .get("schema")
        .and_then(|value| value.get("dependencies"))
        .and_then(|value| value.as_sequence())
    {
        for item in list {
            match item {
                serde_yaml::Value::String(name) => out.push(name.clone()),
                serde_yaml::Value::Mapping(map) => {
                    if let Some(name) = map.get("schema").and_then(|value| value.as_str()) {
                        out.push(name.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    // dictionary: 指向的码表（<name>.dict.yaml / <name>.table.bin）也是依赖：
    // 只被 translator/reverse_lookup 的 dictionary 引用、没有 dependencies 的
    // 码表不能被当垃圾删掉。字典项可以嵌套（translator.dictionary 等），递归收集。
    let mut dictionaries = Vec::new();
    collect_dictionaries(&doc, &mut dictionaries);
    out.extend(dictionaries);
    out
}

/// 递归收集所有 `dictionary: <name>` 的字符串取值（空串 / 通配不算）。
fn collect_dictionaries(value: &serde_yaml::Value, out: &mut Vec<String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (key, item) in map {
                if key.as_str() == Some("dictionary") {
                    if let Some(name) = item.as_str() {
                        let name = name.trim();
                        if !name.is_empty() && name != "*" {
                            out.push(name.to_string());
                        }
                    }
                }
                collect_dictionaries(item, out);
            }
        }
        serde_yaml::Value::Sequence(list) => {
            for item in list {
                collect_dictionaries(item, out);
            }
        }
        _ => {}
    }
}

/// 删除 data/ 或 build/ 里非保留方案的 <id>.schema.yaml 与配套产物。
fn prune_schema_files(
    dir: &Path,
    keep: &BTreeSet<String>,
    known: &[(String, PathBuf)],
    timestamp: &str,
    options: PruneOptions,
    report: &mut PruneReport,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    let ids_here: BTreeSet<String> = names
        .iter()
        .filter_map(|name| name.strip_suffix(".schema.yaml").map(str::to_string))
        .collect();
    for name in names {
        let remove = if let Some(id) = name.strip_suffix(".schema.yaml") {
            !keep.contains(id)
        } else if let Some(id) = strip_product(&name) {
            // 产物只有在能确认归属（同目录的 schema 文件或 data 目录里登记过）时才删，
            // 避免把 luna_pinyin.extended.table.bin 这类码表当成方案产物。
            !keep.contains(&id) && (ids_here.contains(&id) || known.iter().any(|(k, _)| *k == id))
        } else {
            false
        };
        if remove {
            backup_and_remove(&dir.join(&name), timestamp, options, report);
        }
    }
}

fn strip_product(name: &str) -> Option<String> {
    for suffix in PRODUCT_SUFFIXES {
        if let Some(stem) = name.strip_suffix(suffix) {
            return Some(stem.to_string());
        }
    }
    None
}

/// 用户目录里非保留方案的 <id>.custom.yaml。
fn prune_user_customs(
    user: &Path,
    keep: &BTreeSet<String>,
    timestamp: &str,
    options: PruneOptions,
    report: &mut PruneReport,
) {
    let Ok(entries) = fs::read_dir(user) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        let Some(id) = name.strip_suffix(".custom.yaml") else {
            continue;
        };
        if NON_SCHEMA_CUSTOM.contains(&id) || keep.contains(id) {
            continue;
        }
        backup_and_remove(&user.join(&name), timestamp, options, report);
    }
}

/// default.custom.yaml / data/default.yaml 的 schema_list：删掉已移除方案的条目
/// （否则部署会因为「schema 不存在」整次失败）。其余行（注释、page_size…）原样保留。
fn prune_schema_list_file(
    path: &Path,
    keep: &BTreeSet<String>,
    timestamp: &str,
    options: PruneOptions,
    report: &mut PruneReport,
) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let mut out: Vec<&str> = Vec::new();
    let mut in_list = false;
    let mut key_indent = 0usize;
    let mut dropped = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if !in_list && trimmed == "schema_list:" {
            in_list = true;
            key_indent = indent;
            out.push(line);
            continue;
        }
        if in_list && !trimmed.is_empty() && !trimmed.starts_with('-') && indent <= key_indent {
            in_list = false;
        }
        if in_list {
            if let Some(id) = schema_list_item(line) {
                if !keep.contains(id) {
                    dropped += 1;
                    continue;
                }
            }
        }
        out.push(line);
    }
    if dropped == 0 {
        return;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let backup = backup_path(path, timestamp);
    report.backups.push(backup.display().to_string());
    if options.dry_run {
        report
            .removed
            .push(format!("{name}: schema_list -{dropped}"));
        return;
    }
    if let Err(error) = fs::copy(path, &backup) {
        report.failures.push(format!("{}: {error}", path.display()));
        return;
    }
    let mut body = out.join("\n");
    if text.ends_with('\n') {
        body.push('\n');
    }
    match fs::write(path, body) {
        Ok(()) => report.removed.push(format!(
            "{name}: schema_list -{dropped}（{}）",
            path.display()
        )),
        Err(error) => report.failures.push(format!("{}: {error}", path.display())),
    }
}

/// - {schema: x} / - schema: x 到 x。
pub fn schema_list_item(line: &str) -> Option<&str> {
    let mut item = line.trim();
    item = item.strip_prefix('-')?.trim();
    item = item.strip_prefix('{').unwrap_or(item);
    item = item.strip_suffix('}').unwrap_or(item).trim();
    let id = item.strip_prefix("schema:")?.trim();
    let id = id.trim_matches(|ch| ch == '"' || ch == '\'');
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some(id)
}

fn backup_path(path: &Path, timestamp: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut candidate = path.with_file_name(format!("{name}{BACKUP_SUFFIX}{timestamp}"));
    let mut serial = 1;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{name}{BACKUP_SUFFIX}{timestamp}.{serial}"));
        serial += 1;
    }
    candidate
}

fn backup_and_remove(
    path: &Path,
    timestamp: &str,
    options: PruneOptions,
    report: &mut PruneReport,
) {
    let backup = backup_path(path, timestamp);
    report.backups.push(backup.display().to_string());
    report.removed.push(path.display().to_string());
    if options.dry_run {
        return;
    }
    match fs::rename(path, &backup) {
        Ok(()) => {}
        Err(error) => {
            report.removed.pop();
            report.backups.pop();
            let hint = if error.kind() == std::io::ErrorKind::PermissionDenied {
                "（请以管理员身份重试）"
            } else {
                ""
            };
            report
                .failures
                .push(format!("{}: {error}{hint}", path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "rime-sync-prune-{}-{}-{}",
            std::process::id(),
            tag,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir -p");
        }
        fs::write(path, text).expect("write");
    }

    fn fake_schema(dir: &Path, id: &str) {
        write(
            &dir.join(format!("{id}.schema.yaml")),
            &format!("schema:\n  schema_id: {id}\n"),
        );
        for suffix in PRODUCT_SUFFIXES {
            write(&dir.join(format!("{id}{suffix}")), "bin");
        }
    }

    #[test]
    fn retained_list_is_three_with_the_expected_order() {
        assert_eq!(
            retained_schemas(),
            ["luna_pinyin_simp", "wubi_pinyin", "wubi86"]
        );
        assert!(is_retained("wubi86"));
        assert!(!is_retained("terra_pinyin"));
    }

    #[test]
    fn env_switch_recognises_off_values() {
        for value in ["0", "no", "NO", " off ", "false"] {
            assert!(disabled_value(Some(value)), "{value}");
        }
        for value in ["1", "force", "", "yes"] {
            assert!(!disabled_value(Some(value)), "{value}");
        }
        assert!(!disabled_value(None));
    }

    #[test]
    fn schema_list_item_matches_both_styles() {
        assert_eq!(schema_list_item("    - {schema: wubi86}"), Some("wubi86"));
        assert_eq!(
            schema_list_item("  - schema: wubi_pinyin"),
            Some("wubi_pinyin")
        );
        assert_eq!(schema_list_item("  - schema: luna_pinyin_simp # 注"), None);
        assert_eq!(schema_list_item("  page_size: 8"), None);
    }

    #[test]
    fn prunes_extra_schemas_and_keeps_dependencies() {
        let root = scratch("main");
        let data = root.join("data");
        let user = root.join("user");
        let build = user.join("build");
        // 保留 3 个 + 一个只作为 luna_pinyin_simp __include 基类的方案
        write(
            &data.join("luna_pinyin_simp.schema.yaml"),
            "schema:\n  schema_id: luna_pinyin_simp\n__include: luna_pinyin.schema:/\n",
        );
        // luna_pinyin 声明 stroke 依赖：两个都必须在保留集合里
        write(
            &data.join("luna_pinyin.schema.yaml"),
            "schema:\n  schema_id: luna_pinyin\n  dependencies:\n    - stroke\n",
        );
        fake_schema(&data, "wubi_pinyin");
        fake_schema(&data, "wubi86");
        fake_schema(&data, "stroke");
        // 要删掉的方案
        for id in ["terra_pinyin", "bopomofo"] {
            fake_schema(&data, id);
            write(&user.join(format!("{id}.custom.yaml")), "patch: {}\n");
            write(
                &build.join(format!("{id}.schema.yaml")),
                &format!("schema:\n  schema_id: {id}\n"),
            );
            write(&build.join(format!("{id}.table.bin")), "bin");
        }
        for id in ["wubi_pinyin", "wubi86", "luna_pinyin_simp"] {
            write(&user.join(format!("{id}.custom.yaml")), "patch: {}\n");
            write(
                &build.join(format!("{id}.schema.yaml")),
                &format!("schema:\n  schema_id: {id}\n"),
            );
        }
        write(&user.join("weasel.custom.yaml"), "patch: {}\n");
        write(
            &data.join("default.yaml"),
            "schema_list:\n  - schema: luna_pinyin_simp\n  - schema: wubi_pinyin\n  - schema: wubi86\n  - schema: terra_pinyin\n  - schema: bopomofo\n\nswitcher:\n  hotkeys:\n    - Control+grave\n",
        );
        write(
            &user.join("default.custom.yaml"),
            "# 注释保留\npatch:\n  \"menu/page_size\": 8\n  schema_list:\n    - {schema: luna_pinyin_simp}\n    - {schema: wubi_pinyin}\n    - {schema: wubi86}\n    - {schema: terra_pinyin}\n    - {schema: bopomofo}\n\n  other: 1\n",
        );

        let report = prune_paths(
            std::slice::from_ref(&data),
            Some(&user),
            "T0",
            PruneOptions::default(),
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(report
            .removed
            .iter()
            .any(|p| p.ends_with("terra_pinyin.schema.yaml")));
        assert!(report
            .removed
            .iter()
            .any(|p| p.ends_with("bopomofo.table.bin")));
        assert!(!data.join("terra_pinyin.schema.yaml").exists());
        assert!(!user.join("terra_pinyin.custom.yaml").exists());
        assert!(!build.join("bopomofo.schema.yaml").exists());
        // 备份都在原目录，名字带后缀
        assert!(data
            .join("terra_pinyin.schema.yaml.rimesync-removed-T0")
            .is_file());
        assert!(user
            .join("terra_pinyin.custom.yaml.rimesync-removed-T0")
            .is_file());
        // 保留方案与依赖一个字都没动
        for id in [
            "luna_pinyin_simp",
            "wubi_pinyin",
            "wubi86",
            "luna_pinyin",
            "stroke",
        ] {
            assert!(data.join(format!("{id}.schema.yaml")).is_file(), "{id}");
        }
        assert!(user.join("wubi86.custom.yaml").is_file());
        assert!(build.join("wubi86.schema.yaml").is_file());
        assert!(user.join("weasel.custom.yaml").is_file());
        // schema_list 只留下 3 个保留方案，其余行原样
        let text = fs::read_to_string(user.join("default.custom.yaml")).expect("read");
        assert!(!text.contains("terra_pinyin"), "{text}");
        assert!(!text.contains("bopomofo"), "{text}");
        assert!(text.contains("# 注释保留"), "{text}");
        assert!(text.contains("  other: 1"), "{text}");
        assert!(user
            .join("default.custom.yaml.rimesync-removed-T0")
            .is_file());
        // data/default.yaml 的 schema_list 也要裁剪（否则部署会因缺 schema 整次失败）
        let data_default = fs::read_to_string(data.join("default.yaml")).expect("read");
        assert!(!data_default.contains("terra_pinyin"), "{data_default}");
        assert!(!data_default.contains("bopomofo"), "{data_default}");
        assert!(
            data_default.contains("  - schema: wubi86"),
            "{data_default}"
        );
        assert!(data_default.contains("switcher:"), "{data_default}");
        assert!(data.join("default.yaml.rimesync-removed-T0").is_file());

        // 幂等：再跑一次什么都不删
        let again = prune_paths(
            std::slice::from_ref(&data),
            Some(&user),
            "T1",
            PruneOptions::default(),
        );
        assert!(again.failures.is_empty(), "{:?}", again.failures);
        assert_eq!(again.removed.len(), 0, "{:?}", again.removed);
        assert_eq!(again.backups.len(), 0, "{:?}", again.backups);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dictionary_refs_keep_tables_without_dependencies() {
        let root = scratch("dictionary");
        let data = root.join("data");
        // 保留方案 wubi86 只在 translator.dictionary 里引用共享码表：旧实现只看
        // __include/dependencies，会把 shared_dict 的 schema 与 table.bin 删掉。
        write(
            &data.join("wubi86.schema.yaml"),
            "schema:\n  schema_id: wubi86\ntranslator:\n  dictionary: shared_dict\n",
        );
        // 对照：没有任何引用（也没有 dependencies）的码表必须照旧清理。
        write(
            &data.join("other_dict.schema.yaml"),
            "schema:\n  schema_id: other_dict\n",
        );
        write(
            &data.join("shared_dict.schema.yaml"),
            "schema:\n  schema_id: shared_dict\n",
        );
        write(&data.join("shared_dict.table.bin"), "bin");
        write(&data.join("shared_dict.dict.yaml"), "name: shared_dict\n");
        write(&data.join("other_dict.table.bin"), "bin");
        let report = prune_paths(
            std::slice::from_ref(&data),
            None,
            "T0",
            PruneOptions::default(),
        );
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(
            report.keep.iter().any(|id| id == "shared_dict"),
            "dictionary 引用没进闭包：{:?}",
            report.keep
        );
        assert!(data.join("shared_dict.schema.yaml").is_file());
        assert!(data.join("shared_dict.table.bin").is_file());
        assert!(data.join("shared_dict.dict.yaml").is_file());
        assert!(!data.join("other_dict.schema.yaml").exists());
        assert!(!data.join("other_dict.table.bin").exists());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn env_keep_parsing_validates_ids() {
        let (keep, warnings) = parse_keep_env(Some(" wubi86 ,terra_pinyin,, bad id!,中文"));
        assert_eq!(keep, vec!["wubi86", "terra_pinyin"]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let (empty, none) = parse_keep_env(None);
        assert!(empty.is_empty());
        assert!(none.is_empty());
    }

    #[test]
    fn env_keep_prevents_removal() {
        let root = scratch("env-keep");
        let data = root.join("data");
        let user = root.join("user");
        fake_schema(&data, "terra_pinyin");
        let previous = env::var_os(ENV_KEEP);
        env::set_var(ENV_KEEP, "terra_pinyin,bad id!");
        let report = prune_auto_with(
            Some(std::slice::from_ref(&data)),
            Some(&user),
            PruneOptions::default(),
        );
        match previous {
            Some(value) => env::set_var(ENV_KEEP, value),
            None => env::remove_var(ENV_KEEP),
        }
        assert_eq!(report.kept_by_env, vec!["terra_pinyin"]);
        assert!(
            report.warnings.iter().any(|item| item.contains("bad id!")),
            "{:?}",
            report.warnings
        );
        assert!(report.keep.iter().any(|id| id == "terra_pinyin"));
        assert_eq!(report.removed.len(), 0, "{:?}", report.removed);
        assert!(data.join("terra_pinyin.schema.yaml").is_file());
        assert!(data.join("terra_pinyin.table.bin").is_file());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dry_run_reports_without_touching_disk() {
        let root = scratch("dry");
        let data = root.join("data");
        fake_schema(&data, "terra_pinyin");
        let report = prune_paths(
            std::slice::from_ref(&data),
            None,
            "T0",
            PruneOptions { dry_run: true },
        );
        assert!(report.dry_run);
        assert!(report
            .removed
            .iter()
            .any(|p| p.ends_with("terra_pinyin.schema.yaml")));
        assert!(data.join("terra_pinyin.schema.yaml").is_file());
        assert!(!data
            .join("terra_pinyin.schema.yaml.rimesync-removed-T0")
            .exists());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn backup_name_does_not_clobber_previous_runs() {
        let root = scratch("backup");
        let file = root.join("terra_pinyin.schema.yaml");
        write(&file, "x");
        let first = backup_path(&file, "T0");
        write(&first, "old");
        let second = backup_path(&file, "T0");
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with("T0.1"));
        fs::remove_dir_all(&root).ok();
    }
}
