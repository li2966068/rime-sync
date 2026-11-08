//! 预编译 build 产物自举。
//!
//! 为什么在 core：桌面端的启动自举与 rime-sync-cli fix-ime 必须共用同一套逻辑。
//! 0.6.15 实测 /S 静默安装时 NSIS 钩子根本没执行，WeaselDeployer /deploy 也可能
//! 返回 0 却不写任何 build 文件，所以兜底只能由程序自己跑。
//!
//! 可用性判据与安装器一致：build/luna_pinyin.schema.yaml 存在，且同时含
//! rimesync_vnum 与 rimesync_punct（随包 custom.yaml 才有的插件标记）。
//! 只有文件、没有标记 = 官方部署产物，不能直接用。
//!
//! 0.6.23 起不再「只看一个判据文件」：按 `retained-schemas.txt` + 依赖闭包逐个方案
//! 校验 `<id>.schema.yaml`（主方案还要求 `rimesync_vnum`）与配套
//! `*.prism.bin` / `*.table.bin`，缺什么从 `<exe>/resources/prebuilt/build` 补什么。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::{data_patch, deploy, ime_state, prune};

/// 判据文件：0.6.21 起随包只保留 3 个方案，luna_pinyin_simp 是其中的默认方案
/// （luna_pinyin 只是 __include 基类，不再带 rimesync 插件标记）。
pub const PROBE_FILE: &str = "luna_pinyin_simp.schema.yaml";
/// 随包 custom.yaml 才有的插件标记；官方部署产物不含。
pub const MARKERS: [&str; 2] = ["rimesync_vnum", "rimesync_punct"];
/// 单方案状态：本来就好 / 本次补齐 / 补不齐。
pub const STATE_OK: &str = "ok";
pub const STATE_REPAIRED: &str = "repaired";
pub const STATE_MISSING: &str = "missing";

/// 目标方案：retained-schemas.txt 里的主方案 + 依赖闭包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSchema {
    pub id: String,
    /// 主方案 = true：build/<id>.schema.yaml 必须含 rimesync_vnum。
    pub primary: bool,
}

/// 单个方案的 build 产物状态（报告字段 build_schemas 的元素）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaBuild {
    pub id: String,
    pub primary: bool,
    /// ok / repaired / missing（见 STATE_*）。
    pub state: String,
}

impl SchemaBuild {
    /// 只要不是 missing 就算可用。
    pub fn ok(&self) -> bool {
        self.state != STATE_MISSING
    }
}

/// 目标方案集合 = retained-schemas.txt ∪ 依赖闭包（复用 prune::keep_set）。
///
/// 依赖方案同样要校验（<id>.schema.yaml + 至少一个 prism/table/reverse.bin）；
/// 纯 __include 基类（部署器只合并、不落盘 <id>.schema.yaml，例如 luna_pinyin）
/// 在用户目录与 payload 里都没有 schema.yaml 时视为「本就不需要」，不参与判定。
pub fn target_schemas(data_dirs: &[PathBuf]) -> Vec<TargetSchema> {
    let mut out: Vec<TargetSchema> = prune::retained_schemas()
        .iter()
        .map(|id| TargetSchema {
            id: (*id).to_string(),
            primary: true,
        })
        .collect();
    let known = prune::known_schemas(data_dirs);
    let (extra, _) = prune::env_keep();
    for id in prune::keep_set(&known, &extra) {
        if prune::is_retained(&id) {
            continue;
        }
        out.push(TargetSchema { id, primary: false });
    }
    out
}

/// build/<id>.schema.yaml 可用？主方案还要求插件标记。
///
/// 0.6.23：判据只剩一处 —— ime_state::schema_state_in。这里只是薄封装，
/// 保留是为了不动所有调用点（铺制 / 回退 / 报告）。
fn schema_ready(build: &Path, target: &TargetSchema) -> bool {
    ime_state::schema_ok_in(build, &target.id, target.primary)
}

/// 目标方案集合（保留清单 ∪ 依赖闭包，且这一份 payload / 用户目录里真的有它）。
///
/// 0.6.23：豁免规则搬到 ime_state::KeepSet，这里只做类型转换。
fn checked_targets(
    rime_user_dir: &Path,
    data_dirs: &[PathBuf],
    payload_dir: Option<&Path>,
) -> Vec<TargetSchema> {
    ime_state::KeepSet::resolve(rime_user_dir, data_dirs, payload_dir)
        .targets()
        .iter()
        .map(|target| TargetSchema {
            id: target.id.clone(),
            primary: target.primary,
        })
        .collect()
}

/// 只读：按方案检查 build 产物（不写任何文件）。
///
/// 0.6.23：判据与 ime_state::assess 同源（这里只返回 ok/missing 的扁平列表，
/// 给报告与调试用；决策一律走 ime_state::ensure）。
/// payload_dir 可以是 None：此时无法判断「payload 里有、用户目录里没有」的
/// 依赖方案，只靠用户目录自身判定。
pub fn check_build(
    rime_user_dir: &Path,
    payload_dir: Option<&Path>,
    data_dirs: &[PathBuf],
) -> Vec<SchemaBuild> {
    let user_build = rime_user_dir.join("build");
    checked_targets(rime_user_dir, data_dirs, payload_dir)
        .into_iter()
        .map(|target| SchemaBuild {
            state: if schema_ready(&user_build, &target) {
                STATE_OK
            } else {
                STATE_MISSING
            }
            .to_string(),
            primary: target.primary,
            id: target.id,
        })
        .collect()
}

/// 从 payload 补齐单个方案缺的文件（只补缺失；主方案 schema.yaml 缺标记时覆盖一次）。
fn repair_schema(
    payload_build: &Path,
    user_build: &Path,
    target: &TargetSchema,
) -> (usize, Vec<String>) {
    let mut copied = 0;
    let mut errors = Vec::new();
    for suffix in ["schema.yaml", "prism.bin", "table.bin", "reverse.bin"] {
        let name = format!("{}.{suffix}", target.id);
        let source = payload_build.join(&name);
        if !source.is_file() {
            continue;
        }
        let destination = user_build.join(&name);
        let needed = if !destination.is_file() {
            true
        } else if suffix == "schema.yaml" {
            // 存在的 schema.yaml 只有主方案才判标记：缺标记 = 官方部署产物，换掉。
            target.primary && !ime_state::has_marker(&destination)
        } else {
            false
        };
        if !needed {
            continue;
        }
        match fs::copy(&source, &destination) {
            Ok(_) => copied += 1,
            Err(error) => errors.push(format!("{}: {error}", destination.display())),
        }
    }
    (copied, errors)
}

