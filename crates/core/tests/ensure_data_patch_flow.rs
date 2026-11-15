//! C1：`ensure → patch_auto_in` 这条主路径的端到端覆盖。
//!
//! 背景：data 补丁这条线此前**没有任何集成测试**——既有测试要么直接调
//! `data_patch` 的底层函数（绕过 `ensure`），要么在 `ensure` 里把 data 补丁
//! 关掉（`with_auto_data_patch(false)` / `with_patch_data(false)`）。于是
//! 「assess → 判定该打补丁 → 真打了 → 再 assess」这条主线谁都没验过。
//!
//! 这里走完整链路：临时 Rime 用户目录 + 临时共享 data 目录（共享目录
//! **不可写** ⇒ 降级成用户级 `<schema>.custom.yaml`），跑 `ime_state::ensure`，
//! 断言：
//! * 用户目录里那份缺增强组件的 custom.yaml 真被修好（三个组件齐了、顺序对）；
//! * 第二次 ensure 一个字节都不再改（幂等）。
//!
//! 全程只用临时目录：不碰真机的 Rime 目录，也不碰 /usr/share/rime-data。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::ime_state::{self, Action, Mode, Options, Outcome};

/// 共享方案：processors 里故意**没有** rime-sync 的三个增强组件。
const SHARED_SCHEMA: &str = "\
schema:
  schema_id: luna_pinyin_simp
engine:
  processors:
    - recognizer
    - key_binder
    - punctuator
  translators:
    - table_translator
recognizer:
  import_preset: default
";

/// 用户目录里已有的 custom.yaml：同样缺增强组件（227 那台机器的状态）。
const USER_CUSTOM: &str = "\
patch:
  engine:
    processors:
      - recognizer
      - key_binder
      - punctuator
";

/// 编译产物：带 rime-sync 标记（assess 认它当可用），外加一个 .bin。
const BUILD_SCHEMA: &str = "\
engine:
  processors:
    - recognizer
    - rimesync_vnum
    - rimesync_punct
    - rimesync_vselect
";

struct Fixture {
    root: PathBuf,
    rime: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ensure-data-patch-{tag}"));
        let _ = fs::remove_dir_all(&root);
        let rime = root.join("rime");
        let data = root.join("data");
        fs::create_dir_all(rime.join("build")).expect("create rime dir");
        fs::create_dir_all(data.join("home")).expect("create data dir");
        fs::write(
            rime.join("build/luna_pinyin_simp.schema.yaml"),
            BUILD_SCHEMA,
        )
        .expect("write build");
        fs::write(rime.join("build/luna_pinyin_simp.table.bin"), b"table").expect("write bin");
        fs::write(data.join("luna_pinyin_simp.schema.yaml"), SHARED_SCHEMA).expect("write schema");
        // 用户目录里已有的、缺增强组件的 custom.yaml。
        fs::write(rime.join("luna_pinyin_simp.custom.yaml"), USER_CUSTOM).expect("write custom");
        // 共享 data 目录**模拟成不可写**：dir_writable 的探测文件用
        // create_new 打开，同名文件已经存在就会失败 ⇒ 判定为不可写。
        // 这一招不依赖 chmod，root 下同样成立（root 能绕过权限位）。
        fs::write(
            data.join(format!(".rimesync-write-probe-{}", std::process::id())),
            b"block",
        )
        .expect("block write probe");
        Self { root, rime, data }
    }

    fn custom(&self) -> PathBuf {
        self.rime.join("luna_pinyin_simp.custom.yaml")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 环境变量守卫：改完还原，不留给后面的测试。
struct EnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, &Path)]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            saved.push((*key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// ensure 的参数：只跑状态机，绝不装输入法 / 播模板 / 裁剪 / 部署。
fn options() -> Options {
    Options::new(Mode::Fix)
        .with_install_ime(false)
        .with_seed_templates(false)
        .with_deploy(false)
        .with_prune(false)
        .with_version(Some("0.0.0-test".to_string()))
}

fn action<'a>(outcome: &'a Outcome, kind: &str) -> Option<&'a Action> {
    outcome.actions.iter().find(|item| item.kind == kind)
}

/// custom.yaml 里 patch/engine/processors 的名字列表。
///
/// 补丁写出来的是**扁平键** `engine/processors`（rime 的 patch 语义），
/// 但用户手写的也可能是嵌套的 `engine: { processors: ... }` —— 两种都认。
fn processors(text: &str) -> Vec<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(text).expect("custom 必须是合法 YAML");
    let patch = value.get("patch").expect("custom 里必须有 patch 段");
    let list = patch
        .get("engine/processors")
        .or_else(|| {
            patch
                .get("engine")
                .and_then(|engine| engine.get("processors"))
        })
        .and_then(serde_yaml::Value::as_sequence)
        .expect("custom 里必须有 engine/processors");
    list.iter()
        .filter_map(serde_yaml::Value::as_str)
        .map(str::to_string)
        .collect()
}

