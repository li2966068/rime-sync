//! 预编译 build 产物自举。
//!
//! 为什么在 core：桌面端的启动自举与 rime-sync-cli fix-ime 必须共用同一套逻辑。
//! 0.6.15 实测 /S 静默安装时 NSIS 钩子根本没执行，WeaselDeployer /deploy 也可能
//! 返回 0 却不写任何 build 文件，所以兜底只能由程序自己跑。
//!
//! 可用性判据与安装器一致：build/luna_pinyin.schema.yaml 存在，且同时含
//! rimesync_vnum 与 rimesync_punct（随包 custom.yaml 才有的插件标记）。
//! 只有文件、没有标记 = 官方部署产物，不能直接用。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 判据文件：0.6.21 起随包只保留 3 个方案，luna_pinyin_simp 是其中的默认方案
/// （luna_pinyin 只是 __include 基类，不再带 rimesync 插件标记）。
pub const PROBE_FILE: &str = "luna_pinyin_simp.schema.yaml";
/// 随包 custom.yaml 才有的插件标记；官方部署产物不含。
pub const MARKERS: [&str; 2] = ["rimesync_vnum", "rimesync_punct"];
/// 调试用：直接指定预编译产物目录（优先于所有平台路径）。
pub const ENV_PAYLOAD_DIR: &str = "RIMESYNC_PREBUILT_DIR";
/// 与安装器一致的开关：0 = 关闭自举，force = 覆盖已存在的 build 文件。
pub const ENV_MODE: &str = "RIMESYNC_PREBUILT";
/// 自举记录：每次运行追加，供用户排障（不进同步范围）。
const LOG_FILE: &str = ".rimesync-prebuilt.log";

/// 用户目录里 build 产物的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildProbe {
    /// build/ 里连判据文件都没有。
    None,
    /// 有判据文件但没有插件标记 → 官方部署产物，打不出我们这套字。
    Deploy,
    /// 判据文件含两个标记 → 我们这套产物，可用。
    Prebuilt,
}

impl BuildProbe {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Deploy => "deploy",
            Self::Prebuilt => "prebuilt",
        }
    }

    pub fn usable(self) -> bool {
        matches!(self, Self::Prebuilt)
    }
}

impl std::fmt::Display for BuildProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 预编译产物目录及其来源（日志 / 界面展示用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PayloadLocation {
    pub path: PathBuf,
    pub source: String,
}

/// 一次自举的结果。copied / kept / overwritten 只统计 build/；
/// config 单列在 config_copied / config_kept（配置永远只补缺，不覆盖）。
#[derive(Debug, Clone, Serialize)]
pub struct PrebuiltReport {
    pub copied: usize,
    pub kept: usize,
    pub overwritten: usize,
    pub config_copied: usize,
    pub config_kept: usize,
    pub failed: usize,
    /// 产物目录（payload_dir 的绝对/原样路径）。
    pub source: String,
    pub build_ok: bool,
    /// 铺完之后的判据，none / deploy / prebuilt。
    pub probe_after: String,
    /// 是否触发了「build/ 覆盖一次」的第二次机会。
    pub escalated: bool,
    /// 本次自举追加的日志文件；写不进去时为 None（不影响铺文件）。
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// 覆盖已存在的 build/ 文件（等价安装器的 RIMESYNC_PREBUILT=force）。
    pub force: bool,
}

/// 读 build/luna_pinyin.schema.yaml 判断产物是否是我们这套。
pub fn probe_build(rime_user_dir: &Path) -> BuildProbe {
    let path = rime_user_dir.join("build").join(PROBE_FILE);
    let Ok(bytes) = fs::read(&path) else {
        return BuildProbe::None;
    };
    let text = String::from_utf8_lossy(&bytes);
    if MARKERS.iter().all(|marker| text.contains(marker)) {
        BuildProbe::Prebuilt
    } else {
        BuildProbe::Deploy
    }
}

/// 定位随包的预编译产物：环境变量 > 安装目录 > AppImage / deb 路径。
pub fn resolve_payload(exe_dir: Option<&Path>) -> Result<PayloadLocation> {
    resolve_payload_with(exe_dir, env::var_os(ENV_PAYLOAD_DIR).map(PathBuf::from))
}