/// 调试用：直接指定预编译产物目录（优先于所有平台路径）。
pub const ENV_PAYLOAD_DIR: &str = "RIMESYNC_PREBUILT_DIR";
/// 与安装器一致的开关：0 = 关闭自举，force = 覆盖已存在的 build 文件。
pub const ENV_MODE: &str = "RIMESYNC_PREBUILT";
/// 自举记录：每次运行追加，供用户排障（不进同步范围）。
const LOG_FILE: &str = ".rimesync-prebuilt.log";
/// 预编译产物的文件清单（gen-prebuilt-build.py 产出）：按 sha256 校验 payload。
pub const MANIFEST_FILE: &str = "MANIFEST.json";

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
    /// config/ 里被 data 门禁挡下的模板（data 里没有该方案 → 写了会让整次部署失败）。
    pub config_skipped: Vec<String>,
    /// MANIFEST.json 里 sha256 对不上的文件（payload 被截断/篡改）。
    /// 进 problems → 结论降级为 partial，但不中断安装。
    pub manifest_failed: Vec<String>,
    pub failed: usize,
    /// 产物目录（payload_dir 的绝对/原样路径）。
    pub source: String,
    pub build_ok: bool,
    /// 铺完之后的判据，none / deploy / prebuilt。
    pub probe_after: String,
    /// 与 probe_after 同义，供安装器/CLI 用同一个字段名报告（build_source=）。
    pub build_source: String,
    /// 是否触发了「build/ 覆盖一次」的第二次机会。
    pub escalated: bool,
    /// 本次自举追加的日志文件；写不进去时为 None（不影响铺文件）。
    pub log_path: Option<String>,
    /// 按方案检查结果（只列主方案：retained-schemas.txt 的 3 个）。
    pub build_schemas: Vec<SchemaBuild>,
    /// 本次从 payload 补齐的方案（含依赖方案）。
    pub build_repaired: Vec<String>,
    /// 补不齐的方案（payload 也缺文件）。
    pub build_missing: Vec<String>,
    /// 部署手段：none / prebuilt / auto / inproc（见 deploy::DeployMethod）。
    pub deploy_method: String,
    /// ok / partial / failed / skipped（partial = 部署完成但有降级，见 deploy::RESULT_*）。
    pub deploy_result: String,
    pub deploy_ok: bool,
    /// 收尾（touch data + 结束服务）耗时。
    pub deploy_ms: u64,
    /// 明细（决策理由 + 每一步的结果）。
    pub deploy_detail: String,
    /// 进程内部署的一句话结论（没跑时为 "-"）。
    pub deploy_inproc: String,
}

#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// 覆盖已存在的 build/ 文件（等价安装器的 RIMESYNC_PREBUILT=force）。
    pub force: bool,
    /// data 目录（None = 现场探测）。ime_state::ensure 传进来的是**装完输入法之后**
    /// 重新解析的那一份：全新机器上调用方解析时 Weasel 还没装，用旧结果做门禁会把
    /// config 模板整份跳过（M1）。
    pub data_dirs: Option<Vec<PathBuf>>,
}

/// config/ 里受 data 门禁的模板（与 ime_state::seed_templates 同一条规则）。
pub const CONFIG_DEFAULT: &str = "default.custom.yaml";
pub const CONFIG_WEASEL: &str = "weasel.custom.yaml";
pub const CONFIG_SCHEMA_SUFFIX: &str = ".custom.yaml";
/// 默认模板要求 data 里真有这个方案（五笔），否则 librime 部署会把缺失方案计成
/// failure、整次部署失败。
pub const CONFIG_GATE_SCHEMA: &str = "wubi86";

/// data 里有没有 <id>.schema.yaml。
pub fn data_has_schema(data_dirs: &[PathBuf], id: &str) -> bool {
    data_dirs
        .iter()
        .any(|dir| dir.join(format!("{id}.schema.yaml")).is_file())
}

/// payload config/ 的门禁：None = 可以铺，Some(原因) = 跳过。
///
/// 规则与 ime_state::seed_templates 完全一致：模板列了 data 里没有的方案时，
/// 写进用户目录会让 librime 的部署整次失败。未知文件（README 之类）不拦。
pub fn config_gate_skip(name: &str, data_dirs: &[PathBuf]) -> Option<String> {
    if name == CONFIG_DEFAULT {
        return (!data_has_schema(data_dirs, CONFIG_GATE_SCHEMA))
            .then(|| format!("no-{CONFIG_GATE_SCHEMA}-in-data"));
    }
    if name == CONFIG_WEASEL {
        // 外观模板不依赖任何方案。
        return None;
    }
    let id = name.strip_suffix(CONFIG_SCHEMA_SUFFIX)?;
    (!data_has_schema(data_dirs, id)).then(|| format!("no-{id}.schema.yaml-in-data"))
}

