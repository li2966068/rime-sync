//! 回归：`.txt` 只同步「被某个 *.dict.yaml 的 import_tables 引用过」的词表。
//!
//! 这里钉住四条规则漏洞：
//! 1. `import_tables: [a.txt]` 这种**内联数组**写法也要认（否则明明被引用的
//!    词表被判成 Unknown，不同步）。
//! 2. 索引按**相对路径**比较：两个目录下的同名 `.txt` 不许互相撞车。
//! 3. 软链指向的 `*.dict.yaml` 也要扫到（软链是共享词典目录的常见做法）。
//! 4. 远端（其它设备）的 `*.dict.yaml` 引用到的 `.txt` 不许因为「本机没引用」
//!    而被判成 Unknown —— 那样它既不同步也拉不下来，停在半迁移状态。

mod support;

use std::fs;
use std::path::Path;

use rime_sync_core::plan::{self, Category};

use support::temp_dir;

/// 造一个 rime 目录（相对路径 -> 文件内容）。
fn write_scene(rime: &Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let path = rime.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, content).expect("write");
    }
}

fn category(rime: &Path, rel: &str) -> Category {
    let index = plan::scan_dict_source_index(rime);
    plan::classify_with(rel, Some(&index)).category
}

/// 漏洞 1：`import_tables: [a.txt, "b.txt"]` 的内联写法必须被认出来。
#[test]
fn inline_import_tables_are_parsed() {
    let rime = temp_dir("dict-rule-inline");
    write_scene(
        &rime,
        &[
            (
                "a.dict.yaml",
                "name: a\nimport_tables: [words.txt, \"quoted.txt\", 'single.txt']\n...\n",
            ),
            ("words.txt", "a\t1\n"),
            ("quoted.txt", "b\t1\n"),
            ("single.txt", "c\t1\n"),
            ("notes.txt", "随手记的笔记\n"),
        ],
    );

    assert_eq!(category(&rime, "words.txt"), Category::DictSource);
    assert_eq!(category(&rime, "quoted.txt"), Category::DictSource);
    assert_eq!(category(&rime, "single.txt"), Category::DictSource);
    assert_eq!(category(&rime, "notes.txt"), Category::Unknown);
}

/// 漏洞 1 的另一半：缩进列表写法（回归护栏，别改坏）。
#[test]
fn block_import_tables_are_parsed() {
    let rime = temp_dir("dict-rule-block");
    write_scene(
        &rime,
        &[
            (
                "a.dict.yaml",
                "name: a\nimport_tables:\n  - words.txt\n  - 'quoted.txt'\n...\n",
            ),
            ("words.txt", "a\t1\n"),
            ("quoted.txt", "b\t1\n"),
            ("notes.txt", "随手记的笔记\n"),
        ],
    );

    assert_eq!(category(&rime, "words.txt"), Category::DictSource);
    assert_eq!(category(&rime, "quoted.txt"), Category::DictSource);
    assert_eq!(category(&rime, "notes.txt"), Category::Unknown);
}

/// 漏洞 2：同名不同目录不许撞车 —— 引用了 `dict/a.txt`，`extra/a.txt` 不算被引用。
#[test]
fn same_name_in_other_directory_is_not_referenced() {
    let rime = temp_dir("dict-rule-same-name");
    write_scene(
        &rime,
        &[
            (
                "dict/a.dict.yaml",
                "name: a\nimport_tables:\n  - a.txt\n...\n",
            ),
            ("dict/a.txt", "词表\t1\n"),
            ("extra/a.txt", "另一个同名文件\t1\n"),
        ],
    );

    assert_eq!(category(&rime, "dict/a.txt"), Category::DictSource);
    assert_eq!(category(&rime, "extra/a.txt"), Category::Unknown);
}

/// 漏洞 3：软链指向的 `*.dict.yaml` 里的 import_tables 也要生效。
#[test]
fn symlinked_dict_yaml_is_scanned() {
    let rime = temp_dir("dict-rule-symlink");
    let real = rime.join("real");
    fs::create_dir_all(&real).expect("mkdir");
    fs::write(
        real.join("linked.dict.yaml"),
        "name: linked\nimport_tables: [linked.txt]\n...\n",
    )
    .expect("write");
    fs::write(rime.join("linked.txt"), "词表\t1\n").expect("write");

    let link = rime.join("linked.dict.yaml");
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.join("linked.dict.yaml"), &link).expect("symlink");
    #[cfg(windows)]
    if std::os::windows::fs::symlink_file(real.join("linked.dict.yaml"), &link).is_err() {
        // 没有开发者权限的 Windows 机器上建不了软链，跳过这条断言。
        return;
    }

    assert_eq!(category(&rime, "linked.txt"), Category::DictSource);
}

/// 漏洞 4：远端 dict.yaml 引用到的 `.txt` 不许因为本机没引用而变成 Unknown。
///
/// 判据在 plan.rs 里落地：把远端 dict.yaml 的内容并进索引后，远端那个
/// `.txt` 必须被判成词库源（否则它既不同步也拉不下来）。
#[test]
fn remote_dict_source_keeps_remote_txt_syncable() {
    let rime = temp_dir("dict-rule-remote");
    write_scene(
        &rime,
        &[
            // 本机只引用了自己的词表。
            (
                "local.dict.yaml",
                "name: local\nimport_tables: [local.txt]\n...\n",
            ),
            ("local.txt", "本机词表\t1\n"),
        ],
    );

    let mut index = plan::scan_dict_source_index(&rime);
    assert_eq!(
        plan::classify_with("remote.txt", Some(&index)).category,
        Category::Unknown,
        "本机没引用时确实判成 Unknown（这正是远端半迁移的成因）"
    );

    // 把远端 dict.yaml 的内容并进索引（引擎侧需要下载远端文件才能做这一步）。
    index.insert_remote_dict_yaml(
        "remote.dict.yaml",
        "name: remote\nimport_tables: [remote.txt]\n...\n",
    );

    assert_eq!(
        plan::classify_with("remote.txt", Some(&index)).category,
        Category::DictSource,
        "远端自己引用到的 .txt 必须仍然是词库源"
    );
}
