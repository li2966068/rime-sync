//! 把 v 模式的方案补丁直接写进引擎实际加载的 data 方案文件（0.6.18）。
//!
//! 为什么不能只铺 build/：227 实测 %APPDATA%\Rime\build\luna_pinyin.schema.yaml
//! 里已经有 rimesync 组件，rime.dll 里也有插件符号，但打 v1234 没有候选、按
//! a 选词后还多出一个 a——引擎根本没读我们铺的 build，用的是 data 下的原版方案。
//! 该机的 WeaselDeployer /deploy 恒返回 0 且不产出，部署链不可用，所以只能直接改
//! <WeaselRoot>\data\<schema>.schema.yaml（Linux 对应 /usr/share/rime-data/）。
//!
//! 四处补丁（全部幂等，重复执行产出同一份文本）：
//! 1. engine/translators 首位插入 rimesync_vnum
//! 2. engine/processors 在 recognizer 之后插入 rimesync_vselect，其后插入 numpad
//! 3. recognizer/patterns 增加 vnum: '^[vV][0-9.]*$'
//! 4. 文件顶部插入 # patched by rime-sync <version>
//!
//! 写盘纪律：先备份 <file>.rimesync-orig（只做一次），补丁后用 serde_yaml 重新
//! 解析确认合法、核对目标状态，再按原换行风格写成 UTF-8（不带 BOM）。权限不足时
//! 返回可读错误（提示以管理员身份重试），不 panic。

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_yaml::Value;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// 原文件备份后缀：只建一次，重复执行不覆盖。
pub const BACKUP_SUFFIX: &str = ".rimesync-orig";
/// 顶部标记前缀；实际写入时后面跟版本号。
pub const MARKER_PREFIX: &str = "# patched by rime-sync";
/// recognizer/patterns/vnum 的取值：允许中间态 v、v12.，否则点号会被切成标点。
pub const VNUM_PATTERN: &str = "^[vV][0-9.]*$";
pub const VNUM_TRANSLATOR: &str = "rimesync_vnum";
pub const VSELECT_PROCESSOR: &str = "rimesync_vselect";
/// 小键盘归一化处理器（third_party/librime/plugins/rimesync-extra）。
pub const NUMPAD_PROCESSOR: &str = "numpad";
/// 显式指定 data 目录（多个用系统路径分隔符）；空串或 0 = 关闭补丁。测试靠它隔离。
pub const ENV_DATA_DIR: &str = "RIMESYNC_WEASEL_DATA";
/// Linux 的 data 目录。
pub const LINUX_DATA_DIR: &str = "/usr/share/rime-data";
/// Windows 注册表里的 Weasel 配置键。
pub const REG_KEY: &str = r"HKLM\Software\WOW6432Node\Rime\Weasel";
const REG_VALUES: [&str; 2] = ["WeaselRoot", "InstallDir"];
const BOM: char = '\u{feff}';

/// 单文件的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOutcome {
    /// 写入了补丁。
    Patched,
    /// 已经是目标状态，一个字节都没动。
    Unchanged,
    /// 结构上不适用（没有 engine / 列表是 flow 风格等），没动。
    Skipped,
}

/// 一批 data 目录的补丁结果。
#[derive(Debug, Clone, Default, Serialize)]
pub struct DataPatchReport {
    /// 实际检查的 data 目录。
    pub data_dirs: Vec<String>,
    /// 本次真正写入的方案文件。
    pub schemas_patched: Vec<String>,
    /// 已是目标状态、没有写入的方案文件。
    pub schemas_unchanged: Vec<String>,
    /// 跳过的对象（方案文件或 data 目录），后面括号里是原因。
    pub schemas_skipped: Vec<String>,
    /// 备份所在目录（写第一个备份的 data 目录）；没写过备份时为空。
    pub backup_dir: String,
    /// 可读的失败原因（权限不足时带提权提示）。
    pub errors: Vec<String>,
}

impl DataPatchReport {
    /// 本次写入的方案个数，就是 CLI 报告里的 data_patched。
    pub fn patched(&self) -> usize {
        self.schemas_patched.len()
    }

    /// 是否需要提示用户以管理员身份重试。
    pub fn needs_elevation(&self) -> bool {
        self.errors.iter().any(|error| error.contains("管理员"))
    }