/// 读 build/ 判断产物是否是我们这套。
///
/// 0.6.23：只是 ime_state::assess 的薄封装 —— 判据只有一处，不再「这里看双标记、
/// 那里看单标记 + bin」。语义保持兼容：
/// * 没有任何 schema.yaml → none；
/// * 有文件但不可用（缺标记 / 缺 bin / 缺方案）→ deploy（官方部署产物）；
/// * 全部目标方案可用 → prebuilt。
pub fn probe_build(rime_user_dir: &Path) -> BuildProbe {
    let retained = ime_state::KeepSet::resolve(rime_user_dir, &[], None);
    let assessment = ime_state::assess(rime_user_dir, &[], &retained);
    if assessment.build_ok {
        return BuildProbe::Prebuilt;
    }
    if assessment
        .schemas
        .iter()
        .any(|schema| schema.schema_yaml != ime_state::State::Missing)
    {
        BuildProbe::Deploy
    } else {
        BuildProbe::None
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

impl PrebuiltReport {
    /// 空报告：ime_state::ensure 在「不铺也够用 / 铺失败」时用它。
    ///
    /// 字段初值与 install_prebuilt_with 的起点一致；build_* 稍后由最终 assess 覆盖。
    pub fn empty(source: impl Into<String>) -> Self {
        let source = source.into();
        Self {
            copied: 0,
            kept: 0,
            overwritten: 0,
            config_copied: 0,
            config_kept: 0,
            config_skipped: Vec::new(),
            manifest_failed: Vec::new(),
            failed: 0,
            source,
            build_ok: false,
            probe_after: BuildProbe::None.as_str().to_string(),
            build_source: BuildProbe::None.as_str().to_string(),
            escalated: false,
            log_path: None,
            build_schemas: Vec::new(),
            build_repaired: Vec::new(),
            build_missing: Vec::new(),
            deploy_method: deploy::DeployMethod::None.as_str().to_string(),
            deploy_result: deploy::RESULT_SKIPPED.to_string(),
            deploy_ok: false,
            deploy_ms: 0,
            deploy_detail: String::new(),
            deploy_inproc: "-".to_string(),
        }
    }

    /// build_schemas=luna_pinyin_simp:ok,wubi_pinyin:repaired,wubi86:missing
    pub fn build_schemas_line(&self) -> String {
        self.build_schemas
            .iter()
            .map(|schema| format!("{}:{}", schema.id, schema.state))
            .collect::<Vec<_>>()
            .join(",")
    }
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
        config_skipped: Vec::new(),
        manifest_failed: Vec::new(),
        failed: 0,
        source: payload_dir.display().to_string(),
        build_ok: false,
        probe_after: before.as_str().to_string(),
        build_source: before.as_str().to_string(),
        escalated: false,
        log_path: None,
        build_schemas: Vec::new(),
        build_repaired: Vec::new(),
        build_missing: Vec::new(),
        deploy_method: deploy::DeployMethod::None.as_str().to_string(),
        deploy_result: deploy::RESULT_SKIPPED.to_string(),
        deploy_ok: false,
        deploy_ms: 0,
        deploy_detail: String::new(),
        deploy_inproc: "-".to_string(),
    };

    // 0.6.23 A：按方案逐个校验（判断依据在拷贝之前，后面才能区分 repaired / ok）。
    let user_build = rime_user_dir.join("build");
    // 调用方给了 data 目录就用它（ime_state::ensure 给的是装完输入法后重新解析的那份）；
    // 没给才现场探测。
    let data_dirs = options
        .data_dirs
        .clone()
        .unwrap_or_else(data_patch::data_dirs);
    let checked: Vec<TargetSchema> = checked_targets(rime_user_dir, &data_dirs, Some(payload_dir));
    let before_ready: Vec<bool> = checked
        .iter()
        .map(|target| schema_ready(&user_build, target))
        .collect();
    let mut lines = vec![format!(
        "start payload={} rime={} force={} probe_before={}",
        payload_dir.display(),
        rime_user_dir.display(),
        options.force,
        before
    )];

    // 清单（MANIFEST.json）在这里有两个作用：
    // ① 校验 payload 自己没被截断/篡改（sha256 严格校验，失败照旧进 manifest_failed）；
    // ② 作为「版本」的参照物：判断用户目录里那份 build 产物是不是旧版本。
    let manifest = load_manifest(payload_dir);
    let verified = manifest
        .as_ref()
        .map(|manifest| verified_manifest_files(payload_dir, manifest))
        .unwrap_or_default();
    // 第一遍：build/ —— 缺才补；**版本对不上就用新版覆盖**（引擎产物是派生数据）。
    install_build(
        &build_src,
        &rime_user_dir.join("build"),
        options.force,
        manifest.as_ref(),
        &verified,
        &mut report,
        &mut lines,
    )?;
    // config/*.yaml 是用户配置：只补缺失的；force 也不覆盖。
    // M2：与 ime_state::seed_templates 走同一条 data 门禁 —— data 里没有的
    // 方案不能写进 schema_list，否则 librime 部署会把缺失方案计成 failure、整次失败。
    // A-3：config 的铺设**独立于 build/ 是否已可用**（install_payload_config）——
    // 「build 已可用就整段跳过」曾让 Install 模式漏铺 config。
    copy_payload_config(
        payload_dir,
        rime_user_dir,
        &data_dirs,
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
            None,
            &mut report,
            &mut lines,
        )?;
    }

    // 0.6.23 A：整树 copy 只补「缺文件」；主方案 schema.yaml 存在但缺插件标记这类
    // 「有文件却不可用」的情况，按方案覆盖一次（build/ 是派生数据）。已有可用的方案
    // 一个字节都不动（before_ready 为 true 的直接跳过）。
    let mut repair_copies = 0;
    for (target, ready) in checked.iter().zip(before_ready.iter()) {
        if *ready {
            continue;
        }
        let (copied, errors) = repair_schema(&build_src, &user_build, target);
        repair_copies += copied;
        for error in errors {
            report.failed += 1;
            lines.push(format!("failed {}: {error}", target.id));
        }
        if copied > 0 {
            lines.push(format!("repair {} copied={copied}", target.id));
        }
    }

    for (target, ready) in checked.iter().zip(before_ready.iter()) {
        let now = schema_ready(&user_build, target);
        let state = if now {
            if *ready {
                STATE_OK
            } else {
                STATE_REPAIRED
            }
        } else {
            STATE_MISSING
        };
        match state {
            STATE_REPAIRED => report.build_repaired.push(target.id.clone()),
            STATE_MISSING => report.build_missing.push(target.id.clone()),
            _ => {}
        }
        if target.primary {
            report.build_schemas.push(SchemaBuild {
                id: target.id.clone(),
                primary: true,
                state: state.to_string(),
            });
        }
    }

    let after = probe_build(rime_user_dir);
    report.probe_after = after.as_str().to_string();
    report.build_source = after.as_str().to_string();
    // build_ok 不再只看一个判据文件：主方案/依赖方案任何一个补不齐都算不可用。
    report.build_ok = after.usable() && report.build_missing.is_empty();
    // 建议项：MANIFEST.json 接线 —— 铺完 build/ 之后按 files 列表校验 payload 自己的
    // sha256。产物被截断/篡改（下载不全、缓存损坏）时**不能**当成 ok：进
    // report.manifest_failed → problems → 结论降级 partial，但**不中断安装**。
    let manifest_failed = verify_manifest(payload_dir);
    for item in &manifest_failed {
        report.failed += 1;
        lines.push(format!("manifest failed {item}"));
    }
    report.manifest_failed = manifest_failed;
    lines.push(format!(
        "end probe_after={} copied={} kept={} overwritten={} config_copied={} config_kept={} failed={} escalated={} build_ok={} repaired={} missing={} schemas=[{}] manifest_failed={} config_skipped=[{}]",
        after,
        report.copied,
        report.kept,
        report.overwritten,
        report.config_copied,
        report.config_kept,
        report.failed,
        report.escalated,
        report.build_ok,
        repair_copies,
        report.build_missing.len(),
        report.build_schemas_line(),
        report.manifest_failed.len(),
        report.config_skipped.join(","),
    ));
    report.log_path = write_log(rime_user_dir, &lines);
    Ok(report)
}

/// B4：进程内部署会把 build/<id>.schema.yaml 重写成 librime 自己的产物（不带
/// rimesync 插件标记），而 .bin 可能还是随包那份——「schema 来自 inproc、bin 来自
/// prebuilt」的混合态会让引擎按两种来源解释同一套编码（v 模式/标点直出时好时坏）。
///
/// 这里按方案**整体回退**：只要一个方案不完整（主方案缺标记，或缺 prism/table/reverse
/// 里的任何一个），就把它的 schema.yaml 与全部 .bin 一起从随包产物重铺，绝不做单文件
/// 拼接。回退发生后 build_source 记为 prebuilt（日志里能直接看出来）。
///
/// 返回回退过的方案 id。payload 里没有 build/ 时什么也不做。
pub fn enforce_prebuilt_build(
    report: &mut PrebuiltReport,
    rime_user_dir: &Path,
    payload_dir: &Path,
) -> Vec<String> {
    let payload_build = payload_dir.join("build");
    if !payload_build.is_dir() {
        return Vec::new();
    }
    let user_build = rime_user_dir.join("build");
    if !user_build.is_dir() {
        return Vec::new();
    }
    let data_dirs = data_patch::data_dirs();
    let targets: Vec<TargetSchema> = checked_targets(rime_user_dir, &data_dirs, Some(payload_dir));
    let mut reverted: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for target in &targets {
        if schema_ready(&user_build, target) {
            continue;
        }
        let mut copied = 0usize;
        for suffix in ["schema.yaml", "prism.bin", "table.bin", "reverse.bin"] {
            let name = format!("{}.{suffix}", target.id);
            let source = payload_build.join(&name);
            if !source.is_file() {
                continue;
            }
            match fs::copy(&source, user_build.join(&name)) {
                Ok(_) => copied += 1,
                Err(error) => errors.push(format!("{}: {error}", user_build.join(&name).display())),
            }
        }
        if schema_ready(&user_build, target) {
            report.overwritten += copied;
            reverted.push(target.id.clone());
        }
    }

    if !reverted.is_empty() {
        for id in &reverted {
            if !report.build_repaired.contains(id) {
                report.build_repaired.push(id.clone());
            }
            report.build_missing.retain(|missing| missing != id);
        }
        report.build_schemas.clear();
        for target in &targets {
            if !target.primary {
                continue;
            }
            let state = if !schema_ready(&user_build, target) {
                STATE_MISSING
            } else if reverted.contains(&target.id) {
                STATE_REPAIRED
            } else {
                STATE_OK
            };
            if state == STATE_MISSING && !report.build_missing.contains(&target.id) {
                report.build_missing.push(target.id.clone());
            }
            report.build_schemas.push(SchemaBuild {
                id: target.id.clone(),
                primary: true,
                state: state.to_string(),
            });
        }
    }
    let after = probe_build(rime_user_dir);
    report.probe_after = after.as_str().to_string();
    if !reverted.is_empty() {
        // B4：混合/进程内产物已被随包产物整体覆盖，来源就是 prebuilt。
        report.build_source = BuildProbe::Prebuilt.as_str().to_string();
    } else {
        report.build_source = after.as_str().to_string();
    }
    report.build_ok = after.usable() && report.build_missing.is_empty();
    let line = format!(
        "enforce build_source={} reverted=[{}] missing=[{}] errors={} build_ok={}",
        report.build_source,
        reverted.join(","),
        report.build_missing.join(","),
        errors.len(),
        report.build_ok
    );
    let mut lines = vec![line];
    for error in &errors {
        lines.push(format!("enforce_error={error}"));
    }
    report.log_path = write_log(rime_user_dir, &lines);
    reverted
}

