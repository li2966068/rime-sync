//! 合并词典指针：新设备接入时自动把方案指向合并词典。
//!
//! 背景：librime 的 `translator/dictionary` 只接受**一个**字符串，多个词库想合到
//! 一起用必须靠词典头部的 `import_tables`（<name>.dict.yaml 里 `import_tables: [...]`）。
//! 所以「让某个方案用上合并词典」= 在该方案的 <schema>.custom.yaml 里补一条
//!
//! ```text
//! patch:
//!   "translator/dictionary": <合并词典名>
//! ```
//!
//! 本模块负责三件事：
//!
//! 1. `detect`：在本机 Rime 目录里找「合并词典包装器」（带 `import_tables` 的
//!    `*.dict.yaml`），拿到它的名字与它导入的基础词库清单；
//! 2. `schema_dictionary`：算出某个方案最终用的基础词库（跟随 `__include` 链，
//!    方案自身的字面量优先，与 librime 的合并语义一致）；
//! 3. `ensure_pointers`：当「方案用的基础词库 ∈ 合并词典的 import_tables」时，
//!    在该方案的 <schema>.custom.yaml 里补上 `translator/dictionary` 指针。
//!
//! 安全约定：
//!
//! * **只在键缺失时补**。用户手写的 `translator/dictionary`（引号/非引号两种写法
//!   都认）一律保留，绝不覆盖；
//! * 只改「补一行指针」这一件事，文件其余内容、注释、缩进风格、行尾、BOM 全部保留
//!   （用 `yaml_text::RawLines` 逐行编辑，不用会重排全文的 `Doc`）；
//! * 写盘前用 serde_yaml 复核：文件仍是合法 YAML，且指针确实落在 `patch` 下；
//! * 写盘前先备份（`data_patch::backup_path`，只建一次不覆盖）。

use crate::yaml_text::{
    block_end, indent_of, is_blank, is_comment, yaml_key, yaml_scalar, RawLines,
};
use anyhow::{anyhow, Context, Result};
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 合并词典包装器：一个带 `import_tables` 的 `*.dict.yaml`。
#[derive(Debug, Clone)]
pub struct MergedDict {
    /// 词典头里的 `name`（写进 custom.yaml 的就是这个值，不硬编码）。
    pub name: String,
    /// 它导入的基础词库（方案用的基础词库命中其中之一才值得指过来）。
    pub import_tables: Vec<String>,
}

/// 一次指针补写的结果。
#[derive(Debug, Clone)]
pub enum PointerChange {
    /// 指针缺失，已写入。
    Written { schema: String, path: PathBuf },
    /// 已经有 `translator/dictionary`（用户手写或先前已写），保持原样。
    Present { schema: String },
    /// 没有动，原因见 `reason`。
    Skipped { schema: String, reason: String },
}

impl PointerChange {
    /// 是否真的写了东西。
    pub fn wrote(&self) -> bool {
        matches!(self, PointerChange::Written { .. })
    }
}

/// 词典头最多读这么多字节（真实词典头部只有几百字节；异常文件不至于把内存吃满）。
const MAX_HEADER_BYTES: usize = 1024 * 1024;

