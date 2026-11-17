//! 回归（缺陷 3）：Rime 用户目录里**只有被 *.dict.yaml 引用过的 .txt** 才是词库源。
//!
//! 改前：用户目录里任何一个 .txt（随手写的 notes.txt、备份片段）都被当成词库源，
//! 上传到远端 shared/ —— 用户的私人文件就这样进了云端。
//!
//! 现在：判据是「有没有被某个 *.dict.yaml 的 import_tables 引用」；
//! 没被引用的 .txt 归为 Unknown，既不上传，也不会被远端同名文件覆盖。

mod support;

use std::fs;

use rime_sync_core::config::FirstSyncPolicy;
use rime_sync_core::plan::{self, Category};

use support::{engine_for, temp_dir, MockDav};

/// 一个引用 words.txt 的词典 + 一个没被引用的 notes.txt。
fn write_scene(rime: &std::path::Path) {
    fs::write(
        rime.join("a.dict.yaml"),
        "name:\n  a\nimport_tables:\n  - words.txt\n...\n",
    )
    .expect("write dict yaml");
    fs::write(rime.join("words.txt"), "word\t1\n").expect("write words");
    fs::write(rime.join("notes.txt"), "随手记的笔记\n").expect("write notes");
}

/// 分类层面：被引用的 .txt 仍是词库源，没被引用的 .txt 不参与同步。
#[test]
fn only_referenced_txt_files_are_dict_sources() {
    let rime = temp_dir("dict-scope");
    write_scene(&rime);

    let index = plan::scan_dict_source_index(&rime);

    assert!(index.contains("words.txt"), "被引用的词表必须被认出来");
    assert!(!index.contains("notes.txt"), "没被引用的 .txt 不能算词库源");
    assert_eq!(
        plan::classify_with("words.txt", Some(&index)).category,
        Category::DictSource
    );
    assert_eq!(
        plan::classify_with("notes.txt", Some(&index)).category,
        Category::Unknown
    );
    // 没有上下文时保持旧行为：远端清单校验不依赖本机上下文。
    assert_eq!(plan::classify("words.txt").category, Category::DictSource);
}

/// 端到端：同步时未引用的 .txt 不许出现在远端 shared/ 里。
#[test]
fn unreferenced_txt_is_not_uploaded_to_shared() {
    let rime = temp_dir("dict-scope-e2e");
    let state = temp_dir("dict-scope-e2e-state");
    write_scene(&rime);
    let mock = MockDav::start(vec![]);

    let mut engine = engine_for(&mock, &rime, &state, FirstSyncPolicy::Normal);
    let report = engine.transport();

    assert!(
        report.ok,
        "同步应当成功：error={:?} skipped={:?}",
        report.error, report.skipped
    );
    let remote = mock.snapshot();
    assert!(
        remote.contains_key("shared/files/words.txt"),
        "被引用的词表必须上传：{:?}",
        remote.keys().collect::<Vec<_>>()
    );
    assert!(
        !remote.contains_key("shared/files/notes.txt"),
        "没被引用的 .txt 不许上传到 shared/：{:?}",
        remote.keys().collect::<Vec<_>>()
    );
}