#[derive(Debug, Clone, Copy)]
pub struct FinishOptions {
    /// 用户显式来源（设置页「修复输入法」、CLI fix-ime）。
    pub explicit: bool,
    /// C8：本次没有任何方案被改动（内容哈希与上次相同）→ 不刷新 mtime。
    ///
    /// 显式修复每次都会跑一遍；只要 data 方案一个字节都没变，就没必要让引擎把
    /// build/ 全量重建一次（用户会明显感到卡顿）。
    pub skip_touch: bool,
    /// 启动自举（M5）：部署 deferred —— 只刷 mtime，绝不 taskkill、绝不跑部署器。
    pub defer: bool,
}

impl FinishOptions {
    pub fn explicit() -> Self {
        Self {
            explicit: true,
            skip_touch: false,
            defer: false,
        }
    }

    /// 启动自举：不 taskkill、不跑部署器，把部署留给引擎下次启动（deferred）。
    ///
    /// A-2 备注：**「Bootstrap 不部署」由这里控制**，不在 InstallOptions 上。
    /// 具体是 finish_deploy 里的 options.defer 分支：defer=true 时把
    /// deploy::decide() 的计划压平（不 taskkill、不跑部署器），只留 prepare_auto
    /// 去刷 mtime。调用方（ime_state::ensure）按 mode 选 bootstrap() 还是
    /// explicit()。
    pub fn bootstrap() -> Self {
        Self {
            explicit: false,
            skip_touch: false,
            defer: true,
        }
    }
}