fn resolve_payload_with(
    exe_dir: Option<&Path>,
    override_dir: Option<PathBuf>,
) -> Result<PayloadLocation> {
    if let Some(path) = override_dir {
        if path.join("build").is_dir() {
            return Ok(PayloadLocation {
                path,
                source: format!("env:{ENV_PAYLOAD_DIR}"),
            });
        }
        bail!(
            "{ENV_PAYLOAD_DIR}={} 下没有 build/ 子目录，无法作为预编译产物",
            path.display()
        );
    }

    let mut candidates: Vec<(PathBuf, &str)> = Vec::new();
    if let Some(dir) = exe_dir {
        // Windows NSIS：<安装目录>/resources/prebuilt。
        candidates.push((
            dir.join("resources").join("prebuilt"),
            "exe:resources/prebuilt",
        ));
        // Tauri 在 Linux 把 resources 放进 <prefix>/lib/<product>/resources。
        if let Some(prefix) = dir.parent() {
            candidates.push((
                prefix
                    .join("lib")
                    .join("rime-sync")
                    .join("resources")
                    .join("prebuilt"),
                "exe:../lib/rime-sync/resources/prebuilt",
            ));
        }
    }
    if let Some(raw) = env::var_os("APPDIR") {
        let appdir = PathBuf::from(raw);
        candidates.push((
            appdir.join("usr/share/rime-sync/prebuilt"),
            "appimage:usr/share/rime-sync/prebuilt",
        ));
        candidates.push((
            appdir.join("usr/lib/rime-sync/resources/prebuilt"),
            "appimage:usr/lib/rime-sync/resources/prebuilt",
        ));
    }
    #[cfg(unix)]
    candidates.push((
        PathBuf::from("/usr/share/rime-sync/prebuilt"),
        "linux:usr/share/rime-sync/prebuilt",
    ));

    for (path, source) in &candidates {
        if path.join("build").is_dir() {
            return Ok(PayloadLocation {
                path: path.clone(),
                source: (*source).to_string(),
            });
        }
    }
    let checked = candidates
        .iter()
        .map(|(path, _)| format!("  - {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    bail!("未找到预编译产物目录（已检查：\n{checked}\n）；可用 {ENV_PAYLOAD_DIR} 指定");
}

/// 逐文件铺入预编译产物：build/ 缺才铺，config 永不覆盖。
pub fn install_prebuilt(payload_dir: &Path, rime_user_dir: &Path) -> Result<PrebuiltReport> {
    install_prebuilt_with(payload_dir, rime_user_dir, InstallOptions::default())
}

pub fn install_prebuilt_with(
    payload_dir: &Path,
    rime_user_dir: &Path,
    options: InstallOptions,
) -> Result<PrebuiltReport> {
    let build_src = payload_dir.join("build");
    if !build_src.is_dir() {
        bail!(
            "预编译产物不完整：{} 下没有 build/ 目录（检查安装包的 resources/prebuilt，或用 {ENV_PAYLOAD_DIR} 指定）",
            payload_dir.display()
        );
    }
    if !rime_user_dir.is_dir() {
        bail!("Rime 用户目录不存在：{}", rime_user_dir.display());
    }

    let before = probe_build(rime_user_dir);
    let mut report = PrebuiltReport {
        copied: 0,
        kept: 0,
        overwritten: 0,
        config_copied: 0,
        config_kept: 0,
        failed: 0,
        source: payload_dir.display().to_string(),
        build_ok: false,
        probe_after: before.as_str().to_string(),
        escalated: false,
        log_path: None,
    };
    let mut lines = vec![format!(
        "start payload={} rime={} force={} probe_before={}",
        payload_dir.display(),
        rime_user_dir.display(),
        options.force,
        before
    )];

    // 第一遍：build/ 缺才铺。
    copy_tree(
        &build_src,
        &rime_user_dir.join("build"),
        options.force,
        false,
        &mut report,
        &mut lines,
    )?;
    // config/*.yaml 是用户配置：只补缺失的；force 也不覆盖。
    copy_tree(
        &payload_dir.join("config"),
        rime_user_dir,
        false,
        true,
        &mut report,
        &mut lines,
    )?;

    // 第二次机会：build/ 是派生数据，旧产物（例如官方部署、没有插件标记）会挡住
    // 「缺才铺」。这时只对 build/ 覆盖一次；config 绝不参与。
    if !probe_build(rime_user_dir).usable() && !options.force {
        report.escalated = true;
        lines.push("retry scope=build-only reason=probe-after-skip-only".to_string());
        copy_tree(
            &build_src,
            &rime_user_dir.join("build"),
            true,
            false,
            &mut report,
            &mut lines,
        )?;
    }

    let after = probe_build(rime_user_dir);
    report.probe_after = after.as_str().to_string();
    report.build_ok = after.usable();
    lines.push(format!(
        "end probe_after={} copied={} kept={} overwritten={} config_copied={} config_kept={} failed={} escalated={} build_ok={}",
        after,
        report.copied,
        report.kept,
        report.overwritten,
        report.config_copied,
        report.config_kept,
        report.failed,
        report.escalated,
        report.build_ok
    ));
    report.log_path = write_log(rime_user_dir, &lines);
    Ok(report)
}

/// 逐文件复制（不递归）：payload 的 build/ 与 config/ 都是平铺的。
fn copy_tree(
    src: &Path,
    dst: &Path,
    force: bool,
    only_yaml: bool,
    report: &mut PrebuiltReport,
    lines: &mut Vec<String>,
) -> Result<()> {
    if !src.is_dir() {
        // config/ 可能整目录不存在：不是错误。
        return Ok(());
    }
    fs::create_dir_all(dst).with_context(|| format!("创建目录 {}", dst.display()))?;
    let mut files: Vec<PathBuf> = fs::read_dir(src)
        .with_context(|| format!("读取目录 {}", src.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| !only_yaml || is_yaml(path))
        .collect();
    files.sort();

    for path in files {
        let Some(name) = path.file_name() else {
            continue;
        };
        let target = dst.join(name);
        let existed = target.is_file();
        if existed && !force {
            if only_yaml {
                report.config_kept += 1;
            } else {
                report.kept += 1;
            }
            continue;
        }
        match fs::copy(&path, &target) {
            Ok(_) => {
                if existed {
                    report.overwritten += 1;
                    lines.push(format!("overwrite {}", target.display()));
                } else if only_yaml {
                    report.config_copied += 1;
                } else {
                    report.copied += 1;
                }
            }
            Err(error) => {
                report.failed += 1;
                lines.push(format!("failed {}: {error}", target.display()));
            }
        }
    }
    Ok(())
}

fn is_yaml(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
        .unwrap_or(false)
}

/// 追加一条自举记录到 <rime 用户目录>/.rimesync-prebuilt.log。
/// 写不进去只返回 None：日志不能反过来让铺文件失败。
fn write_log(rime_user_dir: &Path, lines: &[String]) -> Option<String> {
    let path = rime_user_dir.join(LOG_FILE);
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut text = String::new();
    for line in lines {
        text.push_str(&format!("[{seconds}] {line}\n"));
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    file.write_all(text.as_bytes()).ok()?;
    Some(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("rime-sync-prebuilt-{}-{seq}", std::process::id()));
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
    const PLAIN: &str = "name: luna_pinyin\nversion: '1.0'\n";

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, text).expect("write file");
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).expect("read file")
    }

    /// 造一份最小 payload：build 两个文件 + config 一个 yaml。
    fn make_payload(root: &Path, marked: bool) {
        write(
            &root.join("build").join(PROBE_FILE),
            if marked { MARKED } else { PLAIN },
        );
        write(&root.join("build").join("luna_pinyin.prism.bin"), "prism");
        write(
            &root.join("config").join("default.custom.yaml"),
            "patch:\n  schema_list: []\n",
        );
    }

    #[test]
    fn probe_reports_none_deploy_and_prebuilt() {
        let tmp = TempDir::new();
        assert_eq!(probe_build(tmp.path()), BuildProbe::None);

        write(&tmp.path().join("build").join(PROBE_FILE), PLAIN);
        assert_eq!(probe_build(tmp.path()), BuildProbe::Deploy);

        write(&tmp.path().join("build").join(PROBE_FILE), MARKED);
        assert_eq!(probe_build(tmp.path()), BuildProbe::Prebuilt);
    }

    #[test]
    fn copies_missing_files_and_reports_ok() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("rime dir");
        make_payload(&payload, true);

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(report.build_ok);
        assert_eq!(report.copied, 2);
        assert_eq!(report.kept, 0);
        assert_eq!(report.overwritten, 0);
        assert_eq!(report.config_copied, 1);
        assert_eq!(report.config_kept, 0);
        assert_eq!(report.failed, 0);
        assert_eq!(report.probe_after, "prebuilt");
        assert!(report.log_path.is_some());
        assert!(rime.join("build").join(PROBE_FILE).is_file());
        assert!(rime.join("build").join("luna_pinyin.prism.bin").is_file());
        assert!(rime.join("default.custom.yaml").is_file());
    }

    #[test]
    fn keeps_existing_files() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_payload(&payload, true);
        // 已经是可用的本机产物：判据文件含两个标记，第一遍就该跳过。
        const LOCAL_MARKED: &str = "local:\n  rimesync_vnum: 1\n  rimesync_punct: 1\n";
        write(&rime.join("build").join(PROBE_FILE), LOCAL_MARKED);
        write(&rime.join("default.custom.yaml"), "patch:\n  local: true\n");

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(report.build_ok);
        assert_eq!(report.copied, 1, "只缺 prism.bin");
        assert_eq!(report.kept, 1);
        assert_eq!(report.overwritten, 0);
        assert_eq!(report.config_copied, 0);
        assert_eq!(report.config_kept, 1);
        assert_eq!(read(&rime.join("build").join(PROBE_FILE)), LOCAL_MARKED);
        assert!(read(&rime.join("default.custom.yaml")).contains("local: true"));
    }

    #[test]
    fn old_build_without_markers_triggers_build_overwrite_only() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_payload(&payload, true);
        // 旧部署产物：文件都在，但没有插件标记。
        write(&rime.join("build").join(PROBE_FILE), PLAIN);
        write(
            &rime.join("build").join("luna_pinyin.prism.bin"),
            "old prism",
        );
        write(&rime.join("default.custom.yaml"), "patch:\n  keep: me\n");

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(report.escalated);
        assert!(report.build_ok);
        assert_eq!(report.probe_after, "prebuilt");
        assert_eq!(report.overwritten, 2, "build/ 两个文件都覆盖");
        assert_eq!(report.config_copied, 0, "config 永不覆盖");
        assert!(read(&rime.join("build").join(PROBE_FILE)).contains("rimesync_punct"));
        assert!(read(&rime.join("default.custom.yaml")).contains("keep: me"));
    }

    #[test]
    fn force_overwrites_build_but_never_config() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_payload(&payload, true);
        write(&rime.join("build").join(PROBE_FILE), MARKED);
        write(&rime.join("build").join("luna_pinyin.prism.bin"), "old");
        write(&rime.join("default.custom.yaml"), "patch:\n  keep: me\n");

        let report = install_prebuilt_with(&payload, &rime, InstallOptions { force: true })
            .expect("install");

        assert!(report.build_ok);
        assert!(!report.escalated, "force 不需要第二次机会");
        assert_eq!(report.overwritten, 2);
        assert_eq!(report.kept, 0);
        assert_eq!(report.config_copied, 0);
        assert_eq!(report.config_kept, 1);
        assert_eq!(
            read(&rime.join("build").join("luna_pinyin.prism.bin")),
            "prism"
        );
        assert!(read(&rime.join("default.custom.yaml")).contains("keep: me"));
    }

    #[test]
    fn missing_payload_build_is_readable_error() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&payload).expect("payload dir");
        fs::create_dir_all(&rime).expect("rime dir");

        let error = install_prebuilt(&payload, &rime).expect_err("should fail");

        let text = format!("{error:#}");
        assert!(text.contains("build/"), "错误要指出缺什么：{text}");
        assert!(text.contains("预编译产物"), "错误要可读：{text}");
    }

    #[test]
    fn resolves_exe_relative_payload() {
        let tmp = TempDir::new();
        write(
            &tmp.path()
                .join("resources")
                .join("prebuilt")
                .join("build")
                .join(PROBE_FILE),
            MARKED,
        );

        let located = resolve_payload_with(Some(tmp.path()), None).expect("locate");

        assert_eq!(located.source, "exe:resources/prebuilt");
        assert_eq!(located.path, tmp.path().join("resources").join("prebuilt"));
    }

    #[test]
    fn env_override_must_point_at_a_payload() {
        let tmp = TempDir::new();
        let error = resolve_payload_with(None, Some(tmp.path().to_path_buf())).expect_err("empty");

        assert!(format!("{error:#}").contains(ENV_PAYLOAD_DIR));
    }
}
