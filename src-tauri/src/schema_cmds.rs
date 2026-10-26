//! 输入方案管理（设置页「输入方案」卡片）。
//!
//! 职责：
//! - 扫描 Rime 用户目录与 shared data 目录里的 *.schema.yaml；
//! - 读写 default.custom.yaml 的 patch.schema_list；
//! - 写前备份，写后调用平台 trigger 重新部署。
//!
//! YAML 改写策略：行级替换列表项，只动 patch.schema_list 的值，
//! 其它键、缩进与注释原样保留。只有 patch 写成行内映射（patch: {...}）
//! 时才退回 serde_yaml 整篇重排（这种写法本来也不带注释）。

use std::path::{Path, PathBuf};

use rime_sync_core::platform::{self, RimeTrigger};
use serde::Serialize;
use serde_yaml::{Mapping, Value};
use tauri::{AppHandle, State};

use crate::{detect_rime, ensure_config, push_log, AppState};

const SCHEMA_SUFFIX: &str = ".schema.yaml";
const CUSTOM_FILE: &str = "default.custom.yaml";
const MAX_ID_LEN: usize = 128;
/// Windows 记事本会写 UTF-8 BOM；解析前剥掉，写回时原样补上。
const BOM: char = '\u{feff}';

fn strip_bom(text: &str) -> (&str, bool) {
    match text.strip_prefix(BOM) {
        Some(rest) => (rest, true),
        None => (text, false),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// local = 用户 rime 目录；shared = 随包 / 发行版 shared data 目录。
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct SetSchemasResult {
    pub schemas: Vec<SchemaView>,
    /// 写前备份路径（文件原本不存在时为 null）。
    pub backup: Option<String>,
    pub deployed: bool,
    /// 部署失败的可读原因；此时配置已写入，前端提示「重试部署」。
    pub deploy_error: Option<String>,
}

/* ---------- 扫描与解析 ---------- */

/// 单段安全 ID：不允许路径分隔符、控制字符，也不允许 . / ..。
fn safe_schema_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN || id == "." || id == ".." {
        return false;
    }
    id.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        // name 可能是列表：取第一个能用的标量。
        Value::Sequence(items) => items.iter().find_map(scalar_text),
        _ => None,
    }
}

/// 解析一个 schema.yaml：返回 (schema id, 显示名)。
/// id 取 schema.schema_id，缺失时退回文件名；name 缺失时退回 id。
fn parse_schema_file(path: &Path) -> Option<(String, String)> {
    let file_name = path.file_name()?.to_str()?;
    let fallback_id = file_name.strip_suffix(SCHEMA_SUFFIX)?.to_string();
    let text = std::fs::read_to_string(path).ok()?;
    let doc: Value = serde_yaml::from_str(&text).ok()?;
    let (raw_id, raw_name) = match doc.get("schema")? {
        Value::Mapping(map) => (
            map.get("schema_id").and_then(scalar_text),
            map.get("name").and_then(scalar_text),
        ),
        other => (scalar_text(other), doc.get("name").and_then(scalar_text)),
    };
    let id = raw_id.unwrap_or(fallback_id);
    let name = raw_name.unwrap_or_else(|| id.clone());
    Some((id, name))
}

/// 用户目录优先：同名 id 以本地文件为准；坏 YAML 直接跳过，不影响其它方案。
fn collect_schemas(
    rime_dir: &Path,
    shared_dirs: &[PathBuf],
) -> Vec<(String, String, &'static str)> {
    let mut found: Vec<(String, String, &'static str)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut scan = |dir: &Path, source: &'static str| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if !path.is_file() {
                continue;
            }
            let Some((id, name)) = parse_schema_file(&path) else {
                continue;
            };
            if !safe_schema_id(&id) || seen.iter().any(|item| item == &id) {
                continue;
            }
            seen.push(id.clone());
            found.push((id, name, source));
        }
    };
    scan(rime_dir, "local");
    for dir in shared_dirs {
        scan(dir, "shared");
    }
    found
}

