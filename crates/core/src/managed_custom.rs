//! rime-sync 自己管理的用户级 <schema>.custom.yaml：识别 + 增量修复。
//!
//! 背景（缺陷三：小键盘在 Windows 上不生效）：librime 的 `patch:` 对列表是
//! **整段替换**（`config_compiler.cc` 的 `EditNode`），而 librime 编译
//! `<schema>.schema` 时会自动挂上 `PatchReference(<schema>.custom, "patch")`。
//! 用户目录里那份 `<schema>.custom.yaml` 只要存在，它里面的
//! `patch.engine/processors` 就会把共享方案里的同名列表整段顶掉。
//!
//! rime-sync v0.6.18 之前生成的模板里 `engine/processors` 只有
//! `rimesync_vselect`、没有 `numpad`（小键盘归一化处理器），于是旧用户目录里
//! 的 custom.yaml 把共享方案侧的 numpad 顶掉了 —— Windows 上小键盘一直没生效。
//!
//! 这里做的事：**只修 rime-sync 自己生成/写过的文件**，把缺失的 `numpad`
//! 补到 `rimesync_vselect` 紧后。其它内容（注释、其它条目、顺序、缩进、换行风格、
//! 结尾换行、BOM）一个字节都不动；用户自己写的文件一律不碰；重复跑结果不变（幂等）。
//!
//! 判定规则（与协调者 2026-11-09 的口径一致）：
//! * **是不是 rime-sync 管理的**：整行 trim 后以 `# 由 rime-sync 生成` 或
//!   `# patched by rime-sync` 开头；
//! * **rime-sync 的引擎定制是不是还在生效**：文件里出现 `rimesync_vnum` /
//!   `rimesync_punct` / `rimesync_vselect` 任一名字；三个都没有 =
//!   用户已经关闭了定制 → 跳过；
//! * **目标列表**：`patch:` 下的 `engine/processors`（扁平斜杠键，也认嵌套写法
//!   `patch: -> engine: -> processors:`）。找不到就不动 —— 凭空新建一个列表等于
//!   顶掉用户整个列表，风险太大；
//! * **列表里有没有 `rimesync_vselect`**：有 → 把 `numpad` 放到它紧后（已有的
//!   numpad 先摘掉，保证只有一份且紧跟 vselect）；没有 → 跳过（`no-vselect`），
//!   不替用户加处理器 —— rime-sync 历来发布的每一版管理型 custom.yaml 都带
//!   rimesync_vselect，vselect 不在说明这份文件被用户大改过。

use anyhow::{anyhow, bail, Context, Result};
use serde_yaml::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::data_patch::{backup_path, NUMPAD_PROCESSOR, VSELECT_PROCESSOR};
use crate::yaml_text::{
    indent_of, is_blank, is_comment, is_flow_value, is_key_at, item_name, Doc, RawLines,
};

/// 模板文件首行的标记（packaging/rime/schemas/*.custom.yaml）。
pub const MANAGED_MARKER: &str = "# 由 rime-sync 生成";
/// data_patch 写的用户级 custom.yaml 顶部标记（与 data_patch::MARKER_PREFIX 同源）。
pub const MANAGED_MARKER_PATCHED: &str = crate::data_patch::MARKER_PREFIX;
/// rimesync 的数值转换翻译器（判断「定制还在生效」用）。
const VNUM_NAME: &str = "rimesync_vnum";
/// rimesync 的标点处理器（判断「定制还在生效」用）。
const PUNCT_NAME: &str = "rimesync_punct";
/// 跳过的原因：不是 rime-sync 管理的文件。
pub const SKIP_NOT_MANAGED: &str = "not-managed";
/// 跳过的原因：用户已关闭 rime-sync 的引擎定制。
pub const SKIP_DISABLED: &str = "rimesync-disabled";
/// 跳过的原因：找不到可安全修改的 processors 列表。
pub const SKIP_NO_PROCESSORS: &str = "no-processors-list";
/// 跳过的原因：列表里没有 rimesync_vselect（用户大改过这份文件，保守跳过）。
pub const SKIP_NO_VSELECT: &str = "no-vselect";

