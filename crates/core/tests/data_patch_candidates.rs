//! C3：data 补丁失败时的错误信息必须**忠实反映候选目录的真实顺序**。
//!
//! 背景：`data_patch` 的用户目录候选里，第一位是**真正在用**的 Rime 目录
//! （fcitx5-rime 的 `UserDataDirPath` 那个，见 `user_fallback_candidates`）。
//! 旧错误信息却写死了两个历史默认路径（`$XDG_DATA_HOME/fcitx5/rime`、
//! `~/.local/share/fcitx5/rime`），既没提真正在用的目录，顺序也和实际用法相反 ——
//! 用户照着改会去改一个 fcitx5 根本不读的目录。
//!
//! 现在错误信息里的候选清单由**同一份候选函数**动态生成，顺序即优先级。
//! 本测试让共享 data 目录不可写（不靠 chmod：用 `create_new` 探测文件占位，
//! root 下同样成立），逼出这条错误信息，然后断言清单第一位就是真正在用的目录。

#![cfg(not(windows))]

use std::fs;
use std::path::{Path, PathBuf};

use rime_sync_core::data_patch::{self, FixSource};

struct Fixture {
    root: PathBuf,
    data: PathBuf,
    in_use: PathBuf,
    legacy: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("data-patch-candidates-{tag}"));
        let _ = fs::remove_dir_all(&root);
        let data = root.join("data");
        let in_use = root.join("in-use-rime");
        let legacy = root.join("xdg/fcitx5/rime");
        for dir in [&data, &in_use, &legacy] {
            fs::create_dir_all(dir).expect("create dir");
        }
        // 共享 data 目录模拟成不可写：dir_writable 的探测用 create_new 打开，
        // 同名文件已存在就会失败 ⇒ 判定不可写（不依赖 chmod，root 下同样成立）。
        fs::write(
            data.join(format!(".rimesync-write-probe-{}", std::process::id())),
            b"block",
        )
        .expect("block write probe");
        Self {
            root,
            data,
            in_use,
            legacy,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 环境变量守卫：改完还原。
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

/// 所有候选都不可写时：错误信息里的候选清单，第一位必须是真正在用的 Rime 目录。
#[test]
fn unwritable_data_dir_reports_in_use_rime_dir_first() {
    let fixture = Fixture::new("order");
    let _env = EnvGuard::set(&[
        ("RIMESYNC_WEASEL_DATA", &fixture.data),
        // 真正在用的 Rime 目录（rime_dir::detect 的最高优先级来源）。
        ("RIME_USER_DATA_DIR", &fixture.in_use),
        // 历史默认路径，作为第二个候选。
        ("XDG_DATA_HOME", &fixture.root.join("xdg")),
    ]);

    // 共享目录里没有保留清单里的方案 ⇒ 降级也写不出 custom.yaml ⇒ 走错误分支。
    let report = data_patch::patch_auto_in(&fixture.in_use, "0.0.0-test", FixSource::Explicit);

    let message = report.data_error.expect("共享目录不可写时必须有可读原因");
    assert!(
        message.contains("不可写"),
        "错误信息要说明不可写：{message}"
    );
    assert!(
        !message.contains("$XDG_DATA_HOME/fcitx5/rime")
            && !message.contains("~/.local/share/fcitx5/rime"),
        "错误信息不该再写死历史默认路径：{message}"
    );

    let in_use = fixture.in_use.display().to_string();
    let legacy = fixture.legacy.display().to_string();
    let at_in_use = message
        .find(&in_use)
        .unwrap_or_else(|| panic!("错误信息里必须有真正在用的 Rime 目录 {in_use}：{message}"));
    let at_legacy = message
        .find(&legacy)
        .unwrap_or_else(|| panic!("错误信息里应列出历史默认路径 {legacy}：{message}"));
    assert!(
        at_in_use < at_legacy,
        "真正在用的目录必须排在候选清单第一位：{message}"
    );

    // 清单本身：第一位就是真正在用的目录。
    let list = message
        .split_once("候选目录按优先级：")
        .map(|(_, rest)| rest)
        .expect("错误信息里要有候选清单");
    let list = list.split('；').next().unwrap_or(list);
    assert!(
        list.starts_with(&in_use),
        "候选清单第一位必须是真正在用的目录：{list}"
    );
    assert!(
        list.contains(&legacy),
        "候选清单也要列出历史默认路径：{list}"
    );
}
