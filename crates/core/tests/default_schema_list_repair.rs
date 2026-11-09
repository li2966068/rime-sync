//! B1：用户目录 default.custom.yaml 的 schema_list 增量补齐。
//!
//! 背景：升级用户的用户目录里 default.custom.yaml 可能长期只列一两个方案
//! （真机 227 只有 luna_pinyin），而 librime 只按 schema_list 生成 build ——
//! 一旦全量重新部署，保留方案的产物不再生成，用户的方案「凭空消失」。
//!
//! 覆盖：
//! * 227 风格文件（只有 luna_pinyin）→ 补齐三个保留方案，原有条目与顺序不动；
//! * 用户自己加的方案（stroke）→ 保留、顺序不变；
//! * 已经齐全 → 逐字节不变（幂等）；
//! * 文件不存在 → 不创建；
//! * 文件里没有 schema_list → 跳过且不破坏文件；
//! * 缺 numpad 的**方案** custom.yaml 不受影响（本函数只碰 default.custom.yaml）；
//! * BOM / 混合行尾 / 末尾换行保持；备份只建一次；data 门禁（allowed）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_core::managed_custom::{
    repair_default_schema_list_file, repair_default_schema_list_in, RepairOutcome,
};
use rime_sync_core::prune::retained_schemas;
use serde_yaml::Value;

/// 227 真机上的 default.custom.yaml（55 字节：BOM + 正文 LF + 末尾 CRLF）。
const REAL227_DEFAULT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real227_default.custom.yaml"
);
/// 227 真机上的方案 custom.yaml（缺陷三现场：缺 numpad）。
const REAL227_SCHEMA_CUSTOM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/real227_luna_pinyin_simp.custom.yaml"
);

/// 最小临时目录（Drop 时清掉）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{tag}-{stamp}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write(path: &Path, body: &[u8]) {
    fs::write(path, body).expect("write fixture");
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).expect("read file")
}