/// 已启用（按配置顺序）在前，其余按名字排序。
fn order_schemas(
    mut found: Vec<(String, String, &'static str)>,
    enabled: &[String],
) -> Vec<SchemaView> {
    let mut views: Vec<SchemaView> = Vec::with_capacity(found.len());
    for id in enabled {
        let Some(index) = found.iter().position(|(item, _, _)| item == id) else {
            continue;
        };
        let (id, name, source) = found.remove(index);
        views.push(SchemaView {
            id,
            name,
            enabled: true,
            source: source.to_string(),
        });
    }
    let mut rest: Vec<SchemaView> = found
        .into_iter()
        .map(|(id, name, source)| SchemaView {
            id,
            name,
            enabled: false,
            source: source.to_string(),
        })
        .collect();
    rest.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    views.extend(rest);
    views
}

fn schema_list_id(value: &Value) -> Option<String> {
    match value {
        // 标准写法：- schema: luna_pinyin / - {schema: luna_pinyin}
        Value::Mapping(map) => map.get("schema").and_then(scalar_text),
        other => scalar_text(other),
    }
}

fn read_enabled(custom: &Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(custom) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("读取 {} 失败：{error}", custom.display())),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let (body, _) = strip_bom(&text);
    let doc: Value = serde_yaml::from_str(body)
        .map_err(|error| format!("{} 不是合法 YAML：{error}", custom.display()))?;
    let Some(Value::Sequence(items)) = doc.get("patch").and_then(|patch| patch.get("schema_list"))
    else {
        return Ok(Vec::new());
    };
    Ok(items.iter().filter_map(schema_list_id).collect())
}

