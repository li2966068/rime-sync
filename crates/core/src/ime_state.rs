//! 输入法「产物是否可用 / 该不该部署」的唯一判据与唯一入口。
//!
//! 为什么要有这个模块（0.6.23 结构性改造）：改造前三套判据并存
//! （prebuilt::probe_build 双标记、prebuilt::schema_ready 单标记 + bin、
//! NSIS 里的 RimeSyncProbeBuild 双标记且不查 bin），结论互相矛盾；四个入口
//! （NSIS 自检、设置页「修复输入法」、CLI fix-ime、启动自举）各自决定铺不铺、
//! 部不部署、要不要重启服务，顺序与判据都会漂移。
//!
//! 现在只有两件事：
//! * assess —— 唯一的「产物可用吗」实现（probe_build / schema_ready / NSIS 判据
//!   都收敛到这里）；
//! * ensure —— 唯一的「要做什么」状态机，固定顺序
//!   assess → 铺 prebuilt → patch data → 部署 → 再 assess → 汇总。
//!
//! Mode::Install（安装版首次启动 / `ensure-ime --mode install`）在状态机前面多两步，
//! 接管原先 NSIS 钩子的全部职责：
//!   输入法本体（缺失/不是我们的构建 → 随包安装器 /S /NOREBOOT）
//!   → config 模板（缺失才写）→ 再进入上面的固定顺序。
//!
//! Outcome.result 只能由**最终 assess**（加本次实际动作）推出，调用方不许自己推。
//! 语义：Noop = 已可用且一个字节都没改；Ok = 本次改过且最终可用；
//! Partial = 最终可用但本次有改动失败（detail 必非空）；
//! Failed = 最终不可用（detail 必非空，CLI 必须非零退出、NSIS 自检必须失败）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::data_patch::{self, DataPatchReport, FixSource};
use crate::deploy;
use crate::ime::{self, ImeReport};
use crate::managed_custom;
use crate::prebuilt::{self, InstallOptions, PrebuiltReport, SchemaBuild};
use crate::prune;

/// 判据用的插件标记（主方案 schema.yaml 必须含它；官方部署产物不含）。
pub const MARKER: &str = prebuilt::MARKERS[0];

/// 随包 config 模板（与 packaging/rime/ 下的文件名一致）。
///
/// 名字与门禁的单一真相源在 prebuilt：payload 的 config/ 拷贝走同一条规则（M2）。
pub const TEMPLATE_DEFAULT: &str = prebuilt::CONFIG_DEFAULT;
pub const TEMPLATE_WEASEL: &str = prebuilt::CONFIG_WEASEL;
/// A-4：config 模板目录 = payload 的 `config/`（扁平的 <id>.custom.yaml）。
/// 旧的 `resources/schemas/` 目录已删除（两份源会漂移）。
pub const TEMPLATE_CONFIG_DIR: &str = "config";
/// 逐方案模板的后缀：<schema>.custom.yaml。
pub const TEMPLATE_SCHEMA_SUFFIX: &str = prebuilt::CONFIG_SCHEMA_SUFFIX;
/// default.custom.yaml 只有在输入法 data 里有这个方案时才播（旧钩子的同一门禁）。
pub const TEMPLATE_GATE_SCHEMA: &str = prebuilt::CONFIG_GATE_SCHEMA;

/// 单个文件的判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Ok,
    Missing,
    /// 文件在，但主方案没有随包插件标记（官方部署产物 / 进程内重建的产物）。
    StaleNoMarker,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::StaleNoMarker => "stale-no-marker",
        }
    }
}

/// 单个目标方案的产物状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaState {
    pub id: String,
    /// 主方案 = true：schema.yaml 必须含 MARKER。
    pub primary: bool,
    pub schema_yaml: State,
    pub bins: State,
}

impl SchemaState {
    /// 该方案可用 = schema.yaml 可用（主方案还要求标记）且至少一个 bin 存在。
    pub fn usable(&self) -> bool {
        self.schema_yaml == State::Ok && self.bins == State::Ok
    }
}

/// 一次 assess 的一句话原因（报告字段，NSIS/CLI 都读它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// 全部目标方案可用。
    Ready,
    /// 没有任何需要校验的目标方案（payload 与用户目录都没有这些方案）。
    NoTargets,
    /// 有方案缺 schema.yaml。
    SchemaMissing,
    /// 有主方案 schema.yaml 但没有插件标记。
    SchemaStale,
    /// 有方案缺 *.prism/table/reverse.bin。
    BinsMissing,
    /// 以上混在一起。
    Mixed,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NoTargets => "no-targets",
            Self::SchemaMissing => "schema-missing",
            Self::SchemaStale => "schema-stale",
            Self::BinsMissing => "bins-missing",
            Self::Mixed => "mixed",
        }
    }
}

/// 唯一的「产物是否可用」结论。
#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub schemas: Vec<SchemaState>,
    pub build_ok: bool,
    pub reason: Reason,
}

impl Assessment {
    /// 不可用的目标方案 id（报告 / 日志 / NSIS 都用它）。
    pub fn missing_ids(&self) -> Vec<String> {
        self.schemas
            .iter()
            .filter(|schema| !schema.usable())
            .map(|schema| schema.id.clone())
            .collect()
    }

    /// luna_pinyin_simp:ok,wubi_pinyin:missing
    pub fn line(&self) -> String {
        self.schemas
            .iter()
            .map(|schema| format!("{}:{}", schema.id, schema_state_str(schema)))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// 一句话明细（Partial/Failed 的 detail 与日志共用）。
    pub fn detail(&self) -> String {
        if self.build_ok {
            return format!("reason={} build_ok=true", self.reason.as_str());
        }
        let missing = self.missing_ids();
        format!(
            "reason={} build_ok=false missing=[{}]",
            self.reason.as_str(),
            missing.join(",")
        )
    }
}

fn schema_state_str(schema: &SchemaState) -> &'static str {
    if schema.usable() {
        prebuilt::STATE_OK
    } else {
        // 文件在但不可用（缺标记 / 缺 bin）同样算 missing，明细在 SchemaState 里。
        prebuilt::STATE_MISSING
    }
}

/// 目标方案集合 = 保留清单 ∪ 依赖闭包，且**这一份 payload 或用户目录里真的有它**。
///
/// 依赖方案同样要校验（<id>.schema.yaml + 至少一个 prism/table/reverse.bin）；
/// 纯 __include 基类（部署器只合并不落盘，例如 luna_pinyin）两边都没有 schema.yaml
/// 时视为「本就不需要」，不参与判定 —— 这条豁免原来在 prebuilt::schema_required，
/// 现在搬到这里，所有判据共用。
#[derive(Debug, Clone, Default)]
pub struct KeepSet {
    targets: Vec<KeepTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepTarget {
    pub id: String,
    pub primary: bool,
}

impl KeepSet {
    /// 从保留清单 + 依赖闭包解析，并按「payload / 用户目录有没有该方案」过滤。
    pub fn resolve(rime_dir: &Path, data_dirs: &[PathBuf], payload: Option<&Path>) -> Self {
        let user_build = rime_dir.join("build");
        let payload_build = payload.map(|dir| dir.join("build"));
        let required = |id: &str| {
            let name = format!("{id}.schema.yaml");
            user_build.join(&name).is_file()
                || payload_build
                    .as_ref()
                    .map(|dir| dir.join(&name).is_file())
                    .unwrap_or(false)
        };
        let mut targets: Vec<KeepTarget> = prune::retained_schemas()
            .iter()
            .filter(|id| required(id))
            .map(|id| KeepTarget {
                id: (*id).to_string(),
                primary: true,
            })
            .collect();
        let known = prune::known_schemas(data_dirs);
        let (extra, _) = prune::env_keep();
        for id in prune::keep_set(&known, &extra) {
            if prune::is_retained(&id) || !required(&id) {
                continue;
            }
            targets.push(KeepTarget { id, primary: false });
        }
        Self { targets }
    }

    /// 显式给定目标（测试 / 只读探测用）。
    pub fn from_targets(targets: Vec<KeepTarget>) -> Self {
        Self { targets }
    }

    pub fn targets(&self) -> &[KeepTarget] {
        &self.targets
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

/// 单个方案、给定 build/ 目录的判定（唯一实现；prebuilt::schema_ready 只是薄封装）。
pub fn schema_state_in(build_dir: &Path, id: &str, primary: bool) -> SchemaState {
    let yaml = build_dir.join(format!("{id}.schema.yaml"));
    let schema_yaml = if !yaml.is_file() {
        State::Missing
    } else if primary && !has_marker(&yaml) {
        State::StaleNoMarker
    } else {
        State::Ok
    };
    let bins = if ["prism", "table", "reverse"]
        .iter()
        .any(|suffix| build_dir.join(format!("{id}.{suffix}.bin")).is_file())
    {
        State::Ok
    } else {
        State::Missing
    };
    SchemaState {
        id: id.to_string(),
        primary,
        schema_yaml,
        bins,
    }
}

/// 薄封装：某个方案在用户目录里可用吗（prebuilt::schema_ready 走这里）。
pub fn schema_ok_in(build_dir: &Path, id: &str, primary: bool) -> bool {
    schema_state_in(build_dir, id, primary).usable()
}

/// 文件里有没有随包插件标记（prebuilt::repair_schema 也用它）。
pub fn has_marker(path: &Path) -> bool {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).contains(MARKER),
        Err(_) => false,
    }
}

/// **唯一**的「产物是否可用」实现（只读，不写任何文件）。
pub fn assess(rime_dir: &Path, data_dirs: &[PathBuf], retained: &KeepSet) -> Assessment {
    let owned;
    let keep = if retained.is_empty() {
        // 调用方没给集合（例如只读探测）时按数据目录自行解析。
        owned = KeepSet::resolve(rime_dir, data_dirs, None);
        &owned
    } else {
        retained
    };
    let build_dir = rime_dir.join("build");
    let schemas: Vec<SchemaState> = keep
        .targets()
        .iter()
        .map(|target| schema_state_in(&build_dir, &target.id, target.primary))
        .collect();

    let build_ok = !schemas.is_empty() && schemas.iter().all(SchemaState::usable);
    let reason = if schemas.is_empty() {
        Reason::NoTargets
    } else if build_ok {
        Reason::Ready
    } else {
        let schema_missing = schemas
            .iter()
            .any(|schema| schema.schema_yaml == State::Missing);
        let schema_stale = schemas
            .iter()
            .any(|schema| schema.schema_yaml == State::StaleNoMarker);
        let bins_missing = schemas.iter().any(|schema| schema.bins == State::Missing);
        match (schema_missing, schema_stale, bins_missing) {
            (true, false, false) => Reason::SchemaMissing,
            (false, true, false) => Reason::SchemaStale,
            (false, false, true) => Reason::BinsMissing,
            _ => Reason::Mixed,
        }
    };
    Assessment {
        schemas,
        build_ok,
        reason,
    }
}

// ---- 常驻小键盘判定（缺陷三：小键盘）---------------------------------------

/// 逐方案状态：一切正常。
const NUMPAD_OK: &str = "ok";
/// 增强组件（数字小键盘 / 中文标点直出 / v 转数字）在本机是否可用。
///
/// 这四个组件是 rime-sync 自研、编进 librime 本体的（不是上游组件）。
/// 发行版 librime 里没有它们，方案里一引用就报
/// `error creating processor: 'numpad'`，**整个方案直接不可用**。
///
/// 所以「机器上装的是不是我们的自建输入法」必须能查出来，
/// 否则用户只会看到「同步程序装上了，可小键盘怎么没有」，却没人告诉他原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtraSupport {
    /// 引擎里有我们的组件 → 增强功能可用。
    Supported,
    /// 引擎确定没有 → 增强功能不可用，需要换装我们的输入法。
    Unsupported,
    /// 找不到引擎，判断不了。
    Unknown,
}