    /// 一句话结论（界面 / 日志共用）。
    pub fn summary(&self) -> String {
        let mut text = format!(
            "data 方案补丁：写入 {}，无需改动 {}，跳过 {}，失败 {}",
            self.schemas_patched.len(),
            self.schemas_unchanged.len(),
            self.schemas_skipped.len(),
            self.errors.len()
        );
        if let Some(first) = self.errors.first() {
            text.push_str(&format!("；首个错误：{first}"));
        }
        text
    }
}

/// 定位引擎实际加载的 data 目录。
///
/// * RIMESYNC_WEASEL_DATA 一旦设置就只用它（空串或 0 = 关闭补丁，测试靠它隔离）；
/// * Windows：注册表 HKLM\Software\WOW6432Node\Rime\Weasel 的 WeaselRoot
///   （没有就 InstallDir）下的 data 子目录；读不到时退回 %ProgramFiles%\Rime\data；
/// * 其它平台：/usr/share/rime-data（不存在就返回空，由调用方报告跳过）。
pub fn data_dirs() -> Vec<PathBuf> {
    if let Some(raw) = env::var_os(ENV_DATA_DIR) {
        return dirs_from_env(&raw);
    }
    let mut dirs = Vec::new();
    // 平台默认目录即使不存在也要留下：patch_dirs 会把它记进 schemas_skipped
    // （「目录不存在」），这正是「不存在则跳过并报告」。
    #[cfg(windows)]
    {
        if let Some(root) = weasel_root_from_registry() {
            push_candidate(&mut dirs, root.join("data"));
        }
        if let Some(program_files) = env::var_os("ProgramFiles") {
            push_candidate(
                &mut dirs,
                PathBuf::from(program_files).join("Rime").join("data"),
            );
        }
    }
    #[cfg(not(windows))]
    push_candidate(&mut dirs, PathBuf::from(LINUX_DATA_DIR));
    dirs
}