/// 目录快照：相对路径 → 字节。用来断言「一个字节都没变」。
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                let name = path
                    .strip_prefix(root)
                    .expect("在 root 之下")
                    .to_string_lossy()
                    .to_string();
                out.insert(name, fs::read(&path).unwrap_or_default());
            }
        }
    }
    out
}

/// ensure → patch_auto_in 的主路径：共享目录不可写 ⇒ 降级写用户级 custom.yaml，
/// 用户那份缺增强组件的 custom.yaml 必须被修好，且顺序正确。
#[test]
fn ensure_patches_user_custom_yaml_through_the_real_data_patch_path() {
    let fixture = Fixture::new("main");
    let _env = EnvGuard::set(&[
        // data 目录解析：只认临时目录，绝不碰真机的 /usr/share/rime-data。
        ("RIMESYNC_WEASEL_DATA", &fixture.data),
        // 用户目录：让 rime_dir 的探测结果与 ensure 用的目录一致。
        ("RIME_USER_DATA_DIR", &fixture.rime),
        // 历史默认路径（~/.local/share/fcitx5/rime）也别混进来。
        ("XDG_DATA_HOME", &fixture.data.join("home")),
    ]);

    let before = fs::read_to_string(fixture.custom()).expect("read custom before");
    assert_eq!(before, USER_CUSTOM, "前置：用户 custom.yaml 确实缺增强组件");

    let outcome = ime_state::ensure(
        &fixture.rime,
        None,
        std::slice::from_ref(&fixture.data),
        &options(),
    );

    // ① 走的是 data 补丁这条线，而且真的写了东西。
    let data_action = action(&outcome, "data").expect("必须有 data 动作");
    assert!(
        data_action.changed,
        "data 动作必须 changed=true：{}",
        data_action.detail
    );
    assert_eq!(
        outcome.data.mode, "custom",
        "共享目录不可写时应降级成用户级 custom.yaml：{:?}",
        outcome.data
    );
    assert!(
        outcome
            .data
            .custom_written
            .iter()
            .any(|item| item == &fixture.custom().display().to_string()),
        "custom.yaml 必须写进用户目录：{:?}",
        outcome.data.custom_written
    );

    // ② 文件真的修好了：三个增强组件齐了，且顺序对。
    let after = fs::read_to_string(fixture.custom()).expect("read repaired custom");
    let names = processors(&after);
    let vselect = names
        .iter()
        .position(|name| name == "rimesync_vselect")
        .unwrap_or_else(|| panic!("custom.yaml 里必须有 rimesync_vselect：{names:?}"));
    assert_eq!(
        names.get(vselect + 1).map(String::as_str),
        Some("numpad"),
        "numpad 必须紧跟 rimesync_vselect：{names:?}"
    );
    let punct = names
        .iter()
        .position(|name| name == "rimesync_punct")
        .unwrap_or_else(|| panic!("custom.yaml 里必须有 rimesync_punct：{names:?}"));
    let punctuator = names
        .iter()
        .position(|name| name == "punctuator")
        .unwrap_or_else(|| panic!("custom.yaml 里必须有 punctuator：{names:?}"));
    assert!(
        punct < punctuator,
        "rimesync_punct 必须排在 punctuator 前面：{names:?}"
    );
    // 共享方案自带的处理器一个都不能丢（custom.yaml 是 patch 语义，列表整段替换）。
    for keep in ["recognizer", "key_binder"] {
        assert!(
            names.iter().any(|name| name == keep),
            "丢了 {keep}：{names:?}"
        );
    }

    // ③ 幂等：第二次 ensure 一个字节都不许再改。
    let snapshot_after_first = snapshot(&fixture.rime);
    let second = ime_state::ensure(
        &fixture.rime,
        None,
        std::slice::from_ref(&fixture.data),
        &options(),
    );
    let snapshot_after_second = snapshot(&fixture.rime);
    let mut changed: Vec<String> = Vec::new();
    for (name, bytes) in &snapshot_after_first {
        match snapshot_after_second.get(name) {
            Some(other) if other == bytes => {}
            Some(_) => changed.push(name.clone()),
            None => changed.push(format!("{name}（消失了）")),
        }
    }
    for name in snapshot_after_second.keys() {
        if !snapshot_after_first.contains_key(name) {
            changed.push(format!("{name}（新增）"));
        }
    }
    assert!(
        changed.is_empty(),
        "第二次 ensure 不许改任何字节，实际变了：{changed:?}"
    );
    let second_data = action(&second, "data").expect("第二次也要有 data 动作");
    assert!(
        !second_data.changed,
        "第二次 ensure 不该再改 data：{}",
        second_data.detail
    );
}