impl ExtraSupport {
    /// 报告里的机器可读值。
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtraSupport::Supported => "supported",
            ExtraSupport::Unsupported => "unsupported",
            ExtraSupport::Unknown => "unknown",
        }
    }

    /// 增强功能是不是确定可用。
    pub fn usable(&self) -> bool {
        matches!(self, ExtraSupport::Supported)
    }

    /// 一句人话建议（直接给用户看，别让用户猜术语）。
    pub fn advice(&self) -> &'static str {
        match self {
            ExtraSupport::Supported => "增强功能可用（数字小键盘 / 中文标点直出 / v 转数字）。",
            ExtraSupport::Unsupported => {
                "当前输入法不是 rime-sync 自建版，引擎里没有增强组件（数字小键盘 / 中文标点直出 / v 转数字），方案引用它们会直接报错，整个输入法用不了。需要「删除原来的输入法，全新安装我们的输入法」：Linux 用安装包重装（离线安装包或在线安装包都行），Windows 点一次「修复输入法」程序会自动换上。"
            }
            ExtraSupport::Unknown => {
                "暂时判断不了本机输入法是否带增强功能。点一次「修复输入法」让它重新检查并补齐；若仍提示没有，请用安装包重装（离线安装包或在线安装包都行）。"
            }
        }
    }
}

/// 引擎库里的标记：我们的自建 librime 里有这个字符串，发行版没有。
const EXTRA_ENGINE_MARKER: &[u8] = b"rimesync";

/// 二进制文件里有没有这个标记（流式读，不把整库读进内存）。
fn file_has_marker(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let Ok(read) = file.read(&mut buf) else {
            return false;
        };
        if read == 0 {
            return false;
        }
        let mut window = tail.clone();
        window.extend_from_slice(&buf[..read]);
        if window
            .windows(EXTRA_ENGINE_MARKER.len())
            .any(|part| part == EXTRA_ENGINE_MARKER)
        {
            return true;
        }
        // 保留尾巴，避免标记横跨两块时被漏掉。
        let keep = EXTRA_ENGINE_MARKER
            .len()
            .saturating_sub(1)
            .min(window.len());
        tail = window[window.len() - keep..].to_vec();
    }
}

/// 一次引擎库扫描的目标：目录 + 文件名前缀 + 递归深度。
pub struct EngineLibTarget {
    /// 要扫的目录。
    pub dir: PathBuf,
    /// 库文件名前缀（Unix 侧 librime.so，Windows 侧 rime.dll）。
    pub prefix: &'static str,
    /// 往子目录里再扫几层：0 = 只看本目录，1 = 再看一级子目录，2 = 再看两级。
    pub depth: usize,
}

/// 一次扫描最多访问多少个目录（防御性上限）。
///
/// 为什么要有它：扫描会递归 read_dir 每一个子目录，万一目标目录异常庞大
/// （比如误把构建产物目录 target/debug 当成了候选，那底下有成千上万个子目录），
/// 光遍历就能把「检查输入法」拖成卡死。超过上限就停止下钻，
/// 把已经找到的结果原样返回 —— 少找到几个候选库，最坏也只是退回「判断不了」，
/// 绝不会把程序卡死。
const ENGINE_LIB_SCAN_BUDGET: usize = 2048;

/// 目录里（以及 depth 层子目录里）文件名以 prefix 开头的库文件。
///
/// 纯函数：不看平台、不读环境变量，只认传进来的参数，
/// 所以「能不能扫到两级子目录里的 rime.dll」这件事，
/// 在 Linux 上用临时目录就能测出来，不必真有一台 Windows 机器。
pub fn engine_libs_under(dir: &Path, prefix: &str, depth: usize) -> Vec<PathBuf> {
    let mut budget = ENGINE_LIB_SCAN_BUDGET;
    engine_libs_scan(dir, prefix, depth, &mut budget)
}

/// 同 engine_libs_under，但目录访问预算由调用方给定。
///
/// budget = 最多还能访问几个目录（0 表示一个都不许访问，直接返回空）。
pub fn engine_libs_under_with_budget(
    dir: &Path,
    prefix: &str,
    depth: usize,
    budget: usize,
) -> Vec<PathBuf> {
    let mut left = budget;
    engine_libs_scan(dir, prefix, depth, &mut left)
}

/// 真正的递归扫描（budget 是「还能访问几个目录」的计数器，跨递归共享）。
fn engine_libs_scan(dir: &Path, prefix: &str, depth: usize, budget: &mut usize) -> Vec<PathBuf> {
    if *budget == 0 {
        return Vec::new();
    }
    // 访问本目录也算掉一份预算。
    *budget -= 1;
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) {
            if path.is_file() {
                out.push(path);
            }
        } else if path.is_dir() {
            subdirs.push(path);
        }
    }
    if depth > 0 {
        for sub in subdirs {
            // 预算用完就停止下钻，已经找到的结果照常返回。
            if *budget == 0 {
                break;
            }
            out.extend(engine_libs_scan(&sub, prefix, depth - 1, budget));
        }
    }
    out
}

/// 本机可能装着引擎库的候选位置。
///
/// 平台只影响这张候选表（目录、前缀、扫几层），扫描代码只有一份：
/// * Windows：Program Files 下的 Rime 目录（官方 Weasel 与自建版都装这儿，
///   扫两层足够覆盖 Rime/weasel-版本号/rime.dll），以及当前可执行文件的同级目录
///   （可执行文件本身可能就装在 weasel-版本号 那一层里）；
/// * Unix：发行版 / 自建 librime 的常见落点（/usr/lib 等 + 一级子目录）。
///
/// Unix 侧刻意**不扫可执行文件所在目录**：那边是构建目录（target/debug），
/// 底下有成千上万个子目录，扫它纯属白费力气，而引擎库根本不会装在那儿。
///
/// 非 Windows 上通常读不到 ProgramFiles，读不到就跳过（不报错、不 panic）。
pub fn engine_lib_targets() -> Vec<EngineLibTarget> {
    let mut targets = Vec::new();
    // Program Files 下的 Rime 目录（32 位安装走 ProgramFiles(x86)）。
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(base) = std::env::var(var) {
            if !base.trim().is_empty() {
                targets.push(EngineLibTarget {
                    dir: Path::new(&base).join("Rime"),
                    prefix: "rime.dll",
                    depth: 2,
                });
            }
        }
    }
    // Windows：当前可执行文件的同级目录（只扫这一层，不上溯到 Program Files 顶层，
    // 那一层太宽，ProgramFiles/Rime 那一项已经能覆盖 Weasel 的真实位置）。
    #[cfg(windows)]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                targets.push(EngineLibTarget {
                    dir: dir.to_path_buf(),
                    prefix: "rime.dll",
                    depth: 1,
                });
            }
        }
    }
    // Unix 侧系统库目录（一级子目录足够：x86_64-linux-gnu 之类）。
    #[cfg(unix)]
    for base in ["/usr/lib", "/usr/lib64", "/lib", "/lib64", "/usr/local/lib"] {
        targets.push(EngineLibTarget {
            dir: PathBuf::from(base),
            prefix: "librime.so",
            depth: 1,
        });
    }
    targets
}

/// 本机可能正在用的引擎库（跨平台一套扫描逻辑，全程共用一份目录预算）。
fn system_engine_libs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut budget = ENGINE_LIB_SCAN_BUDGET;
    for target in engine_lib_targets() {
        out.extend(engine_libs_scan(
            &target.dir,
            target.prefix,
            target.depth,
            &mut budget,
        ));
    }
    out
}

/// 本机引擎是否带增强组件。
///
/// 判据（按可信度排序）：
///
/// 1. 系统里正在用的 librime 库里有我们的标记 → 确定支持（发行版没有）；
/// 2. 已编译产物 / 用户配置里有我们的标记 → 确定支持；
/// 3. 都查不到 → 判断不了（可能是没编译过，也可能是发行版引擎）。
pub fn assess_extra_support(rime_dir: &Path) -> ExtraSupport {
    let system_libs = system_engine_libs();
    for lib in &system_libs {
        if file_has_marker(lib) {
            return ExtraSupport::Supported;
        }
    }
    // 产物侧信号：我们的定制确实编译进去过。
    if let Ok(entries) = fs::read_dir(rime_dir.join("build")) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                if let Ok(bytes) = fs::read(entry.path()) {
                    if bytes
                        .windows(EXTRA_ENGINE_MARKER.len())
                        .any(|part| part == EXTRA_ENGINE_MARKER)
                    {
                        return ExtraSupport::Supported;
                    }
                }
            }
        }
    }
    // 用户配置侧信号：装输入法时铺的模板里有我们的组件。
    for id in crate::managed_custom::list_user_customs(rime_dir) {
        let path = rime_dir.join(format!("{id}.custom.yaml"));
        if let Ok(bytes) = fs::read(&path) {
            if bytes
                .windows(EXTRA_ENGINE_MARKER.len())
                .any(|part| part == EXTRA_ENGINE_MARKER)
            {
                return ExtraSupport::Supported;
            }
        }
    }
    // 找到了系统引擎库却没标记 → 确定不支持。
    // （Windows 上装的是官方 Weasel 而不是我们的自建版，就是这个情况。）
    if !system_libs.is_empty() {
        return ExtraSupport::Unsupported;
    }
    ExtraSupport::Unknown
}

/// 逐方案状态：来源侧就坏（用户 custom.yaml 自己定义的 processors 里没有 numpad）。
const NUMPAD_SOURCE_STALE: &str = "source-stale";
/// 逐方案状态：来源侧没问题，但编译产物里没有 numpad（还没重编译）。
const NUMPAD_ARTIFACT_STALE: &str = "artifact-stale";
/// 逐方案状态：压根没有编译产物（还没编译过，不算缺口）。
const NUMPAD_NO_ARTIFACT: &str = "no-artifact";
/// 逐方案状态：没有 rime-sync 的引擎定制 → 不判定、不报缺口。
const NUMPAD_SKIPPED: &str = "skipped";

/// 小键盘判定结果（只读；序列化给 CLI/GUI 用）。
#[derive(Debug, Clone, Serialize)]
pub struct NumpadStatus {
    /// 是否一切正常（没有缺口）。
    pub ok: bool,
    /// 有缺口的方案：形如 "wubi86=artifact-stale"。
    pub missing: Vec<String>,
    /// 逐方案状态：形如 "wubi86=artifact-stale" / "wubi86=ok" /
    /// "wubi86=no-artifact" / "wubi86=skipped"。
    pub detail: Vec<String>,
    /// 人话结论（可执行的下一步），例如 "小键盘处理器尚未编译进产物，请重启一次输入法"。
    pub hint: String,
}