/// RIMESYNC_WEASEL_DATA 的解析：空串 / 0 = 关闭；其余按系统路径分隔符拆分，
/// 只保留真实存在的目录。
fn dirs_from_env(raw: &OsStr) -> Vec<PathBuf> {
    let text = raw.to_string_lossy();
    if text.trim().is_empty() || text.trim() == "0" {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    for part in env::split_paths(raw) {
        push_dir(&mut dirs, part);
    }
    dirs
}

fn push_dir(dirs: &mut Vec<PathBuf>, candidate: PathBuf) {
    if candidate.is_dir() {
        push_candidate(dirs, candidate);
    }
}

fn push_candidate(dirs: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidate.as_os_str().is_empty() && !dirs.iter().any(|dir| dir == &candidate) {
        dirs.push(candidate);
    }
}

/// 读注册表拿 WeaselRoot / InstallDir。失败（键不存在、权限、非 Windows）返回 None。
#[cfg(windows)]
fn weasel_root_from_registry() -> Option<PathBuf> {
    let output = std::process::Command::new("reg")
        .args(["query", REG_KEY])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    weasel_root_from_reg_output(&String::from_utf8_lossy(&output.stdout))
}

/// 解析 reg query 的输出（下面这段是纯文本，不是 Rust 代码）：
///
/// ```text
/// HKEY_LOCAL_MACHINE\Software\WOW6432Node\Rime\Weasel
///     InstallDir    REG_SZ    C:\Program Files\Rime\weasel-0.17.4
///     WeaselRoot    REG_SZ    C:\Program Files\Rime
/// ```
///
/// 先取 WeaselRoot，没有再取 InstallDir；值里可以带空格。
pub fn weasel_root_from_reg_output(output: &str) -> Option<PathBuf> {
    for wanted in REG_VALUES {
        for line in output.lines() {
            let mut tokens = line.split_whitespace();
            let Some(name) = tokens.next() else { continue };
            if !name.eq_ignore_ascii_case(wanted) {
                continue;
            }
            let Some(kind) = tokens.next() else { continue };
            if !kind.eq_ignore_ascii_case("REG_SZ") {
                continue;
            }
            // 路径可能带空格：从类型标记之后整段取。
            let Some(pos) = line.find(kind) else { continue };
            let value = line[pos + kind.len()..].trim().trim_matches('"');
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// 逐目录给 <data>/*.schema.yaml 打补丁；单个文件失败不中断整批。
pub fn patch_dirs(dirs: &[PathBuf], version: &str) -> DataPatchReport {
    let mut report = DataPatchReport::default();
    for dir in dirs {
        report.data_dirs.push(dir.display().to_string());
        if !dir.is_dir() {
            report
                .schemas_skipped
                .push(format!("{}（目录不存在）", dir.display()));
            continue;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                report.errors.push(io_error(dir, &error));
                continue;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_schema_yaml(path))
            .collect();
        files.sort();
        if files.is_empty() {
            report
                .schemas_skipped
                .push(format!("{}（没有 *.schema.yaml）", dir.display()));
        }
        for file in files {
            match patch_file(&file, version) {
                Ok(PatchOutcome::Patched) => {
                    report.schemas_patched.push(file.display().to_string());
                    if report.backup_dir.is_empty() {
                        report.backup_dir = dir.display().to_string();
                    }
                }
                Ok(PatchOutcome::Unchanged) => {
                    report.schemas_unchanged.push(file.display().to_string())
                }
                Ok(PatchOutcome::Skipped) => report
                    .schemas_skipped
                    .push(format!("{}（结构不适用）", file.display())),
                Err(error) => report.errors.push(format!("{}：{error:#}", file.display())),
            }
        }
    }
    report
}

fn is_schema_yaml(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.ends_with(".schema.yaml"))
        .unwrap_or(false)
}

/// 给一个 <schema>.schema.yaml 打补丁。
///
/// 顺序：读文件 -> serde_yaml 解析（不合法就不动）-> 文本级插入 -> 渲染 -> 与原文
/// 比对（相同即 Unchanged，幂等的定义）-> 再次解析 + 核对目标状态 -> 备份（只做
/// 一次）-> 写回。
pub fn patch_file(path: &Path, version: &str) -> Result<PatchOutcome> {
    let bytes = fs::read(path).with_context(|| format!("读取 {}", path.display()))?;
    let raw = String::from_utf8(bytes.clone()).map_err(|_| anyhow!("文件不是 UTF-8，跳过"))?;
    // 目标是 UTF-8 无 BOM：输入带 BOM 时去掉，写回也不写 BOM。
    let text = raw.strip_prefix(BOM).unwrap_or(&raw);
    let value: Value = serde_yaml::from_str(text)
        .with_context(|| format!("{} 不是合法 YAML，跳过", path.display()))?;

    let mut doc = Doc::parse(text);
    let mut applicable = false;
    applicable |= doc.ensure_translator(&value);
    applicable |= doc.ensure_processors(&value);
    applicable |= doc.ensure_vnum_pattern(&value);
    if applicable {
        // 只有真的补过（或已经是目标状态）才留标记：纯 YAML、没有 engine 的
        // 文件不该被我们写上注释。
        doc.ensure_marker(version);
    }

    let patched = doc.render();
    if patched == text {
        // 一个字节都没变：要么已经打过补丁，要么没有可补的结构。
        return Ok(if applicable || doc.changed {
            PatchOutcome::Unchanged
        } else {
            PatchOutcome::Skipped
        });
    }

    // 安全闸：写盘前重新解析 + 核对目标状态，失败就不写。
    let parsed: Value = serde_yaml::from_str(&patched)
        .with_context(|| format!("补丁后的 {} 不是合法 YAML，已取消写入", path.display()))?;
    verify(&parsed)
        .with_context(|| format!("补丁后的 {} 没达到目标状态，已取消写入", path.display()))?;

    let backup = backup_path(path);
    if !backup.exists() {
        fs::write(&backup, &bytes).with_context(|| format!("备份 {}", backup.display()))?;
    }
    fs::write(path, patched.as_bytes()).map_err(|error| anyhow!(io_error(path, &error)))?;
    Ok(PatchOutcome::Patched)
}

/// <file>.rimesync-orig。
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

fn io_error(path: &Path, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return format!(
            "访问 {} 失败：{error}（需要管理员权限：Windows 请以管理员身份重新运行 \
             rime-sync-cli fix-ime 或 rime-sync；Linux 请用 sudo 重试）",
            path.display()
        );
    }
    format!("访问 {} 失败：{error}", path.display())
}

/// 补丁后的结构自检。只核对本补丁负责的四处，用户自己的其它配置一律不碰。
fn verify(value: &Value) -> Result<()> {
    if let Some(engine) = value.get("engine") {
        if let Some(translators) = engine.get("translators").and_then(Value::as_sequence) {
            if translators.first().and_then(Value::as_str) != Some(VNUM_TRANSLATOR) {
                bail!("engine/translators 首位不是 {VNUM_TRANSLATOR}");
            }
        }
        if let Some(processors) = engine.get("processors").and_then(Value::as_sequence) {
            let names: Vec<&str> = processors.iter().filter_map(Value::as_str).collect();
            let vselect = names
                .iter()
                .position(|name| *name == VSELECT_PROCESSOR)
                .ok_or_else(|| anyhow!("engine/processors 里没有 {VSELECT_PROCESSOR}"))?;
            if names.get(vselect + 1) != Some(&NUMPAD_PROCESSOR) {
                bail!("engine/processors 里 {NUMPAD_PROCESSOR} 没有紧跟 {VSELECT_PROCESSOR}");
            }
            if let Some(recognizer) = names.iter().position(|name| *name == "recognizer") {
                if vselect != recognizer + 1 {
                    bail!("{VSELECT_PROCESSOR} 没有紧跟 recognizer");
                }
            }
        }
    }
    if let Some(patterns) = value
        .get("recognizer")
        .and_then(|recognizer| recognizer.get("patterns"))
    {
        if patterns.get("vnum").is_none() {
            bail!("recognizer/patterns 里没有 vnum");
        }
    }
    Ok(())
}

/* ------------------------------ 文本编辑 ------------------------------ */

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// 这一行是不是「缩进 indent 的 key:」（键后面可以直接带值）。
fn is_key_at(line: &str, key: &str, indent: usize) -> bool {
    if is_blank(line) || is_comment(line) || indent_of(line) != indent {
        return false;
    }
    let rest = &line[indent..];
    let Some(after) = rest.strip_prefix(key) else {
        return false;
    };
    after.starts_with(':')
}

/// 这一行的值是 flow 风格（以 { 或 [ 开头）。正则里的 [0-9] 不算。
fn is_flow_value(line: &str) -> bool {
    let Some((_, value)) = line.split_once(':') else {
        return false;
    };
    let value = value.trim();
    value.starts_with('{') || value.starts_with('[')
}

/// 列表项的名字：- rimesync_vselect / - "table_translator@custom_phrase"。
fn item_name(line: &str) -> String {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('-') else {
        return String::new();
    };
    rest.trim()
        .trim_matches(|ch| ch == '\'' || ch == '"')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch| ch == '\'' || ch == '"')
        .to_string()
}

struct Doc {
    lines: Vec<String>,
    eol: String,
    trailing_newline: bool,
    /// 本次是否真的动过行（用于区分 Unchanged / Skipped）。
    changed: bool,
}

impl Doc {
    fn parse(text: &str) -> Self {
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = text.ends_with('\n');
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
            .collect();
        if trailing_newline {
            lines.pop();
        }
        Self {
            lines,
            eol: eol.to_string(),
            trailing_newline,
            changed: false,
        }
    }

    fn render(&self) -> String {
        let body = self.lines.join(&self.eol);
        if self.trailing_newline {
            format!("{body}{}", self.eol)
        } else {
            body
        }
    }

    /// 顶部标记；已有 # patched by rime-sync 就不再插（不追版本号）。
    fn ensure_marker(&mut self, version: &str) -> bool {
        if self
            .lines
            .iter()
            .any(|line| line.trim_start().starts_with(MARKER_PREFIX))
        {
            return false;
        }
        // YAML 指令（%YAML / %TAG）必须留在最前面。
        let at = self
            .lines
            .iter()
            .position(|line| !line.starts_with('%'))
            .unwrap_or(0);
        self.lines.insert(at, format!("{MARKER_PREFIX} {version}"));
        self.changed = true;
        true
    }

    fn find_block_from(&self, key: &str, indent: usize, from: usize) -> Option<(usize, usize)> {
        let start =
            (from..self.lines.len()).find(|&index| is_key_at(&self.lines[index], key, indent))?;
        let mut end = start + 1;
        while end < self.lines.len() {
            let line = &self.lines[end];
            if is_blank(line) || is_comment(line) {
                end += 1;
                continue;
            }
            if indent_of(line) <= indent {
                break;
            }
            end += 1;
        }
        // 块尾的空行/同级注释不算块内容。
        while end > start + 1 {
            let line = &self.lines[end - 1];
            if is_blank(line) || (is_comment(line) && indent_of(line) <= indent) {
                end -= 1;
            } else {
                break;
            }
        }
        Some((start, end))
    }

    fn find_block(&self, key: &str, indent: usize) -> Option<(usize, usize)> {
        self.find_block_from(key, indent, 0)
    }

    fn item_lines(&self, block: (usize, usize)) -> Vec<usize> {
        (block.0 + 1..block.1)
            .filter(|&index| {
                let line = &self.lines[index];
                !is_comment(line) && line.trim_start().starts_with("- ")
            })
            .collect()
    }

    /// 列表项的统一缩进：取现有项；空列表时比键多 2 格。
    fn item_indent(&self, block: (usize, usize), items: &[usize]) -> usize {
        items
            .first()
            .map(|&index| indent_of(&self.lines[index]))
            .unwrap_or_else(|| indent_of(&self.lines[block.0]) + 2)
    }

    /// 列表是不是 flow 风格（processors: [a, b]）。这种没法按行插，直接放弃。
    fn is_flow_list(&self, key: &str, indent: usize) -> bool {
        let Some((start, _)) = self.find_block(key, indent) else {
            return false;
        };
        is_flow_value(&self.lines[start])
    }

    fn remove_item(&mut self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let victims: Vec<usize> = self
            .item_lines(block)
            .into_iter()
            .filter(|&index| item_name(&self.lines[index]) == item)
            .collect();
        if victims.is_empty() {
            return false;
        }
        for index in victims.into_iter().rev() {
            self.lines.remove(index);
        }
        self.changed = true;
        true
    }

    fn insert_item_after(&mut self, key: &str, indent: usize, anchor: &str, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        let line_indent = self.item_indent(block, &items);
        let Some(index) = items
            .iter()
            .find(|&&index| item_name(&self.lines[index]) == anchor)
            .copied()
        else {
            return false;
        };
        self.lines
            .insert(index + 1, format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn insert_item_before(&mut self, key: &str, indent: usize, anchor: &str, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        let line_indent = self.item_indent(block, &items);
        let Some(index) = items
            .iter()
            .find(|&&index| item_name(&self.lines[index]) == anchor)
            .copied()
        else {
            return false;
        };
        self.lines
            .insert(index, format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn insert_item_front(&mut self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        if items.is_empty() {
            return false;
        }
        let line_indent = self.item_indent(block, &items);
        self.lines
            .insert(items[0], format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn push_item(&mut self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        let line_indent = self.item_indent(block, &items);
        let at = items.last().map(|&index| index + 1).unwrap_or(block.0 + 1);
        self.lines
            .insert(at, format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn contains_item(&self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        self.item_lines(block)
            .into_iter()
            .any(|index| item_name(&self.lines[index]) == item)
    }

    /// engine/translators 首位放 rimesync_vnum。
    fn ensure_translator(&mut self, value: &Value) -> bool {
        let Some(engine) = value.get("engine") else {
            return false;
        };
        if engine.get("translators").is_none() || self.is_flow_list("translators", 2) {
            return false;
        }
        self.remove_item("translators", 2, VNUM_TRANSLATOR);
        self.insert_item_front("translators", 2, VNUM_TRANSLATOR)
            || self.push_item("translators", 2, VNUM_TRANSLATOR)
    }

    /// engine/processors：recognizer 之后是 rimesync_vselect，再后面是 numpad。
    fn ensure_processors(&mut self, value: &Value) -> bool {
        let Some(engine) = value.get("engine") else {
            return false;
        };
        if engine.get("processors").is_none() || self.is_flow_list("processors", 2) {
            return false;
        }
        // 先摘掉同名项，再按目标位置插回去：顺序被用户改过也能纠正。
        self.remove_item("processors", 2, VSELECT_PROCESSOR);
        self.remove_item("processors", 2, NUMPAD_PROCESSOR);

        let anchors = ["recognizer", "key_binder", "speller"];
        let mut placed = false;
        for anchor in anchors {
            if !self.contains_item("processors", 2, anchor) {
                continue;
            }
            if anchor == "recognizer" {
                placed = self.insert_item_after("processors", 2, anchor, VSELECT_PROCESSOR);
            } else {
                placed = self.insert_item_before("processors", 2, anchor, VSELECT_PROCESSOR);
            }
            if placed {
                break;
            }
        }
        if !placed {
            placed = self.push_item("processors", 2, VSELECT_PROCESSOR);
        }
        if !placed {
            return false;
        }
        // numpad 必须紧跟 vselect。
        self.insert_item_after("processors", 2, VSELECT_PROCESSOR, NUMPAD_PROCESSOR)
    }

    /// recognizer/patterns/vnum。
    fn ensure_vnum_pattern(&mut self, value: &Value) -> bool {
        let Some(recognizer) = value.get("recognizer") else {
            return false;
        };
        if recognizer
            .get("patterns")
            .and_then(|patterns| patterns.get("vnum"))
            .is_some()
        {
            return false;
        }
        let Some((start, end)) = self.find_block("recognizer", 0) else {
            return false;
        };
        // flow 风格（recognizer: {…}）没法按行插，放弃。
        if is_flow_value(&self.lines[start]) {
            return false;
        }
        let patterns = (start + 1..end).find(|&index| is_key_at(&self.lines[index], "patterns", 2));
        match patterns {
            Some(line) => {
                let Some(block) = self.find_block_from("patterns", 2, line) else {
                    return false;
                };
                let last = (block.0 + 1..block.1)
                    .rev()
                    .find(|&index| !is_blank(&self.lines[index]))
                    .unwrap_or(block.0);
                if is_flow_value(&self.lines[last]) {
                    // patterns: {} 之类的 flow 映射，放弃。
                    return false;
                }
                self.lines
                    .insert(last + 1, format!("    vnum: '{VNUM_PATTERN}'"));
                self.changed = true;
                true
            }
            None => {
                // recognizer 块里没有 patterns：插在块首。
                self.lines.insert(start + 1, "  patterns:".to_string());
                self.lines
                    .insert(start + 2, format!("    vnum: '{VNUM_PATTERN}'"));
                self.changed = true;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_output_prefers_weasel_root() {
        let output = "\r\nHKEY_LOCAL_MACHINE\\Software\\WOW6432Node\\Rime\\Weasel\r\n    \
            InstallDir    REG_SZ    C:\\Program Files\\Rime\\weasel-0.17.4\r\n    \
            WeaselRoot    REG_SZ    C:\\Program Files\\Rime\r\n";
        assert_eq!(
            weasel_root_from_reg_output(output),
            Some(PathBuf::from(r"C:\Program Files\Rime"))
        );
    }

    #[test]
    fn registry_output_falls_back_to_install_dir() {
        let output = "    InstallDir    REG_SZ    D:\\Apps\\Rime  \r\n";
        assert_eq!(
            weasel_root_from_reg_output(output),
            Some(PathBuf::from(r"D:\Apps\Rime"))
        );
    }

    #[test]
    fn registry_output_without_values_is_none() {
        assert_eq!(
            weasel_root_from_reg_output("HKEY_LOCAL_MACHINE\\Nope\r\n"),
            None
        );
        assert_eq!(
            weasel_root_from_reg_output("    WeaselRoot    REG_DWORD    0x1\r\n"),
            None
        );
    }

    /// 平台默认目录即使不存在也要出现在报告里（「不存在则跳过并报告」）。
    #[cfg(not(windows))]
    #[test]
    fn platform_default_dir_is_always_reported() {
        if env::var_os(ENV_DATA_DIR).is_some() {
            return; // 别的测试/CI 显式指定过 data 目录，跳过
        }
        let dirs = data_dirs();
        assert!(
            dirs.iter().any(|dir| dir == Path::new(LINUX_DATA_DIR)),
            "{dirs:?} 里没有 {LINUX_DATA_DIR}"
        );
    }

    #[test]
    fn env_override_parsing() {
        assert!(dirs_from_env(OsStr::new("")).is_empty());
        assert!(dirs_from_env(OsStr::new("0")).is_empty());
        // 不存在的目录被过滤掉
        assert!(dirs_from_env(OsStr::new("/nonexistent/rime-sync-test")).is_empty());
    }

    #[test]
    fn item_name_handles_quotes_and_tickets() {
        assert_eq!(item_name("    - rimesync_vselect"), "rimesync_vselect");
        assert_eq!(
            item_name("    - \"table_translator@custom_phrase\""),
            "table_translator@custom_phrase"
        );
        assert_eq!(item_name("    - simplifier@zh_hans"), "simplifier@zh_hans");
        assert_eq!(item_name("not an item"), "");
    }
}