/// patch.schema_list 里的方案 id（按文件顺序）。
fn schema_ids(text: &str) -> Vec<String> {
    let value: Value = serde_yaml::from_str(text).expect("valid yaml");
    value
        .get("patch")
        .and_then(Value::as_mapping)
        .and_then(|mapping| mapping.get("schema_list"))
        .and_then(Value::as_sequence)
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    item.get("schema")
                        .and_then(Value::as_str)
                        .or_else(|| item.as_str())
                        .map(|id| id.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ids_of(path: &Path) -> Vec<String> {
    schema_ids(&String::from_utf8(read(path)).expect("utf-8"))
}

/// 227 风格：用户目录里只有 luna_pinyin。
fn write_227_style(dir: &Path) -> PathBuf {
    let path = dir.join("default.custom.yaml");
    write(&path, &read(Path::new(REAL227_DEFAULT)));
    path
}

/// 真机文件补齐后的**精确**期望字节（BOM + LF 正文 + 末尾 CRLF 空行）。
fn expected_227_repaired() -> Vec<u8> {
    let mut out = Vec::from("\u{feff}patch:\n  schema_list:\n");
    out.extend_from_slice(b"    - {schema: luna_pinyin}\n");
    for id in retained_schemas() {
        out.extend_from_slice(format!("    - {{schema: {id}}}\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out
}

#[test]
fn real227_file_gets_retained_schemas_appended() {
    let dir = TempDir::new("schema-list-227");
    let path = write_227_style(dir.path());
    let before = read(&path);

    let outcome = repair_default_schema_list_file(&path, None).expect("repair");
    let RepairOutcome::Repaired { backup } = outcome else {
        panic!("应当补齐，实际 {outcome:?}");
    };
    assert_eq!(
        backup,
        Some(dir.path().join("default.custom.yaml.rimesync-orig"))
    );

    // 精确字节：BOM 保留、正文 LF 保留、末尾 CRLF 空行保留。
    assert_eq!(read(&path), expected_227_repaired());
    // 备份是改之前的原样。
    assert_eq!(read(&backup.expect("backup")), before);

    // 原有条目仍在最前，缺失的按保留清单顺序追加。
    let ids = ids_of(&path);
    assert_eq!(ids[0], "luna_pinyin");
    assert_eq!(&ids[1..], &retained_schemas().to_vec());
}

#[test]
fn user_own_schema_is_kept_and_order_is_stable() {
    let dir = TempDir::new("schema-list-user");
    let path = dir.path().join("default.custom.yaml");
    write(
        &path,
        "patch:\n  \"menu/page_size\": 8\n  schema_list:\n    - {schema: stroke}\n    - {schema: luna_pinyin}\n  punctuator:\n    \"、\": \"、\"\n"
            .as_bytes(),
    );

    let outcome = repair_default_schema_list_file(&path, None).expect("repair");
    assert!(matches!(outcome, RepairOutcome::Repaired { .. }));

    let ids = ids_of(&path);
    assert_eq!(ids[0], "stroke");
    assert_eq!(ids[1], "luna_pinyin");
    assert_eq!(&ids[2..], &retained_schemas().to_vec());

    // 其它内容逐字节不变。
    let text = String::from_utf8(read(&path)).expect("utf-8");
    assert!(text.contains("  \"menu/page_size\": 8\n"));
    assert!(text.ends_with("  punctuator:\n    \"、\": \"、\"\n"));
}

#[test]
fn complete_file_is_untouched_byte_for_byte() {
    let dir = TempDir::new("schema-list-idem");
    let path = write_227_style(dir.path());
    // 先补齐一次。
    repair_default_schema_list_file(&path, None).expect("first repair");
    let after_first = read(&path);

    // 幂等：第二次什么都不做。
    let outcome = repair_default_schema_list_file(&path, None).expect("second repair");
    assert_eq!(outcome, RepairOutcome::Unchanged);
    assert_eq!(read(&path), after_first);
}

#[test]
fn missing_file_is_not_created() {
    let dir = TempDir::new("schema-list-missing");
    let path = dir.path().join("default.custom.yaml");
    let outcome = repair_default_schema_list_in(dir.path(), None).expect("repair");
    assert_eq!(outcome, RepairOutcome::Skipped("missing".to_string()));
    assert!(!path.exists());
}

#[test]
fn file_without_schema_list_is_skipped_untouched() {
    let dir = TempDir::new("schema-list-none");
    let path = dir.path().join("default.custom.yaml");
    let body = b"patch:\n  \"menu/page_size\": 8\n";
    write(&path, body);

    let outcome = repair_default_schema_list_file(&path, None).expect("repair");
    assert_eq!(
        outcome,
        RepairOutcome::Skipped("no-schema-list".to_string())
    );
    assert_eq!(read(&path), body);
}

#[test]
fn empty_schema_list_is_skipped() {
    let dir = TempDir::new("schema-list-empty");
    let path = dir.path().join("default.custom.yaml");
    let body = b"patch:\n  schema_list: []\n";
    write(&path, body);

    let outcome = repair_default_schema_list_file(&path, None).expect("repair");
    assert_eq!(
        outcome,
        RepairOutcome::Skipped("empty-schema-list".to_string())
    );
    assert_eq!(read(&path), body);
}

#[test]
fn schema_custom_with_missing_numpad_is_not_touched() {
    let dir = TempDir::new("schema-list-numpad");
    let path = write_227_style(dir.path());
    // 方案 custom.yaml（缺陷三现场）：本函数不许碰它。
    let custom = dir.path().join("luna_pinyin_simp.custom.yaml");
    let custom_body = read(Path::new(REAL227_SCHEMA_CUSTOM));
    write(&custom, &custom_body);

    repair_default_schema_list_file(&path, None).expect("repair");
    assert_eq!(read(&custom), custom_body, "方案 custom.yaml 不能被改");
}

#[test]
fn existing_backup_is_not_overwritten() {
    let dir = TempDir::new("schema-list-backup");
    let path = write_227_style(dir.path());
    let backup = dir.path().join("default.custom.yaml.rimesync-orig");
    write(&backup, "我是旧备份\n".as_bytes());

    let outcome = repair_default_schema_list_file(&path, None).expect("repair");
    let RepairOutcome::Repaired { backup: created } = outcome else {
        panic!("应当补齐，实际 {outcome:?}");
    };
    assert_eq!(created, None, "备份已存在就不该再报新建");
    assert_eq!(read(&backup), "我是旧备份\n".as_bytes());
}

#[test]
fn allowed_gate_limits_what_gets_appended() {
    let dir = TempDir::new("schema-list-gate");
    let path = write_227_style(dir.path());
    let allowed = vec!["wubi86".to_string()];

    repair_default_schema_list_file(&path, Some(&allowed)).expect("repair");
    let ids = ids_of(&path);
    assert_eq!(ids[0], "luna_pinyin");
    assert_eq!(ids[1], "wubi86");
    assert_eq!(ids.len(), 2, "门禁外的方案不许补");
}

/// ensure 层面的接线：模板播种之后、裁剪之后，用户的 default.custom.yaml
/// 会被**增量补齐**保留方案（Windows / Linux 同一段代码）。
#[test]
fn ensure_appends_retained_schemas_to_user_default_custom() {
    let dir = TempDir::new("schema-list-ensure");
    let rime = dir.path().join("rime");
    fs::create_dir_all(&rime).expect("create rime dir");
    write(
        &rime.join("default.custom.yaml"),
        &read(Path::new(REAL227_DEFAULT)),
    );

    // 随包 data 里真的有这三个保留方案（Windows: Weasel 安装目录；
    // Linux: /usr/share/rime-data）。
    let data = dir.path().join("data");
    fs::create_dir_all(&data).expect("create data dir");
    for id in retained_schemas() {
        write(&data.join(format!("{id}.schema.yaml")), b"name: x\n");
    }

    let opts = rime_sync_core::ime_state::Options::new(rime_sync_core::ime_state::Mode::Fix)
        .with_install_ime(false)
        .with_patch_data(false)
        .with_auto_data_patch(false)
        .with_deploy(false)
        .with_prune(false)
        .with_seed_templates(false)
        .with_always_config(false);
    let outcome = rime_sync_core::ime_state::ensure(&rime, None, &[data], &opts);

    let action = outcome
        .actions
        .iter()
        .find(|action| action.kind == "schema-list")
        .expect("ensure 应当有 schema-list 动作");
    assert!(action.changed, "应当补齐：{action:?}");

    let ids = ids_of(&rime.join("default.custom.yaml"));
    assert_eq!(ids[0], "luna_pinyin");
    assert_eq!(&ids[1..], &retained_schemas().to_vec());
}

/// ensure 的完整流程（seed → 补齐 → prune → assess）跑在真机夹具上：
/// 最终 schema_list **恰好是三个保留方案**（顺序 = 保留清单顺序），
/// 且 BOM、末尾 CRLF 空行、正文行尾全部原样保留。
#[test]
fn ensure_then_prune_leaves_exactly_the_three_retained_schemas() {
    let dir = TempDir::new("schema-list-order");
    let rime = dir.path().join("rime");
    fs::create_dir_all(&rime).expect("create rime dir");
    let target = rime.join("default.custom.yaml");
    write(&target, &read(Path::new(REAL227_DEFAULT)));

    // 随包 data 里真的有这三个保留方案（Windows: Weasel 安装目录；
    // Linux: /usr/share/rime-data）。
    let data = dir.path().join("data");
    fs::create_dir_all(&data).expect("create data dir");
    for id in retained_schemas() {
        write(&data.join(format!("{id}.schema.yaml")), b"name: x\n");
    }

    let opts = rime_sync_core::ime_state::Options::new(rime_sync_core::ime_state::Mode::Fix)
        .with_install_ime(false)
        .with_patch_data(false)
        .with_auto_data_patch(false)
        .with_deploy(false)
        .with_prune(true)
        .with_seed_templates(false)
        .with_always_config(false);
    rime_sync_core::ime_state::ensure(&rime, None, &[data], &opts);

    // 恰好三个保留方案，顺序与保留清单一致（luna_pinyin 被 prune 裁掉）。
    let ids = ids_of(&target);
    assert_eq!(ids, retained_schemas().to_vec());

    // 逐字节：BOM 保留、正文仍是 LF、末尾那个 CRLF 空行仍在。
    let bytes = read(&target);
    assert_eq!(
        bytes,
        b"\xEF\xBB\xBFpatch:\n  schema_list:\n    - {schema: luna_pinyin_simp}\n    - {schema: wubi_pinyin}\n    - {schema: wubi86}\n\r\n"
    );
}

/// 连续两次 ensure：第二次文件逐字节不变（补齐 Unchanged + prune 无可删 → 无 churn）。
#[test]
fn ensure_twice_leaves_the_file_byte_identical() {
    let dir = TempDir::new("schema-list-churn");
    let rime = dir.path().join("rime");
    fs::create_dir_all(&rime).expect("create rime dir");
    let target = rime.join("default.custom.yaml");
    write(&target, &read(Path::new(REAL227_DEFAULT)));

    let data = dir.path().join("data");
    fs::create_dir_all(&data).expect("create data dir");
    for id in retained_schemas() {
        write(&data.join(format!("{id}.schema.yaml")), b"name: x\n");
    }

    let opts = rime_sync_core::ime_state::Options::new(rime_sync_core::ime_state::Mode::Fix)
        .with_install_ime(false)
        .with_patch_data(false)
        .with_auto_data_patch(false)
        .with_deploy(false)
        .with_prune(true)
        .with_seed_templates(false)
        .with_always_config(false);
    rime_sync_core::ime_state::ensure(&rime, None, std::slice::from_ref(&data), &opts);
    let after_first = read(&target);

    rime_sync_core::ime_state::ensure(&rime, None, &[data], &opts);
    assert_eq!(
        read(&target),
        after_first,
        "第二次 ensure 不许再改文件（churn）"
    );
}

/// prune 重写用户文件时逐行保真：BOM、每一行的行尾、末尾空行都不许动。
#[test]
fn prune_keeps_bom_and_mixed_line_endings() {
    let dir = TempDir::new("prune-preserve");
    let rime = dir.path().join("rime");
    fs::create_dir_all(&rime).expect("create rime dir");
    let target = rime.join("default.custom.yaml");
    write(&target, &read(Path::new(REAL227_DEFAULT)));

    // data 里有一个**不在保留清单**的方案（luna_pinyin）→ prune 有东西可删。
    let data = dir.path().join("data");
    fs::create_dir_all(&data).expect("create data dir");
    write(&data.join("luna_pinyin.schema.yaml"), b"name: x\n");

    rime_sync_core::prune::prune_auto_with(
        Some(std::slice::from_ref(&data)),
        Some(&rime),
        rime_sync_core::prune::PruneOptions::default(),
    );

    // 条目被删了，但 BOM / LF 正文 / 末尾 CRLF 空行全在。
    let bytes = read(&target);
    assert_eq!(bytes, b"\xEF\xBB\xBFpatch:\n  schema_list:\n\r\n");
}