/// 在 Rime 目录里探测合并词典包装器。
///
/// 只看顶层（共享词典就落在 Rime 用户目录根下），文件名以 `.dict.yaml` 结尾。
/// 多个候选时取 `import_tables` 最多的那个；并列则取文件名排序靠前的。
pub fn detect(rime_dir: &Path) -> Option<MergedDict> {
    let mut candidates: Vec<MergedDict> = Vec::new();
    let entries = fs::read_dir(rime_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".dict.yaml") {
            continue;
        }
        if let Some(header) = read_dict_header(&path) {
            if !header.import_tables.is_empty() {
                candidates.push(header);
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .import_tables
            .len()
            .cmp(&left.import_tables.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    candidates.into_iter().next()
}

/// 读词典头（librime 同样只读到 `...` 行为止），解析 `name` 与 `import_tables`。
fn read_dict_header(path: &Path) -> Option<MergedDict> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut import_tables: Vec<String> = Vec::new();
    let mut in_import_tables = false;
    let mut read_bytes = 0usize;
    for line in reader.lines() {
        let line = line.ok()?;
        read_bytes += line.len() + 1;
        let trimmed = line.trim();
        if trimmed == "..." {
            break;
        }
        if read_bytes > MAX_HEADER_BYTES {
            return None;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
            continue;
        }
        if in_import_tables {
            if let Some(item) = trimmed.strip_prefix("- ") {
                let item = unquote(item.trim());
                if !item.is_empty() {
                    import_tables.push(item);
                }
                continue;
            }
            in_import_tables = false;
        }
        if let Some(rest) = trimmed.strip_prefix("name:") {
            name = Some(unquote(rest.trim()));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("version:") {
            version = Some(unquote(rest.trim()));
            continue;
        }
        if trimmed.starts_with("import_tables:") {
            in_import_tables = true;
        }
    }
    let name = name?;
    // librime 的 LoadDictHeader 要求 name 与 version 都非空，
    // 缺 version 的词典头 librime 根本加载不了 —— 不能把指针指向它。
    let version = version?;
    if version.is_empty() {
        return None;
    }
    if import_tables.is_empty() {
        return None;
    }
    Some(MergedDict {
        name,
        import_tables,
    })
}

/// 去掉一层引号（词典头里的 `version: "2026.11.11"` 这种写法）。
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// 算出某方案最终用的基础词库（跟随 `__include` 链）。
///
/// 与 librime 的语义一致：被 include 的是「底」，方案自身的字面量覆盖它
/// （`MergeTree(target=include结果, overrides=自身字面量)`）。所以自身有
/// `translator/dictionary` 就用自己的，没有才沿 include 链往上找。
pub fn schema_dictionary(schema_id: &str, data_dirs: &[PathBuf]) -> Option<String> {
    let mut visited = BTreeSet::new();
    let mut depth = 0usize;
    schema_dictionary_inner(schema_id, data_dirs, &mut visited, &mut depth)
}

fn schema_dictionary_inner(
    schema_id: &str,
    data_dirs: &[PathBuf],
    visited: &mut BTreeSet<String>,
    depth: &mut usize,
) -> Option<String> {
    if *depth > 8 || !visited.insert(schema_id.to_string()) {
        return None;
    }
    *depth += 1;
    let path = find_resource_file(schema_id, data_dirs)?;
    let value = load_yaml(&path)?;
    if let Some(dictionary) = value
        .get("translator")
        .and_then(|translator| translator.get("dictionary"))
        .and_then(Value::as_str)
    {
        return Some(dictionary.to_string());
    }
    for include in include_targets(&value) {
        let (resource, local_path) = split_include(&include);
        if resource.trim().is_empty() {
            continue;
        }
        let Some(file) = find_resource_file(&resource, data_dirs) else {
            continue;
        };
        let Some(included) = load_yaml(&file) else {
            continue;
        };
        // include 指向整个文档（"/" 或空）时，继承被 include 文件的最终词库；
        // 指向子节点时，取该子节点里的 dictionary。
        let candidate = if local_path.is_empty() || local_path == "/" {
            schema_dictionary_inner(&resource, data_dirs, visited, depth)
        } else {
            node_dictionary(&included, &local_path)
        };
        if let Some(candidate) = candidate {
            return Some(candidate);
        }
    }
    None
}

/// 在共享数据目录里找资源文件。
///
/// 两种写法都要认：
///
/// * 方案 id（`luna_pinyin_simp`）→ `luna_pinyin_simp.schema.yaml`；
/// * librime 的资源 id（`luna_pinyin.schema`，来自 `__include`）→
///   `luna_pinyin.schema.yaml`。
fn find_resource_file(name: &str, data_dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut candidates: Vec<String> = Vec::new();
    if name.ends_with(".yaml") {
        candidates.push(name.to_string());
    } else {
        candidates.push(format!("{name}.yaml"));
        candidates.push(format!("{name}.schema.yaml"));
    }
    for dir in data_dirs {
        for candidate in &candidates {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// 读 YAML 文件（BOM 先剥掉）。
fn load_yaml(path: &Path) -> Option<Value> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_yaml::from_str::<Value>(text).ok()
}

/// `__include` 可以是单值或列表。
fn include_targets(value: &Value) -> Vec<String> {
    let Some(node) = value.get("__include") else {
        return Vec::new();
    };
    match node {
        Value::String(text) => vec![text.clone()],
        Value::Sequence(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// 拆 `resource[:local/path]`。
fn split_include(include: &str) -> (String, String) {
    match include.find(':') {
        Some(index) => (
            include[..index].trim().to_string(),
            include[index + 1..].trim().to_string(),
        ),
        None => (include.trim().to_string(), String::new()),
    }
}

/// 取 include 子节点里的 `dictionary`（例如 `luna_pinyin.schema:/translator`）。
fn node_dictionary(value: &Value, local_path: &str) -> Option<String> {
    let mut node = value;
    for segment in local_path.split('/') {
        if segment.is_empty() {
            continue;
        }
        node = node.get(segment)?;
    }
    node.get("dictionary")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 给本机各方案补上合并词典指针。
///
/// 判定条件（两条都要满足）：
///
/// * 方案最终用的基础词库 ∈ 合并词典的 `import_tables`；
/// * 该方案的 <schema>.custom.yaml 里**还没有** `translator/dictionary`。
///
/// 只处理**已经存在**的 custom.yaml：不存在就不新建（新建会和 rime-sync 自己的
/// 受管文件流程打架，也会把用户没打算启用的方案拖进来）。
pub fn ensure_pointers(rime_dir: &Path, data_dirs: &[PathBuf]) -> Vec<PointerChange> {
    ensure_pointers_with(rime_dir, data_dirs, &[])
}

pub fn ensure_pointers_with(
    rime_dir: &Path,
    data_dirs: &[PathBuf],
    template_dirs: &[PathBuf],
) -> Vec<PointerChange> {
    let Some(merged) = detect(rime_dir) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    for schema in target_schemas(rime_dir, template_dirs) {
        let path = rime_dir.join(format!("{schema}.custom.yaml"));
        // 文件不在：用模板建一份完整的（增强功能 + 指针），没有模板就不建。
        let created = if path.is_file() {
            false
        } else {
            match seed_from_template(&path, &schema, rime_dir, template_dirs) {
                Ok(Seed::Created) => true,
                Ok(Seed::NoTemplate) => {
                    changes.push(PointerChange::Skipped {
                        schema,
                        reason: if template_dirs.is_empty() {
                            "missing".to_string()
                        } else {
                            "no-template".to_string()
                        },
                    });
                    continue;
                }
                Ok(Seed::EngineUnsupported) => {
                    changes.push(PointerChange::Skipped {
                        schema,
                        reason: "engine-unsupported".to_string(),
                    });
                    continue;
                }
                Err(error) => {
                    changes.push(PointerChange::Skipped {
                        schema,
                        reason: format!("failed: {error:#}"),
                    });
                    continue;
                }
            }
        };
        let Some(dictionary) = schema_dictionary(&schema, data_dirs) else {
            changes.push(PointerChange::Skipped {
                schema,
                reason: "no-dictionary".to_string(),
            });
            continue;
        };
        if dictionary == merged.name {
            changes.push(PointerChange::Skipped {
                schema,
                reason: "already-merged".to_string(),
            });
            continue;
        }
        if !merged
            .import_tables
            .iter()
            .any(|table| table == &dictionary)
        {
            changes.push(PointerChange::Skipped {
                schema,
                reason: "dictionary-not-merged".to_string(),
            });
            continue;
        }
        match write_pointer(&path, &merged.name, !created) {
            Ok(PointerWrite::Written) => changes.push(PointerChange::Written { schema, path }),
            Ok(PointerWrite::Present) => changes.push(PointerChange::Present { schema }),
            Ok(PointerWrite::NoPatchBlock) => changes.push(PointerChange::Skipped {
                schema,
                reason: "no-patch-block".to_string(),
            }),
            Err(error) => changes.push(PointerChange::Skipped {
                schema,
                reason: format!("failed: {error:#}"),
            }),
        }
    }
    changes
}

/// Linux 安装器铺模板的位置（随 rime-inputmethod 一起装）。
pub const LINUX_TEMPLATE_DIR: &str = "/usr/share/rime-sync/defaults/schemas";

/// 候选模板目录，按顺序找；找到第一个带 `<id>.custom.yaml` 的就用。
///
/// * `payload` = 预编译产物目录，它的 `config/` 就是模板所在（A-4 统一后的唯一一份源）。
/// * Linux 走安装器铺的 `/usr/share/rime-sync/defaults/schemas`。
/// * 桌面程序（AppImage / NSIS）再兜一层可执行文件旁的 `resources/prebuilt/config`。
pub fn template_dirs(payload: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(dir) = payload {
        out.push(dir.join("config"));
    }
    out.push(PathBuf::from(LINUX_TEMPLATE_DIR));
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..3 {
            let Some(dir) = cur else {
                break;
            };
            out.push(dir.join("resources").join("prebuilt").join("config"));
            cur = dir.parent().map(|p| p.to_path_buf());
        }
    }
    out
}
/// 要处理的方案：用户目录里已有的 + 模板目录里有的。
///
/// 模板目录里的 default.custom.yaml / weasel.custom.yaml 不是方案，
/// 会在后面查不到 dictionary 时自然被跳过。
fn target_schemas(rime_dir: &Path, template_dirs: &[PathBuf]) -> Vec<String> {
    let mut ids: BTreeSet<String> = crate::managed_custom::list_user_customs(rime_dir)
        .into_iter()
        .collect();
    for dir in template_dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".custom.yaml") {
                if !id.is_empty() {
                    ids.insert(id.to_string());
                }
            }
        }
    }
    ids.into_iter().collect()
}

/// 建模板的结果。
enum Seed {
    /// 建好了。
    Created,
    /// 没有这个方案的模板。
    NoTemplate,
    /// 这台机器的引擎不认识增强组件，建模板会弄坏方案，不建。
    EngineUnsupported,
}
/// 这台机器的引擎支持增强组件（rimesync_vnum 等自研插件）吗？
///
/// 发行版 librime 没有这些插件，方案里一引用就会报
/// `error creating processor: 'rimesync_punct'` 并整个方案不可用。
/// 判据（任一命中即认为支持）：
///
/// * 已部署的 `build/<id>.schema.yaml` 里有 rimesync 标记；
/// * 用户目录里已有别的 custom.yaml 带 rimesync 标记（装输入法时铺的模板）。
fn engine_supports_extra(rime_dir: &Path, schema: &str) -> bool {
    let build = rime_dir.join("build").join(format!("{schema}.schema.yaml"));
    if let Ok(text) = fs::read_to_string(&build) {
        if text.contains("rimesync_vnum") || text.contains("rimesync_punct") {
            return true;
        }
    }
    for id in crate::managed_custom::list_user_customs(rime_dir) {
        let path = rime_dir.join(format!("{id}.custom.yaml"));
        if let Ok(text) = fs::read_to_string(&path) {
            if text.contains("rimesync_vnum") || text.contains("rimesync_punct") {
                return true;
            }
        }
    }
    false
}
/// 用模板建一份完整的 custom.yaml（增强功能 + 后续补指针）。
fn seed_from_template(
    path: &Path,
    schema: &str,
    rime_dir: &Path,
    template_dirs: &[PathBuf],
) -> Result<Seed> {
    let name = format!("{schema}.custom.yaml");
    let mut found: Option<PathBuf> = None;
    for dir in template_dirs {
        let src = dir.join(&name);
        if src.is_file() {
            found = Some(src);
            break;
        }
    }
    let Some(src) = found else {
        return Ok(Seed::NoTemplate);
    };
    // 引擎不认识增强组件就不建：建了会让方案直接报错不可用。
    if !engine_supports_extra(rime_dir, schema) {
        return Ok(Seed::EngineUnsupported);
    }
    fs::copy(&src, path).with_context(|| format!("复制模板 {}", src.display()))?;
    set_private_mode(path)?;
    Ok(Seed::Created)
}

/// 用户目录里的配置文件只给本人读写（安装器铺模板时也是 600）。
#[cfg(unix)]
fn set_private_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("读取 {}", path.display()))?
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms).with_context(|| format!("设置权限 {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// 一次指针补写的落点。#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointerWrite {
    /// 指针缺失，已写入文件。
    Written,
    /// 已经有该键（用户手写或先前已写），文件一个字节都没动。
    Present,
    /// 文件里没有可写的 `patch` 块（结构不明），不擅自改造用户文件。
    NoPatchBlock,
}

/// 在 custom.yaml 的 `patch` 块里补 `"translator/dictionary": <name>`。
fn write_pointer(path: &Path, dict_name: &str, make_backup: bool) -> Result<PointerWrite> {
    let bytes = fs::read(path).with_context(|| format!("读取 {}", path.display()))?;
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| anyhow!("{} 不是 UTF-8，跳过（不覆盖用户文件）", path.display()))?;
    let has_bom = text.starts_with('\u{feff}');
    let body = text.strip_prefix('\u{feff}').unwrap_or(&text);

    // 先用 YAML 解析判断「指针是不是已经有了」（认得引号写法与嵌套写法）。
    if let Ok(value) = serde_yaml::from_str::<Value>(body) {
        if patch_has_dictionary(&value) {
            return Ok(PointerWrite::Present);
        }
    }

    let mut raw = RawLines::parse(body);
    let Some((patch_start, patch_end)) = find_patch_block(&raw.lines) else {
        // 没有 patch 块：结构不明，不擅自改造用户文件。
        return Ok(PointerWrite::NoPatchBlock);
    };
    // 行级再确认一次（YAML 解析失败时的兜底）。
    if block_has_dictionary_key(&raw.lines, patch_start, patch_end) {
        return Ok(PointerWrite::Present);
    }
    let indent = block_child_indent(&raw.lines, patch_start, patch_end);
    let pointer_line = format!(
        "{}{}: {}",
        " ".repeat(indent),
        yaml_key("translator/dictionary"),
        yaml_scalar(dict_name)
    );
    raw.insert(patch_start + 1, &pointer_line);
    let patched = raw.render();

    // 写盘前的安全闸：必须是合法 YAML，且指针确实落在 patch 下。
    let parsed: Value = serde_yaml::from_str(&patched)
        .with_context(|| format!("生成的 {} 不是合法 YAML，已取消写入", path.display()))?;
    if !patch_has_dictionary(&parsed) {
        bail_pointer(path)?;
    }

    if make_backup {
        let backup = crate::data_patch::backup_path(path);
        if !backup.exists() {
            fs::write(&backup, &bytes).with_context(|| format!("备份 {}", backup.display()))?;
        }
    }
    let output = if has_bom {
        format!("\u{feff}{patched}")
    } else {
        patched
    };
    fs::write(path, output.as_bytes())
        .map_err(|error| anyhow!("写入 {}：{error}", path.display()))?;
    Ok(PointerWrite::Written)
}

fn bail_pointer(path: &Path) -> Result<()> {
    Err(anyhow!(
        "生成的 {} 里找不到 translator/dictionary 指针，已取消写入",
        path.display()
    ))
}

/// `patch` 下是否已经有 translator/dictionary（引号写法与嵌套写法都认）。
fn patch_has_dictionary(value: &Value) -> bool {
    let Some(patch) = value.get("patch").and_then(Value::as_mapping) else {
        return false;
    };
    if patch.contains_key(Value::from("translator/dictionary")) {
        return true;
    }
    if let Some(translator) = patch
        .get(Value::from("translator"))
        .and_then(Value::as_mapping)
    {
        if translator.contains_key(Value::from("dictionary")) {
            return true;
        }
    }
    false
}

/// 顶层 `patch:` 块的范围（行号，end 不含）。
fn find_patch_block(lines: &[String]) -> Option<(usize, usize)> {
    for (index, line) in lines.iter().enumerate() {
        if indent_of(line) != 0 {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("patch") {
            if rest.trim_start().starts_with(':') {
                return Some((index, block_end(lines, index, 0)));
            }
        }
    }
    None
}

/// 行级判断 patch 块里有没有 translator/dictionary 键（引号/非引号都认）。
fn block_has_dictionary_key(lines: &[String], start: usize, end: usize) -> bool {
    for line in &lines[start + 1..end] {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        let key = unquote(trimmed[..colon].trim());
        if key == "translator/dictionary" {
            return true;
        }
    }
    false
}

/// patch 块里子项的缩进（沿用文件既有风格；块为空则用 2 空格）。
fn block_child_indent(lines: &[String], start: usize, end: usize) -> usize {
    for line in &lines[start + 1..end] {
        if is_blank(line) || is_comment(line) {
            continue;
        }
        let indent = indent_of(line);
        if indent > 0 {
            return indent;
        }
    }
    2
}
