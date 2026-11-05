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
        Command::new(env!("CARGO_BIN_EXE_rime-sync-cli"))
            .arg("--config")
            .arg(self.tmp.path().join("state/config.json"))
            .arg("--rime-dir")
            .arg(self.rime())
            .arg("fix-ime")
            .arg("--json")
            .env("RIMESYNC_PREBUILT_DIR", self.payload())
            .output()
            .expect("run rime-sync-cli fix-ime")
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