/* ---------- 行级改写 ---------- */

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// 去掉行尾注释（引号内的 # 不算）。
fn strip_comment(value: &str) -> String {
    let trimmed = value.trim_start();
    if trimmed.starts_with('#') {
        return String::new();
    }
    let mut quote: Option<char> = None;
    for (index, ch) in trimmed.char_indices() {
        match quote {
            Some(open) => {
                if ch == open {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '#' if index == 0 || trimmed[..index].ends_with(' ') => {
                    return trimmed[..index].trim_end().to_string();
                }
                _ => {}
            },
        }
    }
    trimmed.trim_end().to_string()
}

/// 解析 key: value 行；列表项（- schema: x）返回 None。
fn split_key(line: &str) -> Option<(usize, String, String)> {
    let indent = indent_of(line);
    let body = &line[indent..];
    if body.is_empty() || body.starts_with('#') {
        return None;
    }
    let colon = body.find(':')?;
    let raw_key = body[..colon].trim();
    if raw_key.is_empty() || raw_key.contains(' ') || raw_key.contains('\t') {
        return None;
    }
    let key = raw_key.trim_matches('"').trim_matches('\'').to_string();
    Some((indent, key, strip_comment(&body[colon + 1..])))
}

fn comment_suffix(line: &str) -> String {
    match line.find(" #") {
        Some(index) => format!(" {}", &line[index + 1..]),
        None => String::new(),
    }
}

struct ListStyle {
    indent: usize,
    flow: bool,
    bare: bool,
}

impl ListStyle {
    fn item(&self, id: &str) -> String {
        let pad = " ".repeat(self.indent);
        if self.bare {
            format!("{pad}- {id}")
        } else if self.flow {
            format!("{pad}- {{schema: {id}}}")
        } else {
            format!("{pad}- schema: {id}")
        }
    }
}

/// 沿用原有列表项写法（flow / block / 裸字符串）与缩进；没有原列表时用 block 写法。
fn detect_style(items: &[String], fallback_indent: usize) -> ListStyle {
    let mut style = ListStyle {
        indent: fallback_indent,
        flow: false,
        bare: false,
    };
    let mut saw_block = false;
    for line in items {
        let trimmed = line.trim();
        if !trimmed.starts_with('-') {
            continue;
        }
        style.indent = indent_of(line);
        let rest = trimmed[1..].trim_start();
        if rest.starts_with('{') {
            style.flow = true;
        } else if rest.is_empty() || rest.starts_with("schema:") {
            saw_block = true;
        } else {
            style.bare = true;
        }
    }
    if style.flow || saw_block {
        style.bare = false;
    }
    style
}

fn join_lines(lines: &[String], newline: &str, trailing_newline: bool) -> String {
    let mut text = lines.join(newline);
    if trailing_newline {
        text.push_str(newline);
    }
    text
}

fn serde_rewrite(text: &str, ids: &[String]) -> Result<String, String> {
    let mut doc: Value =
        serde_yaml::from_str(text).map_err(|error| format!("读取 YAML 失败：{error}"))?;
    let Some(Value::Mapping(patch)) = doc.get_mut("patch") else {
        return Err("patch 不是映射，无法写入 schema_list".to_string());
    };
    let mut list = Vec::with_capacity(ids.len());
    for id in ids {
        let mut item = Mapping::new();
        item.insert(
            Value::String("schema".to_string()),
            Value::String(id.clone()),
        );
        list.push(Value::Mapping(item));
    }
    patch.insert(
        Value::String("schema_list".to_string()),
        Value::Sequence(list),
    );
    serde_yaml::to_string(&doc).map_err(|error| format!("生成 YAML 失败：{error}"))
}

/// 替换 patch.schema_list，保留其它键与注释。
fn rewrite_schema_list(text: &str, ids: &[String]) -> Result<String, String> {
    if text.trim().is_empty() {
        let mut out = String::from("patch:\n  schema_list:\n");
        for id in ids {
            out.push_str(&format!("    - schema: {id}\n"));
        }
        return Ok(out);
    }

    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = text.ends_with('\n');
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    if trailing_newline && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    let patch = lines
        .iter()
        .position(|line| matches!(split_key(line), Some((0, ref key, _)) if key == "patch"));
    let Some(patch) = patch else {
        lines.push("patch:".to_string());
        lines.push("  schema_list:".to_string());
        lines.extend(ids.iter().map(|id| format!("    - schema: {id}")));
        return Ok(join_lines(&lines, newline, trailing_newline));
    };

    let Some((patch_indent, _, patch_value)) = split_key(&lines[patch]) else {
        return Err("无法解析 patch: 行".to_string());
    };
    if !patch_value.is_empty() {
        return serde_rewrite(text, ids);
    }

    // patch 块的结束位置：下一个顶层的非注释行。
    let patch_end = (patch + 1..lines.len())
        .find(|&index| {
            let line = &lines[index];
            !line.trim().is_empty() && indent_of(line) == 0 && !line.trim_start().starts_with('#')
        })
        .unwrap_or(lines.len());

    let schema_list = (patch + 1..patch_end).find(|&index| {
        matches!(
            split_key(&lines[index]),
            Some((indent, ref key, _)) if indent > patch_indent && key == "schema_list"
        )
    });

    let Some(sl) = schema_list else {
        let key_indent = patch_indent + 2;
        let mut inserted = vec![format!("{}schema_list:", " ".repeat(key_indent))];
        inserted.extend(
            ids.iter()
                .map(|id| format!("{}- schema: {id}", " ".repeat(key_indent + 2))),
        );
        lines.splice(patch + 1..patch + 1, inserted);
        return Ok(join_lines(&lines, newline, trailing_newline));
    };

    let Some((sl_indent, _, sl_value)) = split_key(&lines[sl]) else {
        return Err("无法解析 schema_list: 行".to_string());
    };

    // schema_list 的值区域：缩进更深的行，或同级以 - 开头的行。
    let mut region_end = sl + 1;
    let mut cursor = sl + 1;
    while cursor < patch_end {
        let line = &lines[cursor];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            cursor += 1;
            continue;
        }
        let indent = indent_of(line);
        if indent > sl_indent || (indent == sl_indent && trimmed.starts_with('-')) {
            cursor += 1;
            region_end = cursor;
            continue;
        }
        break;
    }

    let style = detect_style(&lines[sl + 1..region_end], sl_indent + 2);
    let items: Vec<String> = ids.iter().map(|id| style.item(id)).collect();

    if sl_value.is_empty() {
        lines.splice(sl + 1..region_end, items);
    } else {
        // schema_list: [] 这类行内写法：整行换成块形式，行尾注释保留。
        let key_line = format!(
            "{}schema_list:{}",
            " ".repeat(sl_indent),
            comment_suffix(&lines[sl])
        );
        let mut block = vec![key_line];
        block.extend(items);
        lines.splice(sl..sl + 1, block);
    }
    Ok(join_lines(&lines, newline, trailing_newline))
}