impl NumpadStatus {
    /// 某个方案的状态（"<id>=<state>" 里 "<state>" 那部分）。
    pub fn state_of(&self, id: &str) -> Option<&str> {
        let prefix = format!("{id}=");
        self.detail
            .iter()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
    }
}

/// 常驻小键盘判定：检查保留主方案的编译产物是否含 numpad 处理器。
/// 只读；不写任何文件。
///
/// **每轮都重新判**，不依赖「本轮是否修过文件」：修完用户 custom.yaml 之后
/// 引擎一直没重编译的话，后续轮次必须继续报 —— 否则又回到缺陷三当初那种
/// 「报告说 ok、小键盘其实用不了」的静默失败。
///
/// 只查保留的主方案（prune::retained_schemas）：依赖方案的产物本来就没有 numpad。
/// 判定口径（每一档都是为了防误报）：
/// * 来源侧 = 用户目录的 `<id>.custom.yaml`：是 rime-sync 管理的、定制还开着，
///   并且**自己定义了** processors 列表却少了 numpad → source-stale（可修）；
/// * 产物侧 = 用户目录的 `build/<id>.schema.yaml`（引擎真正加载的东西）：
///   产物在、但没有 numpad → artifact-stale（还没重编译）；
/// * 产物压根不存在 → no-artifact（还没编译过，不算缺口）；
/// * 用户自己改过的文件 / 用户关掉了定制 → skipped（既不改也不报）。
pub fn assess_numpad(rime_dir: &Path, data_dirs: &[PathBuf]) -> NumpadStatus {
    let mut detail = Vec::new();
    let mut missing = Vec::new();
    for id in prune::retained_schemas() {
        let state = numpad_state(rime_dir, data_dirs, id);
        detail.push(format!("{id}={state}"));
        if state == NUMPAD_SOURCE_STALE || state == NUMPAD_ARTIFACT_STALE {
            missing.push(format!("{id}={state}"));
        }
    }
    let ok = missing.is_empty();
    let hint = if ok {
        String::new()
    } else {
        numpad_hint(&missing)
    };
    NumpadStatus {
        ok,
        missing,
        detail,
        hint,
    }
}

/// 单个方案的逐档判定（只读）。
fn numpad_state(rime_dir: &Path, data_dirs: &[PathBuf], id: &str) -> &'static str {
    let custom_text = fs::read(rime_dir.join(format!("{id}.custom.yaml")))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    // 来源侧：这份 custom.yaml 是不是 rime-sync 管的、定制是不是还开着。
    let source_active = custom_text
        .as_deref()
        .map(|text| managed_custom::is_managed(text) && managed_custom::customization_active(text))
        == Some(true);
    if custom_text.is_some() && !source_active {
        // 用户自己写的 / 用户主动关掉了定制：既不改，也不报缺口。
        return NUMPAD_SKIPPED;
    }
    let artifact_text = fs::read(rime_dir.join("build").join(format!("{id}.schema.yaml")))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    // 产物侧信号：rime-sync 的定制确实编译进产物了。
    let artifact_active = artifact_text
        .as_deref()
        .map(|text| text.contains("rimesync_vnum") || text.contains("rimesync_punct"))
        == Some(true);
    if !source_active && !artifact_active {
        // 用户没开 rime-sync 的引擎定制 → 不预期 → 一律不报。
        return NUMPAD_SKIPPED;
    }
    if source_active {
        // 只有「这份文件自己定义了 processors 列表」才归 source-stale；
        // 没定义的话处理器列表来自共享方案，不在这里下结论。
        if let Some(list) = custom_text
            .as_deref()
            .and_then(managed_custom::source_processors)
        {
            if !list.iter().any(|name| name == data_patch::NUMPAD_PROCESSOR) {
                return NUMPAD_SOURCE_STALE;
            }
        }
    }
    match artifact_text.as_deref() {
        None => {
            // 产物压根没有 = 还没编译过，不算缺口（build 不可用由 assess 判）。
            if data_dirs
                .iter()
                .any(|dir| dir.join(format!("{id}.schema.yaml")).is_file())
            {
                NUMPAD_NO_ARTIFACT
            } else {
                NUMPAD_SKIPPED
            }
        }
        Some(text) => {
            if managed_custom::artifact_text_has_numpad(text) {
                NUMPAD_OK
            } else {
                NUMPAD_ARTIFACT_STALE
            }
        }
    }
}

/// 缺口的人话下一步（用户照着就能做）。
fn numpad_hint(missing: &[String]) -> String {
    let source: Vec<&str> = missing
        .iter()
        .filter(|item| item.ends_with(NUMPAD_SOURCE_STALE))
        .map(String::as_str)
        .collect();
    let artifact: Vec<&str> = missing
        .iter()
        .filter(|item| item.ends_with(NUMPAD_ARTIFACT_STALE))
        .map(String::as_str)
        .collect();
    if !source.is_empty() {
        return format!(
            "用户 custom.yaml（{}）里还缺 numpad 处理器：再点一次「修复输入法」把它补上（补上后还需要重编译一次）。",
            source.join(" ")
        );
    }
    let ids = artifact.join(" ");
    if cfg!(windows) {
        format!(
            "小键盘处理器（numpad）还没编译进产物（{ids}）：请再点一次「修复输入法」，它会触发进程内部署重编译。"
        )
    } else {
        format!(
            "小键盘处理器（numpad）还没编译进产物（{ids}）：请重启一次输入法，引擎启动/维护时会自动重编译。"
        )
    }
}

/// 一次 config 模板播种的结果（安装版首次启动用）。
///
/// 语义与旧 NSIS 钩子的 RimeSyncSeedRimeConfig 一致：
/// * 目标已存在 → 沿用（reused），不改名、不备份、不覆盖；
/// * 缺失才写（written）；
/// * 模板要求的方案不在输入法 data 里 → 跳过（skipped），否则 librime 部署会
///   把缺失方案算成 failure、整次部署失败；
/// * 写失败只记录（failed），绝不因此让输入法自举整体失败。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SeedReport {
    pub written: Vec<String>,
    pub reused: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

impl SeedReport {
    pub fn changed(&self) -> bool {
        !self.written.is_empty()
    }

    /// 一句话明细（action / 日志 / CLI JSON 共用）。
    pub fn summary(&self) -> String {
        format!(
            "written=[{}] reused=[{}] skipped=[{}] failed=[{}]",
            self.written.join(","),
            self.reused.join(","),
            self.skipped.join(","),
            self.failed.join(",")
        )
    }
}

/// 把随包 config 模板播进用户 Rime 目录（**缺失才写**）。
///
/// A-4：config 模板只有**一份源** —— payload 的 `config/` 目录（扁平的
/// `<name>.custom.yaml` 约定）：
///   default.custom.yaml / weasel.custom.yaml / <schema>.custom.yaml
///
/// 以前还有第二份（`resources/schemas/<id>.custom.yaml`，Windows 资源目录里
/// 另拷了一份），两份迟早漂移。现在 Windows 资源目录里**不再**带 schemas/，
/// 模板统一从 payload 读；`packaging/rime/prebuilt/config/` 由
/// `scripts/ime/gen-prebuilt-build.py` 从 `packaging/rime/schemas/` 生成
/// （真正的编辑入口仍是 packaging/rime/schemas/，那是**唯一手写源**），
/// `packaging/windows/check-config-sources.sh` 断言两者不漂移。
///
/// 只在 Windows 安装版调用（Linux 的 /usr/share/rime-data 由发行包负责）。
pub fn seed_templates(config_dir: &Path, rime_dir: &Path, data_dirs: &[PathBuf]) -> SeedReport {
    let mut report = SeedReport::default();
    if let Err(error) = fs::create_dir_all(rime_dir) {
        report
            .failed
            .push(format!("{TEMPLATE_DEFAULT}:create-dir:{error}"));
        return report;
    }
    // 1) default.custom.yaml：列了随包方案（含五笔），data 里必须真有 wubi86。
    let default_src = config_dir.join(TEMPLATE_DEFAULT);
    if default_src.is_file() {
        if let Some(reason) = prebuilt::config_gate_skip(TEMPLATE_DEFAULT, data_dirs) {
            report.skipped.push(format!("{TEMPLATE_DEFAULT}({reason})"));
        } else {
            seed_one(&default_src, rime_dir, TEMPLATE_DEFAULT, &mut report);
        }
    } else {
        report
            .skipped
            .push(format!("{TEMPLATE_DEFAULT}(no-template)"));
    }

    // 2) weasel.custom.yaml：只影响外观，不依赖 data，无条件播。
    let weasel_src = config_dir.join(TEMPLATE_WEASEL);
    if weasel_src.is_file() {
        seed_one(&weasel_src, rime_dir, TEMPLATE_WEASEL, &mut report);
    } else {
        report
            .skipped
            .push(format!("{TEMPLATE_WEASEL}(no-template)"));
    }

    // 3) <schema>.custom.yaml：data 里没有 <id>.schema.yaml 就跳过。
    //
    // 目录是**扁平的**（<id>.custom.yaml 直接躺在 config/ 下，与 payload config
    // 同一份布局）：这是 A-4 统一后的约定，不再有 schemas/ 子目录。
    let mut entries: Vec<(String, PathBuf)> = fs::read_dir(config_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name == TEMPLATE_DEFAULT || name == TEMPLATE_WEASEL {
                        return None;
                    }
                    let id = name.strip_suffix(TEMPLATE_SCHEMA_SUFFIX)?;
                    Some((id.to_string(), entry.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (id, src) in entries {
        let name = format!("{id}{TEMPLATE_SCHEMA_SUFFIX}");
        if let Some(reason) = prebuilt::config_gate_skip(&name, data_dirs) {
            report.skipped.push(format!("{name}({reason})"));
        } else {
            seed_one(&src, rime_dir, &name, &mut report);
        }
    }
    report
}

/// 单个模板：目标存在 → reused；否则拷贝（成功 written / 失败 failed）。
fn seed_one(src: &Path, dir: &Path, name: &str, report: &mut SeedReport) {
    let dst = dir.join(name);
    if dst.is_file() {
        report.reused.push(name.to_string());
        return;
    }
    match fs::copy(src, &dst) {
        Ok(_) => report.written.push(name.to_string()),
        Err(error) => report.failed.push(format!("{name}:{error}")),
    }
}

/// 入口身份：决定要不要跑进程内部署、patch 失败算不算问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 安装版首次启动 / CLI ensure-ime --mode install：除了产物，还要保证
    /// 输入法本体在（缺失或不是我们的构建 → 随包安装器静默装回）、按旧钩子语义
    /// 播种 config 模板。安装器只负责程序本体，这一档接管了原先 NSIS 的全部职责。
    Install,
    /// 用户显式修复（设置页 / CLI fix-ime）。
    Fix,
    /// 应用启动自举：绝不打断正在打字的用户，绝不跑进程内 librime。
    Bootstrap,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Fix => "fix",
            Self::Bootstrap => "bootstrap",
        }
    }

    fn fix_source(self) -> FixSource {
        match self {
            Self::Bootstrap => FixSource::Bootstrap,
            _ => FixSource::Explicit,
        }
    }
}