/// 一个用户级 custom.yaml 的修复结果。
#[derive(Debug, Clone, PartialEq)]
pub enum RepairOutcome {
    /// 改过了；backup=Some(路径) 表示这次新建了备份（已存在则不覆盖）。
    Repaired { backup: Option<PathBuf> },
    /// 已经是目标状态，一个字节都没动。
    Unchanged,
    /// 不该动：非管理文件 / rime-sync 定制已关闭 / 结构不符合预期。
    Skipped(String),
}

/// 只读探测的一个方案条目。
#[derive(Debug, Clone)]
pub struct ScanEntry {
    /// 方案 id。
    pub id: String,
    /// 用户目录里的 <id>.custom.yaml。
    pub path: PathBuf,
    /// 是不是 rime-sync 管理的文件。
    pub managed: bool,
    /// rime-sync 的引擎定制是不是还在生效。
    pub active: bool,
    /// 是不是缺 numpad / 位置不对。
    pub needs_num_pad: bool,
}

/// 去掉 BOM 前缀，返回（有没有 BOM，正文）。
fn strip_bom(raw: &str) -> (bool, &str) {
    match raw.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest),
        None => (false, raw),
    }
}

/// 这个文本是不是 rime-sync 管理的（带任一 rime-sync 标记）。
pub fn is_managed(text: &str) -> bool {
    let (_, body) = strip_bom(text);
    body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with(MANAGED_MARKER) || trimmed.starts_with(MANAGED_MARKER_PATCHED)
    })
}

