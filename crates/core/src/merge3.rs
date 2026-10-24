//! 文件级三向合并（基线 / 本地 / 远端）。
//!
//! PLAN 第 9 节选择的是整文件取舍而不是行级合并：Rime 的 YAML 配置
//! 行级合并后仍可能语义冲突，整文件取舍 + 冲突副本更可预测。

use std::collections::HashSet;

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

/// Rime 自定义配置文件（Weasel/ibus-rime 都会写 `customization:` 头）。
pub fn is_custom_yaml(rel: &str) -> bool {
    rel.ends_with(".custom.yaml")
}

/// *.custom.yaml 的规范化摘要：只对 `patch:` 段做确定性序列化后再哈希。
///
/// 小狼毫每次改设置都会重写文件头的 `customization:`（modified_time /
/// distribution_version），整文件 sha256 会把纯 metadata 变化当成内容变化，
/// 让 base/local/remote 三方永远不同 → 每轮都报冲突。这里只比较 patch 段。
///
/// 解析失败或没有 patch 映射时回退原始字节：拿不准就按老规则比较，
/// 绝不因为规范化失败而放过真实内容差异。
pub fn custom_yaml_digest(bytes: &[u8]) -> String {
    match normalize_custom_yaml(bytes) {
        Some(normalized) => sha256_hex(&normalized),
        None => sha256_hex(bytes),
    }
}

/// 只保留 `patch:` 段并做确定性序列化（映射键排序、递归），用于比较而非写盘。
///
/// 返回 None 表示「无法规范化」：解析失败、顶层不是映射、patch 不是映射。
pub fn normalize_custom_yaml(bytes: &[u8]) -> Option<Vec<u8>> {
    let doc = parse_yaml(bytes)?;
    let patch = patch_mapping(&doc)?;
    let canonical = canonical_value(&Value::Mapping(patch.clone()));
    serde_yaml::to_string(&canonical)
        .ok()
        .map(String::into_bytes)
}