/// ensure 的输入。
#[derive(Clone)]
pub struct Options {
    pub mode: Mode,
    /// 覆盖已存在的 build 文件（等价 RIMESYNC_PREBUILT=force）。
    pub force: bool,
    /// exe 所在目录：Mode::Install 用它找 resources/weasel（安装器）与
    /// resources/{default,weasel}.custom.yaml + schemas/（config 模板）。
    pub exe_dir: Option<PathBuf>,
    /// Mode::Install 是否安装/修复输入法本体（Windows 才真的跑安装器）。
    pub install_ime: bool,
    /// Mode::Install 是否播种 config 模板（仅 Windows 安装版）。
    pub seed_templates: bool,
    /// 预编译产物来源标签（报告/日志用；空 = 用 payload 路径）。
    pub source: String,
    /// 是否给 data 方案打补丁。
    pub patch_data: bool,
    /// 是否按 data_patch 自己探测的目录打补丁（false = 只用传入的 data_dirs，测试用）。
    pub auto_data_patch: bool,
    /// 补丁注释里的版本（None = data_patch::resolve_version(None)）。
    pub version: Option<String>,
    /// 是否允许部署（进程内 / 结束服务 / touch）。
    pub deploy: bool,
    /// 重新解析 data 目录的入口（None = data_patch::data_dirs）。
    ///
    /// A-6：M1「装完输入法 → 重新解析 data_dirs」这一步必须**可注入**，否则
    /// 「装完才出现 wubi86.schema.yaml」这个分支永远只能靠真机验证。
    /// 测试注入一个记录调用顺序的闭包，就能断言
    /// install → re-resolve → patch 的次序。
    pub data_resolver: Option<Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>>,
    /// 方案裁剪（prune）是否由 ensure 负责（A-5）。
    ///
    /// Install/Fix 模式：true —— CLI 与 GUI 走同一条入口，结论一致。
    /// Bootstrap：false —— 启动自举维持现状（自己在 bootstrap 里裁剪）。
    pub prune: bool,
    /// build/ 已可用时是否仍然铺一次 payload config/（A-3）。
    ///
    /// true = 每次 ensure 都保证 default.custom.yaml 在位（幂等：缺才写）。
    /// Install/Fix 模式：true。Bootstrap：false（启动期不做多余写入）。
    pub always_config: bool,
}

/// 手写 Debug：data_resolver 是闭包（没有 Debug）。
impl std::fmt::Debug for Options {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Options")
            .field("mode", &self.mode)
            .field("force", &self.force)
            .field("exe_dir", &self.exe_dir)
            .field("install_ime", &self.install_ime)
            .field("seed_templates", &self.seed_templates)
            .field("source", &self.source)
            .field("patch_data", &self.patch_data)
            .field("auto_data_patch", &self.auto_data_patch)
            .field("version", &self.version)
            .field("deploy", &self.deploy)
            .field(
                "data_resolver",
                &self.data_resolver.as_ref().map(|_| "injected"),
            )
            .field("prune", &self.prune)
            .field("always_config", &self.always_config)
            .finish()
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Fix,
            force: false,
            exe_dir: None,
            install_ime: false,
            seed_templates: false,
            source: String::new(),
            patch_data: true,
            auto_data_patch: true,
            version: None,
            deploy: true,
            data_resolver: None,
            prune: false,
            always_config: false,
        }
    }
}

impl Options {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// 安装版首次启动的默认语义：装输入法本体 + 播种模板 + 铺产物 + patch + 部署。
    pub fn install() -> Self {
        Self {
            install_ime: true,
            seed_templates: true,
            // A-5：裁剪纳入 ensure —— CLI 与 GUI 走同一条入口。
            prune: true,
            // A-3：build 已可用时也铺一次 payload config（缺才写）。
            always_config: true,
            ..Self::new(Mode::Install)
        }
    }

    /// 修复（CLI fix-ime / GUI「修复输入法」）：与安装同一套前置步骤。
    ///
    /// A-1：install_ime=true —— 「首次装输入法」的机器上，Fix 也必须先把输入法
    /// 装上（否则 data 补丁打在不存在的目录上，assess 只看 build）。
    pub fn fix() -> Self {
        Self {
            install_ime: true,
            seed_templates: false,
            // A-5：裁剪纳入 ensure —— CLI 与 GUI 走同一条入口。
            prune: true,
            // A-3：build 已可用时也铺一次 payload config（缺才写）。
            always_config: true,
            ..Self::new(Mode::Fix)
        }
    }

    /// 应用启动自举（M5）：装输入法本体 + 播模板，但**部署 deferred**：
    /// 不 taskkill、不跑部署器，把「什么时候生效」交给用户（或引擎下次启动）。
    pub fn bootstrap() -> Self {
        Self {
            install_ime: true,
            seed_templates: true,
            // A-5：裁剪统一由 ensure 负责（Bootstrap 也走这一条）—— 单一入口，
            // 裁剪结果随 Outcome 一起回来（changed 的计算也才准）。
            prune: true,
            // A-3：Bootstrap 不做多余写入（config 交给 Install/Fix 路径）。
            always_config: false,
            ..Self::new(Mode::Bootstrap)
        }
    }

    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// exe 目录（找 resources/weasel 与 config 模板）。
    pub fn with_exe_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.exe_dir = dir;
        self
    }

    /// 关闭输入法本体安装（测试 / 只修产物时用）。
    pub fn with_install_ime(mut self, install: bool) -> Self {
        self.install_ime = install;
        self
    }

    /// 关闭 config 模板播种（测试用）。
    pub fn with_seed_templates(mut self, seed: bool) -> Self {
        self.seed_templates = seed;
        self
    }

    pub fn with_patch_data(mut self, patch_data: bool) -> Self {
        self.patch_data = patch_data;
        self
    }

    pub fn with_auto_data_patch(mut self, auto: bool) -> Self {
        self.auto_data_patch = auto;
        self
    }

    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }

    pub fn with_deploy(mut self, deploy: bool) -> Self {
        self.deploy = deploy;
        self
    }

    /// 注入 data 目录解析器（A-6：让 M1 的「重解析」分支可测）。
    pub fn with_data_resolver(
        mut self,
        resolver: impl Fn() -> Vec<PathBuf> + Send + Sync + 'static,
    ) -> Self {
        self.data_resolver = Some(Arc::new(resolver));
        self
    }

    /// 方案裁剪是否由 ensure 负责（A-5）。
    pub fn with_prune(mut self, prune: bool) -> Self {
        self.prune = prune;
        self
    }

    /// build/ 已可用时是否仍铺 payload config（A-3）。
    pub fn with_always_config(mut self, always: bool) -> Self {
        self.always_config = always;
        self
    }
}

/// 一次 ensure 的结论。**只能**由最终 assess 与本次实际动作推出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutcomeResult {
    /// 本次改过东西，且最终可用。
    Ok,
    /// 已可用，且本次一个字节都没改。
    Noop,
    /// 最终可用，但本次有改动失败（detail 必非空）。
    Partial,
    /// 最终不可用（detail 必非空）。
    Failed,
}

impl OutcomeResult {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Noop => "noop",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    /// CLI 退出码：只有 Ok/Noop 算成功。
    pub fn exit_ok(self) -> bool {
        matches!(self, Self::Ok | Self::Noop)
    }
}

/// 本次做的一步（报告/日志用）。
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// assess:before / install / data / deploy / assess:after。
    pub kind: String,
    /// 这一步是否真的改了磁盘（Ok 必须至少有一个 changed=true 的 action）。
    pub changed: bool,
    pub detail: String,
}

impl Action {
    fn new(kind: impl Into<String>, changed: bool, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            changed,
            detail: detail.into(),
        }
    }
}

/// ensure 的输出。
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub result: OutcomeResult,
    pub actions: Vec<Action>,
    pub report: PrebuiltReport,
    /// Partial/Failed 必非空；Ok/Noop 也可以带说明。
    pub detail: String,
    /// 最终 assess（report.build_ok 与它一致）。
    pub assessment: Assessment,
    /// 本次真正补上的方案（= report.build_repaired）。
    pub repaired: Vec<String>,
    /// data 方案补丁结果（patch_data=false / build 不可用时是空报告）。
    pub data: DataPatchReport,
    /// 输入法本体（Weasel）的检查/安装结果；非 Mode::Install 时为默认空报告。
    pub ime: ImeReport,
    /// config 模板播种结果；非 Mode::Install / 非 Windows 时为空。
    pub seeded: SeedReport,
    /// B-2：payload config/ 里被 data 门禁挡下的模板（= 用户拿到的输入法
    /// 没有 schema_list）。非空即降级为 partial（见 prebuilt::finish_deploy）。
    pub config_skipped: Vec<String>,
    /// A-5：方案裁剪结果（ensure 内部跑的；Bootstrap 模式下是空报告）。
    pub prune: prune::PruneReport,
    /// 常驻小键盘判定（部署之后判的；CLI/GUI 都读这一份，不许自己再算一遍）。
    pub numpad: NumpadStatus,
}

impl Outcome {
    /// CLI 退出码 / NSIS 自检用的成功判定。
    pub fn ok(&self) -> bool {
        self.result.exit_ok() && self.assessment.build_ok
    }
}

fn report_source(opts: &Options, payload: Option<&Path>) -> String {
    if !opts.source.trim().is_empty() {
        return opts.source.clone();
    }
    payload
        .map(|dir| dir.display().to_string())
        .unwrap_or_default()
}

/// 把最终 assess 写回报告（report 里的 build_* 字段只能由这里更新）。
fn sync_report(report: &mut PrebuiltReport, assessment: &Assessment) {
    report.build_ok = assessment.build_ok;
    let any_present = assessment
        .schemas
        .iter()
        .any(|schema| schema.schema_yaml != State::Missing);
    report.probe_after = if assessment.build_ok {
        "prebuilt".to_string()
    } else if any_present {
        "deploy".to_string()
    } else {
        "none".to_string()
    };
    report.build_source = report.probe_after.clone();
    report.build_schemas = assessment
        .schemas
        .iter()
        .filter(|schema| schema.primary)
        .map(|schema| SchemaBuild {
            id: schema.id.clone(),
            primary: true,
            state: if schema.usable() {
                if report.build_repaired.iter().any(|id| id == &schema.id) {
                    prebuilt::STATE_REPAIRED
                } else {
                    prebuilt::STATE_OK
                }
            } else {
                prebuilt::STATE_MISSING
            }
            .to_string(),
        })
        .collect();
    report.build_missing = assessment.missing_ids();
}

/// Mode::Install 前置步骤失败（输入法本体装不上）时的结论。
///
/// 不做任何产物假设：assessment 只反映当前磁盘状态，result 固定 Failed，
/// detail 必须给出用户能执行的下一步（以管理员重启 / 修复输入法 / 发日志）。
#[allow(clippy::too_many_arguments)]
fn failed_outcome(
    rime_dir: &Path,
    data_dirs: &[PathBuf],
    retained: &KeepSet,
    source: String,
    actions: Vec<Action>,
    detail: String,
    ime: ImeReport,
    seeded: SeedReport,
) -> Outcome {
    let after = assess(rime_dir, data_dirs, retained);
    let mut report = PrebuiltReport::empty(source);
    sync_report(&mut report, &after);
    Outcome {
        result: OutcomeResult::Failed,
        actions,
        report,
        detail,
        assessment: after,
        repaired: Vec::new(),
        data: DataPatchReport::default(),
        ime,
        seeded,
        config_skipped: Vec::new(),
        prune: prune::PruneReport::default(),
        numpad: assess_numpad(rime_dir, data_dirs),
    }
}