/* ---------- 写入 + 部署 ---------- */

fn backup_custom(custom: &Path) -> Result<Option<PathBuf>, String> {
    if !custom.exists() {
        return Ok(None);
    }
    let name = custom
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("文件名不合法：{}", custom.display()))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut candidate = custom.with_file_name(format!("{name}.bak-{stamp}"));
    let mut sequence = 1;
    while candidate.exists() {
        candidate = custom.with_file_name(format!("{name}.bak-{stamp}-{sequence}"));
        sequence += 1;
    }
    std::fs::copy(custom, &candidate)
        .map_err(|error| format!("备份 {} 失败：{error}", custom.display()))?;
    Ok(Some(candidate))
}

fn validate_ids(ids: &[String], known: &[SchemaView]) -> Result<Vec<String>, String> {
    let mut selected: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        if !safe_schema_id(id) {
            return Err(format!("非法的方案 ID：{id}"));
        }
        if !known.iter().any(|schema| &schema.id == id) {
            return Err(format!("未知的输入方案：{id}"));
        }
        if !selected.iter().any(|seen| seen == id) {
            selected.push(id.clone());
        }
    }
    if selected.is_empty() {
        return Err("至少需要启用一个输入方案".to_string());
    }
    Ok(selected)
}

fn apply_schemas(
    rime_dir: &Path,
    shared_dirs: &[PathBuf],
    ids: &[String],
    trigger: &dyn RimeTrigger,
) -> Result<SetSchemasResult, String> {
    let known = order_schemas(collect_schemas(rime_dir, shared_dirs), &[]);
    let selected = validate_ids(ids, &known)?;

    let custom = rime_dir.join(CUSTOM_FILE);
    let original = match std::fs::read_to_string(&custom) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("读取 {} 失败：{error}", custom.display())),
    };
    let (body, has_bom) = strip_bom(&original);
    // 读不懂原文件时拒绝改写：行级替换会覆盖掉用户本来能用的配置。
    if !body.trim().is_empty() {
        serde_yaml::from_str::<Value>(body)
            .map_err(|error| format!("{} 不是合法 YAML，已取消修改：{error}", custom.display()))?;
    }
    let rewritten = rewrite_schema_list(body, &selected)?;
    let updated = if has_bom {
        format!("{BOM}{rewritten}")
    } else {
        rewritten
    };
    serde_yaml::from_str::<Value>(strip_bom(&updated).0)
        .map_err(|error| format!("生成的 YAML 无效，已取消修改：{error}"))?;

    let backup = backup_custom(&custom)?;
    std::fs::write(&custom, &updated)
        .map_err(|error| format!("写入 {} 失败：{error}", custom.display()))?;

    let schemas = order_schemas(collect_schemas(rime_dir, shared_dirs), &selected);
    let backup = backup.map(|path| path.display().to_string());
    match trigger.deploy(rime_dir) {
        Ok(()) => Ok(SetSchemasResult {
            schemas,
            backup,
            deployed: true,
            deploy_error: None,
        }),
        Err(error) => Ok(SetSchemasResult {
            schemas,
            backup,
            deployed: false,
            deploy_error: Some(format!("{error:#}")),
        }),
    }
}

