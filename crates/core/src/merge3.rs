//! 文件级三向合并（基线 / 本地 / 远端）。
//!
//! PLAN 第 9 节选择的是整文件取舍而不是行级合并：Rime 的 YAML 配置
//! 行级合并后仍可能语义冲突，整文件取舍 + 冲突副本更可预测。

use crate::index::sha256_hex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeResult {
    /// 三方一致，无需动作。
    Unchanged,
    /// 只有本地相对基线发生变化。
    TakeLocal,
    /// 只有远端相对基线发生变化。
    TakeRemote,
    /// 两端都变且不一致。
    Conflict,
    /// 远端删除，本地未改，应删除本地。
    DeleteLocal,
    /// 本地删除，远端未改，应传播删除。
    LocalDeleted,
    /// 远端删除但本地改过，保留本地并告警。
    RemoteDeletedLocalModified,
}

pub fn merge3(base: Option<&[u8]>, local: Option<&[u8]>, remote: Option<&[u8]>) -> MergeResult {
    let base = base.map(sha256_hex);
    let local = local.map(sha256_hex);
    let remote = remote.map(sha256_hex);
    merge3_digests(
        base.as_deref(),
        local.as_deref(),
        remote.as_deref(),
    )
}

/// 引擎只持有哈希基线，因此决策也接受摘要而非内容。
pub fn merge3_digests(
    base: Option<&str>,
    local: Option<&str>,
    remote: Option<&str>,
) -> MergeResult {
    match (base, local, remote) {
        (None, None, None) => MergeResult::Unchanged,
        (None, None, Some(_)) => MergeResult::TakeRemote,
        (None, Some(_), None) => MergeResult::TakeLocal,
        (None, Some(l), Some(r)) => {
            if l == r {
                MergeResult::Unchanged
            } else {
                MergeResult::Conflict
            }
        }
        (Some(_), None, None) => MergeResult::Unchanged,
        (Some(b), None, Some(r)) => {
            if b == r {
                MergeResult::LocalDeleted
            } else {
                MergeResult::Conflict
            }
        }
        (Some(b), Some(l), None) => {
            if l == b {
                MergeResult::DeleteLocal
            } else {
                MergeResult::RemoteDeletedLocalModified
            }
        }
        (Some(b), Some(l), Some(r)) => {
            if l == r {
                MergeResult::Unchanged
            } else if l == b {
                MergeResult::TakeRemote
            } else if r == b {
                MergeResult::TakeLocal
            } else {
                MergeResult::Conflict
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &[u8] = b"base";
    const LOCAL: &[u8] = b"local";
    const REMOTE: &[u8] = b"remote";

    #[test]
    fn only_local_changed() {
        assert_eq!(
            merge3(Some(BASE), Some(LOCAL), Some(BASE)),
            MergeResult::TakeLocal
        );
    }

    #[test]
    fn only_remote_changed() {
        assert_eq!(
            merge3(Some(BASE), Some(BASE), Some(REMOTE)),
            MergeResult::TakeRemote
        );
    }

    #[test]
    fn both_made_same_change() {
        assert_eq!(
            merge3(Some(BASE), Some(LOCAL), Some(LOCAL)),
            MergeResult::Unchanged
        );
    }

    #[test]
    fn both_changed_differently_is_conflict() {
        assert_eq!(
            merge3(Some(BASE), Some(LOCAL), Some(REMOTE)),
            MergeResult::Conflict
        );
    }

    #[test]
    fn remote_deleted_local_unchanged() {
        assert_eq!(
            merge3(Some(BASE), Some(BASE), None),
            MergeResult::DeleteLocal
        );
    }

    #[test]
    fn remote_deleted_local_modified_keeps_local() {
        assert_eq!(
            merge3(Some(BASE), Some(LOCAL), None),
            MergeResult::RemoteDeletedLocalModified
        );
    }

    #[test]
    fn local_deleted_remote_unchanged() {
        assert_eq!(
            merge3(Some(BASE), None, Some(BASE)),
            MergeResult::LocalDeleted
        );
    }

    #[test]
    fn both_deleted_is_unchanged() {
        assert_eq!(merge3(Some(BASE), None, None), MergeResult::Unchanged);
    }

    #[test]
    fn both_created_differently_is_conflict() {
        assert_eq!(merge3(None, Some(LOCAL), Some(REMOTE)), MergeResult::Conflict);
    }

    #[test]
    fn digests_match_bytes_variant() {
        let b = sha256_hex(BASE);
        let l = sha256_hex(LOCAL);
        let r = sha256_hex(REMOTE);
        assert_eq!(
            merge3_digests(Some(&b), Some(&l), Some(&r)),
            MergeResult::Conflict
        );
    }
}