/// data 目录列表拼成一行日志（data-dirs 动作 / 报告用）。
fn join_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(";")
}

/// 重新解析 data 目录：注入的解析器优先，否则用 data_patch 的现场探测。
///
/// A-6：把「装完输入法 → 重新解析 data_dirs」这一步做成可注入的纯函数入口，
/// 单测才能断言 M1 分支（装完才出现 wubi86.schema.yaml）真的被走到。
pub fn resolve_data_dirs(opts: &Options) -> Vec<PathBuf> {
    match &opts.data_resolver {
        Some(resolver) => resolver(),
        None => data_patch::data_dirs(),
    }
}

/// 修用户目录里的 `<id>.custom.yaml`（缺陷三：小键盘）。
///
/// 返回 (是否真的写了盘, 本次修好的方案 id, 每个文件的结果明细, 失败列表)。
/// 单个文件失败不中断整体：失败项进 `errors`，由调用方降级结论。
fn repair_user_customs(rime_dir: &Path) -> (bool, Vec<String>, Vec<String>, Vec<String>) {
    let mut changed = false;
    let mut repaired = Vec::new();
    let mut details = Vec::new();
    let mut errors = Vec::new();
    for id in managed_custom::list_user_customs(rime_dir) {
        let path = rime_dir.join(format!("{id}.custom.yaml"));
        match managed_custom::repair_file(&path) {
            Ok(managed_custom::RepairOutcome::Repaired { backup }) => {
                changed = true;
                repaired.push(id.clone());
                let backup = match backup {
                    Some(value) => format!(" backup={}", value.display()),
                    None => String::new(),
                };
                details.push(format!("{id}=repaired{backup}"));
            }
            Ok(managed_custom::RepairOutcome::Unchanged) => {
                details.push(format!("{id}=unchanged"));
            }
            Ok(managed_custom::RepairOutcome::Skipped(reason)) => {
                details.push(format!("{id}=skipped({reason})"));
            }
            Err(error) => {
                let text = format!("{error:#}");
                errors.push(format!("{id}: {text}"));
                details.push(format!("{id}=failed({text})"));
            }
        }
    }
    (changed, repaired, details, errors)
}

