//! 同步历史：<state_dir>/history.jsonl，每次 transport/apply 追加一条，最多 500 条。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub ts: u64,
    pub mode: String,
    pub ok: bool,
    pub pulled: usize,
    pub pushed: usize,
    pub merged: usize,
    pub conflicts: usize,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HistoryEntry {
    pub fn new(ts: u64, mode: &str) -> Self {
        Self {
            ts,
            mode: mode.to_string(),
            ok: true,
            pulled: 0,
            pushed: 0,
            merged: 0,
            conflicts: 0,
            duration_ms: 0,
            backup_id: None,
            error: None,
        }
    }
}

/// 追加一条并按行裁剪最旧记录（超过 limit 时）。写盘使用 tmp + rename。
pub fn append(path: &Path, entry: &HistoryEntry) -> Result<()> {
    append_with_limit(path, entry, HISTORY_LIMIT)
}

pub fn append_with_limit(path: &Path, entry: &HistoryEntry, limit: usize) -> Result<()> {
    let limit = limit.max(1);
    let mut lines = read_lines(path);
    lines.push(serde_json::to_string(entry).context("serialize history entry")?);
    if lines.len() > limit {
        let drop = lines.len() - limit;
        lines.drain(0..drop);
    }
    write_lines(path, &lines)
}

/// 读取最近的 limit 条（时间正序）；损坏的行直接跳过。
pub fn read(path: &Path, limit: usize) -> Result<Vec<HistoryEntry>> {
    let mut entries: Vec<HistoryEntry> = read_lines(path)
        .into_iter()
        .filter_map(|line| serde_json::from_str::<HistoryEntry>(&line).ok())
        .collect();
    if limit > 0 && entries.len() > limit {
        entries.drain(0..entries.len() - limit);
    }
    Ok(entries)
}

fn read_lines(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn write_lines(path: &Path, lines: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
    }
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-history-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("history.jsonl");
        (dir, path)
    }

    #[test]
    fn trims_to_five_hundred_entries() {
        let (dir, path) = temp_path("trim");
        for index in 0..505u64 {
            let mut entry = HistoryEntry::new(index, "transport");
            entry.pulled = index as usize;
            append(&path, &entry).expect("append");
        }
        let text = fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), HISTORY_LIMIT);
        let first: HistoryEntry = serde_json::from_str(lines[0]).expect("first");
        assert_eq!(first.ts, 5);
        let last: HistoryEntry = serde_json::from_str(lines[lines.len() - 1]).expect("last");
        assert_eq!(last.ts, 504);
        let recent = read(&path, 3).expect("read recent");
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[2].ts, 504);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_skips_corrupt_lines_and_missing_file() {
        let (dir, path) = temp_path("corrupt");
        assert!(read(&path, 10).expect("missing").is_empty());
        let mut entry = HistoryEntry::new(7, "apply");
        entry.ok = false;
        entry.error = Some("boom".into());
        append(&path, &entry).expect("append");
        let mut text = fs::read_to_string(&path).expect("read");
        text.push_str("not-json\n");
        fs::write(&path, text).expect("write");
        let entries = read(&path, 10).expect("read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].error.as_deref(), Some("boom"));
        fs::remove_dir_all(&dir).ok();
    }
}