/// 递归重建 Value：映射按键排序，序列保持原顺序（序列顺序有语义）。
fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Mapping(map) => {
            let mut entries: Vec<(String, &Value, Value)> = map
                .iter()
                .map(|(key, item)| (canonical_key(key), key, canonical_value(item)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = Mapping::new();
            for (_, key, item) in entries {
                sorted.insert(key.clone(), item);
            }
            Value::Mapping(sorted)
        }
        Value::Sequence(items) => Value::Sequence(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

/// 键的排序依据：带类型前缀，避免 `1`（数字）与 `"1"`（字符串）排成同一个位置。
fn canonical_key(key: &Value) -> String {
    match key {
        Value::String(text) => format!("s:{text}"),
        Value::Bool(flag) => format!("b:{flag}"),
        Value::Number(number) => format!("n:{number:?}"),
        Value::Null => "z:".to_string(),
        other => format!(
            "o:{}",
            serde_yaml::to_string(other).unwrap_or_default().trim()
        ),
    }
}

/// 只做集合合并的 patch 键：多台机器各自安装的输入方案不能互相冲掉。
const SCHEMA_LIST_KEY: &str = "schema_list";

/// YAML 段落级合并结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// 段落级（patch: key 级）合并成功，bytes 是写回本地的新内容。
    Patch {
        bytes: Vec<u8>,
        conflicts: Vec<String>,
        /// 非冲突的合并说明（如 schema_list 取并集），由调用方写进 Report。
        notes: Vec<String>,
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
    let mut notes = Vec::new();
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
        // schema_list 视为集合：两边都是合法列表就取并集，不看 base。
        // 一台机器新增的方案不能因为「另一边没动」而按单值取舍丢掉。
        let schema_union = schema_list_union(key, local_value, remote_value);
        if local_value == remote_value {
            if let Some(value) = local_value {
                merged.insert(key.clone(), value.clone());
            }
        } else if let Some((value, note)) = schema_union {
            merged.insert(key.clone(), value);
            notes.push(note);
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
        notes,
    })
}

/// schema_list 的集合合并入口。
///
/// 键名不匹配、缺一边、或任一边不是「元素都带非空字符串 schema 字段的序列」时
/// 返回 None，调用方回落到原来的三向冲突逻辑（绝不猜结构）。
fn schema_list_union(
    key: &Value,
    local: Option<&Value>,
    remote: Option<&Value>,
) -> Option<(Value, String)> {
    if !matches!(key, Value::String(name) if name == SCHEMA_LIST_KEY) {
        return None;
    }
    let (local, remote) = (local?, remote?);
    let local_items = valid_schema_list(local)?;
    let remote_items = valid_schema_list(remote)?;
    Some(union_schema_list(local_items, remote_items))
}

/// 合法输入方案列表：序列且每一项都能取出 schema 名。空序列也算合法。
fn valid_schema_list(value: &Value) -> Option<&[Value]> {
    let Value::Sequence(items) = value else {
        return None;
    };
    if items.iter().all(|item| schema_name(item).is_some()) {
        Some(items)
    } else {
        None
    }
}

/// 去重键：元素映射里的非空字符串 `schema` 字段。
fn schema_name(item: &Value) -> Option<&str> {
    let Value::Mapping(map) = item else {
        return None;
    };
    match map.get("schema") {
        Some(Value::String(name)) if !name.is_empty() => Some(name),
        _ => None,
    }
}

/// 并集：先本机顺序，再按远端顺序追加本机没有的方案。
///
/// 同一 schema 只保留第一次出现（本机优先），两侧的重复项都被折叠。
fn union_schema_list(local: &[Value], remote: &[Value]) -> (Value, String) {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut names = Vec::new();
    for item in local.iter().chain(remote.iter()) {
        let Some(name) = schema_name(item) else {
            continue;
        };
        if seen.insert(name.to_string()) {
            names.push(name.to_string());
            items.push(item.clone());
        }
    }
    let note = format!(
        "patch.{SCHEMA_LIST_KEY} merged as set union: local=[{}] remote=[{}] result=[{}]",
        schema_names(local),
        schema_names(remote),
        names.join(", "),
    );
    (Value::Sequence(items), note)
}

fn schema_names(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(schema_name)
        .collect::<Vec<_>>()
        .join(", ")
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
            MergeOutcome::Patch {
                bytes, conflicts, ..
            } => {
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
            MergeOutcome::Patch {
                bytes, conflicts, ..
            } => {
                assert_eq!(conflicts, vec!["patch.a".to_string()]);
                let text = String::from_utf8(bytes).expect("utf8");
                assert!(text.contains("a: 1"));
                assert!(text.contains("b: 3"));
            }
            MergeOutcome::Fallback { reason } => panic!("fallback: {reason}"),
        }
    }
}

#[cfg(test)]
mod schema_list_tests {
    use super::*;

    fn merge(
        base: Option<&[u8]>,
        local: &[u8],
        remote: &[u8],
    ) -> (String, Vec<String>, Vec<String>) {
        match merge_yaml_patch(base, local, Some(remote)).expect("merge") {
            MergeOutcome::Patch {
                bytes,
                conflicts,
                notes,
            } => (String::from_utf8(bytes).expect("utf8"), conflicts, notes),
            MergeOutcome::Fallback { reason } => panic!("unexpected fallback: {reason}"),
        }
    }

    /// 按文件里的顺序读出 schema 名，顺带验证结构合法。
    fn names(text: &str) -> Vec<String> {
        let doc: Value = serde_yaml::from_str(text).expect("yaml");
        let patch = patch_mapping(&doc).expect("patch");
        let list = patch.get("schema_list").expect("schema_list");
        valid_schema_list(list)
            .expect("valid schema list")
            .iter()
            .map(|item| schema_name(item).expect("schema name").to_string())
            .collect()
    }

    #[test]
    fn same_set_in_different_order_has_no_conflict() {
        let base = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let local = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n";
        let remote = b"patch:\n  schema_list:\n    - schema: wubi86\n    - schema: luna_pinyin\n";
        let (text, conflicts, notes) = merge(Some(base), local, remote);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        // 本机顺序优先。
        assert_eq!(names(&text), ["luna_pinyin", "wubi86"]);
        assert!(
            notes.iter().any(|note| note.contains("set union")),
            "{notes:?}"
        );
    }

    #[test]
    fn local_only_schema_is_preserved() {
        let base = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let local = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n";
        let remote = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let (text, conflicts, _) = merge(Some(base), local, remote);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(names(&text), ["luna_pinyin", "wubi86"]);
    }

    #[test]
    fn remote_only_schema_is_appended_in_remote_order() {
        let base = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let local = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let remote = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: cangjie\n";
        let (text, conflicts, _) = merge(Some(base), local, remote);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(names(&text), ["luna_pinyin", "cangjie"]);
    }

    #[test]
    fn list_versus_scalar_falls_back_to_conflict() {
        let base = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let local_list =
            b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n";
        let remote_scalar = b"patch:\n  schema_list: luna_pinyin\n";
        let (text, conflicts, notes) = merge(Some(base), local_list, remote_scalar);
        assert_eq!(conflicts, ["patch.schema_list"]);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(text.contains("wubi86"), "local value must win: {text}");

        let local_scalar = b"patch:\n  schema_list: luna_pinyin\n";
        let remote_list =
            b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: cangjie\n";
        let (text, conflicts, notes) = merge(Some(base), local_scalar, remote_list);
        assert_eq!(conflicts, ["patch.schema_list"]);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(text.contains("schema_list: luna_pinyin"), "{text}");
    }

    #[test]
    fn invalid_items_fall_back_to_conflict() {
        let base = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n";
        let remote = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: cangjie\n";
        // 元素不是映射：拿不到去重键。
        let local_string_item =
            b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - wubi86\n";
        let (_, conflicts, notes) = merge(Some(base), local_string_item, remote);
        assert_eq!(conflicts, ["patch.schema_list"]);
        assert!(notes.is_empty(), "{notes:?}");

        // 映射里没有 schema 字段。
        let local_no_field =
            b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - name: wubi86\n";
        let (_, conflicts, notes) = merge(Some(base), local_no_field, remote);
        assert_eq!(conflicts, ["patch.schema_list"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn empty_lists_are_valid_collections() {
        // 本机清空、远端新增 → 并集保留远端的方案。
        let (text, conflicts, _) = merge(
            Some(b"patch:\n  schema_list:\n    - schema: luna_pinyin\n"),
            b"patch:\n  schema_list: []\n",
            b"patch:\n  schema_list:\n    - schema: cangjie\n",
        );
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(names(&text), ["cangjie"]);

        // 远端清空不得抹掉本机方案。
        let (text, conflicts, _) = merge(
            None,
            b"patch:\n  schema_list:\n    - schema: luna_pinyin\n",
            b"patch:\n  schema_list: []\n",
        );
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(names(&text), ["luna_pinyin"]);

        // 两边都是空列表：合法，结果仍是空列表。
        let (text, conflicts, _) = merge(
            Some(b"patch:\n  schema_list:\n    - schema: luna_pinyin\n"),
            b"patch:\n  schema_list: []\n",
            b"patch:\n  schema_list: []\n",
        );
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert!(names(&text).is_empty(), "{text}");
        assert!(text.contains("schema_list: []"), "{text}");
    }

    #[test]
    fn duplicate_schemas_are_collapsed_local_first() {
        let local = b"patch:\n  schema_list:\n    - schema: luna_pinyin\n    - schema: luna_pinyin\n    - schema: wubi86\n";
        let remote = b"patch:\n  schema_list:\n    - schema: wubi86\n    - schema: cangjie\n";
        let (text, conflicts, _) = merge(None, local, remote);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(names(&text), ["luna_pinyin", "wubi86", "cangjie"]);
    }

    #[test]
    fn other_patch_keys_keep_three_way_rules() {
        let base = b"patch:\n  menu/page_size: 9\n  schema_list:\n    - schema: luna_pinyin\n";
        let local = b"patch:\n  menu/page_size: 10\n  schema_list:\n    - schema: luna_pinyin\n    - schema: wubi86\n";
        let remote = b"patch:\n  menu/page_size: 12\n  schema_list:\n    - schema: luna_pinyin\n    - schema: cangjie\n";
        let (text, conflicts, _) = merge(Some(base), local, remote);
        assert_eq!(conflicts, ["patch.menu/page_size"]);
        assert_eq!(names(&text), ["luna_pinyin", "wubi86", "cangjie"]);
        assert!(text.contains("menu/page_size: 10"), "{text}");
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::*;

    const HEADER_OLD: &str = concat!(
        "# Rime settings\n",
        "patch:\n",
        "  \"menu/page_size\": 9\n",
        "  style:\n",
        "    color_scheme: dark\n",
        "    font_point: 12\n",
        "customization:\n",
        "  distribution_code_name: Weasel\n",
        "  distribution_version: \"0.16.3\"\n",
        "  modified_time: \"Tue Oct 21 10:00:00 2025\"\n",
    );

    /// 同一份 patch，只有 customization 头不同（小狼毫每次改设置都会重写）。
    const HEADER_NEW: &str = concat!(
        "patch:\n",
        "  style:\n",
        "    font_point: 12\n",
        "    color_scheme: dark\n",
        "  \"menu/page_size\": 9\n",
        "customization:\n",
        "  distribution_code_name: Weasel\n",
        "  distribution_version: \"0.17.0\"\n",
        "  modified_time: \"Wed Oct 22 11:11:11 2025\"\n",
    );

    #[test]
    fn customization_header_is_ignored() {
        let old = HEADER_OLD.as_bytes();
        let new = HEADER_NEW.as_bytes();
        assert_ne!(
            sha256_hex(old),
            sha256_hex(new),
            "raw bytes must differ (that is the reported bug)"
        );
        assert_eq!(custom_yaml_digest(old), custom_yaml_digest(new));
    }

    #[test]
    fn key_order_does_not_change_the_digest() {
        let first = b"patch:\n  a: 1\n  b: 2\n";
        let second = b"patch:\n  b: 2\n  a: 1\n";
        assert_eq!(custom_yaml_digest(first), custom_yaml_digest(second));
    }

    #[test]
    fn real_patch_change_changes_the_digest() {
        let base = HEADER_OLD.as_bytes();
        let changed = HEADER_OLD.replace("menu/page_size\": 9", "menu/page_size\": 6");
        assert_ne!(
            custom_yaml_digest(base),
            custom_yaml_digest(changed.as_bytes())
        );
    }

    #[test]
    fn non_yaml_falls_back_to_raw_bytes() {
        let broken = b"\tbad: yaml";
        assert!(normalize_custom_yaml(broken).is_none());
        assert_eq!(custom_yaml_digest(broken), sha256_hex(broken));
    }

    #[test]
    fn missing_patch_mapping_falls_back_to_raw_bytes() {
        let no_patch = b"not_patch:\n  a: 1\n";
        assert!(normalize_custom_yaml(no_patch).is_none());
        assert_eq!(custom_yaml_digest(no_patch), sha256_hex(no_patch));

        let scalar_patch = b"patch: scalar\n";
        assert!(normalize_custom_yaml(scalar_patch).is_none());
        assert_eq!(custom_yaml_digest(scalar_patch), sha256_hex(scalar_patch));
    }

    #[test]
    fn non_mapping_document_falls_back_to_raw_bytes() {
        let sequence = b"- patch: 1\n";
        assert!(normalize_custom_yaml(sequence).is_none());
        assert_eq!(custom_yaml_digest(sequence), sha256_hex(sequence));
    }
}
