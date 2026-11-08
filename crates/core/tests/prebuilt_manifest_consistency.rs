//! prebuilt 清单（MANIFEST.json）与产物的一致性 + 铺制策略的回归测试。
//!
//! 现场故障（v0.6.24 真机首启降级）：随包产物的 MANIFEST.json 记的是补丁前的字节，
//! 产物却是补丁后的字节 —— 安装时 verify_manifest 把每个对不上的文件算成一次失败
//! （copy_failed=16 / manifest_failed=build/default.yaml:size:expected=5294 actual=5463），
//! 结论降级成 partial/degraded；同时 build/ 只补不缺，旧版引擎产物留在用户目录里，
//! build_ok=false（schema-stale）→ deploy_result=partial。
//!
//! 本文件钉住两件事：
//! 1) 清单必须与产物逐字节一致（大小 + sha256）：仓库里随包的产物也要过这一关；
//! 2) 铺制策略：build/ 版本对不上就换成新版本（且**不算失败**），
//!    config/ 里用户已有的配置一律保留（不覆盖、不改名、不算失败）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rime_sync_core::ime_state::{self, Mode, Options, OutcomeResult};
use rime_sync_core::prebuilt;

/// 随包保留的三个方案（packaging/rime/retained-schemas.txt 的同一份清单）。
const SCHEMAS: [&str; 3] = ["luna_pinyin_simp", "wubi_pinyin", "wubi86"];
/// 随包插件标记：没有它，引擎产物会被判成 stale（schema-stale）。
const MARKED: &str = "patch:\n  rimesync_vnum: '['\n  rimesync_punct: ']'\n";
const DATA_SAMPLE: &str = "\
engine:
  processors:
    - ascii_composer
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
";

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 递归列出目录下的文件（相对路径，用 / 分隔）。
fn list_files(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(relative);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "rime-sync-prebuilt-manifest-{}-{seq}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["payload/build", "payload/config", "rime/build", "data"] {
            fs::create_dir_all(root.join(dir)).expect("create fixture dir");
        }
        let fixture = Self { root };
        // data 目录：三份保留方案的源文件（data 补丁会改它们）。
        for id in SCHEMAS {
            fixture.write(&format!("data/{id}.schema.yaml"), DATA_SAMPLE);
        }
        // payload：三份方案的 schema.yaml（带标记）+ 编译产物 + 一个 config 模板。
        for id in SCHEMAS {
            fixture.write(
                &format!("payload/build/{id}.schema.yaml"),
                &format!("# {id} v2\n{MARKED}"),
            );
            fixture.write(
                &format!("payload/build/{id}.prism.bin"),
                &format!("bin {id} v2"),
            );
        }
        fixture.write("payload/build/default.yaml", "# default v2\n");
        fixture.write(
            "payload/config/default.custom.yaml",
            "patch:\n  schema_list:\n    - luna_pinyin_simp\n",
        );
        fixture.write_manifest();
        fixture
    }

    fn payload(&self) -> PathBuf {
        self.root.join("payload")
    }

    fn rime(&self) -> PathBuf {
        self.root.join("rime")
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, text).expect("write fixture file");
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.root.join(relative)).expect("read fixture file")
    }

    /// 按 payload 的**最终内容**生成 MANIFEST.json（与生成器脚本同一套算法）。
    fn write_manifest(&self) {
        let mut files = Vec::new();
        let mut total = 0usize;
        for sub in ["build", "config"] {
            let dir = self.payload().join(sub);
            let mut names: Vec<String> = fs::read_dir(&dir)
                .expect("read payload dir")
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            for name in names {
                let bytes = fs::read(dir.join(&name)).expect("read payload file");
                total += bytes.len();
                files.push(serde_json::json!({
                    "path": format!("{sub}/{name}"),
                    "bytes": bytes.len(),
                    "sha256": sha256_hex(&bytes),
                }));
            }
        }
        let manifest = serde_json::json!({
            "files": files,
            "total_bytes": total,
            "generated_at": "2026-11-08T00:00:00Z",
        });
        fs::write(
            self.payload().join("MANIFEST.json"),
            serde_json::to_string_pretty(&manifest).expect("serialize manifest") + "\n",
        )
        .expect("write manifest");
    }

    fn install(&self) -> prebuilt::PrebuiltReport {
        prebuilt::install_prebuilt_with(
            &self.payload(),
            &self.rime(),
            prebuilt::InstallOptions {
                force: false,
                data_dirs: Some(vec![self.data()]),
            },
        )
        .expect("install prebuilt")
    }

    fn options(mode: Mode) -> Options {
        Options::new(mode)
            // 只用夹具里的 data 目录：绝不碰真机上的 Weasel / /usr/share/rime-data。
            .with_auto_data_patch(false)
            .with_patch_data(true)
            .with_version(Some("0.0.0-prebuilt-manifest".to_string()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 1) 仓库里随包的产物：清单必须与产物逐字节一致（大小 + sha256）。
///
/// 这条是「构建期缺陷」的守门：生成器现在按最终内容算清单，CI 也会用同一套
/// 算法校验发布产物；仓库里的产物必须自己先过关。
#[test]
fn repo_payload_manifest_matches_payload() {
    let payload = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/rime/prebuilt")
        .canonicalize()
        .expect("canonicalize payload dir");
    if !payload.join("MANIFEST.json").is_file() {
        eprintln!("skip: 仓库里没有随包产物");
        return;
    }
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(payload.join("MANIFEST.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let entries = manifest["files"].as_array().expect("manifest.files");
    assert!(!entries.is_empty(), "清单里没有文件");
    let mut listed: Vec<String> = Vec::new();
    for entry in entries {
        let rel = entry["path"].as_str().expect("manifest path");
        let expected_bytes = entry["bytes"].as_u64().expect("manifest bytes") as usize;
        let expected_sha = entry["sha256"].as_str().expect("manifest sha256");
        let bytes = fs::read(payload.join(rel)).unwrap_or_else(|error| {
            panic!("{rel} 缺失（清单 expected={expected_bytes} 字节）：{error}")
        });
        assert_eq!(
            bytes.len(),
            expected_bytes,
            "{rel} 大小与清单不符：expected={expected_bytes} actual={}",
            bytes.len()
        );
        assert_eq!(
            sha256_hex(&bytes),
            expected_sha,
            "{rel} sha256 与清单不符：expected={expected_sha} actual={}",
            sha256_hex(&bytes)
        );
        listed.push(rel.to_string());
    }
    // 反向：产物里不能有清单没登记的文件（清单必须描述全这份产物）。
    for rel in list_files(&payload) {
        if rel == "MANIFEST.json" || rel == ".gitignore" {
            continue;
        }
        assert!(
            listed.iter().any(|item| item == &rel),
            "{rel} 没登记在清单里（清单与产物对不上）"
        );
    }
}

/// 2) build/ 版本对不上 → 换成新版本，且**不算失败**；当前版本的编译产物不许动。
///
/// 这是现场那条链路的另一半：旧版引擎产物留在用户目录里 → schema-stale →
/// build_ok=false → deploy_result=partial。现在旧版本会被换成新版本。
#[test]
fn stale_build_artifact_is_overwritten_and_not_failed() {
    let fixture = Fixture::new("stale-build");
    // 旧版本留下的产物：没有插件标记、bin 也是旧的。
    fixture.write(
        "rime/build/luna_pinyin_simp.schema.yaml",
        "# luna_pinyin_simp v1\n",
    );
    fixture.write(
        "rime/build/luna_pinyin_simp.prism.bin",
        "bin luna_pinyin_simp v1",
    );
    // 另一个方案是当前版本：它的编译产物（用户词库）不许被覆盖。
    fixture.write(
        "rime/build/wubi86.schema.yaml",
        &format!("# wubi86 v2\n{MARKED}"),
    );
    fixture.write("rime/build/wubi86.prism.bin", "user dictionary bin");

    let report = fixture.install();

    assert!(
        report.manifest_failed.is_empty(),
        "清单与 payload 一致，不该报失败：{:?}",
        report.manifest_failed
    );
    assert_eq!(
        report.failed, 0,
        "铺制不该有失败（failed={}）",
        report.failed
    );
    assert!(
        report.overwritten >= 2,
        "旧版本应被覆盖成新版本：overwritten={}",
        report.overwritten
    );
    let schema = fixture.read("rime/build/luna_pinyin_simp.schema.yaml");
    assert!(
        schema.contains("# luna_pinyin_simp v2"),
        "旧 schema.yaml 没换成新版本：{schema}"
    );
    assert_eq!(
        fixture.read("rime/build/luna_pinyin_simp.prism.bin"),
        "bin luna_pinyin_simp v2",
        "旧方案的编译产物应跟着换新"
    );
    assert_eq!(
        fixture.read("rime/build/wubi86.prism.bin"),
        "user dictionary bin",
        "当前版本的编译产物（用户词库）不许被覆盖"
    );
}

/// 3) config/ 里用户已有的配置：保留、不覆盖、不算失败。
#[test]
fn user_config_is_kept_and_never_failed() {
    let fixture = Fixture::new("config-keep");
    let user_config = "patch:\n  schema_list:\n    - luna_pinyin_simp\n  __mine: keep\n";
    fixture.write("rime/default.custom.yaml", user_config);

    let report = fixture.install();

    assert_eq!(
        report.failed, 0,
        "铺制不该有失败（failed={}）",
        report.failed
    );
    assert!(
        report.config_kept >= 1,
        "用户已有的配置应被保留：config_kept={}",
        report.config_kept
    );
    assert_eq!(
        fixture.read("rime/default.custom.yaml"),
        user_config,
        "用户配置被覆盖了"
    );
}

/// 4) 干净安装：结论必须是 ok，且没有 copy_failed / manifest_failed。
#[test]
fn clean_install_ends_ok_without_copy_failed() {
    let fixture = Fixture::new("clean-install");
    let options = Fixture::options(Mode::Install);
    let outcome = ime_state::ensure(
        &fixture.rime(),
        Some(&fixture.payload()),
        &[fixture.data()],
        &options,
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Ok,
        "干净安装应成功：detail={}",
        outcome.detail
    );
    assert!(
        !outcome.detail.contains("copy_failed="),
        "干净安装不该有 copy_failed：detail={}",
        outcome.detail
    );
    assert!(
        !outcome.detail.contains("manifest_failed="),
        "干净安装不该有 manifest_failed：detail={}",
        outcome.detail
    );
}

/// 5) 从旧版本升级：旧的引擎产物换新，结论必须是 ok（不是 partial/degraded）。
#[test]
fn upgrade_from_old_version_ends_ok_without_copy_failed() {
    let fixture = Fixture::new("upgrade");
    for id in SCHEMAS {
        fixture.write(
            &format!("rime/build/{id}.schema.yaml"),
            &format!("# {id} v1\n"),
        );
        fixture.write(
            &format!("rime/build/{id}.prism.bin"),
            &format!("bin {id} v1"),
        );
    }
    fixture.write("rime/build/default.yaml", "# default v1\n");
    // wubi86 已经是当前版本：它的编译产物（用户词库）不许被顺带覆盖。
    fixture.write(
        "rime/build/wubi86.schema.yaml",
        &format!("# wubi86 v2\n{MARKED}"),
    );
    fixture.write("rime/build/wubi86.prism.bin", "user dictionary bin");

    let options = Fixture::options(Mode::Install);
    let outcome = ime_state::ensure(
        &fixture.rime(),
        Some(&fixture.payload()),
        &[fixture.data()],
        &options,
    );
    assert_eq!(
        outcome.result,
        OutcomeResult::Ok,
        "升级安装应成功（不能是 partial/degraded）：detail={}",
        outcome.detail
    );
    assert!(
        !outcome.detail.contains("copy_failed="),
        "升级安装不该有 copy_failed：detail={}",
        outcome.detail
    );
    assert!(
        !outcome.detail.contains("manifest_failed="),
        "升级安装不该有 manifest_failed：detail={}",
        outcome.detail
    );
    let schema = fixture.read("rime/build/luna_pinyin_simp.schema.yaml");
    assert!(
        schema.contains("# luna_pinyin_simp v2"),
        "旧引擎产物没换成新版本：{schema}"
    );
    assert_eq!(
        fixture.read("rime/build/wubi86.prism.bin"),
        "user dictionary bin",
        "当前版本的编译产物（用户词库）不许被顺带覆盖"
    );
}

/// 6) sha256 严格性不放宽：payload 文件与清单对不上时，不许拿它覆盖用户目录里
///    的好文件，且必须报出来（manifest_failed）。
#[test]
fn corrupt_payload_does_not_overwrite_user_build() {
    let fixture = Fixture::new("corrupt");
    // 用户目录里是好的当前版本。
    fixture.write(
        "rime/build/luna_pinyin_simp.schema.yaml",
        &format!("# luna_pinyin_simp v2\n{MARKED}"),
    );
    fixture.write(
        "rime/build/luna_pinyin_simp.prism.bin",
        "bin luna_pinyin_simp v2",
    );
    // payload 那份被改过：清单对不上（模拟产物被截断/篡改）。
    // 故意**只改一个字节**（长度不变）：这样只有 sha256 校验能抓住它。
    fixture.write(
        "payload/build/luna_pinyin_simp.schema.yaml",
        &format!("# luna_pinyin_simp v3\n{MARKED}"),
    );

    let report = fixture.install();

    assert!(
        !report.manifest_failed.is_empty(),
        "payload 与清单对不上必须报出来（sha256 严格性不放宽）"
    );
    assert!(
        report
            .manifest_failed
            .iter()
            .any(|item| item.contains("luna_pinyin_simp.schema.yaml") && item.contains("sha256")),
        "失败信息必须点名文件与 sha256：{:?}",
        report.manifest_failed
    );
    assert!(
        report.failed >= 1,
        "清单对不上的 payload 要算失败：failed={}",
        report.failed
    );
    assert!(
        fixture
            .read("rime/build/luna_pinyin_simp.schema.yaml")
            .contains("# luna_pinyin_simp v2"),
        "用户目录里好的产物不许被可疑 payload 覆盖"
    );
}

/// 7) 没有 MANIFEST.json 的旧版 payload：退回「缺失才补」，不覆盖已有产物。
#[test]
fn payload_without_manifest_keeps_existing_build_files() {
    let fixture = Fixture::new("legacy");
    fs::remove_file(fixture.payload().join("MANIFEST.json")).expect("remove manifest");
    // 用户目录里那份与 payload 不同（但本身可用：带标记、bin 在位）。
    fixture.write(
        "rime/build/luna_pinyin_simp.schema.yaml",
        &format!("# luna_pinyin_simp user-build\n{MARKED}"),
    );
    fixture.write(
        "rime/build/luna_pinyin_simp.prism.bin",
        "bin luna_pinyin_simp user",
    );

    let report = fixture.install();

    assert_eq!(
        report.failed, 0,
        "铺制不该有失败（failed={}）",
        report.failed
    );
    assert!(
        fixture
            .read("rime/build/luna_pinyin_simp.schema.yaml")
            .contains("# luna_pinyin_simp user-build"),
        "没有清单时不该覆盖用户已有的产物"
    );
    assert_eq!(
        fixture.read("rime/build/luna_pinyin_simp.prism.bin"),
        "bin luna_pinyin_simp user",
        "没有清单时不该覆盖用户已有的编译产物"
    );
}

/// 8) 引擎只改 __build_info 构建戳时不算「版本变了」：用户的产物不该被反复重写。
#[test]
fn build_info_only_difference_is_not_a_version_change() {
    let fixture = Fixture::new("build-info");
    let payload = fixture.read("payload/build/luna_pinyin_simp.schema.yaml");
    fixture.write(
        "rime/build/luna_pinyin_simp.schema.yaml",
        &format!("__build_info: compiled at 2026-11-08 12:00:00\n{payload}"),
    );

    let report = fixture.install();

    assert_eq!(
        report.failed, 0,
        "铺制不该有失败（failed={}）",
        report.failed
    );
    assert_eq!(
        report.overwritten, 0,
        "只有 __build_info 不同不算版本变化，不该重写用户的产物"
    );
    assert!(
        fixture
            .read("rime/build/luna_pinyin_simp.schema.yaml")
            .contains("__build_info: compiled at 2026-11-08 12:00:00"),
        "用户的产物被重写了"
    );
}