/// 收尾：把「部署」这件事做完（0.6.23）。
///
/// 调用时机：build/ 铺完、data 方案补丁也写完**之后**（否则 touch 出来的 mtime
/// 会让引擎拿未打补丁的 data 去重建）。顺序：
/// 1. 决策（见 deploy::decide）；
/// 2. 需要时先让条件成立：只 touch data 下相关 schema 的 mtime（**不写 user.yaml**，
///    那是跨设备同步的文件）；
/// 3. 需要时结束 WeaselServer（部署前已经停过就跳过；绝不在会话 0 里 Start-Process）。
///
/// pre_stop 是**部署之前**那次停服务的结局（0.6.23）：显式部署先停服务再跑 librime，
/// 停不下来时调用方已经跳过了进程内部署，这里把决策降级成 prebuilt
/// （deploy_method=prebuilt reason=server-alive）。
pub fn finish_deploy(
    report: &mut PrebuiltReport,
    rime_user_dir: &Path,
    data_dirs: &[PathBuf],
    options: FinishOptions,
    inproc: Option<&crate::librime::InprocReport>,
    pre_stop: Option<&deploy::ServerStopOutcome>,
) -> deploy::DeployOutcome {
    let started = Instant::now();
    let inproc_ok = inproc.map(|value| value.deploy_ok).unwrap_or(false);
    let payload_files = report.copied + report.kept + report.overwritten + report.failed;
    let wrote_build = report.copied + report.overwritten > 0 || !report.build_repaired.is_empty();
    let input = deploy::platform_input(
        options.explicit,
        payload_files > 0,
        report.build_ok,
        wrote_build,
        inproc_ok,
    );
    let mut plan = deploy::decide(&input);
    let mut detail: Vec<String> = Vec::new();
    // B2：失败与告警不能只由 taskkill 决定。这里把三个来源都收集起来：
    // 产物补不齐（prebuilt 没铺成功）、进程内部署失败、touch 失败。
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if !report.build_missing.is_empty() {
        problems.push(format!("build_missing={}", report.build_missing.join(",")));
    }
    if !report.build_ok {
        problems.push(format!("build_unusable probe={}", report.probe_after));
    }
    // B-2：config 模板被 data 门禁挡下 = 用户拿到的输入法**没有 schema_list**
    // （default.custom.yaml 才是写 schema_list 的那个）。这不是「可忽略的告警」：
    // 用户看到的是「报告说 ok，但输入法没有方案可选」。必须降级成 partial，
    // 让 CLI / GUI / 启动日志都能看到。
    if !report.config_skipped.is_empty() {
        problems.push(format!(
            "config_skipped={}",
            report.config_skipped.join(",")
        ));
    }
    // MANIFEST 校验失败（文件被截断/篡改）：同样进 problems → partial。
    if !report.manifest_failed.is_empty() {
        problems.push(format!(
            "manifest_failed={}",
            report.manifest_failed.join(",")
        ));
    }
    if let Some(inproc) = inproc {
        if !inproc.deploy_ok {
            problems.push(format!("inproc_failed method={}", inproc.deploy_method));
        }
    }
    if let Some(stop) = pre_stop {
        detail.push(format!(
            "server_before={} {}",
            stop.state.as_str(),
            stop.detail
        ));
        if stop.ok() {
            // 部署前已经停过服务：收尾不再重复 taskkill（更不会拉起）。
            plan.stop_server = false;
        } else {
            // 服务还活着，user.yaml 仍可能被它写：跳过进程内部署，退回 prebuilt。
            plan = deploy::DeployPlan::server_alive_degraded();
        }
    }
    // M5：启动自举 —— 一个外部命令都不跑。decide() 的 prebuilt 分支本来会
    // stop_server=true（taskkill），这里必须无条件压平；prepare_auto 保留，
    // 让引擎在下次启动时按 mtime 自己重建。
    if options.defer {
        plan = deploy::defer_plan(plan);
        detail.push(format!("deferred reason={}", plan.reason));
    }
    // C8：本次没有任何方案被改动（内容哈希与上次相同）→ 不刷 mtime，
    // 否则每点一次「修复输入法」都会让引擎把 build/ 全量重建一遍。
    if options.skip_touch && deploy::skip_prepare_auto(&mut plan) {
        detail.push("touch=skipped reason=schemas-unchanged".to_string());
    }
    if plan.prepare_auto {
        let ids: Vec<String> = target_schemas(data_dirs)
            .into_iter()
            .map(|target| target.id)
            .collect();
        let build_dir = rime_user_dir.join("build");
        // B2：共享 data 目录不可写时改刷用户目录同名方案（两者都失败才算失败）。
        let auto = deploy::prepare_auto_deploy(
            data_dirs,
            &ids,
            Some(build_dir.as_path()),
            Some(rime_user_dir),
        );
        detail.push(format!(
            "touched={} auto_fallback={} auto_failed={}",
            auto.touched.len(),
            auto.fallbacks.len(),
            auto.failed.len()
        ));
        for fallback in &auto.fallbacks {
            warnings.push(format!("touch_fallback={fallback}"));
            detail.push(format!("touch_fallback={fallback}"));
        }
        for error in &auto.failed {
            problems.push("touch_failed".to_string());
            detail.push(format!("auto_error={error}"));
        }
    }
    let mut outcome = deploy::apply(&plan);
    if outcome.has_warning() {
        warnings.push(outcome.detail.clone());
    }
    if !outcome.detail.is_empty() && outcome.detail != plan.reason {
        detail.push(outcome.detail.clone());
    }

    // B2：结果汇总。build/ 最终不可用 = 没有能喂给引擎的产物，这是唯一真正的失败判据；
    // 其余失败/告警（touch 失败、进程内部署失败、taskkill 失败、产物缺方案）一律降级成
    // partial，绝不再写 ok。
    let hard_failure = !report.build_ok;
    let result = if hard_failure {
        deploy::RESULT_FAILED
    } else if !problems.is_empty() || !warnings.is_empty() {
        deploy::RESULT_PARTIAL
    } else {
        outcome.result
    };
    if result != outcome.result {
        detail.push(format!(
            "status reasons={}",
            problems
                .iter()
                .chain(warnings.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        ));
        outcome.result = result;
        outcome.detail = detail.join(" ");
    }

    report.deploy_method = plan.method.as_str().to_string();
    report.deploy_result = result.to_string();
    report.deploy_ok = result != deploy::RESULT_FAILED;
    report.deploy_ms = started.elapsed().as_millis() as u64;
    report.deploy_detail = format!("reason={} {}", plan.reason, detail.join(" "));
    report.deploy_inproc = match (inproc, pre_stop) {
        (Some(inproc), _) => format!(
            "method={} ok={} ms={} lib={} detail={}",
            inproc.deploy_method, inproc.deploy_ok, inproc.deploy_ms, inproc.library, inproc.detail
        ),
        (None, Some(stop)) if !stop.ok() => format!(
            "skipped reason={} ({})",
            deploy::REASON_SERVER_ALIVE,
            stop.detail
        ),
        _ => "-".to_string(),
    };
    let line = format!(
        "deploy method={} result={} ok={} ms={} detail={}",
        report.deploy_method,
        report.deploy_result,
        report.deploy_ok,
        report.deploy_ms,
        report.deploy_detail
    );
    let _ = write_log(rime_user_dir, std::slice::from_ref(&line));
    outcome
}

/// 铺 payload 的 config/ 到 Rime 用户目录（**只补缺失的**，force 也不覆盖）。
///
/// A-3：从 install_prebuilt_with 里拆出来，让「铺 config」不再依附于
/// 「build/ 不可用」这个前提：Install 模式下 build 早就可用（用户自己装过）
/// 时，config 仍必须铺一次（幂等：缺才写）。
///
/// 门禁与 seed_templates 同一条（config_gate_skip）：data 里没有的方案不写，
/// 否则 librime 部署会把缺失方案计成 failure、整次部署失败。跳过的条目进
/// report.config_skipped → problems → 结论降级 partial（B-2）。
pub fn copy_payload_config(
    payload_dir: &Path,
    rime_user_dir: &Path,
    data_dirs: &[PathBuf],
    report: &mut PrebuiltReport,
    lines: &mut Vec<String>,
) -> Result<()> {
    copy_tree(
        &payload_dir.join("config"),
        rime_user_dir,
        false,
        true,
        Some(data_dirs),
        report,
        lines,
    )
}

/// 铺 build/：缺失才补 + **版本对不上就用新版覆盖**。
///
/// 语义：build/ 下装的是**引擎产物**（*.schema.yaml / default.yaml /
/// *.bin），不是用户数据 —— 用户目录里那份是旧引擎/旧方案编译出来的，用新版覆盖
/// 是对的（引擎本来也会重建）。config/ 下的用户配置相反：只补缺失、永不覆盖
/// （见 copy_payload_config）。
///
/// 「版本对不上」的判据：
/// - 有 MANIFEST.json 时，用户目录里那份与 payload 这一份**内容不同**
///   （YAML 忽略引擎写的 __build_info 构建戳）= 不是同一版产物 = 用新版覆盖；
/// - 拿来覆盖的 payload 文件必须**通过清单校验**（size + sha256）：清单对不上的
///   文件不覆盖用户那份好文件（sha256 校验的严格性一点没放宽，只是拒绝拿
///   可疑内容去覆盖）；
/// - 没有 MANIFEST.json（旧版 payload）→ 退回「缺失才补」，不覆盖。
fn install_build(
    build_src: &Path,
    user_build: &Path,
    force: bool,
    manifest: Option<&Manifest>,
    verified: &BTreeSet<String>,
    report: &mut PrebuiltReport,
    lines: &mut Vec<String>,
) -> Result<()> {
    if !build_src.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(user_build).with_context(|| format!("创建目录 {}", user_build.display()))?;
    let mut files: Vec<PathBuf> = fs::read_dir(build_src)
        .with_context(|| format!("读取目录 {}", build_src.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    // 先算「哪些方案过期了」：版本载体 = 方案 schema.yaml（default.yaml 单独算）。
    // 一个方案过期 = 它的 schema.yaml 与 payload 那份不是同一版 → 它的 .bin
    // 也一并换新（.bin 是按 schema 编译的，schema 换版了 bin 必须跟上）。
    let mut stale: BTreeSet<String> = BTreeSet::new();
    for path in &files {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_build_version_file(name) {
            continue;
        }
        let target = user_build.join(name);
        let (Ok(user_bytes), Ok(payload_bytes)) = (fs::read(&target), fs::read(path)) else {
            continue;
        };
        if !same_build_version(&user_bytes, &payload_bytes, is_yaml(path)) {
            let scheme = build_scheme_id(name);
            stale.insert(scheme.clone());
            lines.push(format!("stale scheme={scheme} file={name}"));
        }
    }

    for path in &files {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let target = user_build.join(name);
        let existed = target.is_file();
        let rel = format!("build/{name}");
        // 只有通过清单校验的 payload 文件才允许覆盖用户目录里已有的产物。
        let trusted = manifest.is_some() && verified.contains(&rel);
        let overwrite = force || (existed && stale.contains(&build_scheme_id(name)) && trusted);
        if existed && !overwrite {
            report.kept += 1;
            continue;
        }
        match fs::copy(path, &target) {
            Ok(_) => {
                if existed {
                    report.overwritten += 1;
                    lines.push(format!("overwrite {} reason=stale-build", target.display()));
                } else {
                    report.copied += 1;
                    lines.push(format!("copy {}", target.display()));
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

/// 逐文件复制（不递归）：payload 的 build/ 与 config/ 都是平铺的。
fn copy_tree(
    src: &Path,
    dst: &Path,
    force: bool,
    only_yaml: bool,
    // Some = 走 payload config/ 的 data 门禁；None = 不门禁（build/ 产物）。
    gate: Option<&[PathBuf]>,
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
        if let Some(data_dirs) = gate {
            let text = name.to_string_lossy();
            if let Some(reason) = config_gate_skip(&text, data_dirs) {
                report.config_skipped.push(format!("{text}:{reason}"));
                lines.push(format!("config skipped {text} reason={reason}"));
                continue;
            }
        }
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

/// MANIFEST.json 里的一条文件记录。
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFile {
    /// 相对 payload 根的路径（如 build/default.yaml）。
    pub path: String,
    /// 文件字节数（清单必须在**最终内容**上算，见 gen-prebuilt-build.py）。
    #[serde(default)]
    pub bytes: u64,
    /// 文件内容的 sha256。
    pub sha256: String,
}

/// MANIFEST.json（gen-prebuilt-build.py 产出）。
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub files: Vec<ManifestFile>,
}

impl Manifest {
    /// 清单里某个相对路径的记录。
    pub fn file(&self, rel: &str) -> Option<&ManifestFile> {
        self.files.iter().find(|item| item.path == rel)
    }
}

/// 读 payload 的 MANIFEST.json。
///
/// 没有清单 = 旧版 payload（0.6.24 及更早）→ None：不做版本判定，
/// 铺装退回「缺失才补」的旧行为（不会凭空覆盖用户目录里的东西）。
pub fn load_manifest(payload_dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(payload_dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 清单校验**通过**的文件（相对 payload 根的路径）。
///
/// 只有这些文件允许去覆盖用户目录里已有的产物：清单对不上的文件
/// （payload 被截断/篡改，或清单本身是过期的）绝不能覆盖用户那份好文件。
/// sha256 校验的严格性在这里保持不变 —— 校验不过就是不过。
fn verified_manifest_files(payload_dir: &Path, manifest: &Manifest) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for item in &manifest.files {
        let Ok(bytes) = fs::read(payload_dir.join(&item.path)) else {
            continue;
        };
        if item.bytes > 0 && bytes.len() as u64 != item.bytes {
            continue;
        }
        if !sha256_hex(&bytes).eq_ignore_ascii_case(&item.sha256) {
            continue;
        }
        out.insert(item.path.clone());
    }
    out
}

/// 去掉引擎写的 __build_info 构建戳（版本判定用）。
///
/// librime 每次部署都会重写 build/<id>.schema.yaml，其中 __build_info 记的是
/// 源文件的 mtime：同一份内容、不同的构建时刻 = 字节不同但**版本相同**。
/// 判「用户目录里那份是不是同一版」时必须忽略它，否则每次部署后都会被误判成
/// 「旧版本」而反复覆盖（引擎产物永远追不上自己的构建戳）。
fn strip_build_info(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut in_build_info = false;
    for line in text.lines() {
        if !in_build_info {
            if line.starts_with("__build_info:") {
                in_build_info = true;
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // 顶层键（非缩进、非空行）→ __build_info 块结束。
        if !line.is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
            in_build_info = false;
            out.push_str(line);
            out.push('\n');
        }
    }
    out.into_bytes()
}

/// 两个 build 产物是不是「同一版」。
///
/// YAML（schema.yaml / default.yaml）：忽略引擎的 __build_info 构建戳后内容相同
/// 就算同一版；其余（*.bin）逐字节比较。
fn same_build_version(user: &[u8], payload: &[u8], yaml: bool) -> bool {
    if user == payload {
        return true;
    }
    if yaml {
        return strip_build_info(user) == strip_build_info(payload);
    }
    false
}

/// build/ 里承载「版本」的文件：方案 schema.yaml 与编译出来的 default.yaml。
fn is_build_version_file(name: &str) -> bool {
    name == "default.yaml" || name.ends_with(".schema.yaml")
}

/// build 文件所属的方案 id（default.yaml 单独一个 id，不会撞上方案名）。
fn build_scheme_id(name: &str) -> String {
    if name == "default.yaml" {
        return "\u{0}default".to_string();
    }
    name.split('.').next().unwrap_or(name).to_string()
}

/// 按 MANIFEST.json 的 files 列表校验 payload 里的文件（sha256）。
///
/// 返回**不匹配/缺失/读不出**的条目（每条一个可读原因）。
///
/// 为什么放在铺完 build/ 之后：产物被截断（下载不全）、缓存损坏、或被人改过，
/// 都会让 librime 拿一份坏产物去部署 —— 表现是「装了但打不出字」或部署静默失败。
/// 这里只**记录**（进 problems → partial），不中断安装：坏一个文件不该让
/// 「其余方案全部可用」变成「全都没有」。
///
/// MANIFEST.json 不存在 = 旧版 payload（没有清单）→ 不校验，不算失败。
pub fn verify_manifest(payload_dir: &Path) -> Vec<String> {
    let path = payload_dir.join(MANIFEST_FILE);
    if !path.is_file() {
        return Vec::new();
    }
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => return vec![format!("{MANIFEST_FILE}:read:{error}")],
    };
    let manifest: Manifest = match serde_json::from_str(&text) {
        Ok(manifest) => manifest,
        Err(error) => return vec![format!("{MANIFEST_FILE}:parse:{error}")],
    };
    let mut problems = Vec::new();
    for item in &manifest.files {
        let file = payload_dir.join(&item.path);
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(error) => {
                problems.push(format!("{}:missing:{error}", item.path));
                continue;
            }
        };
        if item.bytes > 0 && bytes.len() as u64 != item.bytes {
            problems.push(format!(
                "{}:size:expected={} actual={}",
                item.path,
                item.bytes,
                bytes.len()
            ));
            continue;
        }
        let digest = sha256_hex(&bytes);
        if !digest.eq_ignore_ascii_case(&item.sha256) {
            problems.push(format!(
                "{}:sha256:expected={} actual={}",
                item.path, item.sha256, digest
            ));
        }
    }
    problems
}

/// 字节的 sha256（小写 hex）。
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

    /// 门禁数据：config/default.custom.yaml 只有在 data 里真有 wubi86 时才允许铺。
    fn gate_data(root: &Path) -> PathBuf {
        let data = root.join("data");
        write(&data.join("wubi86.schema.yaml"), "name: wubi86\n");
        data
    }

    /// 带 data 目录的安装：测试绝不许读宿主机的 /usr/share/rime-data。
    fn install_with_data(payload: &Path, rime: &Path, data: &Path) -> Result<PrebuiltReport> {
        install_prebuilt_with(
            payload,
            rime,
            InstallOptions {
                data_dirs: Some(vec![data.to_path_buf()]),
                ..InstallOptions::default()
            },
        )
    }

    /// 造一份最小 payload：build 两个文件 + config 一个 yaml。
    fn make_payload(root: &Path, marked: bool) {
        write(
            &root.join("build").join(PROBE_FILE),
            if marked { MARKED } else { PLAIN },
        );
        write(
            &root.join("build").join("luna_pinyin_simp.prism.bin"),
            "prism",
        );
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
        // 0.6.23：判据唯一 = schema.yaml（主方案带插件标记）+ 至少一个 bin。
        // 只有标记、没有 bin 仍然不可用（deploy）。
        assert_eq!(probe_build(tmp.path()), BuildProbe::Deploy);

        write(
            &tmp.path().join("build").join("luna_pinyin_simp.prism.bin"),
            "prism",
        );
        assert_eq!(probe_build(tmp.path()), BuildProbe::Prebuilt);
    }

    #[test]
    fn copies_missing_files_and_reports_ok() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("rime dir");
        make_payload(&payload, true);

        let report = install_with_data(&payload, &rime, &gate_data(tmp.path())).expect("install");

        assert!(report.build_ok);
        assert_eq!(report.copied, 2);
        assert_eq!(report.kept, 0);
        assert_eq!(report.overwritten, 0);
        assert_eq!(report.config_copied, 1);
        assert_eq!(report.config_kept, 0);
        assert!(report.config_skipped.is_empty());
        assert_eq!(report.failed, 0);
        assert_eq!(report.probe_after, "prebuilt");
        assert!(report.log_path.is_some());
        assert!(rime.join("build").join(PROBE_FILE).is_file());
        assert!(rime
            .join("build")
            .join("luna_pinyin_simp.prism.bin")
            .is_file());
        assert!(rime.join("default.custom.yaml").is_file());
    }

    /// M2：payload config/ 必须与 ime_state::seed_templates 走同一条 data 门禁。
    ///
    /// data 里没有 wubi86 时，default.custom.yaml 的 schema_list 会把一个不存在的
    /// 方案写进用户目录 —— librime 部署会把缺失方案计成 failure、整次部署失败。
    #[test]
    fn config_is_gated_by_schemas_present_in_data() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("rime dir");
        make_payload(&payload, true);
        write(
            &payload.join("config").join("luna_pinyin_simp.custom.yaml"),
            "patch:\n  menu/page_size: 9\n",
        );
        write(
            &payload.join("config").join("wubi86.custom.yaml"),
            "patch:\n  menu/page_size: 9\n",
        );
        // data 只有 luna：default（要求 wubi86）与 wubi86.custom 都必须跳过。
        let data = tmp.path().join("data");
        write(&data.join("luna_pinyin_simp.schema.yaml"), "name: luna\n");

        let report = install_with_data(&payload, &rime, &data).expect("install");

        assert_eq!(
            report.config_copied, 1,
            "只铺 data 里真有的 luna_pinyin_simp"
        );
        assert_eq!(
            report.config_skipped,
            vec![
                "default.custom.yaml:no-wubi86-in-data".to_string(),
                "wubi86.custom.yaml:no-wubi86.schema.yaml-in-data".to_string(),
            ]
        );
        assert!(!rime.join("default.custom.yaml").exists());
        assert!(!rime.join("wubi86.custom.yaml").exists());
        assert!(rime.join("luna_pinyin_simp.custom.yaml").is_file());
        assert!(report.build_ok, "config 被跳不影响 build 可用");
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

        let report = install_with_data(&payload, &rime, &gate_data(tmp.path())).expect("install");

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
            &rime.join("build").join("luna_pinyin_simp.prism.bin"),
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
        write(
            &rime.join("build").join("luna_pinyin_simp.prism.bin"),
            "old",
        );
        write(&rime.join("default.custom.yaml"), "patch:\n  keep: me\n");

        let report = install_prebuilt_with(
            &payload,
            &rime,
            InstallOptions {
                force: true,
                // A-2: InstallOptions 不再有 explicit（死字段）。
                data_dirs: Some(vec![gate_data(tmp.path())]),
            },
        )
        .expect("install");

        assert!(report.build_ok);
        assert!(!report.escalated, "force 不需要第二次机会");
        assert_eq!(report.overwritten, 2);
        assert_eq!(report.kept, 0);
        assert_eq!(report.config_copied, 0);
        assert_eq!(report.config_kept, 1);
        assert_eq!(
            read(&rime.join("build").join("luna_pinyin_simp.prism.bin")),
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

    // ---- 0.6.23 A：按方案校验并补齐 ----

    /// 三个保留方案的最小 payload（每个方案 schema.yaml + 一个 bin）。
    fn make_three_schema_payload(root: &Path) {
        for (id, bins) in [
            ("luna_pinyin_simp", vec!["prism.bin"]),
            ("wubi_pinyin", vec!["prism.bin"]),
            ("wubi86", vec!["prism.bin", "table.bin"]),
        ] {
            write(
                &root.join("build").join(format!("{id}.schema.yaml")),
                MARKED,
            );
            for bin in bins {
                write(&root.join("build").join(format!("{id}.{bin}")), id);
            }
        }
        write(
            &root.join("config").join("default.custom.yaml"),
            "patch:\n  schema_list: []\n",
        );
    }

    fn state_of(report: &PrebuiltReport, id: &str) -> String {
        report
            .build_schemas
            .iter()
            .find(|schema| schema.id == id)
            .map(|schema| schema.state.clone())
            .unwrap_or_else(|| "absent".to_string())
    }

    #[test]
    fn repairs_only_the_missing_schema_and_bin() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        // 用户目录里三个方案都已就绪，只缺 wubi86 的 schema.yaml 与 wubi_pinyin 的 bin。
        fs::create_dir_all(rime.join("build")).expect("build dir");
        for id in ["luna_pinyin_simp", "wubi_pinyin", "wubi86"] {
            write(
                &rime.join("build").join(format!("{id}.schema.yaml")),
                MARKED,
            );
        }
        write(
            &rime.join("build").join("luna_pinyin_simp.prism.bin"),
            "local",
        );
        write(&rime.join("build").join("wubi_pinyin.prism.bin"), "local");
        write(&rime.join("build").join("wubi86.prism.bin"), "local");
        write(&rime.join("build").join("wubi86.table.bin"), "local");
        fs::remove_file(rime.join("build").join("wubi86.schema.yaml")).expect("rm schema");
        fs::remove_file(rime.join("build").join("wubi_pinyin.prism.bin")).expect("rm bin");

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(report.build_ok, "{report:?}");
        assert_eq!(report.copied, 2, "只补缺失的两个文件");
        assert_eq!(report.overwritten, 0, "已有可用产物不改动");
        assert_eq!(report.build_repaired, vec!["wubi_pinyin", "wubi86"]);
        assert!(report.build_missing.is_empty());
        assert_eq!(state_of(&report, "luna_pinyin_simp"), STATE_OK);
        assert_eq!(state_of(&report, "wubi_pinyin"), STATE_REPAIRED);
        assert_eq!(state_of(&report, "wubi86"), STATE_REPAIRED);
        assert_eq!(
            read(&rime.join("build").join("luna_pinyin_simp.prism.bin")),
            "local",
            "已有可用的产物一个字节都不动"
        );
    }

    #[test]
    fn complete_build_is_left_alone() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(rime.join("build")).expect("build dir");
        for (id, bin) in [
            ("luna_pinyin_simp", "prism"),
            ("wubi_pinyin", "prism"),
            ("wubi86", "prism"),
        ] {
            write(
                &rime.join("build").join(format!("{id}.schema.yaml")),
                MARKED,
            );
            write(&rime.join("build").join(format!("{id}.{bin}.bin")), "local");
        }
        write(&rime.join("build").join("wubi86.table.bin"), "local");

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(report.build_ok);
        assert_eq!(report.copied, 0);
        assert_eq!(report.overwritten, 0);
        assert!(report.build_repaired.is_empty());
        assert!(report.build_missing.is_empty());
        assert_eq!(state_of(&report, "wubi86"), STATE_OK);
        assert_eq!(
            report.build_schemas_line(),
            "luna_pinyin_simp:ok,wubi_pinyin:ok,wubi86:ok"
        );
    }

    #[test]
    fn unusable_primary_schema_is_overwritten_once() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(rime.join("build")).expect("build dir");
        write(
            &rime.join("build").join("luna_pinyin_simp.schema.yaml"),
            MARKED,
        );
        write(
            &rime.join("build").join("luna_pinyin_simp.prism.bin"),
            "local",
        );
        write(&rime.join("build").join("wubi_pinyin.schema.yaml"), MARKED);
        write(&rime.join("build").join("wubi_pinyin.prism.bin"), "local");
        // 官方部署产物：文件在，但没有插件标记 → 必须换掉。
        write(&rime.join("build").join("wubi86.schema.yaml"), PLAIN);
        write(&rime.join("build").join("wubi86.prism.bin"), "local");
        write(&rime.join("build").join("wubi86.table.bin"), "local");

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(report.build_ok, "{report:?}");
        assert_eq!(state_of(&report, "wubi86"), STATE_REPAIRED);
        assert!(
            read(&rime.join("build").join("wubi86.schema.yaml")).contains("rimesync_vnum"),
            "缺标记的 schema.yaml 必须被 prebuilt 覆盖"
        );
    }

    #[test]
    fn missing_payload_files_are_reported_not_panicked() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");
        // prebuilt 自己也缺 wubi86 的产物：schema.yaml 在，bin 一个都没有。
        fs::remove_file(payload.join("build").join("wubi86.prism.bin")).expect("rm prism");
        fs::remove_file(payload.join("build").join("wubi86.table.bin")).expect("rm table");

        let report = install_prebuilt(&payload, &rime).expect("install");

        assert!(!report.build_ok, "补不齐就不能报可用");
        assert_eq!(report.build_missing, vec!["wubi86"]);
        assert_eq!(state_of(&report, "wubi86"), STATE_MISSING);
        assert_eq!(state_of(&report, "luna_pinyin_simp"), STATE_REPAIRED);
        assert!(report.build_schemas_line().contains("wubi86:missing"));
    }

    // ---- 0.6.23 B4：inproc 产物与 prebuilt 混用时整体回退 ----

    #[test]
    fn inproc_output_is_wholly_replaced_by_prebuilt() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");
        let mut report = install_prebuilt(&payload, &rime).expect("install");
        assert!(report.build_ok, "{report:?}");

        // 模拟进程内部署：schema.yaml 被 librime 重写成「没有插件标记」的版本，
        // .bin 也被换成 inproc 自己产出的内容——这正是混合态的起点。
        write(&rime.join("build").join("wubi86.schema.yaml"), PLAIN);
        write(&rime.join("build").join("wubi86.prism.bin"), "inproc-prism");
        write(&rime.join("build").join("wubi86.table.bin"), "inproc-table");
        assert!(!schema_ready(
            &rime.join("build"),
            &TargetSchema {
                id: "wubi86".to_string(),
                primary: true,
            }
        ));

        let reverted = enforce_prebuilt_build(&mut report, &rime, &payload);

        assert_eq!(reverted, vec!["wubi86".to_string()]);
        // 三个文件一起来自 prebuilt：不允许 schema 来自 inproc、bin 来自 prebuilt。
        assert!(
            read(&rime.join("build").join("wubi86.schema.yaml")).contains("rimesync_vnum"),
            "schema.yaml 必须来自 prebuilt"
        );
        assert_eq!(read(&rime.join("build").join("wubi86.prism.bin")), "wubi86");
        assert_eq!(read(&rime.join("build").join("wubi86.table.bin")), "wubi86");
        assert_eq!(report.build_source, "prebuilt");
        assert!(report.build_ok, "{report:?}");
        assert_eq!(state_of(&report, "wubi86"), STATE_REPAIRED);

        let log = read(&rime.join(LOG_FILE));
        assert!(log.contains("build_source=prebuilt"), "{log}");
        assert!(log.contains("reverted=[wubi86]"), "{log}");
        // 其它方案一个字节都不动。
        assert_eq!(
            read(&rime.join("build").join("luna_pinyin_simp.prism.bin")),
            "luna_pinyin_simp"
        );
    }

    /// 只少了 bin 也要整方案回退（schema.yaml 与 .bin 一起重铺）。
    #[test]
    fn a_missing_bin_also_rolls_back_the_schema_yaml() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");
        let mut report = install_prebuilt(&payload, &rime).expect("install");
        // 只删 bins：schema.yaml 仍在（且带标记），旧的 repair 逻辑不会重铺 schema。
        fs::remove_file(rime.join("build").join("wubi86.prism.bin")).expect("rm prism");
        fs::remove_file(rime.join("build").join("wubi86.table.bin")).expect("rm table");
        write(&rime.join("build").join("wubi86.schema.yaml"), PLAIN);

        let reverted = enforce_prebuilt_build(&mut report, &rime, &payload);

        assert_eq!(reverted, vec!["wubi86".to_string()]);
        assert!(read(&rime.join("build").join("wubi86.schema.yaml")).contains("rimesync_vnum"));
        assert_eq!(read(&rime.join("build").join("wubi86.prism.bin")), "wubi86");
        assert_eq!(read(&rime.join("build").join("wubi86.table.bin")), "wubi86");
    }

    /// 全部就绪时什么都不做，也不改写 build_source。
    #[test]
    fn enforcement_is_a_noop_when_every_schema_is_ready() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");
        let mut report = install_prebuilt(&payload, &rime).expect("install");
        let before = report.build_source.clone();

        let reverted = enforce_prebuilt_build(&mut report, &rime, &payload);

        assert!(reverted.is_empty());
        assert_eq!(report.build_source, before);
        assert!(
            read(&rime.join(LOG_FILE)).contains("reverted=[]"),
            "幂等调用也要留下痕迹（reverted 为空）"
        );
    }

    // ---- 0.6.23 B2/C8：deploy_result 汇总 ----

    fn fake_inproc(ok: bool) -> crate::librime::InprocReport {
        crate::librime::InprocReport {
            deploy_method: if ok {
                crate::librime::METHOD_OK.to_string()
            } else {
                crate::librime::METHOD_FAILED.to_string()
            },
            deploy_ok: ok,
            deploy_ms: 1,
            library: "-".to_string(),
            detail: "test".to_string(),
            shared_data_dir: "-".to_string(),
            user_data_dir: "-".to_string(),
        }
    }

    /// 进程内部署失败但 build 可用 → partial（不能写 ok）。
    #[test]
    fn finish_deploy_reports_partial_when_inproc_failed() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");
        let mut report = install_prebuilt(&payload, &rime).expect("install");
        assert!(report.build_ok, "{report:?}");

        let outcome = finish_deploy(
            &mut report,
            &rime,
            &[],
            FinishOptions::explicit(),
            Some(&fake_inproc(false)),
            None,
        );

        assert_eq!(report.deploy_result, deploy::RESULT_PARTIAL, "{report:?}");
        assert_eq!(outcome.result, deploy::RESULT_PARTIAL);
        assert!(report.deploy_ok, "partial 不是致命失败");
        assert!(
            report.deploy_detail.contains("inproc_failed"),
            "{}",
            report.deploy_detail
        );
    }

    /// build 最终不可用 → failed（不得写 ok / partial）。
    #[test]
    fn finish_deploy_is_failed_when_the_build_is_unusable() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");
        let mut report = install_prebuilt(&payload, &rime).expect("install");
        // 模拟「随包产物也补不齐」：主方案缺 bin。
        report.build_ok = false;
        report.build_missing = vec!["wubi86".to_string()];

        let outcome = finish_deploy(
            &mut report,
            &rime,
            &[],
            FinishOptions::explicit(),
            None,
            None,
        );

        assert_eq!(report.deploy_result, deploy::RESULT_FAILED, "{report:?}");
        assert_eq!(outcome.result, deploy::RESULT_FAILED);
        assert!(!report.deploy_ok);
        assert!(
            report.deploy_detail.contains("build_missing=wubi86"),
            "{}",
            report.deploy_detail
        );
    }

    #[test]
    fn check_build_is_read_only() {
        let tmp = TempDir::new();
        let payload = tmp.path().join("payload");
        let rime = tmp.path().join("rime");
        make_three_schema_payload(&payload);
        fs::create_dir_all(&rime).expect("rime dir");

        let schemas = check_build(&rime, Some(&payload), &[]);

        assert_eq!(schemas.len(), 3, "只列 3 个主方案");
        assert!(schemas.iter().all(|schema| schema.state == STATE_MISSING));
        assert!(!rime.join("build").exists(), "只读检查不许写盘");
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