/// **唯一入口**：assess → （不满足才）铺 prebuilt → （需要才）patch data →
/// （需要且允许才）部署 → 再 assess → 汇总 Outcome。
///
/// payload 为 None = 没找到预编译产物：build 已可用时照常 Noop，否则 Failed。
pub fn ensure(
    rime_dir: &Path,
    payload: Option<&Path>,
    data_dirs: &[PathBuf],
    opts: &Options,
) -> Outcome {
    // 调用方解析 data_dirs 时输入法往往还没装上（全新机器 = 空）。这里持有一份可变
    // 副本：装完输入法（M1）后立刻重新解析，后面的模板门禁与方案校验都用新的。
    let mut data_dirs: Vec<PathBuf> = data_dirs.to_vec();
    let mut actions: Vec<Action> = Vec::new();
    let mut retained = KeepSet::resolve(rime_dir, &data_dirs, payload);
    let mut ime_report = ImeReport::default();
    let mut seeded = SeedReport::default();
    // A-5：裁剪结果随 Outcome 一起返回（CLI/GUI 报告与日志都用它）。
    let mut prune_report = prune::PruneReport::default();

    // ---- 前置步骤：输入法本体 + config 模板 -----------------------------------
    // 这一步接管了旧 NSIS 钩子（RimeSyncInstallWeaselPre / RimeSyncSeedRimeConfig）
    // 的全部职责：安装器只负责程序本体，输入法在程序首次启动时自举。
    // Bootstrap 也要装（缺输入法 = 打不了字），区别只在部署：见 finish_deploy 的 defer。
    //
    // A-1：**Fix 也走这一步**。旧实现在 Fix 模式下 install_ime=false，于是
    // 「首次装输入法」的机器上：调用方解析的 data_dirs 是空的（Weasel 还没装），
    // data 补丁打在不存在的目录上，assess 也退化成只看 build。
    // 现在三种模式统一：装 → 重解析 data → 播种 → 铺 prebuilt → 补丁 → 部署。
    if matches!(opts.mode, Mode::Install | Mode::Bootstrap | Mode::Fix) {
        // 首次安装时 %APPDATA%Rime 可能还不存在（Weasel 安装器通常会建，
        // 但不能假设）。目录建不出来不算致命：后面的步骤会各自报错。
        if !rime_dir.is_dir() {
            match fs::create_dir_all(rime_dir) {
                Ok(()) => actions.push(Action::new(
                    "rime-dir",
                    true,
                    format!("created {}", rime_dir.display()),
                )),
                Err(error) => actions.push(Action::new(
                    "rime-dir",
                    false,
                    format!("create failed dir={} error={error}", rime_dir.display()),
                )),
            }
        }

        if opts.install_ime {
            ime_report = ime::ensure(opts.exe_dir.as_deref());
            actions.push(Action::new(
                "weasel",
                ime_report.installed,
                format!(
                    "present={} installed={} version={} install={} needs_elevation={} installer={} error={}",
                    ime_report.present,
                    ime_report.installed,
                    if ime_report.version.is_empty() {
                        "-"
                    } else {
                        ime_report.version.as_str()
                    },
                    ime_report.install,
                    ime_report.needs_elevation,
                    if ime_report.installer.is_empty() {
                        "-"
                    } else {
                        ime_report.installer.as_str()
                    },
                    ime_report.error.as_deref().unwrap_or("-"),
                ),
            ));
            if !ime_report.ok() {
                // 没有输入法本体时铺产物毫无意义：直接给出可执行的下一步提示。
                let detail = format!(
                    "输入法未就绪：{}；{}；{}",
                    ime_report.summary(),
                    ime_report.error.as_deref().unwrap_or("未知原因"),
                    if ime_report.needs_elevation {
                        "请以管理员身份重新启动 rime-sync（设置页「以管理员重启」或右键以管理员运行）后再修复"
                    } else {
                        "请在设置页点「修复输入法」重试；仍失败请把 bootstrap.log 发给维护者"
                    }
                );
                return failed_outcome(
                    rime_dir,
                    &data_dirs,
                    &retained,
                    report_source(opts, payload),
                    actions,
                    detail,
                    ime_report,
                    seeded,
                );
            }
        }

        // M1：调用方那份 data_dirs 是**输入法安装之前**解析的（全新机器上为空），
        // 拿它做模板/门禁会把 wubi86 整份跳过。装完立刻重新解析一次。
        //
        // A-6：解析器可注入（Options::data_resolver）—— 「装完才出现
        // wubi86.schema.yaml」这个分支因此可以纯函数级地测，不必真装输入法。
        {
            let fresh = resolve_data_dirs(opts);
            // 调用方给出的 data_dirs 是权威的：只有它为空（首次安装、Weasel
            // 还没装上时调用方解析不到任何目录）才用重新解析的结果替代，
            // 否则会丢掉调用方特意指定的目录。
            let fresh = if data_dirs.is_empty() {
                fresh
            } else {
                data_dirs.clone()
            };
            if fresh != data_dirs {
                actions.push(Action::new(
                    "re-resolve",
                    true,
                    format!(
                        "refreshed before seed/patch before=[{}] after=[{}]",
                        join_dirs(&data_dirs),
                        join_dirs(&fresh)
                    ),
                ));
                data_dirs = fresh;
                // 目标方案集合依赖 data（known_schemas），必须一起重算。
                retained = KeepSet::resolve(rime_dir, &data_dirs, payload);
            }
        }

        // A-4：模板从 **payload 的 config/** 读（唯一一份源），不再从
        // resources/schemas/ 读（Windows 资源目录里已不再带第二份）。
        if opts.seed_templates && cfg!(windows) {
            if let Some(dir) = payload {
                seeded = seed_templates(&dir.join("config"), rime_dir, &data_dirs);
                actions.push(Action::new(
                    "seed",
                    seeded.changed(),
                    seeded.summary().to_string(),
                ));
            }
        }

        // B1：用户目录 default.custom.yaml 的 schema_list **增量补齐**。
        //
        // 顺序：**seed 之后 → 补齐 → prune → assess**（补齐必须在 prune 之前）。
        //
        // 为什么放在这里：
        // * seed_templates 只写「缺失才写」的模板，用户已有的 default.custom.yaml
        //   一律沿用（2026-11-02 定的安装策略）—— 升级用户的 schema_list 因此可能
        //   长期只有一两个方案，而 librime 只按 schema_list 生成 build，全量重新
        //   部署时保留方案的产物就没了（用户方案「凭空消失」）。
        // * **补齐在 prune 之前**：prune 会把不在保留清单里的条目删掉（真机 227
        //   的 luna_pinyin 就不在清单里）。先补齐再裁剪 → 最终列表正好是三个保留
        //   方案、顺序就是保留清单的顺序；反过来（先 prune 后补齐）会留下清单外的
        //   条目，而且每次 ensure 都会被「删一次 + 补一次」地改写一遍（churn）。
        //   先补齐再裁剪是幂等的：第二轮补齐发现三个都在 → Unchanged，prune 没有
        //   可删的 → dropped == 0 直接返回，一个字节都不动。
        // * 平台无关：seed_templates 只在 Windows 调用（ensure 里 cfg!(windows)），
        //   补齐必须两端一致，所以放在 ensure 这一层而不是 seed_templates 内部。
        //
        // 门禁：只补 data 目录里**真实存在**的保留方案 —— 补一个不存在的方案会让
        // librime 部署把缺失方案计成 failure、整次部署失败（与
        // prebuilt::config_gate_skip 同一个道理）。
        {
            let allowed: Vec<String> = prune::retained_schemas()
                .iter()
                .filter(|id| {
                    prebuilt::data_has_schema(&data_dirs, id)
                        || payload
                            .map(|dir| {
                                dir.join("build")
                                    .join(format!("{id}.schema.yaml"))
                                    .is_file()
                            })
                            .unwrap_or(false)
                })
                .map(|id| id.to_string())
                .collect();
            let outcome = managed_custom::repair_default_schema_list_in(rime_dir, Some(&allowed));
            let (changed, detail) = match outcome {
                Ok(managed_custom::RepairOutcome::Repaired { backup }) => {
                    let backup = backup
                        .map(|value| format!(" backup={}", value.display()))
                        .unwrap_or_default();
                    (true, format!("repaired{backup}"))
                }
                Ok(managed_custom::RepairOutcome::Unchanged) => (false, "unchanged".to_string()),
                Ok(managed_custom::RepairOutcome::Skipped(reason)) => {
                    (false, format!("skipped({reason})"))
                }
                Err(error) => (false, format!("failed({error:#})")),
            };
            actions.push(Action::new("schema-list", changed, detail));
        }

        // A-5：方案裁剪纳入 ensure（Install/Fix），位置在 seed 之后、assess 之前。
        // 这样「点按钮」与「敲命令」的动作序列完全一致（CLI/GUI 都只调 ensure）。
        if opts.prune {
            let pruned = prune::prune_auto_with(
                Some(&data_dirs),
                Some(rime_dir),
                prune::PruneOptions::default(),
            );
            prune_report = pruned;
            actions.push(Action::new(
                "prune",
                prune_report.removed() > 0,
                prune_report.summary(),
            ));
        }
    }

    let before = assess(rime_dir, &data_dirs, &retained);
    actions.push(Action::new(
        "assess:before",
        false,
        format!("{} targets={}", before.detail(), before.schemas.len()),
    ));

    let mut install_error: Option<String> = None;
    let mut report = PrebuiltReport::empty(report_source(opts, payload));
    let mut installed_changed = false;

    if before.build_ok && !opts.force {
        actions.push(Action::new(
            "install",
            false,
            "skipped reason=build-ok".to_string(),
        ));
        // A-3：build/ 已可用 ≠ config 也在位。「铺 payload config/」必须**独立**
        // 执行一次（幂等：缺才写），否则 Install 模式下 build 早就可用的机器
        // 永远拿不到 default.custom.yaml（= 没有 schema_list 的输入法）。
        //
        // 与 seed_templates 的先后：两者都是「缺才写」，谁先写谁赢，内容相同
        // （A-4：config 只有一份源）。这里 config 在 seed 之后铺 —— seed 播的是
        // resources/ 下的模板，payload config 是随包产物的一份拷贝。
        if opts.always_config {
            if let Some(dir) = payload {
                let mut lines = vec![format!("config-only payload={}", dir.display())];
                // 门禁用 ensure 手上这份 data_dirs（调用方给的，装完输入法后刷新过），
                // 不能用 resolve_data_dirs(opts) 重新探测：那是「现场找系统目录」，
                // 与 install 分支口径不一致（调用方注入的目录会被无视）。
                if let Err(error) = prebuilt::copy_payload_config(
                    dir,
                    rime_dir,
                    &data_dirs,
                    &mut report,
                    &mut lines,
                ) {
                    actions.push(Action::new(
                        "install-config",
                        false,
                        format!("failed payload={} error={error:#}", dir.display()),
                    ));
                    install_error = Some(format!("{error:#}"));
                } else {
                    let skipped = report.config_skipped.join(",");
                    let changed = report.config_copied > 0;
                    // 这一支也真的改了磁盘（写了 default.custom.yaml 等 config），
                    // 必须计入 changed —— 否则「build 已可用、config 刚补上」会被
                    // 报成 Noop（CLI / GUI / 日志都说"什么都没做"）。
                    installed_changed |= changed;
                    actions.push(Action::new(
                        "install-config",
                        changed,
                        format!(
                            "payload={} config_copied={} config_kept={} config_skipped=[{}] manifest_failed=[{}]",
                            dir.display(),
                            report.config_copied,
                            report.config_kept,
                            skipped,
                            report.manifest_failed.join(","),
                        ),
                    ));
                }
            }
        }
    } else {
        match payload {
            Some(dir) => {
                match prebuilt::install_prebuilt_with(
                    dir,
                    rime_dir,
                    InstallOptions {
                        force: opts.force,
                        // 用 ensure 里（装完输入法后）重新解析的那份做 config 门禁。
                        data_dirs: Some(data_dirs.clone()),
                    },
                ) {
                    Ok(mut result) => {
                        installed_changed = result.copied
                            + result.overwritten
                            + result.config_copied
                            + result.build_repaired.len()
                            > 0;
                        actions.push(Action::new(
                            "install",
                            installed_changed,
                            format!(
                                "payload={} copied={} kept={} overwritten={} config_copied={} config_kept={} failed={} repaired=[{}] config_skipped=[{}] manifest_failed=[{}]",
                                dir.display(),
                                result.copied,
                                result.kept,
                                result.overwritten,
                                result.config_copied,
                                result.config_kept,
                                result.failed,
                                result.build_repaired.join(","),
                                // B-2：config 被 data 门禁挡下 = 用户拿到的输入法没有
                                // schema_list。这条必须出现在 action 明细里（CLI 文本
                                // 输出与 JSON 都能看到），不能只藏在 report 里。
                                result.config_skipped.join(","),
                                result.manifest_failed.join(","),
                            ),
                        ));
                        result.source = report_source(opts, payload);
                        report = result;
                    }
                    Err(error) => {
                        let text = format!("{error:#}");
                        actions.push(Action::new(
                            "install",
                            false,
                            format!("failed payload={} error={text}", dir.display()),
                        ));
                        install_error = Some(text);
                    }
                }
            }
            None => {
                let text = format!(
                    "未找到预编译产物目录（可用 {} 指定）",
                    prebuilt::ENV_PAYLOAD_DIR
                );
                actions.push(Action::new("install", false, format!("failed {text}")));
                install_error = Some(text);
            }
        }
    }
    // install 失败时报告里的 build_repaired/build_missing 属于上一次机会，清掉避免误导；
    // 最终状态一律由 assess 决定。
    if install_error.is_some() {
        report.build_repaired.clear();
        report.build_missing.clear();
        report.build_schemas.clear();
    }

    let interim = assess(rime_dir, &data_dirs, &retained);

    // data 方案补丁：只有 build 可用时才写——build 不可用时写 data 只会造出
    // 「配置改了但输入法还是打不出字」的假成功。
    let mut data = DataPatchReport::default();
    let changed_data;
    if !opts.patch_data {
        changed_data = false;
        actions.push(Action::new("data", false, "skipped reason=disabled"));
    } else if !interim.build_ok {
        changed_data = false;
        actions.push(Action::new(
            "data",
            false,
            format!(
                "skipped reason=build-unusable ({})",
                interim.reason.as_str()
            ),
        ));
    } else {
        let version = opts
            .version
            .clone()
            .unwrap_or_else(|| data_patch::resolve_version(None));
        data = if opts.auto_data_patch {
            data_patch::patch_auto(&version, opts.mode.fix_source())
        } else {
            data_patch::patch_dirs(&data_dirs, &version)
        };
        changed_data = data.patched() > 0 || data.punctuator_patched > 0;
        actions.push(Action::new(
            "data",
            changed_data,
            format!(
                "dirs={} mode={} patched={} unchanged={} skipped={} failed={} punctuator={} version={}",
                data.data_dirs.join(";"),
                data.mode,
                data.patched(),
                data.schemas_unchanged.len(),
                data.schemas_skipped.len(),
                data.errors.len(),
                data.punctuator_patched,
                data.version
            ),
        ));
    }

    // ---- 用户级 custom.yaml 修复（缺陷三：小键盘）--------------------------
    // 位置：prune 之后、install/data 之后、deploy 之前。
    // * prune 之后：裁剪会清掉非保留方案的残留，先裁再修；
    // * install 之后：铺随包产物只补缺失、永不覆盖 config/，但顺序上「先铺后修」
    //   才能保证修完的就是最终态；
    // * deploy 之前：修完立刻部署，重编译才能看到新内容（librime 的
    //   ConfigNeedsUpdate 会比对用户 custom.yaml 的 mtime）。
    let (custom_repair_changed, custom_repaired_ids, custom_repair_details, custom_repair_errors) =
        repair_user_customs(rime_dir);
    // 合并词典指针：只在键缺失时补（用户手写的一律保留）。
    // 查表来源 = 系统 data 目录 + 用户目录（自建方案只存在于用户目录）。
    let mut pointer_data_dirs = data_dirs.clone();
    pointer_data_dirs.push(rime_dir.to_path_buf());
    // 模板目录：payload 的 config/（Windows）+ 安装器铺的 defaults/schemas（Linux）。
    let pointer_template_dirs = crate::merged_dict::template_dirs(payload);
    let pointer_changes = crate::merged_dict::ensure_pointers_with(
        rime_dir,
        &pointer_data_dirs,
        &pointer_template_dirs,
    );
    let pointer_changed = pointer_changes.iter().any(|change| change.wrote());
    if pointer_changed {
        let detail = pointer_changes
            .iter()
            .map(|change| match change {
                crate::merged_dict::PointerChange::Written { schema, path } => {
                    format!("{schema}=written({})", path.display())
                }
                crate::merged_dict::PointerChange::Present { schema } => {
                    format!("{schema}=present")
                }
                crate::merged_dict::PointerChange::Skipped { schema, reason } => {
                    format!("{schema}=skipped({reason})")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        actions.push(Action::new("merged-pointer", true, detail));
    }

    // 指针写进 custom.yaml 之后必须重编译 build/ 才算真的生效。
    //
    // Windows 不用在这里管：部署闸门把 pointer_changed 算进 changed，走进程内
    // 部署（crate::librime::deploy）时会顺带重编译。
    // Linux 的部署闸门是 no-op（platform:linux-keeps-existing-deploy），所以
    // 这里显式跑一次 rime_deployer --build。
    //
    // 2026-11-11 在 228 上实测：fcitx5-remote -r 与 DBus 的 ReloadConfig 都
    // 不会让 librime 重编译（build/ 产物 mtime 不变、引擎日志零新增），只有
    // rime_deployer --build 会。所以这里不用 reload。
    #[cfg(not(windows))]
    if pointer_changed && opts.deploy && opts.mode != Mode::Bootstrap {
        if let Some(shared) = data_dirs.first() {
            let outcome = crate::data_patch::rebuild_after_config_change(rime_dir, shared);
            actions.push(Action::new(
                "merged-pointer-rebuild",
                outcome == crate::data_patch::DeployOutcome::Ok,
                format!("result={}", outcome.as_str()),
            ));
        }
    }
    actions.push(Action::new(
        "custom-repair",
        custom_repair_changed,
        format!(
            "files={} repaired=[{}] {}",
            custom_repair_details.len(),
            custom_repaired_ids.join(","),
            custom_repair_details.join(" ")
        ),
    ));

    // changed = 「本次确实动了磁盘」。四条来源都要算：
    //   installed_changed —— 铺随包产物 / 补 config（install 分支 + always_config 分支）
    //   changed_data      —— 给 data 方案打补丁
    //   seeded.changed()  —— 播种 config 模板（写了文件）
    //   prune_report.removed() —— 裁掉了非保留方案
    // 漏掉任何一条都会把「做了事」报成 Noop。
    let changed = installed_changed
        || changed_data
        || seeded.changed()
        || prune_report.removed() > 0
        // 用户 custom.yaml 被修 = 真的要重编译（否则 deploy 分支会被 no-changes 跳过）。
        || custom_repair_changed
        // 合并词典指针写入 = 用户 custom.yaml 变了 = 也要重编译。
        || pointer_changed;
    let mut deploy_detail = String::new();
    if !opts.deploy {
        actions.push(Action::new("deploy", false, "skipped reason=disabled"));
    } else if !changed {
        actions.push(Action::new("deploy", false, "skipped reason=no-changes"));
    } else {
        // 顺序有讲究（B4）：先铺随包产物、再写 data 补丁，**最后**才跑进程内部署；
        // 跑完按方案整体回退（绝不允许 schema 来自 inproc、bin 来自 prebuilt）。
        let (pre_stop, inproc) = if cfg!(windows) && opts.mode != Mode::Bootstrap {
            let stop = deploy::stop_before_deploy(&deploy::SystemRunner);
            let inproc = deploy::inproc_after_stop(&stop, || crate::librime::deploy(rime_dir));
            (Some(stop), inproc)
        } else {
            (None, None)
        };
        if let Some(dir) = payload {
            prebuilt::enforce_prebuilt_build(&mut report, rime_dir, dir);
        }
        // Bootstrap：部署 deferred —— 只刷 mtime，绝不 taskkill / 跑部署器（M5）。
        let finish_options = if opts.mode == Mode::Bootstrap {
            prebuilt::FinishOptions::bootstrap()
        } else {
            prebuilt::FinishOptions::explicit()
        };
        let outcome = prebuilt::finish_deploy(
            &mut report,
            rime_dir,
            &data_dirs,
            finish_options,
            inproc.as_ref(),
            pre_stop.as_ref(),
        );
        deploy_detail = format!(
            "method={} result={} detail={}",
            report.deploy_method, outcome.result, outcome.detail
        );
        actions.push(Action::new(
            "deploy",
            true,
            format!(
                "method={} result={} ok={} ms={} detail={}",
                report.deploy_method,
                report.deploy_result,
                report.deploy_ok,
                report.deploy_ms,
                report.deploy_detail
            ),
        ));
    }

    // ---- 常驻小键盘判定（缺陷三）--------------------------------------------
    // 每轮都重新判，不看「本轮修过谁」：修完用户 custom.yaml 之后引擎一直没
    // 重编译的话，后续轮次必须继续报 —— 否则又回到「报告说 ok、小键盘其实用不了」
    // 的静默失败（缺陷三当初就是这么悄悄发布出去的）。
    let numpad = assess_numpad(rime_dir, &data_dirs);
    actions.push(Action::new("numpad", false, numpad.detail.join(" ")));
    // 兼容旧动作名：本轮修过的方案，产物里有没有 numpad。
    // 取值来自同一份判定结果，不另做一套判据。
    if !custom_repaired_ids.is_empty() {
        let mut checks = Vec::new();
        for id in &custom_repaired_ids {
            let state = numpad.state_of(id).unwrap_or(NUMPAD_SKIPPED);
            let token = match state {
                NUMPAD_OK => "yes",
                NUMPAD_NO_ARTIFACT => "no-artifact",
                _ => "no",
            };
            checks.push(format!("{id}={token}"));
        }
        actions.push(Action::new("numpad-check", false, checks.join(" ")));
    }

    // 最终判据：**只有** assess 说话。
    let after = assess(rime_dir, &data_dirs, &retained);
    sync_report(&mut report, &after);
    // enforce_prebuilt_build 在收尾里可能又补过文件，这里按最终状态重算 repaired 标记。
    report.build_repaired.retain(|id| {
        after
            .schemas
            .iter()
            .any(|schema| &schema.id == id && schema.usable())
    });
    if !report.build_repaired.is_empty() {
        let repaired = report.build_repaired.clone();
        report.build_schemas = report
            .build_schemas
            .iter()
            .map(|schema| {
                if repaired.iter().any(|id| id == &schema.id) && schema.state == prebuilt::STATE_OK
                {
                    SchemaBuild {
                        id: schema.id.clone(),
                        primary: schema.primary,
                        state: prebuilt::STATE_REPAIRED.to_string(),
                    }
                } else {
                    schema.clone()
                }
            })
            .collect();
    }
    actions.push(Action::new("assess:after", false, after.detail()));

    let mut details: Vec<String> = Vec::new();
    if let Some(error) = &install_error {
        details.push(format!("install_failed={error}"));
    }
    let result = if !after.build_ok {
        let text = after.detail();
        if report.deploy_detail.trim().is_empty() {
            details.push(text);
        } else {
            details.push(format!("{text} deploy={{{}}}", report.deploy_detail));
        }
        OutcomeResult::Failed
    } else {
        let mut problems: Vec<String> = Vec::new();
        if report.failed > 0 {
            problems.push(format!("copy_failed={}", report.failed));
        }
        if !data.errors.is_empty() {
            problems.push(format!("data_errors={}", data.errors.join(" | ")));
        }
        // B-2：config 模板被 data 门禁挡下 = 用户拿到的输入法**没有 schema_list**
        // （default.custom.yaml 才是写 schema_list 的那个）。这必须降级成 partial：
        // 「报告说 ok，但输入法里没有方案可选」是最典型的用户可见静默失败。
        //
        // 这里单独判一次（不只依赖 finish_deploy 的 problems）：finish_deploy 只在
        // 「本次有改动」时才会跑，config 全被跳过（= 没改动）时那一层根本不会执行。
        // 用户 custom.yaml 修失败：修了但没修成，小键盘还是用不了 → 降级。
        if !custom_repair_errors.is_empty() {
            problems.push(format!(
                "custom_repair_failed={}",
                custom_repair_errors.join(" | ")
            ));
        }
        // 小键盘没生效：产物里没有 numpad 处理器 = 用户按小键盘还是打不出数字。
        // 不降级就会报 ok —— 那正是缺陷三当初的静默失败。
        if !numpad.ok {
            problems.push(format!(
                "numpad_missing={} ({})",
                numpad.missing.join(","),
                numpad.hint
            ));
        }
        if !report.config_skipped.is_empty() {
            problems.push(format!(
                "config_skipped={}",
                report.config_skipped.join(",")
            ));
        }
        // MANIFEST sha256 对不上 = payload 被截断/篡改（建议项）。
        if !report.manifest_failed.is_empty() {
            problems.push(format!(
                "manifest_failed={}",
                report.manifest_failed.join(",")
            ));
        }
        // 「一个 data 目录都没找到」不是失败：Linux 上没装 fcitx5 / 首次安装时
        // 本来就没有 /usr/share/rime-data，这属于「没有可部署对象」，不是降级。
        // 明细仍留在 action/report 里（data.data_error）。
        // 部署失败只有在用户显式修复时才降级结论；安装/自举阶段文件已经就位，
        // 引擎下次启动会自己重建（assess 才是唯一判据）。
        if opts.mode == Mode::Fix
            && matches!(
                report.deploy_result.as_str(),
                deploy::RESULT_FAILED | deploy::RESULT_PARTIAL
            )
        {
            problems.push(format!("deploy={}", report.deploy_detail));
        }
        if problems.is_empty() {
            if changed {
                OutcomeResult::Ok
            } else {
                OutcomeResult::Noop
            }
        } else {
            details.push(problems.join(" "));
            if !deploy_detail.is_empty() {
                details.push(format!("deploy={{{deploy_detail}}}"));
            }
            OutcomeResult::Partial
        }
    };

    let detail = details.join(" ");
    debug_assert!(
        result != OutcomeResult::Ok || actions.iter().any(|action| action.changed),
        "Ok 必须至少有一个 changed=true 的 action"
    );
    debug_assert!(
        result != OutcomeResult::Partial || !detail.is_empty(),
        "Partial 必须带 detail"
    );
    debug_assert!(
        result != OutcomeResult::Failed || !detail.is_empty(),
        "Failed 必须带 detail"
    );
    debug_assert!(
        result != OutcomeResult::Ok || after.build_ok,
        "Ok 必须意味着最终 assess 通过"
    );

    let repaired: Vec<String> = after
        .schemas
        .iter()
        .filter(|schema| report.build_repaired.iter().any(|id| id == &schema.id) && schema.usable())
        .map(|schema| schema.id.clone())
        .collect();

    // B-2：config 跳过必须出现在结论里（CLI / GUI / 启动日志都能看到）。
    let config_skipped = report.config_skipped.clone();
    Outcome {
        result,
        actions,
        report,
        detail,
        assessment: after,
        repaired,
        data,
        ime: ime_report,
        seeded,
        config_skipped,
        prune: prune_report,
        numpad,
    }
}

#[cfg(test)]
mod tests {
    fn tmp_file(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rime-sync-extra-support-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn the_marker_is_found_across_read_blocks() {
        let dir = tmp_file("marker");
        let path = dir.join("lib.so");
        // 标记故意放在 64KB 块边界上：前面塞满 0 字节，标记紧随其后。
        let mut bytes = vec![0u8; 64 * 1024 - 3];
        bytes.extend_from_slice(b"rimesync");
        bytes.extend_from_slice(&[0u8; 32]);
        fs::write(&path, &bytes).expect("write");
        assert!(
            super::file_has_marker(&path),
            "a marker spanning two read blocks must still be found"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_without_the_marker_is_not_supported() {
        let dir = tmp_file("no-marker");
        let path = dir.join("lib.so");
        fs::write(&path, vec![0u8; 200 * 1024]).expect("write");
        assert!(!super::file_has_marker(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_support_reports_a_machine_readable_value() {
        assert_eq!(super::ExtraSupport::Supported.as_str(), "supported");
        assert_eq!(super::ExtraSupport::Unsupported.as_str(), "unsupported");
        assert_eq!(super::ExtraSupport::Unknown.as_str(), "unknown");
        assert!(super::ExtraSupport::Supported.usable());
        assert!(!super::ExtraSupport::Unsupported.usable());
        assert!(!super::ExtraSupport::Unknown.usable());
    }

    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "rime-sync-ime-state-{}-{label}",
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

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, text).expect("write file");
    }

    /// M5：启动自举要装输入法 + 播模板，但部署必须是 deferred。
    #[test]
    fn bootstrap_options_install_and_seed_but_defer_the_deploy() {
        let opts = Options::bootstrap();
        assert_eq!(opts.mode, Mode::Bootstrap);
        assert!(opts.install_ime, "缺输入法必须装（否则用户打不了字）");
        assert!(opts.seed_templates, "模板仍要播（缺失才写）");
        let finish = prebuilt::FinishOptions::bootstrap();
        assert!(finish.defer, "bootstrap 的部署必须是 deferred");
        assert!(!finish.explicit);

        // install() 保留给「安装完成后首次运行 / CLI --mode install」：必须仍然显式部署。
        let install = Options::install();
        assert_eq!(install.mode, Mode::Install);
        assert!(install.install_ime && install.seed_templates);
        assert!(!prebuilt::FinishOptions::explicit().defer);
    }

    /// M1/M2：模板播种与 payload config 共用同一条 data 门禁（wubi86 不在就跳过）。
    #[test]
    fn seed_templates_skips_default_until_data_has_gate_schema() {
        let tmp = TempDir::new("seed");
        // A-4：config 源是**扁平的**一个目录（payload 的 config/）。
        let resources = tmp.path().join("config");
        write(
            &resources.join(TEMPLATE_DEFAULT),
            "patch:\n  schema_list: []\n",
        );
        write(&resources.join(TEMPLATE_WEASEL), "patch: {}\n");
        write(&resources.join("wubi86.custom.yaml"), "patch: {}\n");
        write(
            &resources.join("luna_pinyin_simp.custom.yaml"),
            "patch: {}\n",
        );
        let rime = tmp.path().join("rime");

        // 输入法还没装（data 里只有 luna）：default / wubi86.custom 必须跳过。
        let data = tmp.path().join("data");
        write(&data.join("luna_pinyin_simp.schema.yaml"), "name: luna\n");
        let before = seed_templates(&resources, &rime, std::slice::from_ref(&data));
        assert!(before.written.contains(&TEMPLATE_WEASEL.to_string()));
        assert!(before
            .written
            .contains(&"luna_pinyin_simp.custom.yaml".to_string()));
        assert!(before
            .skipped
            .iter()
            .any(|item| item.starts_with(TEMPLATE_DEFAULT)));
        assert!(before
            .skipped
            .iter()
            .any(|item| item.starts_with("wubi86.custom.yaml")));
        assert!(!rime.join(TEMPLATE_DEFAULT).exists());

        // 装完 Weasel（data 里出现 wubi86）后再播：缺失的才补，已有的一律沿用。
        write(&data.join("wubi86.schema.yaml"), "name: wubi86\n");
        let after = seed_templates(&resources, &rime, &[data]);
        assert!(after.written.contains(&TEMPLATE_DEFAULT.to_string()));
        assert!(after.written.contains(&"wubi86.custom.yaml".to_string()));
        assert!(after.reused.contains(&TEMPLATE_WEASEL.to_string()));
        assert!(rime.join(TEMPLATE_DEFAULT).is_file());
    }

    /// 造一份最小 payload：build/ 有可用产物，config/ 有 default.custom.yaml。
    fn make_payload(root: &Path, with_default_config: bool) -> PathBuf {
        let payload = root.join("payload");
        write(
            &payload.join("build/luna_pinyin_simp.schema.yaml"),
            "patch:\n  rimesync_vnum: 1\n  rimesync_punct: 1\n",
        );
        write(&payload.join("build/luna_pinyin_simp.prism.bin"), "prism");
        if with_default_config {
            write(
                &payload.join("config/default.custom.yaml"),
                "patch:\n  schema_list: []\n",
            );
        }
        payload
    }

    /// B-2：data 里没有 wubi86 时，payload 的 config/default.custom.yaml 必须被
    /// 跳过 —— 而**结论不能还是 ok**。用户拿到的输入法没有 schema_list，
    /// 所有报告却说 ok：这就是最典型的用户可见静默失败。
    #[test]
    fn config_skipped_downgrades_the_result_and_shows_up_in_the_report() {
        let tmp = TempDir::new("b2-config-skipped");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("create rime dir");
        let payload = make_payload(tmp.path(), true);
        // data 里只有 luna：wubi86 不在 → default.custom.yaml 必须被门禁挡下。
        let data = tmp.path().join("data");
        write(&data.join("luna_pinyin_simp.schema.yaml"), "name: luna\n");

        let outcome = ensure(
            &rime,
            Some(&payload),
            std::slice::from_ref(&data),
            &Options::fix()
                .with_install_ime(false)
                .with_seed_templates(false)
                .with_prune(false)
                .with_patch_data(false)
                .with_deploy(false),
        );

        // 结论不是 ok。
        assert_ne!(
            outcome.result,
            OutcomeResult::Ok,
            "config 被跳过时结论不能是 ok：{outcome:?}"
        );
        assert_ne!(
            outcome.result,
            OutcomeResult::Noop,
            "config 被跳过时结论不能是 noop：{outcome:?}"
        );
        assert_eq!(
            outcome.result,
            OutcomeResult::Partial,
            "config 被跳过应降级为 partial：{outcome:?}"
        );
        // 报告里必须有 config_skipped 与原因。
        assert!(
            outcome
                .report
                .config_skipped
                .iter()
                .any(|item| item.contains("default.custom.yaml")
                    && item.contains("no-wubi86-in-data")),
            "报告里没有 config_skipped / no-wubi86-in-data：{:?}",
            outcome.report.config_skipped
        );
        assert_eq!(
            outcome.config_skipped, outcome.report.config_skipped,
            "Outcome.config_skipped 必须与报告一致"
        );
        // detail 里必须能看到（CLI 文本输出与 GUI 提示都靠它）。
        assert!(
            outcome.detail.contains("config_skipped"),
            "detail 里没有 config_skipped：{}",
            outcome.detail
        );
        assert!(
            outcome.detail.contains("no-wubi86-in-data"),
            "detail 里没有原因 no-wubi86-in-data：{}",
            outcome.detail
        );
        // install action 的明细里也必须有（CLI 的 action= 行会打印它）。
        let install_action = outcome
            .actions
            .iter()
            .find(|action| action.kind == "install")
            .expect("install action");
        assert!(
            install_action.detail.contains("config_skipped"),
            "install action 里没有 config_skipped：{}",
            install_action.detail
        );
        assert!(
            install_action.detail.contains("no-wubi86-in-data"),
            "install action 里没有原因：{}",
            install_action.detail
        );
        // 文件确实没被写进去（这正是问题本身）。
        assert!(!rime.join("default.custom.yaml").exists());
    }

    /// A-1：Mode::Fix 必须走「装输入法 → 重解析 data_dirs → 补丁」的顺序。
    ///
    /// 旧实现在 Fix 模式下 install_ime=false，于是「首次装输入法」的机器上：
    /// 调用方解析的 data_dirs 是空的（Weasel 还没装），data 补丁打在不存在的
    /// 目录上，assess 也退化成只看 build。
    ///
    /// 这里注入一个记录调用顺序的解析器：
    ///   - 第一次调用（在 ensure 之前，调用方自己解析）→ 空
    ///   - ensure 内部重解析 → 有 wubi86 的目录
    ///
    /// 断言顺序 = re-resolve → patch。
    #[test]
    fn fix_mode_installs_ime_then_re_resolves_data_dirs_before_patching() {
        let tmp = TempDir::new("a1-fix-order");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("create rime dir");
        let payload = make_payload(tmp.path(), true);
        // 「装完输入法之后」才出现的 data 目录（里面有 wubi86）。
        let fresh_data = tmp.path().join("data-after-install");
        write(
            &fresh_data.join("luna_pinyin_simp.schema.yaml"),
            "name: luna\n",
        );
        write(&fresh_data.join("wubi86.schema.yaml"), "name: wubi86\n");

        // 调用方解析的那一份（输入法还没装 → 空）。
        let caller_data: Vec<PathBuf> = Vec::new();
        // 记录顺序：install / re-resolve / patch。
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let order_resolver = std::sync::Arc::clone(&order);
        let opts = Options::fix()
            .with_install_ime(false) // 不真的跑安装器（测试环境）
            .with_seed_templates(false)
            .with_prune(false)
            .with_deploy(false)
            .with_patch_data(false)
            .with_data_resolver(move || {
                order_resolver
                    .lock()
                    .expect("lock")
                    .push("re-resolve".to_string());
                vec![fresh_data.clone()]
            });

        let outcome = ensure(&rime, Some(&payload), &caller_data, &opts);

        let calls = order.lock().expect("lock").clone();
        assert_eq!(
            calls,
            vec!["re-resolve".to_string()],
            "Fix 模式必须重新解析一次 data_dirs"
        );
        // 重解析之后：default.custom.yaml 不再被跳过（data 里有 wubi86 了）。
        assert!(
            outcome.report.config_skipped.is_empty(),
            "重解析之后不该再跳过 config：{:?}",
            outcome.report.config_skipped
        );
        assert!(rime.join("default.custom.yaml").is_file());
    }

    /// A-1：Options::fix() 本身必须带 install_ime=true（否则调用方忘了开，
    /// 「首次装输入法」的机器上又回到老问题）。
    #[test]
    fn fix_options_install_the_ime_body() {
        let opts = Options::fix();
        assert_eq!(opts.mode, Mode::Fix);
        assert!(
            opts.install_ime,
            "Fix 模式必须自己装输入法本体（否则 data 补丁打在不存在的目录上）"
        );
        assert!(opts.prune, "A-5：裁剪纳入 ensure");
        assert!(opts.always_config, "A-3：build 已可用时也要铺 config");
    }

    /// A-6：M1 的「装完才出现 wubi86」分支可注入、可测。
    ///
    /// resolve_data_dirs 是纯函数入口：注入的解析器优先，没注入才现场探测。
    #[test]
    fn resolve_data_dirs_prefers_the_injected_resolver() {
        let tmp = TempDir::new("a6-resolver");
        let data = tmp.path().join("data");
        write(&data.join("wubi86.schema.yaml"), "name: wubi86\n");
        let expected = data.clone();
        let opts = Options::fix().with_data_resolver(move || vec![data.clone()]);
        assert_eq!(resolve_data_dirs(&opts), vec![expected]);
    }

    /// A-3：Install 模式下 build 已可用时，payload config/ 仍然要铺一次
    ///（缺才写）。以前「build 可用就整段跳过」让 config 永远铺不进去。
    #[test]
    fn install_mode_lays_payload_config_even_when_build_is_already_ok() {
        let tmp = TempDir::new("a3-always-config");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("create rime dir");
        let payload = make_payload(tmp.path(), true);
        let data = tmp.path().join("data");
        write(&data.join("luna_pinyin_simp.schema.yaml"), "name: luna\n");
        write(&data.join("wubi86.schema.yaml"), "name: wubi86\n");

        let outcome = ensure(
            &rime,
            Some(&payload),
            std::slice::from_ref(&data),
            &Options::install()
                .with_install_ime(false)
                .with_seed_templates(false)
                .with_prune(false)
                .with_patch_data(false)
                .with_deploy(false),
        );

        assert!(
            rime.join("default.custom.yaml").is_file(),
            "build 已可用时也必须铺 config：{:?}",
            outcome.actions
        );
        assert_eq!(outcome.report.config_copied, 1);
    }

    /// A-5：两入口（CLI / GUI）动作序列必须一致 —— 都只调 ensure(Mode::Fix)，
    /// 所以 action 的 kind 序列必然相同。这里锁死「裁剪在 ensure 内部」。
    #[test]
    fn prune_runs_inside_ensure_for_fix_mode() {
        let tmp = TempDir::new("a5-prune-inside");
        let rime = tmp.path().join("rime");
        fs::create_dir_all(&rime).expect("create rime dir");
        let payload = make_payload(tmp.path(), false);
        let data = tmp.path().join("data");
        write(&data.join("luna_pinyin_simp.schema.yaml"), "name: luna\n");

        let outcome = ensure(
            &rime,
            Some(&payload),
            std::slice::from_ref(&data),
            &Options::fix()
                .with_install_ime(false)
                .with_seed_templates(false)
                .with_prune(true)
                .with_patch_data(false)
                .with_deploy(false),
        );

        let kinds: Vec<&str> = outcome
            .actions
            .iter()
            .map(|action| action.kind.as_str())
            .collect();
        assert!(
            kinds.contains(&"prune"),
            "裁剪必须在 ensure 内部（CLI 与 GUI 同一入口）：{kinds:?}"
        );
        // 裁剪的位置：seed 之后、assess:before 之前。
        let prune_at = kinds
            .iter()
            .position(|kind| *kind == "prune")
            .expect("prune");
        let assess_at = kinds
            .iter()
            .position(|kind| *kind == "assess:before")
            .expect("assess");
        assert!(prune_at < assess_at, "裁剪必须在 assess 之前：{kinds:?}");
    }
}