/// 这个文件是不是 rime-sync 管理的（读失败 / 不是 UTF-8 一律 false）。
pub fn is_managed_file(path: &Path) -> bool {
    match fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => is_managed(&text),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// rime-sync 的引擎定制是不是还在生效。
///
/// 文件里出现 rimesync_vnum / rimesync_punct / rimesync_vselect 任一名字就算生效；
/// 三个都没有 = 用户已经关闭了 rime-sync 的引擎定制（模板里写明的关闭方式）。
fn rimesync_active(text: &str) -> bool {
    let (_, body) = strip_bom(text);
    body.contains(VNUM_NAME) || body.contains(PUNCT_NAME) || body.contains(VSELECT_PROCESSOR)
}

/// 一份 processors 列表在文件里的位置。
struct ProcessorsList {
    /// 列表项所在行（按出现顺序）。
    items: Vec<usize>,
    /// 列表项的统一缩进（新插入的行用它）。
    item_indent: usize,
}

/// 块的结束行（不含）：与 yaml_text 里 find_block_from 的收尾逻辑一致。
fn block_end(lines: &[String], start: usize, indent: usize) -> usize {
    let mut end = start + 1;
    while end < lines.len() {
        let line = &lines[end];
        if is_blank(line) || is_comment(line) {
            end += 1;
            continue;
        }
        if indent_of(line) <= indent {
            break;
        }
        end += 1;
    }
    // 块尾的空行 / 同级注释不算块内容。
    while end > start + 1 {
        let line = &lines[end - 1];
        if is_blank(line) || (is_comment(line) && indent_of(line) <= indent) {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// 在 [from, to) 里找「缩进 indent 的 key:」。
fn find_key_in(
    lines: &[String],
    key: &str,
    indent: usize,
    from: usize,
    to: usize,
) -> Option<usize> {
    let to = to.min(lines.len());
    (from..to).find(|&index| is_key_at(&lines[index], key, indent))
}

/// 块内子键的实际缩进（空块按 fallback 算）。
fn child_indent_of(lines: &[String], start: usize, end: usize, fallback: usize) -> usize {
    (start + 1..end)
        .find(|&index| !is_blank(&lines[index]) && !is_comment(&lines[index]))
        .map(|index| indent_of(&lines[index]))
        .unwrap_or(fallback)
}

/// 列表项所在行（跳过注释行）。
fn collect_items(lines: &[String], key_index: usize, end: usize) -> Vec<usize> {
    (key_index + 1..end)
        .filter(|&index| {
            let line = &lines[index];
            !is_comment(line) && line.trim_start().starts_with("- ")
        })
        .collect()
}

/// 找出要修的那份 processors 列表。
///
/// 两种写法都认：`patch:` 下的扁平斜杠键 `engine/processors:`（缩进 2，模板就是
/// 这个写法），以及嵌套写法 `patch:` -> `engine:` -> `processors:`。找不到就不动。
fn find_processors(doc: &Doc) -> Option<ProcessorsList> {
    let lines = &doc.lines;
    // 先框定 patch 段的范围；没有 patch 段就在整个文件里找。
    let (search_from, search_to) = match find_key_in(lines, "patch", 0, 0, lines.len()) {
        Some(index) => (index + 1, block_end(lines, index, 0)),
        None => (0, lines.len()),
    };
    find_processors_in(lines, search_from, search_to)
}

/// 在 [from, to) 区间里找 processors 列表（find_processors 的可复用内核）。
fn find_processors_in(
    lines: &[String],
    search_from: usize,
    search_to: usize,
) -> Option<ProcessorsList> {
    let key_index = match find_key_in(lines, "engine/processors", 2, search_from, search_to) {
        Some(index) => index,
        None => {
            // 嵌套写法兜底：patch -> engine -> processors。
            let engine_index = find_key_in(lines, "engine", 2, search_from, search_to)?;
            let engine_end = block_end(lines, engine_index, 2);
            let child = child_indent_of(lines, engine_index, engine_end, 4);
            find_key_in(lines, "processors", child, engine_index + 1, engine_end)?
        }
    };
    let key_indent = indent_of(&lines[key_index]);
    // flow 风格（processors: [a, b]）没法按行插，直接放弃。
    if is_flow_value(&lines[key_index]) {
        return None;
    }
    let end = block_end(lines, key_index, key_indent);
    let items = collect_items(lines, key_index, end);
    let item_indent = items
        .first()
        .map(|&index| indent_of(&lines[index]))
        .unwrap_or(key_indent + 2);
    Some(ProcessorsList { items, item_indent })
}

/// 列表项的名字（复用 yaml_text 的实现）。
fn names_of(lines: &[String], items: &[usize]) -> Vec<String> {
    items
        .iter()
        .map(|&index| item_name(&lines[index]))
        .collect()
}

/// 插入一行列表项。
fn insert_item(doc: &mut Doc, at: usize, indent: usize, item: &str) {
    doc.lines
        .insert(at, format!("{}- {item}", " ".repeat(indent)));
    doc.changed = true;
}

/// 摘掉列表里所有的 numpad 行（返回摘掉的行号，倒序，便于从后往前删）。
fn numpad_lines(lines: &[String], items: &[usize]) -> Vec<usize> {
    items
        .iter()
        .copied()
        .filter(|&index| item_name(&lines[index]) == NUMPAD_PROCESSOR)
        .collect()
}

/// 这份 processors 列表要不要改。
enum ListPlan {
    /// 已经是目标状态（rimesync_vselect 后面紧跟 numpad，且只有一份 numpad）。
    Unchanged,
    /// 要改：把 numpad 放到 rimesync_vselect 紧后（其它条目顺序原样保留）。
    Repair,
    /// 列表里没有 rimesync_vselect：保守跳过，不替用户加处理器。
    NoVSelect,
}

/// 按目标状态修这份 processors 列表。
fn repair_processors(doc: &mut Doc) -> ListPlan {
    let Some(list) = find_processors(doc) else {
        return ListPlan::Unchanged;
    };
    let names = names_of(&doc.lines, &list.items);
    let Some(index) = names.iter().position(|name| name == VSELECT_PROCESSOR) else {
        // rimesync_vselect 不在：这份文件被用户大改过（rime-sync 历来发布的每一版
        // 管理型 custom.yaml 都带它），保守跳过比替用户加回处理器安全。
        return ListPlan::NoVSelect;
    };
    let numpads = numpad_lines(&doc.lines, &list.items);
    let adjacent = names
        .get(index + 1)
        .map(|name| name == NUMPAD_PROCESSOR)
        .unwrap_or(false);
    if adjacent && numpads.len() == 1 {
        // 已经就位：一个字节都不动。
        return ListPlan::Unchanged;
    }
    let item_indent = list.item_indent;
    // 摘掉列表里已有的 numpad：目标状态要求它紧跟 vselect，而且只能有一份。
    for index in numpads.into_iter().rev() {
        doc.lines.remove(index);
    }
    // 摘完之后重新取一次列表（行号变了）。
    let Some(list) = find_processors(doc) else {
        return ListPlan::Unchanged;
    };
    let names = names_of(&doc.lines, &list.items);
    let Some(index) = names.iter().position(|name| name == VSELECT_PROCESSOR) else {
        return ListPlan::NoVSelect;
    };
    // numpad 紧跟 rimesync_vselect。
    let at = list.items[index] + 1;
    insert_item(doc, at, item_indent, NUMPAD_PROCESSOR);
    ListPlan::Repair
}

/// 校验修复后的文本：必须是合法 YAML，且 processors 列表里 vselect 后面紧跟 numpad。
fn verify_repaired(text: &str, path: &Path) -> Result<()> {
    let value: Value = serde_yaml::from_str(text).map_err(|error| {
        anyhow!(
            "修复后的 {} 不是合法 YAML，已取消写入：{error}",
            path.display()
        )
    })?;
    let patch = value.get("patch").and_then(Value::as_mapping);
    let list = patch
        .and_then(|mapping| mapping.get("engine/processors"))
        .or_else(|| {
            patch
                .and_then(|mapping| mapping.get("engine"))
                .and_then(|engine| engine.get("processors"))
        })
        .and_then(Value::as_sequence)
        .ok_or_else(|| {
            anyhow!(
                "修复后的 {} 里找不到 patch 下的 processors 列表，已取消写入",
                path.display()
            )
        })?;
    let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
    let index = names
        .iter()
        .position(|name| *name == VSELECT_PROCESSOR)
        .ok_or_else(|| {
            anyhow!(
                "修复后的 {} 里没有 {VSELECT_PROCESSOR}，已取消写入",
                path.display()
            )
        })?;
    if names.get(index + 1) != Some(&NUMPAD_PROCESSOR) {
        bail!(
            "修复后的 {} 里 {NUMPAD_PROCESSOR} 没有紧跟 {VSELECT_PROCESSOR}，已取消写入",
            path.display()
        );
    }
    Ok(())
}

/// 修一个用户级 custom.yaml（幂等）。
pub fn repair_file(path: &Path) -> Result<RepairOutcome> {
    let bytes = fs::read(path).with_context(|| format!("读取 {}", path.display()))?;
    let raw = String::from_utf8(bytes.clone())
        .map_err(|_| anyhow!("{} 不是 UTF-8，已跳过", path.display()))?;
    let (has_bom, text) = strip_bom(&raw);
    if !is_managed(text) {
        return Ok(RepairOutcome::Skipped(SKIP_NOT_MANAGED.to_string()));
    }
    if !rimesync_active(text) {
        // 用户主动关闭了 rime-sync 的引擎定制：绝不再加回 vselect / numpad。
        return Ok(RepairOutcome::Skipped(SKIP_DISABLED.to_string()));
    }
    let mut doc = Doc::parse(text);
    match repair_processors(&mut doc) {
        ListPlan::Unchanged => {
            // 没找到可安全修改的 processors 列表，或者已经是目标状态。
            if find_processors(&doc).is_none() {
                return Ok(RepairOutcome::Skipped(SKIP_NO_PROCESSORS.to_string()));
            }
            return Ok(RepairOutcome::Unchanged);
        }
        ListPlan::NoVSelect => {
            // 列表里没有 rimesync_vselect：不替用户加处理器。
            return Ok(RepairOutcome::Skipped(SKIP_NO_VSELECT.to_string()));
        }
        ListPlan::Repair => {}
    }
    let patched = doc.render();
    if patched == text {
        return Ok(RepairOutcome::Unchanged);
    }
    // 安全闸：写盘前重新解析 + 核对目标状态，失败就不写。
    verify_repaired(&patched, path)?;
    // 备份（只建一次，已存在不覆盖）。
    let backup = backup_path(path);
    let mut created = None;
    if !backup.exists() {
        fs::write(&backup, &bytes).with_context(|| format!("备份 {}", backup.display()))?;
        created = Some(backup);
    }
    let output = if has_bom {
        format!("\u{feff}{patched}")
    } else {
        patched
    };
    fs::write(path, output.as_bytes())
        .map_err(|error| anyhow!("写入 {}：{error}", path.display()))?;
    Ok(RepairOutcome::Repaired { backup: created })
}

/// 按方案 id 批量修用户目录里的 custom.yaml。
///
/// 文件不存在的不算错误（记 Skipped("missing")）；其它错误原样往上抛。
pub fn repair_user_dir(
    rime_dir: &Path,
    schema_ids: &[String],
) -> Result<Vec<(String, RepairOutcome)>> {
    let mut out = Vec::new();
    for id in schema_ids {
        let path = rime_dir.join(format!("{id}.custom.yaml"));
        if !path.is_file() {
            out.push((id.clone(), RepairOutcome::Skipped("missing".to_string())));
            continue;
        }
        out.push((id.clone(), repair_file(&path)?));
    }
    Ok(out)
}

/// 只读探测用户目录里的 custom.yaml（不改任何文件）。
pub fn scan_user_dir(rime_dir: &Path, schema_ids: &[String]) -> Vec<ScanEntry> {
    schema_ids
        .iter()
        .map(|id| {
            let path = rime_dir.join(format!("{id}.custom.yaml"));
            let text = fs::read(&path)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            let managed = text.as_deref().map(is_managed).unwrap_or(false);
            let active = text.as_deref().map(rimesync_active).unwrap_or(false);
            let needs_num_pad = match text.as_deref() {
                Some(body) if managed && active => needs_repair(body),
                _ => false,
            };
            ScanEntry {
                id: id.clone(),
                path,
                managed,
                active,
                needs_num_pad,
            }
        })
        .collect()
}

/// 这份文本是不是还缺 numpad / 位置不对（只读判断，不改文件）。
///
/// 只在「列表里有 rimesync_vselect」时才算要修：vselect 都不在的文件会被跳过，
/// 报成 needs_num_pad = true 会误导人。
fn needs_repair(text: &str) -> bool {
    let doc = Doc::parse(text);
    let Some(list) = find_processors(&doc) else {
        return false;
    };
    let names = names_of(&doc.lines, &list.items);
    let Some(index) = names.iter().position(|name| name == VSELECT_PROCESSOR) else {
        return false;
    };
    let adjacent = names
        .get(index + 1)
        .map(|name| name == NUMPAD_PROCESSOR)
        .unwrap_or(false);
    let numpads = numpad_lines(&doc.lines, &list.items).len();
    !(adjacent && numpads == 1)
}

/// 列出用户目录里全部 `<id>.custom.yaml` 的方案 id（按字典序）。
///
/// 扫全目录而不是只扫保留方案：用户切到哪个方案，哪个方案的 custom.yaml 就生效
/// （librime 编译 `<id>.schema` 时一定会挂上 `PatchReference(<id>.custom)`）。
/// `repair_file` 内部已经保证「非 rime-sync 管理的文件一个字都不改」，
/// 所以扫全目录是安全的。
pub fn list_user_customs(rime_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(rime_dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".custom.yaml")?.to_string();
            if id.is_empty() {
                return None;
            }
            Some(id)
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// 编译产物 `build/<id>.schema.yaml` 里是不是含 numpad 处理器（只读）。
///
/// 这是「用户 custom.yaml 修好了 → 引擎真的重编译了」的验证口：
/// 用户目录里那份 custom.yaml 会整段顶掉方案的 `engine/processors`，
/// 产物里没有 numpad 就说明小键盘还是用不了。
pub fn compiled_has_numpad(rime_dir: &Path, id: &str) -> bool {
    let path = rime_dir.join("build").join(format!("{id}.schema.yaml"));
    let Ok(bytes) = fs::read(&path) else {
        return false;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return false;
    };
    artifact_text_has_numpad(&text)
}

/// 产物文本里有没有 numpad 处理器（只读）。
///
/// 产物是引擎真正加载的东西，所以它是「小键盘行不行」的权威判据；
/// 拆成文本版是为了让判定函数能只读地复用同一份实现。
pub fn artifact_text_has_numpad(text: &str) -> bool {
    if let Ok(value) = serde_yaml::from_str::<Value>(text) {
        if let Some(list) = value
            .get("engine")
            .and_then(|engine| engine.get("processors"))
            .and_then(Value::as_sequence)
        {
            return list
                .iter()
                .filter_map(Value::as_str)
                .any(|name| name == NUMPAD_PROCESSOR);
        }
    }
    // 产物结构不是预期时退回文本判断（宁可宽判，也不要漏判）。
    text.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == NUMPAD_PROCESSOR || trimmed == format!("- {NUMPAD_PROCESSOR}")
    })
}

/// 用户 custom.yaml 自己定义的 `engine/processors` 列表（只读）。
///
/// 返回 None = 这份文件没有在 `patch:` 段里定义 processors 列表 ——
/// 这种情况下处理器列表来自共享方案，**不能**在这里下「来源缺 numpad」的结论
/// （共享方案侧是 `__include:` 的父方案，直接读会读不到 → 误判）。
pub fn source_processors(text: &str) -> Option<Vec<String>> {
    let doc = Doc::parse(text);
    let lines = &doc.lines;
    // 只认 patch 段里的列表：patch 之外的 engine/processors 不会顶掉方案自带的列表。
    let patch_index = find_key_in(lines, "patch", 0, 0, lines.len())?;
    let from = patch_index + 1;
    let to = block_end(lines, patch_index, 0);
    let list = find_processors_in(lines, from, to)?;
    Some(names_of(lines, &list.items))
}

/// rime-sync 的引擎定制是不是还在生效（对外只读入口）。
///
/// 文件里出现 rimesync_vnum / rimesync_punct / rimesync_vselect 任一名字就算生效；
/// 三个都没有 = 用户已经关闭了 rime-sync 的引擎定制（模板里写明的关闭方式）。
pub fn customization_active(text: &str) -> bool {
    rimesync_active(text)
}

// ---------------------------------------------------------------------------
// B1：用户目录 default.custom.yaml 的 schema_list 增量补齐
// ---------------------------------------------------------------------------

/// 跳过的原因：文件不存在（**不创建** —— 建文件是 seed_templates 的职责）。
pub const SKIP_FILE_MISSING: &str = "missing";
/// 跳过的原因：文件里没有 schema_list（不凭空造一个）。
pub const SKIP_NO_SCHEMA_LIST: &str = "no-schema-list";
/// 跳过的原因：schema_list 是空列表（用户可能故意清空，不替他填）。
pub const SKIP_EMPTY_SCHEMA_LIST: &str = "empty-schema-list";

/// 找 schema_list 键：优先 patch 段里的（标准写法 patch: → schema_list:），
/// 其次顶层写法（没有 patch 段的用户文件）。
fn find_schema_list_key(lines: &[String]) -> Option<usize> {
    if let Some(patch_index) = find_key_in(lines, "patch", 0, 0, lines.len()) {
        let patch_end = block_end(lines, patch_index, 0);
        let from = patch_index + 1;
        let to = patch_end;
        let child = child_indent_of(lines, patch_index, patch_end, 2);
        if let Some(index) = find_key_in(lines, "schema_list", child, from, to) {
            return Some(index);
        }
        // 缩进不规整时的兜底：patch 块内任意缩进的 schema_list 键。
        if let Some(index) = (from..to).find(|&index| {
            let indent = indent_of(&lines[index]);
            is_key_at(&lines[index], "schema_list", indent)
        }) {
            return Some(index);
        }
    }
    find_key_in(lines, "schema_list", 0, 0, lines.len())
}

/// 一条 schema_list 项的方案 id。
///
/// 认三种写法：`- {schema: x}`（随包模板与真机都是这种）、`- schema: x`、
/// `- x`（裸 id）。
fn schema_entry_id(line: &str) -> Option<String> {
    if let Some(id) = crate::prune::schema_list_item(line) {
        return Some(id.to_string());
    }
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('-')?.trim();
    if rest.is_empty() || rest.contains(':') || rest.contains('{') || rest.contains(' ') {
        return None;
    }
    let id = rest.trim_matches(|ch| ch == '\'' || ch == '"');
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// 校验补齐后的文本：合法 YAML，且 schema_list = 原有条目（顺序不变）+ 缺失项。
fn verify_schema_list(
    text: &str,
    existing: &[String],
    missing: &[String],
    path: &Path,
) -> Result<()> {
    let value: Value = serde_yaml::from_str(text).map_err(|error| {
        anyhow!(
            "补齐后的 {} 不是合法 YAML，已取消写入：{error}",
            path.display()
        )
    })?;
    let list = value
        .get("patch")
        .and_then(Value::as_mapping)
        .and_then(|mapping| mapping.get("schema_list"))
        .and_then(Value::as_sequence)
        .ok_or_else(|| {
            anyhow!(
                "补齐后的 {} 里找不到 patch.schema_list，已取消写入",
                path.display()
            )
        })?;
    let ids: Vec<String> = list
        .iter()
        .filter_map(|item| {
            item.get("schema")
                .and_then(Value::as_str)
                .or_else(|| item.as_str())
                .map(|value| value.to_string())
        })
        .collect();
    if ids.len() != existing.len() + missing.len() {
        bail!(
            "补齐后的 {} 里 schema_list 条目数不对（期望 {}，实际 {}），已取消写入",
            path.display(),
            existing.len() + missing.len(),
            ids.len()
        );
    }
    if !existing.is_empty() && ids[..existing.len()] != existing[..] {
        bail!(
            "补齐后的 {} 里 schema_list 原有条目或顺序变了，已取消写入",
            path.display()
        );
    }
    for id in missing {
        if !ids.iter().any(|value| value == id) {
            bail!("补齐后的 {} 里没有补上 {id}，已取消写入", path.display());
        }
    }
    Ok(())
}

/// 补齐用户目录 default.custom.yaml 的 `patch.schema_list`（幂等，跨平台）。
///
/// * 只动 schema_list 这一段：文件里其它内容（menu/page_size、punctuator、
///   用户自己的 patch、注释、缩进、混合行尾、BOM、末尾换行）逐字节不变；
/// * 用户已有的条目全部保留、原顺序不变，缺失的保留方案**追加**到列表末尾；
/// * 已经齐全 → 一个字节都不动（Unchanged）；
/// * 改之前先备份 `<file>.rimesync-orig`（已存在则不覆盖）；
/// * 文件不存在 → 不创建（Skipped(missing)）；文件里没有 schema_list → 跳过。
///
/// `allowed` = 只允许补入这些方案 id（None = 不门禁，补全部保留方案）。
/// 门禁的意义：保留方案里若某个方案在 data 目录里不存在，补进去会让
/// librime 的部署把缺失方案计成 failure、整次部署失败 —— 所以生产路径按
/// data 里真实存在的方案来补（见 ime_state::ensure）。
pub fn repair_default_schema_list_file(
    path: &Path,
    allowed: Option<&[String]>,
) -> Result<RepairOutcome> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RepairOutcome::Skipped(SKIP_FILE_MISSING.to_string()));
        }
        Err(error) => {
            return Err(anyhow!("读取 {}：{error}", path.display()));
        }
    };
    let raw = String::from_utf8(bytes.clone())
        .map_err(|_| anyhow!("{} 不是 UTF-8，已跳过", path.display()))?;
    let (has_bom, text) = strip_bom(&raw);
    let mut doc = RawLines::parse(text);
    let Some(key_index) = find_schema_list_key(&doc.lines) else {
        // 没有 schema_list：不凭空造一个（用户可能只想要默认行为）。
        return Ok(RepairOutcome::Skipped(SKIP_NO_SCHEMA_LIST.to_string()));
    };
    let key_indent = indent_of(&doc.lines[key_index]);
    let end = block_end(&doc.lines, key_index, key_indent);
    let items = collect_items(&doc.lines, key_index, end);
    if items.is_empty() {
        // 空列表：用户可能故意清空，不替他填。
        return Ok(RepairOutcome::Skipped(SKIP_EMPTY_SCHEMA_LIST.to_string()));
    }
    let existing: Vec<String> = items
        .iter()
        .filter_map(|&index| schema_entry_id(&doc.lines[index]))
        .collect();
    let required: Vec<String> = crate::prune::retained_schemas()
        .iter()
        .filter(|id| match allowed {
            Some(list) => list.iter().any(|value| value == *id),
            None => true,
        })
        .map(|id| id.to_string())
        .collect();
    let missing: Vec<String> = required
        .iter()
        .filter(|id| !existing.contains(id))
        .cloned()
        .collect();
    if missing.is_empty() {
        // 已经齐全：一个字节都不动。
        return Ok(RepairOutcome::Unchanged);
    }
    // 追加（已有条目与顺序一律不动）：写法与缩进跟随现有第一条。
    let flow = doc.lines[items[0]].contains("{schema:");
    let indent = " ".repeat(indent_of(&doc.lines[items[0]]));
    let start = items.last().copied().unwrap_or(key_index) + 1;
    for (offset, id) in missing.iter().enumerate() {
        let entry = if flow {
            format!("{indent}- {{schema: {id}}}")
        } else {
            format!("{indent}- schema: {id}")
        };
        doc.insert(start + offset, &entry);
    }
    let patched = doc.render();
    if patched == text {
        return Ok(RepairOutcome::Unchanged);
    }
    // 安全闸：写盘前重新解析 + 核对，失败就不写。
    verify_schema_list(&patched, &existing, &missing, path)?;
    // 备份（只建一次，已存在不覆盖）。
    let backup = backup_path(path);
    let mut created = None;
    if !backup.exists() {
        fs::write(&backup, &bytes).with_context(|| format!("备份 {}", backup.display()))?;
        created = Some(backup);
    }
    let output = if has_bom {
        format!("\u{feff}{patched}")
    } else {
        patched
    };
    fs::write(path, output.as_bytes())
        .map_err(|error| anyhow!("写入 {}：{error}", path.display()))?;
    Ok(RepairOutcome::Repaired { backup: created })
}

/// 补齐用户目录（`rime_dir`）里 default.custom.yaml 的 schema_list。
pub fn repair_default_schema_list(rime_dir: &Path) -> Result<RepairOutcome> {
    repair_default_schema_list_in(rime_dir, None)
}

/// 补齐用户目录里 default.custom.yaml 的 schema_list（带 data 门禁）。
///
/// `allowed` = 允许补入的方案 id（通常是 data 里真实存在的保留方案）。
pub fn repair_default_schema_list_in(
    rime_dir: &Path,
    allowed: Option<&[String]>,
) -> Result<RepairOutcome> {
    let path = rime_dir.join(crate::prune::USER_DEFAULT_CUSTOM);
    repair_default_schema_list_file(&path, allowed)
}
