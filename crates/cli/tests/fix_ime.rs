//! fix-ime 集成测试：假 payload + 临时 Rime 目录，不触碰真实配置。

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
        let path = std::env::temp_dir().join(format!(
            "rime-sync-cli-fix-ime-{}-{seq}",
            std::process::id()
        ));
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

const MARKED: &str = "patch:\n  rimesync_vnum: '['\n  rimesync_punct: ']'\n";

struct Fixture {
    tmp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new();
        for dir in ["payload/build", "payload/config", "rime", "state"] {
            fs::create_dir_all(tmp.path().join(dir)).expect("create dir");
        }
        fs::write(
            tmp.path().join("payload/build/luna_pinyin.schema.yaml"),
            MARKED,
        )
        .expect("write schema");
        fs::write(
            tmp.path().join("payload/build/luna_pinyin.prism.bin"),
            "prism",
        )
        .expect("write prism");
        fs::write(
            tmp.path().join("payload/config/default.custom.yaml"),
            "patch:\n  schema_list: []\n",
        )
        .expect("write config");
        Self { tmp }
    }

    fn payload(&self) -> PathBuf {
        self.tmp.path().join("payload")
    }

    fn rime(&self) -> PathBuf {
        self.tmp.path().join("rime")
    }

    fn run(&self) -> std::process::Output {
        self.run_with_data(None)
    }

    /// data 补丁默认关闭（空串）：CI 机器上可能装着真 Weasel，不能改真目录。
    fn run_with_data(&self, data_dir: Option<&Path>) -> std::process::Output {
        self.run_with(data_dir, false)
    }

    fn run_with(&self, data_dir: Option<&Path>, no_data_patch: bool) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"));
        command
            .arg("--config")
            .arg(self.tmp.path().join("state/config.json"))
            .arg("--rime-dir")
            .arg(self.rime())
            .arg("fix-ime")
            .arg("--json");
        if no_data_patch {
            command.arg("--no-data-patch");
        }
        command.env("RIMESYNC_PREBUILT_DIR", self.payload()).env(
            "RIMESYNC_WEASEL_DATA",
            data_dir.map(Path::as_os_str).unwrap_or_default(),
        );
        command.output().expect("run rime-sync-cli fix-ime")
    }
}

#[test]
fn fix_ime_json_copies_missing_files() {
    let fixture = Fixture::new();

    let output = fixture.run();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse report json");
    assert_eq!(report["copied"], 2);
    assert_eq!(report["kept"], 0);
    assert_eq!(report["config_copied"], 1);
    assert_eq!(report["build_ok"], true);
    assert_eq!(report["probe_after"], "prebuilt");
    assert!(fixture
        .rime()
        .join("build/luna_pinyin.schema.yaml")
        .is_file());
    assert!(fixture.rime().join("build/luna_pinyin.prism.bin").is_file());
    assert!(fixture.rime().join("default.custom.yaml").is_file());
}

#[test]
fn fix_ime_json_keeps_existing_files() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.rime().join("build")).expect("create build dir");
    fs::write(fixture.rime().join("build/luna_pinyin.schema.yaml"), MARKED)
        .expect("write local schema");
    fs::write(
        fixture.rime().join("default.custom.yaml"),
        "patch:\n  keep: me\n",
    )
    .expect("write local config");

    let output = fixture.run();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse report json");
    assert_eq!(report["copied"], 1, "只缺 prism.bin");
    assert_eq!(report["kept"], 1);
    assert_eq!(report["config_kept"], 1);
    assert_eq!(
        fs::read_to_string(fixture.rime().join("default.custom.yaml")).expect("read config"),
        "patch:\n  keep: me\n"
    );
}

const DATA_SAMPLE: &str = "\
engine:
  processors:
    - ascii_composer
    - recognizer
    - key_binder
    - speller
    - punctuator
    - selector
  translators:
    - script_translator
punctuator:
  import_preset: symbols
recognizer:
  import_preset: default
  patterns:
    punct: '^/([0-9]+)$'
";

#[test]
fn fix_ime_patches_weasel_data_schemas() {
    let fixture = Fixture::new();
    let data = fixture.tmp.path().join("weasel-data");
    fs::create_dir_all(&data).expect("create data dir");
    fs::write(data.join("luna_pinyin.schema.yaml"), DATA_SAMPLE).expect("write data schema");

    let first = fixture.run_with_data(Some(&data));
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("parse report json");
    assert_eq!(report["data_patched"], 1, "本次写入了 1 个 data 方案");
    assert_eq!(
        report["data"]["schemas_patched"].as_array().unwrap().len(),
        1
    );
    assert_eq!(report["data"]["backup_dir"], data.display().to_string());
    assert_eq!(report["copied"], 2, "原有的 build 铺设不受影响");

    let schema = data.join("luna_pinyin.schema.yaml");
    let patched = fs::read_to_string(&schema).expect("read patched");
    assert!(patched.starts_with("# patched by rime-sync "));
    assert!(patched.contains("rimesync_vnum"));
    assert!(patched.contains("rimesync_vselect"));
    assert!(patched.contains("numpad"));
    assert!(data.join("luna_pinyin.schema.yaml.rimesync-orig").is_file());

    let second = fixture.run_with_data(Some(&data));
    assert!(second.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("parse report json");
    assert_eq!(report["data_patched"], 0, "重复执行不重复写");
    assert_eq!(
        report["data"]["schemas_unchanged"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fs::read_to_string(&schema).expect("read again"),
        patched,
        "幂等：第二次一个字节都不改"
    );
}

#[test]
fn fix_ime_no_data_patch_leaves_the_file_alone() {
    let fixture = Fixture::new();
    let data = fixture.tmp.path().join("weasel-data");
    fs::create_dir_all(&data).expect("create data dir");
    let schema = data.join("luna_pinyin.schema.yaml");
    fs::write(&schema, DATA_SAMPLE).expect("write data schema");

    let output = fixture.run_with(Some(&data), true);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse report json");
    assert_eq!(report["data_patched"], 0);
    assert_eq!(
        fs::read_to_string(&schema).expect("read schema"),
        DATA_SAMPLE,
        "--no-data-patch 一个字节都不该动"
    );
    assert!(!data.join("luna_pinyin.schema.yaml.rimesync-orig").exists());
}
