//! 方案裁剪的「先预演、后确认」：桌面端这一层的行为。
//!
//! 回归（缺陷 D2 一族）：程序以前在首启自举时直接删掉不在保留名单里的
//! <id>.custom.yaml 并重写 schema_list —— 用户既没确认过、也没法预演，
//! 「我正在用的方案凭空消失」就是这么来的。
//!
//! 现在必须满足：
//! * 预演不动磁盘（一个字节都没变）；
//! * 预演结果说清三件事：删哪些文件、schema_list 变成什么样、哪些方案会消失；
//! * 确认执行才真的删，且与老行为一致、幂等；
//! * 取消不产生任何副作用（连令牌都作废）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rime_sync_app_lib::prune_cmds;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rime-sync-prune-ui-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// 造一个「有东西要删」的场景。
fn scene(tag: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(tag);
    let data = root.join("data");
    let user = root.join("user");
    fs::create_dir_all(&data).expect("mkdir data");
    fs::create_dir_all(&user).expect("mkdir user");
    fs::write(
        data.join("terra_pinyin.schema.yaml"),
        "schema:\n  schema_id: terra_pinyin\n",
    )
    .expect("write schema");
    fs::write(data.join("terra_pinyin.table.bin"), "bin").expect("write table");
    fs::write(user.join("terra_pinyin.custom.yaml"), "patch: {}\n").expect("write custom");
    fs::write(user.join("wubi86.custom.yaml"), "patch: {}\n").expect("write kept custom");
    fs::write(
        user.join("default.custom.yaml"),
        "patch:\n  schema_list:\n    - {schema: luna_pinyin_simp}\n    - {schema: wubi86}\n    - {schema: terra_pinyin}\n",
    )
    .expect("write default");
    (data, user)
}

/// 目录树快照（相对路径 + 内容）：证明「一个字节都没变」。
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                out.push((relative, fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn preview_does_not_change_a_single_byte() {
    let root = temp_dir("readonly");
    let (data, user) = scene("readonly");
    let before = snapshot(&root);
    let preview = prune_cmds::preview_at(Some(std::slice::from_ref(&data)), Some(&user));
    let after = snapshot(&root);
    assert_eq!(before, after, "预演不能动磁盘上的任何一个字节");
    assert!(preview.has_changes);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn preview_reports_files_new_schema_list_and_affected_schemas() {
    let root = temp_dir("content");
    let (data, user) = scene("content");
    let preview = prune_cmds::preview_at(Some(std::slice::from_ref(&data)), Some(&user));

    // 1) 会被删的文件。
    assert!(
        preview
            .files
            .iter()
            .any(|path| path.ends_with("terra_pinyin.schema.yaml")),
        "{:?}",
        preview.files
    );
    assert!(
        preview
            .files
            .iter()
            .any(|path| path.ends_with("terra_pinyin.custom.yaml")),
        "{:?}",
        preview.files
    );
    // 保留方案的 custom.yaml 不在待删清单里。
    assert!(
        !preview
            .files
            .iter()
            .any(|path| path.ends_with("wubi86.custom.yaml")),
        "{:?}",
        preview.files
    );

    // 2) 删除后的 schema_list。
    let list = preview
        .schema_lists
        .iter()
        .find(|item| item.path.ends_with("default.custom.yaml"))
        .expect("必须有 default.custom.yaml 的预览");
    assert!(!list.after.contains("terra_pinyin"), "{}", list.after);
    assert!(list.after.contains("wubi86"), "{}", list.after);
    assert_eq!(list.dropped, 1);

    // 3) 会因此消失的方案。
    assert_eq!(preview.affected_schemas, vec!["terra_pinyin".to_string()]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn confirm_deletes_and_is_idempotent() {
    let root = temp_dir("confirm");
    let (data, user) = scene("confirm");
    let preview = prune_cmds::preview_at(Some(std::slice::from_ref(&data)), Some(&user));
    let report = prune_cmds::confirm_at(
        &preview.token,
        Some(std::slice::from_ref(&data)),
        Some(&user),
    )
    .expect("确认应成功");
    assert!(!report.removed.is_empty());
    assert!(!data.join("terra_pinyin.schema.yaml").exists());
    assert!(!user.join("terra_pinyin.custom.yaml").exists());
    let text = fs::read_to_string(user.join("default.custom.yaml")).expect("read");
    assert!(!text.contains("terra_pinyin"), "{text}");

    // 幂等：再预演一次没有东西可删，再确认也不动磁盘。
    let again = prune_cmds::preview_at(Some(std::slice::from_ref(&data)), Some(&user));
    assert!(!again.has_changes, "{:?}", again.files);
    let before = snapshot(&root);
    let second =
        prune_cmds::confirm_at(&again.token, Some(std::slice::from_ref(&data)), Some(&user))
            .expect("没有东西可删时确认也应成功");
    assert_eq!(second.removed.len(), 0);
    assert_eq!(before, snapshot(&root), "重复确认不能改动任何文件");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cancel_has_no_side_effects_and_invalidates_the_token() {
    let root = temp_dir("cancel");
    let (data, user) = scene("cancel");
    let preview = prune_cmds::preview_at(Some(std::slice::from_ref(&data)), Some(&user));
    let before = snapshot(&root);
    prune_cmds::cancel_token(&preview.token).expect("取消应成功");
    // 取消不碰磁盘。
    assert_eq!(before, snapshot(&root), "取消不能改动任何文件");
    // 被取消的令牌再拿来确认必须被拒绝（不能因为「前端藏起来」就又删了）。
    let error = prune_cmds::confirm_at(
        &preview.token,
        Some(std::slice::from_ref(&data)),
        Some(&user),
    )
    .expect_err("取消后的令牌必须被拒绝");
    assert!(error.contains("取消"), "{error}");
    // 一个字节都没被删。
    assert!(data.join("terra_pinyin.schema.yaml").is_file());
    assert!(user.join("terra_pinyin.custom.yaml").is_file());
    let _ = fs::remove_dir_all(&root);
}
