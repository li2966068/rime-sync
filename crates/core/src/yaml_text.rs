//! 纯文本 YAML 编辑器（跨平台，不依赖 serde_yaml）。
//!
//! 这里只做「按行改」这一层：解析成行、定位某个键的块、增删列表项、原样渲染回去。
//! 目的是改共享方案文件（data_patch.rs）或改用户目录里 rime-sync 自己生成的
//! custom.yaml（managed_custom.rs）时，只动要动的那几行 —— 注释、缩进、换行风格、
//! 结尾换行、BOM 一律原样保留。
//!
//! 本模块不碰 YAML 语义（不解析、不校验），需要语义时由调用方用 serde_yaml 校验。

use crate::data_patch::MARKER_PREFIX;

pub(crate) fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

pub(crate) fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

pub(crate) fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// 这一行是不是「缩进 indent 的 key:」（键后面可以直接带值）。
pub(crate) fn is_key_at(line: &str, key: &str, indent: usize) -> bool {
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
pub(crate) fn is_flow_value(line: &str) -> bool {
    let Some((_, value)) = line.split_once(':') else {
        return false;
    };
    let value = value.trim();
    value.starts_with('{') || value.starts_with('[')
}

/// 列表项的名字：- rimesync_vselect / - "table_translator@custom_phrase"。
pub(crate) fn item_name(line: &str) -> String {
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

/// YAML 键的写法：双引号 + 反斜杠转义（反斜杠键写成 "\\"）。
pub(crate) fn yaml_key(key: &str) -> String {
    format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 简单标量的写法：普通标识符原样，其余用双引号包起来。
pub(crate) fn yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

pub(crate) struct Doc {
    pub(crate) lines: Vec<String>,
    pub(crate) eol: String,
    pub(crate) trailing_newline: bool,
    /// 本次是否真的动过行（用于区分 Unchanged / Skipped）。
    pub(crate) changed: bool,
}

impl Doc {
    pub(crate) fn parse(text: &str) -> Self {
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

    pub(crate) fn render(&self) -> String {
        let body = self.lines.join(&self.eol);
        if self.trailing_newline {
            format!("{body}{}", self.eol)
        } else {
            body
        }
    }

    /// 顶部标记；已有 # patched by rime-sync 就不再插（不追版本号）。
    pub(crate) fn ensure_marker(&mut self, version: &str) -> bool {
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

    pub(crate) fn find_block_from(
        &self,
        key: &str,
        indent: usize,
        from: usize,
    ) -> Option<(usize, usize)> {
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

    pub(crate) fn find_block(&self, key: &str, indent: usize) -> Option<(usize, usize)> {
        self.find_block_from(key, indent, 0)
    }

    pub(crate) fn item_lines(&self, block: (usize, usize)) -> Vec<usize> {
        (block.0 + 1..block.1)
            .filter(|&index| {
                let line = &self.lines[index];
                !is_comment(line) && line.trim_start().starts_with("- ")
            })
            .collect()
    }

    /// 列表项的统一缩进：取现有项；空列表时比键多 2 格。
    pub(crate) fn item_indent(&self, block: (usize, usize), items: &[usize]) -> usize {
        items
            .first()
            .map(|&index| indent_of(&self.lines[index]))
            .unwrap_or_else(|| indent_of(&self.lines[block.0]) + 2)
    }

    /// 列表是不是 flow 风格（processors: [a, b]）。这种没法按行插，直接放弃。
    pub(crate) fn is_flow_list(&self, key: &str, indent: usize) -> bool {
        let Some((start, _)) = self.find_block(key, indent) else {
            return false;
        };
        is_flow_value(&self.lines[start])
    }

    pub(crate) fn remove_item(&mut self, key: &str, indent: usize, item: &str) -> bool {
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

    pub(crate) fn insert_item_after(
        &mut self,
        key: &str,
        indent: usize,
        anchor: &str,
        item: &str,
    ) -> bool {
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

    pub(crate) fn insert_item_before(
        &mut self,
        key: &str,
        indent: usize,
        anchor: &str,
        item: &str,
    ) -> bool {
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

    pub(crate) fn insert_item_front(&mut self, key: &str, indent: usize, item: &str) -> bool {
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

    pub(crate) fn push_item(&mut self, key: &str, indent: usize, item: &str) -> bool {
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

    pub(crate) fn contains_item(&self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        self.item_lines(block)
            .into_iter()
            .any(|index| item_name(&self.lines[index]) == item)
    }
}

// ---------------------------------------------------------------------------
// 保行尾的行级编辑（共用工具）
// ---------------------------------------------------------------------------
//
// yaml_text::Doc 把整个文件的行尾统一成一种（CRLF 或 LF），适合「整份重写」
// 的场景；改**用户自己**的文件时不能用它 —— 真机（227）的 default.custom.yaml
// 是 BOM + 混合行尾（正文 LF、末尾 CRLF 空行），统一行尾等于改掉了用户文件里
// 不该动的字节。RawLines 逐行记住自己的行尾，改完再原样拼回去。
//
pub(crate) struct RawLines {
    /// 每一行的内容（不含行尾）。
    pub(crate) lines: Vec<String>,
    /// 每一行后面的行尾（"" = 文件末尾没有换行）。
    pub(crate) eols: Vec<String>,
}

impl RawLines {
    pub(crate) fn parse(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut eols = Vec::new();
        let mut rest = text;
        while let Some(index) = rest.find('\n') {
            let (line, tail) = rest.split_at(index);
            let tail = &tail[1..];
            let (line, eol) = match line.strip_suffix('\r') {
                Some(stripped) => (stripped.to_string(), "\r\n".to_string()),
                None => (line.to_string(), "\n".to_string()),
            };
            lines.push(line);
            eols.push(eol);
            rest = tail;
        }
        if !rest.is_empty() {
            lines.push(rest.to_string());
            eols.push(String::new());
        }
        Self { lines, eols }
    }

    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        for (index, line) in self.lines.iter().enumerate() {
            out.push_str(line);
            out.push_str(&self.eols[index]);
        }
        out
    }

    /// 在第 at 行之前插入一行（at 可以等于行数 = 末尾追加）。
    ///
    /// 行尾跟随被跟随的那一行；末尾追加且原文件没有末尾换行时，把末尾换行
    /// 补在原末行后面（风格跟随更前面的一行），新行自己不带行尾 ——
    /// 这样「文件末尾有没有换行」这个属性保持不变。
    pub(crate) fn insert(&mut self, at: usize, text: &str) {
        let count = self.lines.len();
        if at >= count {
            let last_eol = self.eols.last().cloned().unwrap_or_default();
            let eol = if !last_eol.is_empty() {
                last_eol
            } else {
                let fallback = self
                    .eols
                    .iter()
                    .rev()
                    .find(|value| !value.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "\n".to_string());
                if count > 0 {
                    self.eols[count - 1] = fallback;
                }
                String::new()
            };
            self.lines.push(text.to_string());
            self.eols.push(eol);
            return;
        }
        let eol = if at == 0 {
            self.eols[0].clone()
        } else {
            self.eols[at - 1].clone()
        };
        let eol = if eol.is_empty() {
            "\n".to_string()
        } else {
            eol
        };
        self.lines.insert(at, text.to_string());
        self.eols.insert(at, eol);
    }
    /// 只保留给定的行（其余丢掉），每行连同自己的行尾一起保留。
    pub(crate) fn select(&self, indices: &[usize]) -> RawLines {
        let mut lines = Vec::with_capacity(indices.len());
        let mut eols = Vec::with_capacity(indices.len());
        for &index in indices {
            lines.push(self.lines[index].clone());
            eols.push(self.eols[index].clone());
        }
        RawLines { lines, eols }
    }
}