/* ---------- shared data 目录 ---------- */

fn push_dir(dirs: &mut Vec<PathBuf>, candidate: PathBuf) {
    if candidate.is_dir() && !dirs.iter().any(|dir| dir == &candidate) {
        dirs.push(candidate);
    }
}

fn shared_data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(raw) = std::env::var_os("RIME_SHARED_DATA_DIR") {
        for part in std::env::split_paths(&raw) {
            push_dir(&mut dirs, part);
        }
    }
    #[cfg(target_os = "linux")]
    {
        for candidate in ["/usr/share/rime-data", "/usr/local/share/rime-data"] {
            push_dir(&mut dirs, PathBuf::from(candidate));
        }
    }
    #[cfg(windows)]
    {
        // 安装目录（NSIS 默认就是 exe 所在目录）下的 data / resources/data。
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                push_dir(&mut dirs, dir.join("data"));
                push_dir(&mut dirs, dir.join("resources").join("data"));
            }
        }
        // Weasel 把方案装在 <ProgramFiles>\Rime\weasel-<ver>\data。
        for key in ["ProgramFiles", "ProgramFiles(x86)"] {
            let Some(root) = std::env::var_os(key) else {
                continue;
            };
            let rime = PathBuf::from(root).join("Rime");
            push_dir(&mut dirs, rime.join("data"));
            if let Ok(entries) = std::fs::read_dir(&rime) {
                let mut children: Vec<PathBuf> =
                    entries.flatten().map(|entry| entry.path()).collect();
                children.sort();
                for child in children {
                    push_dir(&mut dirs, child.join("data"));
                }
            }
        }
    }
    dirs
}

/* ---------- 命令 ---------- */

fn list_for_dir(rime_dir: &Path, shared_dirs: &[PathBuf]) -> Result<Vec<SchemaView>, String> {
    let enabled = match read_enabled(&rime_dir.join(CUSTOM_FILE)) {
        Ok(list) => list,
        Err(error) => {
            // 读不懂就按「全部未启用」展示：至少还能看到装了哪些方案。
            eprintln!("[rime-sync] {error}");
            Vec::new()
        }
    };
    Ok(order_schemas(
        collect_schemas(rime_dir, shared_dirs),
        &enabled,
    ))
}

