//! 文件级三向合并（基线 / 本地 / 远端）。
//!
//! PLAN 第 9 节选择的是整文件取舍而不是行级合并：Rime 的 YAML 配置
//! 行级合并后仍可能语义冲突，整文件取舍 + 冲突副本更可预测。

use anyhow::Result;
use serde_yaml::{Mapping, Value};

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
    merge3_digests(base.as_deref(), local.as_deref(), remote.as_deref())
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

/// YAML 段落级合并结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// 段落级（patch: key 级）合并成功，bytes 是写回本地的新内容。
    Patch {
        bytes: Vec<u8>,
        conflicts: Vec<String>,
    },
    /// 结构不符合预期 / 解析失败 → 调用方回落文件级合并，绝不中断。
    Fallback { reason: String },
}

/// 只用于 *.custom.yaml：对顶层 patch: 映射做 key 级三向合并。
///
/// 一侧相对基线未变的 key 直接取另一侧；两侧都变且不同 → 保留本地并记录冲突。
/// base / remote 为 None 表示该侧不存在（远端删除交给文件级处理）。
pub fn merge_yaml_patch(
    base: Option<&[u8]>,
    local: &[u8],
    remote: Option<&[u8]>,
) -> Result<MergeOutcome> {
    let local_doc = match parse_yaml(local) {
        Some(doc) => doc,
        None => {
            return Ok(MergeOutcome::Fallback {
                reason: "local file is not valid yaml".to_string(),
            })
        }
    };
    let Some(local_patch) = patch_mapping(&local_doc) else {
        return Ok(MergeOutcome::Fallback {
            reason: "local file has no patch mapping".to_string(),
        });
    };
    let Some(remote_bytes) = remote else {
        return Ok(MergeOutcome::Fallback {
            reason: "remote file is missing".to_string(),
        });
    };
    let remote_doc = match parse_yaml(remote_bytes) {
        Some(doc) => doc,
        None => {
            return Ok(MergeOutcome::Fallback {
                reason: "remote file is not valid yaml".to_string(),
            })
        }
    };
    let Some(remote_patch) = patch_mapping(&remote_doc) else {
        return Ok(MergeOutcome::Fallback {
            reason: "remote file has no patch mapping".to_string(),
        });
    };
    let base_patch = match base {
        Some(bytes) => match parse_yaml(bytes) {
            Some(doc) => match doc.get("patch") {
                None => Mapping::new(),
                Some(Value::Mapping(map)) => map.clone(),
                Some(_) => {
                    return Ok(MergeOutcome::Fallback {
                        reason: "base patch is not a mapping".to_string(),
                    })
                }
            },
            None => {
                return Ok(MergeOutcome::Fallback {
                    reason: "base file is not valid yaml".to_string(),
                })
            }
        },
        None => Mapping::new(),
    };

    let mut merged = Mapping::new();
    let mut conflicts = Vec::new();
    let mut keys: Vec<&Value> = local_patch.keys().collect();
    for key in remote_patch.keys() {
        if !local_patch.contains_key(key) {
            keys.push(key);
        }
    }
    for key in keys {
        let local_value = local_patch.get(key);
        let remote_value = remote_patch.get(key);
        let base_value = base_patch.get(key);
        if local_value == remote_value {
            if let Some(value) = local_value {
                merged.insert(key.clone(), value.clone());
            }
        } else if local_value == base_value {
            // 本地没动，跟远端（None 表示远端删除该 key）。
            if let Some(value) = remote_value {
                merged.insert(key.clone(), value.clone());
            }
        } else if remote_value == base_value {
            if let Some(value) = local_value {
                merged.insert(key.clone(), value.clone());
            }
        } else {
            // 三方差分冲突：保留本地，交给引擎生成 conflict 副本。
            conflicts.push(format!("patch.{}", key_label(key)));
            if let Some(value) = local_value {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    let mut doc = local_doc;
    if let Value::Mapping(map) = &mut doc {
        map.insert(Value::String("patch".to_string()), Value::Mapping(merged));
    } else {
        return Ok(MergeOutcome::Fallback {
            reason: "local document is not a mapping".to_string(),
        });
    }
    let text = match serde_yaml::to_string(&doc) {
        Ok(text) => text,
        Err(error) => {
            return Ok(MergeOutcome::Fallback {
                reason: format!("serialize merged yaml: {error}"),
            })
        }
    };
    Ok(MergeOutcome::Patch {
        bytes: text.into_bytes(),
        conflicts,
    })
}

fn parse_yaml(bytes: &[u8]) -> Option<Value> {
    serde_yaml::from_slice::<Value>(bytes).ok()
}

fn patch_mapping(doc: &Value) -> Option<&Mapping> {
    match doc.get("patch") {
        Some(Value::Mapping(map)) => Some(map),
        _ => None,
    }
}

fn key_label(key: &Value) -> String {
    match key {
        Value::String(text) => text.clone(),
        other => serde_yaml::to_string(other)
            .map(|text| text.trim().to_string())
            .unwrap_or_default(),
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
        assert_eq!(
            merge3(None, Some(LOCAL), Some(REMOTE)),
            MergeResult::Conflict
        );
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
#[cfg(test)]
mod patch_tests {
    use super::*;

    const BASE: &[u8] = b"patch:\n  a: 1\n  b: 2\n  c: 3\nother:\n  keep: me\n";
    const LOCAL: &[u8] = b"patch:\n  a: 10\n  b: 2\n  c: 3\nother:\n  keep: me\n";
    const REMOTE: &[u8] = b"patch:\n  a: 1\n  b: 20\n  c: 3\n  d: 4\nother:\n  changed: true\n";

    fn merged_text(outcome: MergeOutcome) -> String {
        match outcome {
            MergeOutcome::Patch { bytes, .. } => String::from_utf8(bytes).expect("utf8"),
            MergeOutcome::Fallback { reason } => panic!("unexpected fallback: {reason}"),
        }
    }

    #[test]
    fn disjoint_patch_keys_merge_without_conflict() {
        let outcome = merge_yaml_patch(Some(BASE), LOCAL, Some(REMOTE)).expect("merge");
        match &outcome {
            MergeOutcome::Patch { conflicts, .. } => assert!(conflicts.is_empty()),
            MergeOutcome::Fallback { reason } => panic!("fallback: {reason}"),
        }
        let text = merged_text(outcome);
        assert!(text.contains("a: 10"), "{text}");
        assert!(text.contains("b: 20"), "{text}");
        assert!(text.contains("d: 4"), "{text}");
        // patch 之外的段落保持本地内容。
        assert!(text.contains("keep: me"), "{text}");
        assert!(!text.contains("changed: true"), "{text}");
    }

    #[test]
    fn same_key_conflict_keeps_local_and_reports() {
        let base = b"patch:\n  a: 1\n";
        let local = b"patch:\n  a: 2\n";
        let remote = b"patch:\n  a: 3\n";
        let outcome = merge_yaml_patch(Some(base), local, Some(remote)).expect("merge");
        match outcome {
            MergeOutcome::Patch { bytes, conflicts } => {
                assert_eq!(conflicts, vec!["patch.a".to_string()]);
                let text = String::from_utf8(bytes).expect("utf8");
                assert!(text.contains("a: 2"), "{text}");
            }
            MergeOutcome::Fallback { reason } => panic!("fallback: {reason}"),
        }
    }

    #[test]
    fn invalid_yaml_falls_back_instead_of_failing() {
        let outcome =
            merge_yaml_patch(Some(BASE), b"patch: [unclosed", Some(REMOTE)).expect("no error");
        assert!(matches!(outcome, MergeOutcome::Fallback { .. }));
        let outcome = merge_yaml_patch(Some(BASE), b"patch:\n  a: 1\n", Some(b"\tbad: yaml"))
            .expect("no error");
        assert!(matches!(outcome, MergeOutcome::Fallback { .. }));
        let outcome = merge_yaml_patch(Some(BASE), b"patch:\n  a: 1\n", None).expect("no error");
        assert!(matches!(outcome, MergeOutcome::Fallback { .. }));
    }

    #[test]
    fn missing_patch_mapping_falls_back() {
        let outcome = merge_yaml_patch(None, b"not_patch:\n  a: 1\n", Some(b"patch:\n  a: 2\n"))
            .expect("no error");
        match outcome {
            MergeOutcome::Fallback { reason } => assert!(reason.contains("patch")),
            MergeOutcome::Patch { .. } => panic!("should fall back"),
        }
        let outcome = merge_yaml_patch(None, b"patch: scalar\n", Some(b"patch:\n  a: 2\n"))
            .expect("no error");
        assert!(matches!(outcome, MergeOutcome::Fallback { .. }));
    }

    #[test]
    fn remote_deleted_key_is_removed_when_local_untouched() {
        let base = b"patch:\n  a: 1\n  b: 2\n";
        let local = b"patch:\n  a: 1\n  b: 2\n";
        let remote = b"patch:\n  a: 1\n";
        let outcome = merge_yaml_patch(Some(base), local, Some(remote)).expect("merge");
        let text = merged_text(outcome);
        assert!(text.contains("a: 1"));
        assert!(!text.contains("b:"), "{text}");
    }

    #[test]
    fn local_deleted_key_wins_when_remote_untouched() {
        let base = b"patch:\n  a: 1\n  b: 2\n";
        let local = b"patch:\n  a: 1\n";
        let remote = b"patch:\n  a: 1\n  b: 2\n";
        let outcome = merge_yaml_patch(Some(base), local, Some(remote)).expect("merge");
        let text = merged_text(outcome);
        assert!(!text.contains("b:"), "{text}");
    }

    #[test]
    fn absent_base_is_conservative() {
        let local = b"patch:\n  a: 1\n";
        let remote = b"patch:\n  a: 2\n  b: 3\n";
        let outcome = merge_yaml_patch(None, local, Some(remote)).expect("merge");
        match outcome {
            MergeOutcome::Patch { bytes, conflicts } => {
                assert_eq!(conflicts, vec!["patch.a".to_string()]);
                let text = String::from_utf8(bytes).expect("utf8");
                assert!(text.contains("a: 1"));
                assert!(text.contains("b: 3"));
            }
            MergeOutcome::Fallback { reason } => panic!("fallback: {reason}"),
        }
    }
}
