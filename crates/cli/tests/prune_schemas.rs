//! prune-schemas 集成测试：临时目录，不碰真实 Rime 数据。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("rime-sync-cli-prune-{}-{seq}", std::process::id()));
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

fn make_schema(data: &Path, id: &str) {
    fs::create_dir_all(data).expect("data dir");
    fs::write(
        data.join(format!("{id}.schema.yaml")),
        format!("schema:\n  schema_id: {id}\n"),
    )
    .expect("write schema");
    for suffix in [".prism.bin", ".table.bin", ".reverse.bin"] {
        fs::write(data.join(format!("{id}{suffix}")), "bin").expect("write artifact");
    }
}

fn run(tmp: &TempDir, data: &Path, user: &Path, enabled: bool) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"))
        .arg("--config")
        .arg(tmp.path().join("state/config.json"))
        .arg("prune-schemas")
        .arg("--json")
        .arg("--data-dir")
        .arg(data)
        .arg("--user-dir")
        .arg(user)
        .env("RIMESYNC_PRUNE_SCHEMAS", if enabled { "1" } else { "0" })
        // 不碰真实输入法目录：目录全部由参数指定，这里再兜底关掉自动探测。
        .env("RIMESYNC_WEASEL_DATA", "")
        .env("RIMESYNC_PREBUILT", "0")
        .output()
        .expect("run rime-sync-cli prune-schemas");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse prune json")
}

fn backups_with_prefix(dir: &Path, prefix: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(prefix) && name.contains(".rimesync-removed-"))
        .collect()
}

#[test]
fn prune_schemas_removes_extras_and_keeps_the_retained_three() {
    let tmp = TempDir::new();
    let data = tmp.path().join("data");
    let user = tmp.path().join("user");
    for id in ["luna_pinyin_simp", "wubi_pinyin", "wubi86", "terra_pinyin"] {
        make_schema(&data, id);
    }
    fs::create_dir_all(user.join("build")).expect("build dir");
    fs::write(user.join("terra_pinyin.custom.yaml"), "patch: {}\n").expect("custom");
    fs::write(user.join("wubi86.custom.yaml"), "patch: {}\n").expect("custom");
    fs::write(user.join("weasel.custom.yaml"), "patch: {}\n").expect("weasel");
    fs::write(user.join("build/terra_pinyin.schema.yaml"), "schema: {}\n").expect("build schema");
    fs::write(user.join("build/wubi86.schema.yaml"), "schema: {}\n").expect("build schema");

    let report = run(&tmp, &data, &user, true);

    assert_eq!(
        report["retained"],
        serde_json::json!(["luna_pinyin_simp", "wubi_pinyin", "wubi86"])
    );
    assert!(report["removed"].as_array().map(|a| a.len()).unwrap_or(0) >= 3);
    // 要删的：schema + 3 个产物 + 用户 custom + build schema
    assert!(!data.join("terra_pinyin.schema.yaml").exists());
    assert!(!data.join("terra_pinyin.table.bin").exists());
    assert!(!user.join("terra_pinyin.custom.yaml").exists());
    assert!(!user.join("build/terra_pinyin.schema.yaml").exists());
    // 备份留在原目录，名字带统一后缀
    assert!(!backups_with_prefix(&data, "terra_pinyin.schema.yaml").is_empty());
    assert!(!backups_with_prefix(&user, "terra_pinyin.custom.yaml").is_empty());
    // 保留的与不属于方案的 custom 一个字节都不动
    assert!(data.join("luna_pinyin_simp.schema.yaml").is_file());
    assert!(data.join("wubi86.table.bin").is_file());
    assert!(user.join("wubi86.custom.yaml").is_file());
    assert!(user.join("weasel.custom.yaml").is_file());
    assert!(user.join("build/wubi86.schema.yaml").is_file());

    // 再跑一次：幂等，不再有新的 removed/backups
    let again = run(&tmp, &data, &user, true);
    assert_eq!(again["removed"], serde_json::json!([]));
    assert_eq!(again["backups"], serde_json::json!([]));
}

#[test]
fn prune_schemas_honours_the_env_switch() {
    let tmp = TempDir::new();
    let data = tmp.path().join("data");
    let user = tmp.path().join("user");
    make_schema(&data, "terra_pinyin");
    fs::create_dir_all(&user).expect("user dir");

    let report = run(&tmp, &data, &user, false);

    assert_eq!(report["disabled"], true);
    assert_eq!(report["removed"], serde_json::json!([]));
    assert!(data.join("terra_pinyin.schema.yaml").is_file());
}