#[tauri::command]
pub fn cmd_list_schemas(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<SchemaView>, String> {
    let config = ensure_config(&app, &state.0)?;
    let rime = detect_rime(&config)?.path;
    list_for_dir(&rime, &shared_data_dirs())
}

#[tauri::command]
pub fn cmd_set_schemas(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<SetSchemasResult, String> {
    let config = ensure_config(&app, &state.0)?;
    let rime = detect_rime(&config)?.path;
    let trigger = platform::default_trigger(config.stop_fcitx5_during_apply);
    let result = apply_schemas(&rime, &shared_data_dirs(), &ids, trigger.as_ref())?;
    let enabled = result
        .schemas
        .iter()
        .filter(|schema| schema.enabled)
        .count();
    match &result.deploy_error {
        None => push_log(
            &state.0,
            format!("输入方案已更新（启用 {enabled} 个），已重新部署"),
        ),
        Some(error) => push_log(
            &state.0,
            format!("输入方案已保存（启用 {enabled} 个），但重新部署失败：{error}"),
        ),
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rime_sync_core::platform::FakeTrigger;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-schemas-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
        fs::write(path, text).expect("write");
    }

    fn schema_file(id: &str, name: Option<&str>) -> String {
        match name {
            Some(name) => format!("schema:\n  schema_id: {id}\n  name: {name}\n"),
            None => format!("schema:\n  schema_id: {id}\n"),
        }
    }

    #[test]
    fn parses_name_list_first_item() {
        let dir = temp_dir("name-list");
        write(
            &dir.join("pinyin.schema.yaml"),
            "schema:\n  schema_id: pinyin\n  name:\n    - 拼音\n    - Pinyin\n",
        );
        let found = collect_schemas(&dir, &[]);
        assert_eq!(
            found,
            vec![("pinyin".to_string(), "拼音".to_string(), "local")]
        );
    }

    #[test]
    fn missing_name_falls_back_to_schema_id() {
        let dir = temp_dir("no-name");
        write(
            &dir.join("wubi86.schema.yaml"),
            &schema_file("wubi86", None),
        );
        let found = collect_schemas(&dir, &[]);
        assert_eq!(
            found,
            vec![("wubi86".to_string(), "wubi86".to_string(), "local")]
        );
    }

    #[test]
    fn missing_schema_id_falls_back_to_file_name() {
        let dir = temp_dir("no-id");
        write(
            &dir.join("double_pinyin.schema.yaml"),
            "schema:\n  name: 双拼\n",
        );
        let found = collect_schemas(&dir, &[]);
        assert_eq!(
            found,
            vec![("double_pinyin".to_string(), "双拼".to_string(), "local")]
        );
    }

    #[test]
    fn bad_yaml_is_skipped_without_failing_the_scan() {
        let dir = temp_dir("bad-yaml");
        write(
            &dir.join("broken.schema.yaml"),
            "schema: [unclosed\n  name: : :\n",
        );
        write(
            &dir.join("ok.schema.yaml"),
            &schema_file("ok", Some("可用")),
        );
        let found = collect_schemas(&dir, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "ok");
    }

    #[test]
    fn unknown_ids_are_ignored_when_ordering() {
        let dir = temp_dir("order");
        write(&dir.join("b.schema.yaml"), &schema_file("b", Some("乙")));
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        write(&dir.join("c.schema.yaml"), &schema_file("c", Some("丙")));
        let ordered = order_schemas(collect_schemas(&dir, &[]), &["c".into(), "gone".into()]);
        let ids: Vec<&str> = ordered.iter().map(|schema| schema.id.as_str()).collect();
        // 其余按名字的码位序：丙(U+4E19) < 乙(U+4E59) < 甲(U+7532)。
        assert_eq!(ids, vec!["c", "b", "a"]);
        assert!(ordered[0].enabled);
        assert!(!ordered[1].enabled);
    }

    #[test]
    fn local_schema_wins_over_shared() {
        let local = temp_dir("local-wins");
        let shared = temp_dir("shared-src");
        write(
            &local.join("luna_pinyin.schema.yaml"),
            &schema_file("luna_pinyin", Some("本地")),
        );
        write(
            &shared.join("luna_pinyin.schema.yaml"),
            &schema_file("luna_pinyin", Some("随包")),
        );
        write(
            &shared.join("wubi86.schema.yaml"),
            &schema_file("wubi86", Some("五笔")),
        );
        let found = collect_schemas(&local, std::slice::from_ref(&shared));
        assert_eq!(found.len(), 2);
        let local_entry = found
            .iter()
            .find(|(id, _, _)| id == "luna_pinyin")
            .expect("local");
        assert_eq!(local_entry.1, "本地");
        assert_eq!(local_entry.2, "local");
        let shared_entry = found
            .iter()
            .find(|(id, _, _)| id == "wubi86")
            .expect("shared");
        assert_eq!(shared_entry.2, "shared");
    }

    #[test]
    fn reads_enabled_order_from_custom_yaml() {
        let dir = temp_dir("read-enabled");
        write(
            &dir.join(CUSTOM_FILE),
            "patch:\n  schema_list:\n    - {schema: b}\n    - schema: a\n",
        );
        assert_eq!(
            read_enabled(&dir.join(CUSTOM_FILE)).expect("read"),
            vec!["b", "a"]
        );
    }

    #[test]
    fn set_keeps_other_keys_and_comments() {
        let dir = temp_dir("preserve");
        let custom = dir.join(CUSTOM_FILE);
        let original = "# 顶层注释\npatch:\n  \"menu/page_size\": 9  # 每页候选\n  schema_list:\n    - schema: a\n    - schema: b\n\n# 尾部注释\n";
        write(&custom, original);
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        write(&dir.join("b.schema.yaml"), &schema_file("b", Some("乙")));
        write(&dir.join("c.schema.yaml"), &schema_file("c", Some("丙")));

        let trigger = FakeTrigger::new();
        let result = apply_schemas(&dir, &[], &["b".into(), "c".into()], &trigger).expect("apply");
        assert!(result.deployed);
        assert!(result.deploy_error.is_none());

        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.contains("# 顶层注释"), "{text}");
        assert!(text.contains("# 每页候选"), "{text}");
        assert!(text.contains("# 尾部注释"), "{text}");
        assert!(text.contains("\"menu/page_size\": 9"), "{text}");
        assert!(text.contains("- schema: b"), "{text}");
        assert!(text.contains("- schema: c"), "{text}");
        assert!(!text.contains("- schema: a"), "{text}");
        // 缩进与顺序都按配置来。
        let b = text.find("- schema: b").expect("b");
        let c = text.find("- schema: c").expect("c");
        assert!(b < c);
        assert_eq!(&text[b - 4..b], "    ");
        let backup = result.backup.expect("backup");
        assert!(Path::new(&backup).is_file());
        assert!(backup.contains(".bak-"));
        assert_eq!(fs::read_to_string(&backup).expect("backup text"), original);
    }

    #[test]
    fn set_preserves_flow_style_items() {
        let dir = temp_dir("flow");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "patch:\n  schema_list:\n    - {schema: a}\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        write(&dir.join("b.schema.yaml"), &schema_file("b", Some("乙")));
        apply_schemas(&dir, &[], &["a".into(), "b".into()], &FakeTrigger::new()).expect("apply");
        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.contains("- {schema: a}"), "{text}");
        assert!(text.contains("- {schema: b}"), "{text}");
    }

    #[test]
    fn set_inserts_schema_list_when_missing() {
        let dir = temp_dir("insert");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "patch:\n  \"menu/page_size\": 7\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        apply_schemas(&dir, &[], &["a".into()], &FakeTrigger::new()).expect("apply");
        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.contains("  schema_list:\n    - schema: a\n"), "{text}");
        assert!(text.contains("\"menu/page_size\": 7"), "{text}");
    }

    #[test]
    fn set_creates_custom_yaml_when_missing() {
        let dir = temp_dir("create");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        let result = apply_schemas(&dir, &[], &["a".into()], &FakeTrigger::new()).expect("apply");
        assert!(result.backup.is_none());
        let text = fs::read_to_string(dir.join(CUSTOM_FILE)).expect("read");
        assert_eq!(text, "patch:\n  schema_list:\n    - schema: a\n");
    }

    #[test]
    fn set_replaces_inline_schema_list() {
        let dir = temp_dir("inline");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "patch:\n  schema_list: []  # 待定\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        apply_schemas(&dir, &[], &["a".into()], &FakeTrigger::new()).expect("apply");
        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.contains("schema_list: # 待定"), "{text}");
        assert!(text.contains("  - schema: a"), "{text}");
    }

    #[test]
    fn invalid_ids_are_rejected_before_writing() {
        let dir = temp_dir("reject");
        let custom = dir.join(CUSTOM_FILE);
        let original = "patch:\n  schema_list:\n    - schema: a\n";
        write(&custom, original);
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));

        for ids in [
            vec!["../escape".to_string()],
            vec!["a/b".to_string()],
            vec!["..".to_string()],
            vec!["unknown".to_string()],
            vec![String::new()],
            Vec::new(),
        ] {
            assert!(
                apply_schemas(&dir, &[], &ids, &FakeTrigger::new()).is_err(),
                "{ids:?}"
            );
        }
        assert_eq!(fs::read_to_string(&custom).expect("read"), original);
    }

    #[test]
    fn deploy_failure_still_writes_the_file() {
        let dir = temp_dir("deploy-fail");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "patch:\n  schema_list:\n    - schema: a\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        write(&dir.join("b.schema.yaml"), &schema_file("b", Some("乙")));

        let result = apply_schemas(
            &dir,
            &[],
            &["b".into(), "a".into()],
            &FakeTrigger::failing("deploy"),
        )
        .expect("apply returns a result");
        assert!(!result.deployed);
        assert!(result.deploy_error.is_some());
        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.contains("- schema: b"), "{text}");
        assert!(result
            .schemas
            .iter()
            .any(|schema| schema.id == "b" && schema.enabled));
    }

    #[test]
    fn broken_custom_yaml_is_not_overwritten() {
        let dir = temp_dir("broken-custom");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "patch: [unclosed\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        let error =
            apply_schemas(&dir, &[], &["a".into()], &FakeTrigger::new()).expect_err("reject");
        assert!(error.contains("不是合法 YAML"), "{error}");
        assert_eq!(
            fs::read_to_string(&custom).expect("read"),
            "patch: [unclosed\n"
        );
    }

    #[test]
    fn list_reports_source_and_enabled_state() {
        let local = temp_dir("list-local");
        let shared = temp_dir("list-shared");
        write(
            &local.join(CUSTOM_FILE),
            "patch:\n  schema_list:\n    - schema: wubi86\n",
        );
        write(
            &shared.join("wubi86.schema.yaml"),
            &schema_file("wubi86", Some("五笔")),
        );
        write(
            &local.join("luna_pinyin.schema.yaml"),
            &schema_file("luna_pinyin", Some("朙月拼音")),
        );
        let list = list_for_dir(&local, &[shared]).expect("list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "wubi86");
        assert!(list[0].enabled);
        assert_eq!(list[0].source, "shared");
        assert_eq!(list[1].id, "luna_pinyin");
        assert!(!list[1].enabled);
        assert_eq!(list[1].source, "local");
    }

    #[test]
    fn crlf_line_endings_are_preserved() {
        let dir = temp_dir("crlf");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "patch:\r\n  schema_list:\r\n    - schema: a\r\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        write(&dir.join("b.schema.yaml"), &schema_file("b", Some("乙")));
        apply_schemas(&dir, &[], &["b".into()], &FakeTrigger::new()).expect("apply");
        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.contains("- schema: b\r\n"), "{text:?}");
        assert!(!text.contains("- schema: a"), "{text:?}");
    }

    #[test]
    fn utf8_bom_is_preserved() {
        let dir = temp_dir("bom");
        let custom = dir.join(CUSTOM_FILE);
        write(&custom, "\u{feff}patch:\n  schema_list:\n    - schema: a\n");
        write(&dir.join("a.schema.yaml"), &schema_file("a", Some("甲")));
        write(&dir.join("b.schema.yaml"), &schema_file("b", Some("乙")));
        assert_eq!(read_enabled(&custom).expect("read"), vec!["a"]);

        apply_schemas(&dir, &[], &["b".into()], &FakeTrigger::new()).expect("apply");
        let text = fs::read_to_string(&custom).expect("read");
        assert!(text.starts_with('\u{feff}'), "{text:?}");
        assert!(text.contains("- schema: b"), "{text}");
        assert!(!text.contains("- schema: a"), "{text}");
        assert_eq!(read_enabled(&custom).expect("read"), vec!["b"]);
    }
}
