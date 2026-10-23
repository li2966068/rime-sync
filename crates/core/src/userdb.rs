//! *.userdb.txt 词条级冲突统计：只统计，绝不自动合并。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 单侧快照最多取 5 条样本。
pub const SAMPLE_LIMIT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserdbStats {
    pub only_local: usize,
    pub only_remote: usize,
    pub both_changed: usize,
    pub sample: Vec<String>,
}

/// tab 分隔的词条：第一列是词条 key，整行内容用于判断两边是否都改过。
fn parse(bytes: &[u8]) -> Option<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut entries = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, _rest)) = line.split_once('\t') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        entries.insert(key.to_string(), line.to_string());
    }
    if entries.is_empty() && !text.trim().is_empty() {
        // 非空但一条 tab 词条都没有：不能可靠统计 → 降级为 None。
        return None;
    }
    Some(entries)
}

/// 统计两侧词条差异；任何解析失败返回 None（调用方降级为 null，不影响主流程）。
pub fn diff_stats(local: &[u8], remote: &[u8]) -> Option<UserdbStats> {
    let local_entries = parse(local)?;
    let remote_entries = parse(remote)?;
    let mut stats = UserdbStats {
        only_local: 0,
        only_remote: 0,
        both_changed: 0,
        sample: Vec::new(),
    };
    for (key, line) in &local_entries {
        match remote_entries.get(key) {
            None => {
                stats.only_local += 1;
                push_sample(&mut stats.sample, format!("-{key}"));
            }
            Some(other) if other != line => {
                stats.both_changed += 1;
                push_sample(&mut stats.sample, format!("~{key}"));
            }
            Some(_) => {}
        }
    }
    for key in remote_entries.keys() {
        if !local_entries.contains_key(key) {
            stats.only_remote += 1;
            push_sample(&mut stats.sample, format!("+{key}"));
        }
    }
    Some(stats)
}

/// 一次 userdb 导出文本里的词条数（非空、非注释行）；非 UTF-8 返回 None。
pub fn entry_count(bytes: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(bytes).ok()?;
    Some(
        text.lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .count(),
    )
}

fn push_sample(sample: &mut Vec<String>, item: String) {
    if sample.len() < SAMPLE_LIMIT {
        sample.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_three_kinds_of_differences() {
        let local = b"a\t1\nb\t2\nc\t3\n";
        let remote = b"c\t9\nd\t4\n";
        let stats = diff_stats(local, remote).expect("stats");
        assert_eq!(stats.only_local, 2);
        assert_eq!(stats.only_remote, 1);
        assert_eq!(stats.both_changed, 1);
        assert_eq!(stats.sample.len(), 4);
        assert!(!stats.sample.iter().any(|item| item.len() > 16));
    }

    #[test]
    fn identical_sides_have_zero_conflicts_and_sample_is_capped() {
        let local = b"a\t1\nb\t2\n";
        let stats = diff_stats(local, local).expect("stats");
        assert_eq!(
            stats,
            UserdbStats {
                only_local: 0,
                only_remote: 0,
                both_changed: 0,
                sample: vec![]
            }
        );

        let local: Vec<u8> = (0..9)
            .map(|index| format!("k{index}\t1\n"))
            .collect::<String>()
            .into_bytes();
        let stats = diff_stats(&local, b"z\t9\n").expect("stats");
        assert_eq!(stats.only_local, 9);
        assert_eq!(stats.sample.len(), SAMPLE_LIMIT);
    }

    #[test]
    fn degrades_to_none_on_unparseable_input() {
        assert!(diff_stats(b"not-a-tab-line\n", b"a\t1\n").is_none());
        assert!(diff_stats(&[0xff, 0xfe], b"a\t1\n").is_none());
        // 空快照是合法的：0 冲突。
        let stats = diff_stats(b"", b"").expect("empty");
        assert_eq!(stats.only_local + stats.only_remote + stats.both_changed, 0);
    }

    #[test]
    fn entry_count_skips_blank_and_comment_lines() {
        assert_eq!(entry_count(b""), Some(0));
        assert_eq!(entry_count(b"# comment\n\na\t1\nb\t2\n"), Some(2));
        assert_eq!(entry_count(&[0xff, 0xfe]), None);
    }
}
