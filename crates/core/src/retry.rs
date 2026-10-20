//! 失败退避：1 → 2 → 4 → 8 → 16 → 30 分钟，上限 30；成功即复位。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BACKOFF_MINUTES: [u64; 6] = [1, 2, 4, 8, 16, 30];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryState {
    #[serde(default)]
    pub failures: u32,
    /// 下一次允许尝试的 Unix 秒；成功后清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<u64>,
}

impl RetryState {
    /// 第 n 次连续失败（1-based）对应的等待分钟数。
    pub fn delay_minutes(&self) -> u64 {
        match self.failures {
            0 => 0,
            n => BACKOFF_MINUTES[(n as usize - 1).min(BACKOFF_MINUTES.len() - 1)],
        }
    }

    pub fn record_failure(&mut self, now: u64) -> u64 {
        self.failures = self.failures.saturating_add(1);
        let next = now.saturating_add(self.delay_minutes() * 60);
        self.next_retry_at = Some(next);
        next
    }

    pub fn record_success(&mut self) {
        self.failures = 0;
        self.next_retry_at = None;
    }

    /// 距离下次重试还有多少秒（已到点则 0）。
    pub fn remaining_secs(&self, now: u64) -> u64 {
        self.next_retry_at
            .map(|next| next.saturating_sub(now))
            .unwrap_or(0)
    }

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, format!("{text}\n"))
            .with_context(|| format!("write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_sequence_1_2_4_8_16_30_and_caps() {
        let mut state = RetryState::default();
        let mut now = 1_000_000u64;
        let mut waits = Vec::new();
        for _ in 0..7 {
            let next = state.record_failure(now);
            waits.push((next - now) / 60);
            now = next;
        }
        assert_eq!(waits, vec![1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn success_resets_backoff() {
        let mut state = RetryState::default();
        state.record_failure(100);
        state.record_failure(160);
        assert_eq!(state.failures, 2);
        assert!(state.next_retry_at.is_some());
        state.record_success();
        assert_eq!(state.failures, 0);
        assert_eq!(state.next_retry_at, None);
        assert_eq!(state.delay_minutes(), 0);
        assert_eq!(state.remaining_secs(0), 0);
    }

    #[test]
    fn retry_state_roundtrips_on_disk() {
        let dir = std::env::temp_dir().join(format!("rime-sync-retry-{}", std::process::id()));
        let path = dir.join("retry.json");
        let mut state = RetryState::default();
        let next = state.record_failure(42);
        state.save(&path).expect("save");
        let loaded = RetryState::load(&path).expect("load");
        assert_eq!(loaded.failures, 1);
        assert_eq!(loaded.next_retry_at, Some(next));
        assert_eq!(loaded.remaining_secs(next), 0);
        assert_eq!(
            RetryState::load(&dir.join("missing.json"))
                .expect("missing")
                .failures,
            0
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
