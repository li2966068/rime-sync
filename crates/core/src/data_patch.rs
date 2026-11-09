//! 把 v 模式的方案补丁直接写进引擎实际加载的 data 方案文件（0.6.18）。
//!
//! 为什么不能只铺 build/：227 实测 %APPDATA%\Rime\build\luna_pinyin.schema.yaml
//! 里已经有 rimesync 组件，rime.dll 里也有插件符号，但打 v1234 没有候选、按
//! a 选词后还多出一个 a——引擎根本没读我们铺的 build，用的是 data 下的原版方案。
//! 该机的 WeaselDeployer /deploy 恒返回 0 且不产出，部署链不可用，所以只能直接改
//! <WeaselRoot>\data\<schema>.schema.yaml（Linux 对应 /usr/share/rime-data/）。
//!
//! 五处补丁（全部幂等，重复执行产出同一份文本）：
//! 1. engine/translators 首位插入 rimesync_vnum
//! 2. engine/processors 在 recognizer 之后插入 rimesync_vselect，其后插入 numpad
//! 3. recognizer/patterns 增加 vnum: '^[vV][0-9.]*$'（幂等判据是「recognizer 子树
//!    里只留一份 vnum，而且必须落在 patterns 下」：vnum 可能落在 recognizer 的其它
//!    层级（那些位置 Rime 不认），补丁器只认 patterns 一个位置时会再插一份，同一个
//!    映射里出现两个 vnum 键，YAML 直接非法）
//! 4. punctuator/half_shape 深合并 "\\" -> 、 与 ":" -> ：（0.6.20；只在缺失时注入）
//! 5. 文件顶部插入 # patched by rime-sync <version>
//!
//! 写盘纪律：先备份 <file>.rimesync-orig（只做一次），补丁后用 serde_yaml 重新
//! 解析确认合法、核对目标状态，再按原换行风格写成 UTF-8（不带 BOM）。权限不足时
//! 返回可读错误（提示以管理员身份重试），不 panic。解析失败（文件本来就有重复键
//! 之类）时给出可读错误并原样保留文件，绝不写坏，也绝不静默跳过。
//!
//! 0.6.22：
//! * 注释版本不再写死 crate 版本（0.1.0）：build.rs 从 src-tauri/tauri.conf.json
//!   注入发布版本，运行时可用 RIMESYNC_VERSION 覆盖（见 resolve_version）。
//!
//! Linux 降级（共享 data 目录不可写）：
//! * **不再**往用户目录写方案文件（librime 的共享方案只从共享 data 目录读，用户
//!   目录只认 *.custom.yaml 与 build/，写进去等于静默失效）：改为在用户目录写
//!   <schema>.custom.yaml，用 patch 语义把引擎改动叠到共享方案上，写完触发一次
//!   重新部署；没有用户目录则 skipped + 提示 sudo。
//! * custom.yaml 的 engine/processors、engine/translators 是整段替换，写的是
//!   「展开 __include 后的完整列表 + 插入项」；用户已有的同键配置一律保留。

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::prune;
use crate::yaml_text::{
    indent_of, is_blank, is_comment, is_flow_value, is_key_at, yaml_key, yaml_scalar, Doc,
};
#[cfg(not(windows))]
use serde_yaml::Mapping;
use serde_yaml::Value;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;

/// 原文件备份后缀：只建一次，重复执行不覆盖。
pub const BACKUP_SUFFIX: &str = ".rimesync-orig";
/// 损坏文件现场副本的后缀前缀（同一文件只留一份）。
pub const BROKEN_PREFIX: &str = ".rimesync-broken";
/// 顶部标记前缀；实际写入时后面跟版本号。
pub const MARKER_PREFIX: &str = "# patched by rime-sync";
/// recognizer/patterns/vnum 的取值：允许中间态 v、v12.，否则点号会被切成标点。
pub const VNUM_PATTERN: &str = "^[vV][0-9.]*$";
pub const VNUM_TRANSLATOR: &str = "rimesync_vnum";
pub const VSELECT_PROCESSOR: &str = "rimesync_vselect";
/// 小键盘归一化处理器（third_party/librime/plugins/rimesync-extra）。
pub const NUMPAD_PROCESSOR: &str = "numpad";
/// punctuator 深合并（0.6.20）：中文态反斜杠与冒号直出。
pub const PUNCTUATOR_KEY: &str = "punctuator";
pub const HALF_SHAPE_KEY: &str = "half_shape";
pub const IMPORT_PRESET_KEY: &str = "import_preset";
pub const DEFAULT_PUNCTUATOR_PRESET: &str = "default";
/// 反斜杠键（YAML 里写 "\\"）直出「、」。
pub const PUNCT_BACKSLASH: &str = "\\";
pub const PUNCT_BACKSLASH_COMMIT: &str = "、";
/// Shift+; 的冒号键直出「：」。
pub const PUNCT_COLON: &str = ":";
pub const PUNCT_COLON_COMMIT: &str = "：";
/// 用户级降级补丁：共享 data 目录不可写时写 <schema>.custom.yaml（patch 语义）。
pub const CUSTOM_SUFFIX: &str = ".custom.yaml";
/// 报告里的处理路径：直接改共享方案 / 写用户级 custom.yaml / 完全跳过。
pub const MODE_SCHEMA: &str = "schema";
pub const MODE_CUSTOM: &str = "custom";
pub const MODE_SKIPPED: &str = "skipped";
/// 部署触发结果。
pub const DEPLOY_OK: &str = "ok";
pub const DEPLOY_FAILED: &str = "failed";
pub const DEPLOY_SKIPPED: &str = "skipped";
/// 启动自举写了新文件但没有主动重载：留给用户点「重新部署」或重启输入法。
pub const DEPLOY_DEFERRED: &str = "deferred";
/// 部署命令（Linux）：先让 fcitx5 重载，没有再直接让 rime_deployer 构建。
#[cfg(not(windows))]
pub const DEPLOY_RELOAD: &str = "fcitx5-remote";
#[cfg(not(windows))]
pub const DEPLOY_BUILDER: &str = "rime_deployer";
/// 显式指定 data 目录（多个用系统路径分隔符）；空串或 0 = 关闭补丁。测试靠它隔离。
pub const ENV_DATA_DIR: &str = "RIMESYNC_WEASEL_DATA";
/// Linux 的 data 目录。
pub const LINUX_DATA_DIR: &str = "/usr/share/rime-data";
/// 数据补丁注释的版本覆盖（0.6.22）：设置后优先于构建期版本。
pub const ENV_VERSION: &str = "RIMESYNC_VERSION";
/// 构建期注入的发布版本（build.rs 读 src-tauri/tauri.conf.json / CHANGELOG）。
pub const BUILD_VERSION: &str = env!("RIMESYNC_BUILD_VERSION");
/// Windows 注册表里的 Weasel 配置键（32 位视图）。
pub const REG_KEY: &str = r"HKLM\Software\WOW6432Node\Rime\Weasel";
/// 同一个键的 64 位视图（某些安装方式把值写在这里）。
pub const REG_KEY_64: &str = r"HKLM\Software\Rime\Weasel";
const BOM: char = '\u{feff}';

/// 单文件的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchOutcome {
    /// 写入了补丁。
    Patched,
    /// 已经是目标状态，一个字节都没动。
    Unchanged,
    /// 结构上不适用（没有 engine / 列表是 flow 风格等），没动。
    Skipped,
    /// 文件本来是非法 YAML（同一个映射里两遍同一个键），已用同目录的
    /// *.rimesync-orig 原始备份覆盖回来并重新打了补丁。
    Recovered,
}

/// 一批 data 目录的补丁结果。
#[derive(Debug, Clone, Serialize)]
pub struct DataPatchReport {
    /// 实际打补丁的 data 目录（0 或 1 个；找不到就是空）。
    pub data_dirs: Vec<String>,
    /// 实际使用的 data 目录；空串 = 没找到可用目录。
    pub data_dir: String,
    /// 补丁注释里写的版本（0.6.22：CLI 与 GUI 共用同一个来源）。
    pub version: String,
    /// schema / custom / skipped（见 MODE_*）。
    pub mode: String,
    /// 是否降级到用户目录写入（Linux；共享 data 目录不可写时）。
    pub fallback: bool,
    /// 本次写入的用户级 <schema>.custom.yaml。
    pub custom_written: Vec<String>,
    /// custom 模式下因不在保留清单（含依赖闭包）而没有生成 custom.yaml 的方案个数；
    /// 具体 id 见 schemas_skipped。
    pub skipped_not_retained: usize,
    /// 写 custom.yaml 后的部署触发结果：ok / failed / skipped / deferred（见 DEPLOY_*）。
    pub deploy_triggered: String,
    /// 本次探测过的全部候选（含不存在的），按探测顺序。
    pub data_candidates: Vec<DataDirCandidate>,
    /// 一个候选都不存在时的可读原因（列出探测过的路径）。
    pub data_error: Option<String>,
    /// 本次注入的 punctuator 条目个数（"\\" 与 ":" 各算一个）。
    pub punctuator_patched: usize,
    /// 跳过的 punctuator 条目与原因（已存在 / 结构不适用）。
    pub punctuator_skipped: Vec<String>,
    /// 本次真正写入的方案文件。
    pub schemas_patched: Vec<String>,
    /// 已是目标状态、没有写入的方案文件。
    pub schemas_unchanged: Vec<String>,
    /// 跳过的对象（方案文件或 data 目录），后面括号里是原因。
    pub schemas_skipped: Vec<String>,
    /// 用 *.rimesync-orig 备份恢复回来并重新打补丁的文件（每条一句人话）。
    pub recovered: Vec<String>,
    /// 备份所在目录（写第一个备份的 data 目录）；没写过备份时为空。
    pub backup_dir: String,
    /// 可读的失败原因（权限不足时带提权提示）。
    pub errors: Vec<String>,
}

impl Default for DataPatchReport {
    fn default() -> Self {
        Self {
            data_dirs: Vec::new(),
            data_dir: String::new(),
            version: String::new(),
            mode: MODE_SKIPPED.to_string(),
            fallback: false,
            custom_written: Vec::new(),
            skipped_not_retained: 0,
            deploy_triggered: DEPLOY_SKIPPED.to_string(),
            data_candidates: Vec::new(),
            data_error: None,
            punctuator_patched: 0,
            punctuator_skipped: Vec::new(),
            schemas_patched: Vec::new(),
            schemas_unchanged: Vec::new(),
            schemas_skipped: Vec::new(),
            recovered: Vec::new(),
            backup_dir: String::new(),
            errors: Vec::new(),
        }
    }
}

impl DataPatchReport {
    /// 本次写入的方案个数，就是 CLI 报告里的 data_patched。
    ///
    /// custom 模式下写的是 <schema>.custom.yaml，同样计入（那是引擎真正会读的产物）。
    pub fn patched(&self) -> usize {
        self.schemas_patched.len()
    }

    /// 是否需要提示用户以管理员身份重试。
    pub fn needs_elevation(&self) -> bool {
        self.errors
            .iter()
            .chain(self.data_error.iter())
            .any(|error| {
                error.contains("管理员") || error.contains("sudo") || error.contains("root")
            })
    }

    /// 一句话结论（界面 / 日志共用）。
    pub fn summary(&self) -> String {
        let mut text = format!(
            "data 方案补丁：写入 {}，无需改动 {}，跳过 {}，失败 {}，punctuator 注入 {}（跳过 {}）",
            self.schemas_patched.len(),
            self.schemas_unchanged.len(),
            self.schemas_skipped.len(),
            self.errors.len(),
            self.punctuator_patched,
            self.punctuator_skipped.len()
        );
        if self.mode == MODE_CUSTOM {
            let dir = if self.data_dir.is_empty() {
                "共享 Rime data 目录"
            } else {
                self.data_dir.as_str()
            };
            text.push_str(&format!(
                "（{dir} 不可写，已改写 {} 个用户级 custom.yaml",
                self.custom_written.len()
            ));
            match self.deploy_triggered.as_str() {
                DEPLOY_OK => text.push_str("，已触发重新部署）"),
                DEPLOY_FAILED => text.push_str("，自动部署失败，请手动「重新部署」）"),
                DEPLOY_DEFERRED => {
                    text.push_str("，已更新输入法配置，请点「重新部署」或重启输入法）")
                }
                _ => text.push_str("，未找到部署命令，请手动「重新部署」）"),
            }
            if self.skipped_not_retained > 0 {
                text.push_str(&format!(
                    "（非保留方案 {} 个未生成 custom.yaml）",
                    self.skipped_not_retained
                ));
            }
        }
        if let Some(first) = self.errors.first() {
            text.push_str(&format!("；首个错误：{first}"));
        }
        text
    }
}

/// 注册表里 Weasel 的两个目录值（原样，未拼 data 子目录）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeaselRegistryValues {
    /// 程序根目录，例：C:\Program Files\Rime。
    pub weasel_root: Option<PathBuf>,
    /// 实际安装目录，例：C:\Program Files\Rime\weasel-0.17.4。
    pub install_dir: Option<PathBuf>,
}

/// 一个探测过的候选目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataDirCandidate {
    pub path: String,
    /// env / registry:WeaselRoot / glob:InstallDir\\weasel-* / fallback:…（来源便于排障）。
    pub source: String,
    pub exists: bool,
}

/// data 目录解析结果。
#[derive(Debug, Clone, Default)]
pub struct DataDirResolution {
    /// 实际使用的目录（0 或 1 个；第一个命中的胜出）。
    pub dirs: Vec<PathBuf>,
    /// 探测过的全部候选，按顺序（含不存在的）。
    pub candidates: Vec<DataDirCandidate>,
    /// 一个候选都不存在时的可读原因。
    pub error: Option<String>,
}

/// 定位引擎实际加载的 data 目录（0.6.20 重写）。
///
/// 227 实测：注册表 WeaselRoot 指向 C:\Program Files\Rime，真正的 data 在
/// C:\Program Files\Rime\weasel-0.17.4\data；0.6.19 只拼了 <WeaselRoot>\data，
/// 目录不存在，一处补丁都没打。探测顺序（逐项判存在、全部记进候选）：
///
/// 1. RIMESYNC_WEASEL_DATA 一旦设置就只用它（空串或 0 = 关闭补丁，测试靠它隔离）；
/// 2. 注册表 WeaselRoot → <WeaselRoot>\data；
/// 3. <WeaselRoot>\weasel-*\data（取版本号最大的一个）；
/// 4. 注册表 InstallDir → <InstallDir>\data，再试 <InstallDir>\weasel-*\data；
/// 5. 两个注册值都缺失 → %ProgramFiles%\Rime 下的前两种布局；
/// 6. 兜底 %ProgramFiles%\Rime\data（0.6.19 及以前的旧行为）；
/// 7. 其它平台：/usr/share/rime-data。
pub fn resolve_data_dirs() -> DataDirResolution {
    if let Some(raw) = env::var_os(ENV_DATA_DIR) {
        return resolve_env(&raw);
    }
    #[cfg(windows)]
    {
        let program_files = env::var_os("ProgramFiles").map(PathBuf::from);
        return resolve_windows(&weasel_registry(), program_files.as_deref());
    }
    #[cfg(not(windows))]
    {
        let mut resolution = DataDirResolution::default();
        push_candidate(
            &mut resolution,
            PathBuf::from(LINUX_DATA_DIR),
            "platform:linux",
        );
        finish(resolution)
    }
}

/// 兼容入口：只返回实际要打补丁的目录（0 或 1 个）。
pub fn data_dirs() -> Vec<PathBuf> {
    resolve_data_dirs().dirs
}

/// 补丁注释使用的版本（0.6.22）。
///
/// 顺序：显式参数（GUI 传 tauri app version）> `RIMESYNC_VERSION` 环境变量 >
/// 构建期注入的 `BUILD_VERSION`（build.rs 从 src-tauri/tauri.conf.json 读，
/// 缺失时退回 CHANGELOG 顶部版本）。CLI 不再写死 `CARGO_PKG_VERSION`（0.1.0），
/// 与 GUI 写同一个发布版本。
pub fn resolve_version(explicit: Option<&str>) -> String {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return value.to_string();
    }
    if let Ok(value) = env::var(ENV_VERSION) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    BUILD_VERSION.to_string()
}

/// Linux 的用户级 data 目录候选（存在才算）：$XDG_DATA_HOME/fcitx5/rime，
/// 再是 ~/.local/share/fcitx5/rime。Windows 没有这个降级路径。
pub fn user_fallback_candidates() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        Vec::new()
    }
    #[cfg(not(windows))]
    {
        let mut out: Vec<PathBuf> = Vec::new();
        if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
            let path = PathBuf::from(xdg).join("fcitx5").join("rime");
            if path.is_dir() {
                out.push(path);
            }
        }
        if let Some(home) = env::var_os("HOME") {
            let path = PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("fcitx5")
                .join("rime");
            if path.is_dir() && !out.contains(&path) {
                out.push(path);
            }
        }
        out
    }
}

/// 目录能否新建文件（只读的 /usr/share/rime-data 会被判 false）。
fn dir_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".rimesync-write-probe-{}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn describe_dirs(dirs: &[PathBuf], fallback_text: &str) -> String {
    if dirs.is_empty() {
        return fallback_text.to_string();
    }
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join("、")
}

/// 解析 data 目录并打补丁（CLI / 设置页 / 启动自举统一走这里）。
///
/// source 决定写完 custom.yaml 后要不要触发重载：启动自举绝不能打断用户打字。
pub fn patch_auto(version: &str, source: FixSource) -> DataPatchReport {
    let resolution = resolve_data_dirs();
    patch_auto_from(resolution, user_fallback_candidates(), version, source)
}

/// 部署触发（可注入：单测用桩替换，避免真的去跑 fcitx5-remote / rime_deployer）。
type DeployFn<'a> = dyn Fn(&Path, &Path) -> DeployOutcome + 'a;

/// 部署触发的来源（决定要不要打断正在用输入法的用户）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixSource {
    /// 用户点「修复输入法」或手动跑 rime-sync-cli fix-ime：允许触发重载。
    Explicit,
    /// 应用启动时的后台自举：只记录 deferred，绝不主动重载。
    Bootstrap,
}

impl FixSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FixSource::Explicit => "explicit",
            FixSource::Bootstrap => "bootstrap",
        }
    }
}

/// 部署触发结果，报告里序列化成 DEPLOY_*。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployOutcome {
    Ok,
    Failed,
    Skipped,
    /// 没有执行任何外部命令：留给用户手动重新部署（Bootstrap 来源）。
    Deferred,
}

impl DeployOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            DeployOutcome::Ok => DEPLOY_OK,
            DeployOutcome::Failed => DEPLOY_FAILED,
            DeployOutcome::Skipped => DEPLOY_SKIPPED,
            DeployOutcome::Deferred => DEPLOY_DEFERRED,
        }
    }
}

/// 同 patch_auto，但目录解析结果与降级候选由调用方给出（测试 / 安装器用）。
///
/// Windows 保持原行为：不可写就报提权提示。Linux 在共享目录不可写时把改动写进
/// 用户目录的 <schema>.custom.yaml（librime 只从共享目录读方案本体，用户目录只认
/// *.custom.yaml 与 build/）；用户目录也没有就只报告 skipped + 需 sudo，不写任何文件。
pub fn patch_auto_from(
    resolution: DataDirResolution,
    fallbacks: Vec<PathBuf>,
    version: &str,
    source: FixSource,
) -> DataPatchReport {
    #[cfg(not(windows))]
    {
        patch_auto_with(resolution, fallbacks, version, source, &deploy_outcome)
    }
    #[cfg(windows)]
    {
        patch_auto_with(resolution, fallbacks, version, source, &|_, _| {
            DeployOutcome::Skipped
        })
    }
}

fn patch_auto_with(
    resolution: DataDirResolution,
    fallbacks: Vec<PathBuf>,
    version: &str,
    _source: FixSource,
    _deploy: &DeployFn<'_>,
) -> DataPatchReport {
    let mut candidates = resolution.candidates.clone();
    for path in &fallbacks {
        let text = path.display().to_string();
        if !candidates.iter().any(|item| item.path == text) {
            candidates.push(DataDirCandidate {
                path: text,
                source: "fallback:user".to_string(),
                exists: path.is_dir(),
            });
        }
    }

    let mut report;
    #[cfg(windows)]
    {
        report = patch_dirs(&resolution.dirs, version);
        report.data_error = resolution.error.clone();
    }
    #[cfg(not(windows))]
    {
        if resolution.dirs.is_empty() {
            // 一个 data 目录都没有（例如没装 rime-data）：没有方案可读，什么都不写。
            report = patch_dirs(&[], version);
            report.data_error = resolution.error.clone();
        } else if resolution.dirs.iter().any(|dir| dir_writable(dir)) {
            report = patch_dirs(&resolution.dirs, version);
            report.data_error = resolution.error.clone();
        } else if let Some(user) = fallbacks.first() {
            report = patch_custom(&resolution.dirs, user, version);
            report.fallback = true;
            report.data_error = Some(if report.custom_written.is_empty() {
                // 有用户目录但一份都没写出来（方案读不了 / 结构不适用）：如实说，别谎报降级成功。
                format!(
                    "{} 不可写，也没能写出用户目录 {} 下的 custom.yaml（原因见 errors / skipped）；请用 sudo 重试 rime-sync-cli fix-ime",
                    describe_dirs(&resolution.dirs, "Rime data 目录"),
                    user.display()
                )
            } else {
                // 只有真写了新文件才谈部署触发；Bootstrap 来源绝不跑外部命令，
                // 否则启动自举会把用户正在用的 fcitx5 重载掉。
                report.deploy_triggered = match _source {
                    FixSource::Bootstrap => DEPLOY_DEFERRED.to_string(),
                    FixSource::Explicit => match resolution.dirs.first() {
                        Some(shared) => _deploy(user, shared).as_str().to_string(),
                        None => DEPLOY_SKIPPED.to_string(),
                    },
                };
                let deploy_hint = match report.deploy_triggered.as_str() {
                    DEPLOY_OK => "，已触发重新部署",
                    DEPLOY_FAILED => "，自动部署失败，请手动「重新部署」",
                    DEPLOY_DEFERRED => "，已更新输入法配置，请点「重新部署」或重启输入法",
                    _ => "，未找到部署命令，请手动「重新部署」",
                };
                format!(
                    "{} 不可写，已改写用户目录 {} 下的 <schema>.custom.yaml（仅当前用户生效）{deploy_hint}",
                    describe_dirs(&resolution.dirs, "Rime data 目录"),
                    user.display()
                )
            });
        } else {
            report = patch_dirs(&[], version);
            report.data_error = Some(format!(
                "{} 不可写，且没有可用的用户目录（$XDG_DATA_HOME/fcitx5/rime 或 ~/.local/share/fcitx5/rime）；已跳过 data 补丁，请用 sudo 重试 rime-sync-cli fix-ime",
                describe_dirs(&resolution.dirs, "Rime data 目录")
            ));
            report
                .schemas_skipped
                .push("data 补丁：目标目录不可写，需要 sudo（已跳过）".to_string());
        }
    }
    report.data_candidates = candidates;
    report
}

/// 一条外部命令的执行结果（可注入：单测断言尝试顺序，不真的跑命令）。
#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunResult {
    Ok,
    Failed,
    /// 命令不存在（PATH 里没有）。
    Missing,
}

#[cfg(not(windows))]
fn run_command(program: &str, args: &[&OsStr]) -> RunResult {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => RunResult::Ok,
        Ok(_) => RunResult::Failed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RunResult::Missing,
        Err(_) => RunResult::Failed,
    }
}

/// 写 custom.yaml 后触发一次重新部署：先让 fcitx5 重载，没有再让 rime_deployer
/// 直接重建。两个命令都不存在 = skipped（调用方提示手动「重新部署」）。
#[cfg(not(windows))]
fn deploy_outcome(user_dir: &Path, shared_dir: &Path) -> DeployOutcome {
    deploy_outcome_with(&run_command, user_dir, shared_dir)
}

/// deploy_outcome 的实现（命令执行器可注入）。
#[cfg(not(windows))]
fn deploy_outcome_with(
    run: &dyn Fn(&str, &[&OsStr]) -> RunResult,
    user_dir: &Path,
    shared_dir: &Path,
) -> DeployOutcome {
    // 顺序固定：先试轻量的 fcitx5 重载，失败 / 不存在再退回 rime_deployer 重建。
    if run(DEPLOY_RELOAD, &[OsStr::new("-r")]) == RunResult::Ok {
        return DeployOutcome::Ok;
    }
    match run(
        DEPLOY_BUILDER,
        &[
            OsStr::new("--build"),
            user_dir.as_os_str(),
            shared_dir.as_os_str(),
        ],
    ) {
        RunResult::Ok => DeployOutcome::Ok,
        RunResult::Missing => DeployOutcome::Skipped,
        RunResult::Failed => DeployOutcome::Failed,
    }
}

/// 单个方案的 custom.yaml 结果。
#[cfg(not(windows))]
enum CustomOutcome {
    /// 写入（路径，是否留了 .rimesync-orig 备份）。
    Written(PathBuf, bool),
    /// 已经是目标状态，一个字节都没动。
    Unchanged(PathBuf),
    /// 结构不适用（原因）。
    Skipped(&'static str),
}

/// 共享 data 目录不可写时的降级：给每个 <schema>.schema.yaml 生成 / 合并一份
/// 用户级 <schema>.custom.yaml。共享目录只读，一个字节都不动。
#[cfg(not(windows))]
fn patch_custom(shared_dirs: &[PathBuf], user_dir: &Path, version: &str) -> DataPatchReport {
    let mut report = DataPatchReport {
        version: version.to_string(),
        mode: MODE_CUSTOM.to_string(),
        ..DataPatchReport::default()
    };
    if !user_dir.is_dir() {
        report
            .errors
            .push(format!("用户目录 {} 不存在（已跳过）", user_dir.display()));
        return report;
    }
    // 方案集合与 prune 同源（retained-schemas.txt + 依赖闭包）：集合外的方案
    // 一个都不写，否则给已裁掉的方案生成 custom.yaml，prune 下一次启动又删掉它。
    let known = prune::known_schemas(shared_dirs);
    let keep = prune::keep_set(&known, &prune::env_keep().0);
    for dir in shared_dirs {
        report.data_dirs.push(dir.display().to_string());
        if !dir.is_dir() {
            report
                .schemas_skipped
                .push(format!("{}（目录不存在）", dir.display()));
            continue;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                report.errors.push(io_error(dir, &error));
                continue;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_schema_yaml(path))
            .collect();
        files.sort();
        if files.is_empty() {
            report
                .schemas_skipped
                .push(format!("{}（没有 *.schema.yaml）", dir.display()));
        }
        for file in files {
            let Some(id) = schema_id(&file) else {
                report
                    .schemas_skipped
                    .push(format!("{}（不是 <id>.schema.yaml）", file.display()));
                continue;
            };
            if !keep.contains(&id) {
                report.skipped_not_retained += 1;
                report.schemas_skipped.push(format!(
                    "{}（{id} 不在保留清单及依赖闭包里，未生成 custom.yaml）",
                    file.display()
                ));
                continue;
            }
            match write_user_custom(&file, user_dir, version) {
                Ok(CustomOutcome::Written(path, backed_up)) => {
                    report.custom_written.push(path.display().to_string());
                    report.schemas_patched.push(path.display().to_string());
                    if backed_up && report.backup_dir.is_empty() {
                        report.backup_dir = user_dir.display().to_string();
                    }
                }
                Ok(CustomOutcome::Unchanged(path)) => {
                    report.schemas_unchanged.push(path.display().to_string())
                }
                Ok(CustomOutcome::Skipped(reason)) => report
                    .schemas_skipped
                    .push(format!("{}（{reason}）", file.display())),
                Err(error) => report.errors.push(format!("{}：{error:#}", file.display())),
            }
        }
    }
    report.data_dir = report.data_dirs.first().cloned().unwrap_or_default();
    report
}

/// <shared>/<id>.schema.yaml 的方案 id（就是文件名去掉后缀）。
#[cfg(not(windows))]
fn schema_id(schema_file: &Path) -> Option<String> {
    schema_file
        .file_name()?
        .to_str()?
        .strip_suffix(".schema.yaml")
        .map(str::to_string)
}

/// <shared>/<id>.schema.yaml -> <user>/<id>.custom.yaml。
#[cfg(not(windows))]
fn custom_target(schema_file: &Path, user_dir: &Path) -> Option<PathBuf> {
    let name = schema_file.file_name()?.to_str()?;
    let id = name.strip_suffix(".schema.yaml")?;
    Some(user_dir.join(format!("{id}{CUSTOM_SUFFIX}")))
}

/// 读方案文件并把 \`__include\` 展开（只支持根包含 `xxx.schema:/`）。
///
/// luna_pinyin_simp.schema.yaml 这种方案自己只有 `__include: luna_pinyin.schema:/`
/// 和 schema 段，engine/processors 全在父方案里；不展开的话会误判成「没有 engine」，
/// 主方案反而降级不生效。
#[cfg(not(windows))]
fn load_schema_doc(dir: &Path, file: &Path, depth: usize) -> Result<Value> {
    let bytes = fs::read(file).with_context(|| format!("读取 {}", file.display()))?;
    let raw = String::from_utf8(bytes).map_err(|_| anyhow!("文件不是 UTF-8，跳过"))?;
    let text = raw.strip_prefix(BOM).unwrap_or(&raw);
    let value: Value = serde_yaml::from_str(text)
        .with_context(|| format!("{} 不是合法 YAML，跳过", file.display()))?;
    let Some(map) = value.as_mapping() else {
        bail!("{} 顶层不是映射", file.display());
    };
    let Some(include) = map.get(Value::from("__include")).and_then(Value::as_str) else {
        return Ok(value);
    };
    if depth >= 4 {
        bail!("__include 嵌套超过 4 层");
    }
    // 只认整棵方案包含；`a/b` 这种子树包含不猜。
    let Some(name) = include
        .strip_suffix(":/")
        .or_else(|| include.strip_suffix(':'))
    else {
        bail!("__include: {include} 不是根包含，跳过");
    };
    let base_file = match name.strip_suffix(".schema") {
        Some(name) => dir.join(format!("{name}.schema.yaml")),
        None => dir.join(format!("{name}.yaml")),
    };
    if !base_file.is_file() {
        bail!("__include 指向的 {} 不存在", base_file.display());
    }
    let mut base = load_schema_doc(dir, &base_file, depth + 1)?;
    merge_values(&mut base, &value);
    Ok(base)
}

/// 递归合并映射（列表/标量由 overlay 整段替换），与 rime 的 include 语义一致。
#[cfg(not(windows))]
fn merge_values(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Mapping(base_map), Value::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(key.clone()) {
                    Some(slot) => merge_values(slot, value),
                    None => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

/// 生成 / 合并一个用户级 <schema>.custom.yaml。
///
/// custom.yaml 是 patch 语义：engine/processors、engine/translators 这类列表是
/// **整段替换**，所以必须写出完整的目标列表（共享方案原列表 + 我们插入的项）。
/// 只写增量的几个名字会让引擎丢掉 recognizer/speller 等处理器，输入法直接不可用。
#[cfg(not(windows))]
fn write_user_custom(schema_file: &Path, user_dir: &Path, version: &str) -> Result<CustomOutcome> {
    let target = custom_target(schema_file, user_dir)
        .ok_or_else(|| anyhow!("{} 不是 <schema>.schema.yaml", schema_file.display()))?;
    let dir = schema_file
        .parent()
        .ok_or_else(|| anyhow!("{} 没有父目录", schema_file.display()))?;
    let schema = load_schema_doc(dir, schema_file, 0)?;
    let Some(engine) = schema.get("engine").and_then(Value::as_mapping) else {
        return Ok(CustomOutcome::Skipped(
            "方案里没有 engine（展开 __include 后仍缺失）",
        ));
    };
    let Some(processors) = list_at(engine, &["processors"]) else {
        return Ok(CustomOutcome::Skipped("方案里没有 engine/processors 列表"));
    };

    let existing = match fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(anyhow!(io_error(&target, &error))),
    };
    let mut root = match &existing {
        Some(bytes) => {
            let raw = String::from_utf8(bytes.clone())
                .map_err(|_| anyhow!("{} 不是 UTF-8，跳过（不覆盖用户文件）", target.display()))?;
            let text = raw.strip_prefix(BOM).unwrap_or(&raw);
            let value: Value = serde_yaml::from_str(text).with_context(|| {
                format!("{} 不是合法 YAML，跳过（不覆盖用户文件）", target.display())
            })?;
            if !value.is_mapping() {
                return Ok(CustomOutcome::Skipped("已有 custom.yaml 顶层不是映射"));
            }
            value
        }
        None => Value::Mapping(Mapping::new()),
    };
    let before = root.clone();

    let root_map = match &mut root {
        Value::Mapping(map) => map,
        _ => return Ok(CustomOutcome::Skipped("已有 custom.yaml 顶层不是映射")),
    };
    if !root_map.contains_key(Value::from("patch")) {
        root_map.insert(Value::from("patch"), Value::Mapping(Mapping::new()));
    }
    let patch = match root_map.get_mut(Value::from("patch")) {
        Some(Value::Mapping(map)) => map,
        _ => return Ok(CustomOutcome::Skipped("已有 custom.yaml 的 patch 不是映射")),
    };

    let half_shape = match get_path(patch, &["punctuator", "half_shape"]) {
        Some(Value::Mapping(map)) => Some(map.clone()),
        Some(_) => return Ok(CustomOutcome::Skipped("punctuator/half_shape 不是映射")),
        None => None,
    };

    // engine/processors：完整列表。已有 custom.yaml 里的列表优先（用户可能改过；
    // 幂等也靠它：第二次运行算出来的还是同一份）。想跟随新方案重算就删掉该文件。
    let base = list_at(patch, &["engine", "processors"]).unwrap_or(processors);
    remove_nested(patch, &["engine", "processors"]);
    patch.insert(
        Value::from("engine/processors"),
        string_list(&processor_target(&base)),
    );

    // engine/translators：首位 rimesync_vnum，其余顺序不动。
    let translators =
        list_at(patch, &["engine", "translators"]).or_else(|| list_at(engine, &["translators"]));
    if let Some(translators) = translators {
        let mut merged: Vec<String> = translators
            .into_iter()
            .filter(|name| name != VNUM_TRANSLATOR)
            .collect();
        merged.insert(0, VNUM_TRANSLATOR.to_string());
        remove_nested(patch, &["engine", "translators"]);
        patch.insert(Value::from("engine/translators"), string_list(&merged));
    }

    // recognizer/patterns/vnum：用户自己写过就不覆盖。
    if !contains_path(patch, &["recognizer", "patterns", "vnum"]) {
        patch.insert(
            Value::from("recognizer/patterns/vnum"),
            Value::from(VNUM_PATTERN),
        );
    }

    // punctuator/half_shape：只补缺的两个键，用户已有的同键配置一律保留。
    let mut half = half_shape.unwrap_or_default();
    for (key, commit) in [
        (PUNCT_BACKSLASH, PUNCT_BACKSLASH_COMMIT),
        (PUNCT_COLON, PUNCT_COLON_COMMIT),
    ] {
        if !half.contains_key(Value::from(key)) {
            half.insert(Value::from(key), Value::from(commit));
        }
    }
    patch.insert(Value::from("punctuator/half_shape"), Value::Mapping(half));

    if root == before {
        return Ok(CustomOutcome::Unchanged(target));
    }
    let rendered =
        serde_yaml::to_string(&root).with_context(|| format!("序列化 {}", target.display()))?;
    let header = format!(
        "{MARKER_PREFIX} {version}\n# 用户级降级：共享 data 目录不可写，这里用 patch 叠加引擎改动（本文件其它内容保留）\n"
    );
    let body = format!("{header}{rendered}");
    // 安全闸：写盘前重新解析一次，确认还是同一棵树。
    let parsed: Value = serde_yaml::from_str(&body)
        .with_context(|| format!("生成的 {} 不是合法 YAML，已取消写入", target.display()))?;
    if parsed != root {
        bail!("生成的 {} 与目标结构不一致，已取消写入", target.display());
    }
    let backed_up = match &existing {
        Some(bytes) => {
            let backup = backup_path(&target);
            if !backup.exists() {
                fs::write(&backup, bytes).with_context(|| format!("备份 {}", backup.display()))?;
            }
            true
        }
        None => false,
    };
    fs::write(&target, body.as_bytes()).map_err(|error| anyhow!(io_error(&target, &error)))?;
    Ok(CustomOutcome::Written(target, backed_up))
}

/// 在完整处理器列表上按 schema 模式同样的锚点插入 rimesync_vselect + numpad：
/// recognizer 之后优先，否则 key_binder / speller 之前，都没有就追加到末尾。
#[cfg(not(windows))]
fn processor_target(base: &[String]) -> Vec<String> {
    let mut order: Vec<String> = base
        .iter()
        .filter(|name| *name != VSELECT_PROCESSOR && *name != NUMPAD_PROCESSOR)
        .cloned()
        .collect();
    let anchor = order
        .iter()
        .position(|name| name == "recognizer")
        .map(|index| index + 1)
        .or_else(|| {
            order
                .iter()
                .position(|name| name == "key_binder" || name == "speller")
        })
        .unwrap_or(order.len());
    order.insert(anchor, VSELECT_PROCESSOR.to_string());
    order.insert(anchor + 1, NUMPAD_PROCESSOR.to_string());
    order
}

#[cfg(not(windows))]
fn string_list(items: &[String]) -> Value {
    Value::Sequence(
        items
            .iter()
            .map(|item| Value::from(item.as_str()))
            .collect(),
    )
}

/// 取 patch 里的列表：先认 "a/b" 路径写法，再认嵌套映射写法。
#[cfg(not(windows))]
fn list_at(map: &Mapping, path: &[&str]) -> Option<Vec<String>> {
    get_path(map, path)?
        .as_sequence()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// 按路径取值，两种写法都认（"a/b/c" 与 a: {b: {c: …}}）。
#[cfg(not(windows))]
fn get_path<'a>(map: &'a Mapping, path: &[&str]) -> Option<&'a Value> {
    if let Some(value) = map.get(Value::from(path.join("/"))) {
        return Some(value);
    }
    let (head, tail) = path.split_first()?;
    match map.get(Value::from(*head))? {
        Value::Mapping(next) if !tail.is_empty() => get_path(next, tail),
        value if tail.is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(not(windows))]
fn contains_path(map: &Mapping, path: &[&str]) -> bool {
    get_path(map, path).is_some()
}

/// 删掉嵌套写法（engine: {processors: […] }），避免和 "a/b" 路径写法并存。
#[cfg(not(windows))]
fn remove_nested(map: &mut Mapping, path: &[&str]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    if parents.is_empty() {
        return map.remove(Value::from(*last)).is_some();
    }
    let removed = match map.get_mut(Value::from(parents[0])) {
        Some(Value::Mapping(next)) => remove_nested(next, &path[1..]),
        _ => false,
    };
    if removed {
        let empty = matches!(
            map.get(Value::from(parents[0])),
            Some(Value::Mapping(next)) if next.is_empty()
        );
        if empty {
            map.remove(Value::from(parents[0]));
        }
    }
    removed
}

/// RIMESYNC_WEASEL_DATA 的解析：空串 / 0 = 关闭（不是错误）；其余按系统路径
/// 分隔符拆分，逐个探测存在性。
fn resolve_env(raw: &OsStr) -> DataDirResolution {
    let mut resolution = DataDirResolution::default();
    let text = raw.to_string_lossy();
    if text.trim().is_empty() || text.trim() == "0" {
        return resolution;
    }
    for part in env::split_paths(raw) {
        push_candidate(&mut resolution, part, "env:RIMESYNC_WEASEL_DATA");
    }
    finish(resolution)
}

/// Windows 的解析顺序，纯函数（Linux 上也能单测）。
pub fn resolve_windows(
    registry: &WeaselRegistryValues,
    program_files: Option<&Path>,
) -> DataDirResolution {
    let mut resolution = DataDirResolution::default();
    if let Some(root) = &registry.weasel_root {
        push_candidate(&mut resolution, root.join("data"), "registry:WeaselRoot");
        push_glob_candidates(&mut resolution, root, "glob:WeaselRoot\\weasel-*");
    }
    if let Some(dir) = &registry.install_dir {
        push_candidate(&mut resolution, dir.join("data"), "registry:InstallDir");
        push_glob_candidates(&mut resolution, dir, "glob:InstallDir\\weasel-*");
    }
    if registry.weasel_root.is_none() && registry.install_dir.is_none() {
        if let Some(program_files) = program_files {
            let rime = program_files.join("Rime");
            push_candidate(
                &mut resolution,
                rime.join("data"),
                "default:%ProgramFiles%\\Rime",
            );
            push_glob_candidates(
                &mut resolution,
                &rime,
                "glob:%ProgramFiles%\\Rime\\weasel-*",
            );
        }
    }
    if let Some(program_files) = program_files {
        push_candidate(
            &mut resolution,
            program_files.join("Rime").join("data"),
            "fallback:%ProgramFiles%\\Rime\\data",
        );
    }
    finish(resolution)
}

fn push_glob_candidates(resolution: &mut DataDirResolution, root: &Path, source: &str) {
    for dir in weasel_version_dirs(root) {
        push_candidate(resolution, dir.join("data"), source);
    }
}

/// 记一个候选；第一个存在的目录成为实际使用的目录（顺序即优先级）。
fn push_candidate(resolution: &mut DataDirResolution, path: PathBuf, source: &str) {
    if path.as_os_str().is_empty() {
        return;
    }
    let text = path.display().to_string();
    if resolution.candidates.iter().any(|item| item.path == text) {
        return;
    }
    let exists = path.is_dir();
    if exists && resolution.dirs.is_empty() {
        resolution.dirs.push(path);
    }
    resolution.candidates.push(DataDirCandidate {
        path: text,
        source: source.to_string(),
        exists,
    });
}

/// 一个候选都不存在时，把探测过的路径写成一句可读原因。
fn finish(mut resolution: DataDirResolution) -> DataDirResolution {
    if resolution.dirs.is_empty() && !resolution.candidates.is_empty() {
        let probed: Vec<String> = resolution
            .candidates
            .iter()
            .map(|item| format!("{}（{}，目录不存在）", item.path, item.source))
            .collect();
        resolution.error = Some(format!(
            "未找到可用的 Rime data 目录：已探测 {} 个候选，全部不存在：{}",
            probed.len(),
            probed.join("；")
        ));
    }
    resolution
}

/// <root>\weasel-<版本> 目录，按版本从高到低（0.17.10 排在 0.17.4 前面）。
pub fn weasel_version_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<(Vec<u64>, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            weasel_version(&name).map(|version| (version, entry.path()))
        })
        .collect();
    found.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    found.into_iter().map(|(_, path)| path).collect()
}

/// "weasel-0.17.4" → [0, 17, 4]；不是这个形状（没有数字版本）返回 None。
pub fn weasel_version(name: &str) -> Option<Vec<u64>> {
    let suffix = name.strip_prefix("weasel-")?;
    if suffix.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in suffix.split('.') {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    Some(parts)
}

/// 读注册表拿 WeaselRoot / InstallDir（32 位视图 + 64 位视图都试）。
#[cfg(windows)]
pub fn weasel_registry() -> WeaselRegistryValues {
    // Weasel 是 32 位程序，主键在 WOW6432Node；某些安装方式会写进 64 位视图，
    // 所以两个视图都读，值以先命中的为准。
    let queries: [(&str, Option<&str>); 3] = [
        (REG_KEY, None),
        (REG_KEY, Some("/reg:32")),
        (REG_KEY_64, Some("/reg:64")),
    ];
    let mut values = WeaselRegistryValues::default();
    for (key, view) in queries {
        let Some(output) = reg_query(key, view) else {
            continue;
        };
        let parsed = weasel_registry_from_output(&output);
        values.weasel_root = values.weasel_root.or(parsed.weasel_root);
        values.install_dir = values.install_dir.or(parsed.install_dir);
    }
    values
}

#[cfg(windows)]
fn reg_query(key: &str, view: Option<&str>) -> Option<String> {
    let mut command = std::process::Command::new("reg");
    command.args(["query", key]);
    if let Some(view) = view {
        command.arg(view);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// 解析 reg query 的输出（下面这段是纯文本，不是 Rust 代码）：
///
/// ```text
/// HKEY_LOCAL_MACHINE\Software\WOW6432Node\Rime\Weasel
///     InstallDir    REG_SZ    C:\Program Files\Rime\weasel-0.17.4
///     WeaselRoot    REG_SZ    C:\Program Files\Rime
/// ```
///
/// 两个值都保留（WeaselRoot 用来找版本目录，InstallDir 兜底）；值里可以带空格。
pub fn weasel_registry_from_output(output: &str) -> WeaselRegistryValues {
    let mut values = WeaselRegistryValues::default();
    for line in output.lines() {
        let mut tokens = line.split_whitespace();
        let Some(name) = tokens.next() else { continue };
        let Some(kind) = tokens.next() else { continue };
        if !kind.eq_ignore_ascii_case("REG_SZ") {
            continue;
        }
        // 路径可能带空格：从类型标记之后整段取。
        let Some(pos) = line.find(kind) else { continue };
        let value = line[pos + kind.len()..].trim().trim_matches('"');
        if value.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("WeaselRoot") && values.weasel_root.is_none() {
            values.weasel_root = Some(PathBuf::from(value));
        } else if name.eq_ignore_ascii_case("InstallDir") && values.install_dir.is_none() {
            values.install_dir = Some(PathBuf::from(value));
        }
    }
    values
}

/// 逐目录给 <data>/*.schema.yaml 打补丁；单个文件失败不中断整批。
pub fn patch_dirs(dirs: &[PathBuf], version: &str) -> DataPatchReport {
    let mut report = DataPatchReport {
        version: version.to_string(),
        ..DataPatchReport::default()
    };
    for dir in dirs {
        report.data_dirs.push(dir.display().to_string());
        if !dir.is_dir() {
            report
                .schemas_skipped
                .push(format!("{}（目录不存在）", dir.display()));
            continue;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                report.errors.push(io_error(dir, &error));
                continue;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_schema_yaml(path))
            .collect();
        files.sort();
        if files.is_empty() {
            report
                .schemas_skipped
                .push(format!("{}（没有 *.schema.yaml）", dir.display()));
        }
        for file in files {
            match patch_file_detailed(&file, version) {
                Ok((outcome, punctuator)) => {
                    report.punctuator_patched += punctuator.patched;
                    for skipped in &punctuator.skipped {
                        report
                            .punctuator_skipped
                            .push(format!("{}：{skipped}", file.display()));
                    }
                    match outcome {
                        PatchOutcome::Patched => {
                            report.schemas_patched.push(file.display().to_string());
                            if report.backup_dir.is_empty() {
                                report.backup_dir = dir.display().to_string();
                            }
                        }
                        PatchOutcome::Unchanged => {
                            report.schemas_unchanged.push(file.display().to_string())
                        }
                        PatchOutcome::Skipped => report
                            .schemas_skipped
                            .push(format!("{}（结构不适用）", file.display())),
                        PatchOutcome::Recovered => {
                            report.schemas_patched.push(file.display().to_string());
                            report.recovered.push(format!(
                                "{}：已用 {} 备份恢复并重新打补丁",
                                file.display(),
                                backup_path(&file).display()
                            ));
                            if report.backup_dir.is_empty() {
                                report.backup_dir = dir.display().to_string();
                            }
                        }
                    }
                }
                Err(error) => report.errors.push(format!("{}：{error:#}", file.display())),
            }
        }
    }
    report.data_dir = report.data_dirs.first().cloned().unwrap_or_default();
    report.mode = if report.data_dirs.is_empty() {
        MODE_SKIPPED
    } else {
        MODE_SCHEMA
    }
    .to_string();
    report
}

fn is_schema_yaml(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.ends_with(".schema.yaml"))
        .unwrap_or(false)
}

/// punctuator 深合并的结果（0.6.20）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PunctuatorPatch {
    /// 本次注入的条目个数（"\\" 与 ":" 各算一个）。
    pub patched: usize,
    /// 跳过的条目与原因（已存在 / 结构不适用）。
    pub skipped: Vec<String>,
    /// 本次真正注入的键（写盘前核对目标状态用，不进报告）。
    injected: Vec<String>,
}

impl PunctuatorPatch {
    fn mark(&mut self, key: &str) {
        self.patched += 1;
        self.injected.push(key.to_string());
    }
}

/// 给一个 <schema>.schema.yaml 打补丁（不看 punctuator 明细）。
pub fn patch_file(path: &Path, version: &str) -> Result<PatchOutcome> {
    patch_file_detailed(path, version).map(|(outcome, _)| outcome)
}

/// 给一个 <schema>.schema.yaml 打补丁，并返回 punctuator 深合并结果。
///
/// 顺序：读文件 -> serde_yaml 解析（不合法就不动）-> 文本级插入 -> 渲染 -> 与原文
/// 比对（相同即 Unchanged，幂等的定义）-> 再次解析 + 核对目标状态 -> 备份（只做
/// 一次）-> 写回。
pub fn patch_file_detailed(path: &Path, version: &str) -> Result<(PatchOutcome, PunctuatorPatch)> {
    patch_file_detailed_inner(path, version, true)
}

/// 真正的补丁流程。allow_recovery=false 时不走「从备份恢复」那条路（恢复流程自己
/// 会再调一次本函数，避免递归）。
fn patch_file_detailed_inner(
    path: &Path,
    version: &str,
    allow_recovery: bool,
) -> Result<(PatchOutcome, PunctuatorPatch)> {
    let bytes = fs::read(path).with_context(|| format!("读取 {}", path.display()))?;
    let raw = String::from_utf8(bytes.clone()).map_err(|_| anyhow!("文件不是 UTF-8，跳过"))?;
    // 目标是 UTF-8 无 BOM：输入带 BOM 时去掉，写回也不写 BOM。
    let text = raw.strip_prefix(BOM).unwrap_or(&raw);
    let value: Value = match serde_yaml::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            // 错因是「同一个映射里两遍同一个键」（0.6.19 时代两条补丁路径各写
            // 一份 vnum）时，按「零损优先」的顺序自救：
            // ① 文本级去重：只删掉多余的那一行，文件里其余内容（用户手工改的、
            //    注释、格式）全部保留 —— 不需要备份，备份被清掉也能自愈；
            // ② 同目录的 *.rimesync-orig 备份覆盖回来重打补丁（去重不适用时兜底）；
            // ③ 两条都不行才报错，且原文件一个字节不动。
            if allow_recovery && duplicate_key_of(&error.to_string()).is_some() {
                if let Some(deduped) = dedupe_duplicate_keys(text) {
                    // 损坏现场留一份（同目录 <file>.rimesync-broken-*），用户可回溯。
                    keep_broken_copy(path, &bytes)?;
                    fs::write(path, deduped.as_bytes())
                        .map_err(|error| anyhow!(io_error(path, &error)))?;
                    // 继续走正常流程：幂等判据会把序列里的重复项也归一。
                    let (_, punctuator) = patch_file_detailed_inner(path, version, false)?;
                    return Ok((PatchOutcome::Recovered, punctuator));
                }
                if let Some(result) = recover_from_backup(path, version, &bytes)? {
                    return Ok(result);
                }
            }
            return Err(anyhow!(
                "{} 不是合法 YAML，已跳过补丁：{}",
                path.display(),
                yaml_error_hint(&error)
            ));
        }
    };

    let mut doc = Doc::parse(text);
    let mut applicable = false;
    applicable |= doc.ensure_translator(&value);
    applicable |= doc.ensure_processors(&value);
    applicable |= doc.ensure_vnum_pattern(&value);
    // punctuator 只在「能按行安全改写」的方案上做：flow 风格 / 没有 engine 的文件
    // 连别的补丁都没动，这里也不该凭空塞一个新段。
    let punctuator = if applicable {
        let patch = doc.ensure_punctuator(&value);
        applicable |= patch.patched > 0;
        patch
    } else {
        PunctuatorPatch::default()
    };
    if applicable {
        // 只有真的补过（或已经是目标状态）才留标记：纯 YAML、没有 engine 的
        // 文件不该被我们写上注释。
        doc.ensure_marker(version);
    }

    let patched = doc.render();
    if patched == text {
        // 一个字节都没变：要么已经打过补丁，要么没有可补的结构。
        let outcome = if applicable || doc.changed {
            PatchOutcome::Unchanged
        } else {
            PatchOutcome::Skipped
        };
        return Ok((outcome, punctuator));
    }

    // 安全闸：写盘前重新解析 + 核对目标状态，失败就不写。
    let parsed = check_rendered_yaml(&patched)
        .with_context(|| format!("补丁后的 {} 不是合法 YAML，已取消写入", path.display()))?;
    verify(&parsed)
        .with_context(|| format!("补丁后的 {} 没达到目标状态，已取消写入", path.display()))?;
    verify_punctuator(&parsed, &punctuator)
        .with_context(|| format!("补丁后的 {} 没达到目标状态，已取消写入", path.display()))?;

    let backup = backup_path(path);
    if !backup.exists() {
        fs::write(&backup, &bytes).with_context(|| format!("备份 {}", backup.display()))?;
    }
    fs::write(path, patched.as_bytes()).map_err(|error| anyhow!(io_error(path, &error)))?;
    Ok((PatchOutcome::Patched, punctuator))
}

/// 文本级去重：把「同一个映射里写了两遍的键」的多余那一份删掉，返回修好的
/// 文本。删不掉就返回 None，交给上层退回「用备份覆盖回来」那条路。
///
/// 为什么要有这条：0.6.19 时代有两条补丁路径同时往同一个方案文件写设置，于是
/// recognizer/patterns 下出现两份 vnum（一份双引号、一份单引号）。YAML 禁止同
/// 一个映射里出现两个同名键，serde_yaml 与 librime 都会拒绝，整个方案加载不出来。
///
/// 这条路径比「用 *.rimesync-orig 备份覆盖回来」更好在两点：
/// - 不需要备份就能自愈（备份可能被用户清掉，也可能文件是别处弄坏的）；
/// - 零损：只删多余的那一行，文件里其余内容（用户手工改的、注释、格式）全部保留。
///
/// 安全的边界：每删一行就重新解析一次，删到能解析为止；解析不过、或重复项写在
/// 同一行里（flow 风格）删不动，就返回 None 走备份那条路，绝不硬改。
fn dedupe_duplicate_keys(text: &str) -> Option<String> {
    const MAX_ROUNDS: usize = 16;
    let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    let keep_trailing_newline = text.ends_with('\n');
    let mut removed = false;
    for _ in 0..MAX_ROUNDS {
        let joined = lines.join("\n");
        let error = match serde_yaml::from_str::<Value>(&joined) {
            Ok(_) => break,
            Err(error) => error,
        };
        let key = duplicate_key_of(&error.to_string())?;
        let reported = error.location()?.line().saturating_sub(1);
        // 该键在文本里出现的所有行（只认「key: 值」这种映射键写法）。
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line_carries_key(line, &key))
            .map(|(index, _)| index)
            .collect();
        if hits.len() < 2 {
            // 不是行级重复（例如两处写在同一行的 flow 风格里）：删行救不了。
            return None;
        }
        // 保留第一份，删后面那份里离报错位置最近的一行。
        let first = hits[0];
        let target = hits
            .iter()
            .copied()
            .find(|index| *index != first && index.abs_diff(reported) <= 3)
            .or_else(|| hits.iter().copied().find(|index| *index != first))?;
        lines.remove(target);
        removed = true;
    }
    if !removed {
        return None;
    }
    let joined = lines.join("\n");
    // 最后必须真的能解析：删行有可能破坏结构，这时老老实实退回备份那条路。
    serde_yaml::from_str::<Value>(&joined).ok()?;
    Some(if keep_trailing_newline {
        format!("{joined}\n")
    } else {
        joined
    })
}

/// 这一行是不是「key: 值」写法的那个键。
/// 序列项（- key）不算 —— 序列里出现同名项是合法的 YAML，重复键问题只出在映射里。
fn line_carries_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    match trimmed.strip_prefix(key) {
        Some(rest) => rest.trim_start().starts_with(':'),
        None => false,
    }
}

/// <file>.rimesync-orig。
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

fn io_error(path: &Path, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return format!(
            "访问 {} 失败：{error}（需要管理员权限：Windows 请以管理员身份重新运行 \
             rime-sync-cli fix-ime 或 rime-sync；Linux 请用 sudo 重试）",
            path.display()
        );
    }
    format!("访问 {} 失败：{error}", path.display())
}

/// 用同目录的 *.rimesync-orig 原始备份救回损坏的方案文件，并重新打补丁。
///
/// 什么时候用：文件本身已经是非法 YAML（同一个映射里两遍同一个键），而同目录
/// 还有 rime-sync 第一次打补丁时留下的原始备份。这时最有价值的动作不是报错，
/// 而是把用户现场另存一份、再用原始备份覆盖回来重打补丁 —— 227 实测这条路径能
/// 把不可用的方案救回来（check-ime 从 failed / missing=[...] 变成 ok / missing=[]）。
/// 没有备份、或备份自己也不合法，就没什么可救的：返回 None，交给上层报错。
fn recover_from_backup(
    path: &Path,
    version: &str,
    broken: &[u8],
) -> Result<Option<(PatchOutcome, PunctuatorPatch)>> {
    let backup = backup_path(path);
    if !backup.is_file() {
        return Ok(None);
    }
    let original = fs::read(&backup).with_context(|| format!("读取备份 {}", backup.display()))?;
    let original_text = String::from_utf8(original.clone())
        .map_err(|_| anyhow!("备份 {} 不是 UTF-8", backup.display()))?;
    // 备份自己也要是合法 YAML：备份坏了就没什么可救的，不动用户的文件。
    let original_text = original_text.strip_prefix(BOM).unwrap_or(&original_text);
    if serde_yaml::from_str::<Value>(original_text).is_err() {
        return Ok(None);
    }
    // 保留用户现场：损坏的文件另存一份（同一文件只留一份，见 keep_broken_copy）。
    keep_broken_copy(path, broken)?;
    // 用原始备份覆盖回来，重新走一次完整补丁流程。
    fs::write(path, &original).map_err(|error| anyhow!(io_error(path, &error)))?;
    let (_, punctuator) = patch_file_detailed_inner(path, version, false)?;
    Ok(Some((PatchOutcome::Recovered, punctuator)))
}

/// 损坏文件的现场副本：<file>.rimesync-broken-<时间戳>-<内容哈希>。
///
/// 同一文件只留一份：第二次 ensure 不再攒新的现场文件，也不覆盖已有的那份。
fn keep_broken_copy(path: &Path, content: &[u8]) -> Result<Option<PathBuf>> {
    if let Some(existing) = existing_broken_copy(path) {
        return Ok(Some(existing));
    }
    let target = broken_copy_path(path, content);
    fs::write(&target, content).map_err(|error| anyhow!(io_error(&target, &error)))?;
    Ok(Some(target))
}

/// 同目录里已经存在的现场副本（<file>.rimesync-broken*）。
fn existing_broken_copy(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let prefix = format!("{name}{BROKEN_PREFIX}");
    let dir = path.parent()?;
    let entries = fs::read_dir(dir).ok()?;
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.starts_with(&prefix))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// 现场副本的文件名：时间戳 + 内容哈希，保证不同内容不会互相覆盖。
fn broken_copy_path(path: &Path, content: &[u8]) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    name.push_str(&format!("{BROKEN_PREFIX}-{nanos}-{:016x}", fnv1a(content)));
    path.with_file_name(name)
}

/// FNV-1a：只用来给现场副本起个唯一名字，不需要密码学强度（不新增依赖）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 写盘前的安全闸：渲染出来的文本必须还是合法 YAML。
///
/// serde_yaml 遇到同一个映射里写了两遍的键会直接失败（duplicate entry with key
/// "vnum"）。这里把它变成一句能看懂的话（哪个键重复、怎么修）并交给上层取消
/// 写入：宁可不动文件，也不能落一个非法 YAML 下去。
pub fn check_rendered_yaml(text: &str) -> Result<Value> {
    serde_yaml::from_str(text).map_err(|error| anyhow!("{}", yaml_error_hint(&error)))
}

/// YAML 解析失败时的可读原因。
///
/// serde_yaml 的重复键报错是 "duplicate entry with key \"vnum\""，用户看不懂
/// 哪个文件、哪个键坏了：这里把键名点出来，并说清楚怎么修。
fn yaml_error_hint(error: &serde_yaml::Error) -> String {
    let message = error.to_string();
    match duplicate_key_of(&message) {
        Some(key) => format!(
            "文件里出现了重复的键 {key}（同一个映射里写了两遍），已尝试自动去重但没能\
             安全修好，请手动删掉多余的那一行后重试；\
             同目录的 *.rimesync-orig 备份也可以直接覆盖回来"
        ),
        None => message,
    }
}

/// 从 serde_yaml 的报错里取出重复的键名。
fn duplicate_key_of(message: &str) -> Option<String> {
    const MARKER: &str = "duplicate entry with key ";
    let at = message.find(MARKER)? + MARKER.len();
    let rest = message[at..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 补丁后的结构自检。只核对本补丁负责的四处，用户自己的其它配置一律不碰。
fn verify(value: &Value) -> Result<()> {
    if let Some(engine) = value.get("engine") {
        if let Some(translators) = engine.get("translators").and_then(Value::as_sequence) {
            if translators.first().and_then(Value::as_str) != Some(VNUM_TRANSLATOR) {
                bail!("engine/translators 首位不是 {VNUM_TRANSLATOR}");
            }
        }
        if let Some(processors) = engine.get("processors").and_then(Value::as_sequence) {
            let names: Vec<&str> = processors.iter().filter_map(Value::as_str).collect();
            let vselect = names
                .iter()
                .position(|name| *name == VSELECT_PROCESSOR)
                .ok_or_else(|| anyhow!("engine/processors 里没有 {VSELECT_PROCESSOR}"))?;
            if names.get(vselect + 1) != Some(&NUMPAD_PROCESSOR) {
                bail!("engine/processors 里 {NUMPAD_PROCESSOR} 没有紧跟 {VSELECT_PROCESSOR}");
            }
            if let Some(recognizer) = names.iter().position(|name| *name == "recognizer") {
                if vselect != recognizer + 1 {
                    bail!("{VSELECT_PROCESSOR} 没有紧跟 recognizer");
                }
            }
        }
    }
    if let Some(patterns) = value
        .get("recognizer")
        .and_then(|recognizer| recognizer.get("patterns"))
    {
        if patterns.get("vnum").is_none() {
            bail!("recognizer/patterns 里没有 vnum");
        }
    }
    Ok(())
}

/// punctuator 目标状态核对：只核对本次真的注入的键（已有键属于用户配置，不碰）。
fn verify_punctuator(value: &Value, patch: &PunctuatorPatch) -> Result<()> {
    if patch.injected.is_empty() {
        return Ok(());
    }
    let half = value
        .get(PUNCTUATOR_KEY)
        .and_then(|punctuator| punctuator.get(HALF_SHAPE_KEY))
        .ok_or_else(|| anyhow!("{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY} 缺失"))?;
    for key in &patch.injected {
        let entry = half
            .get(key.as_str())
            .ok_or_else(|| anyhow!("{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY} 缺少 {key}"))?;
        // 注入的是标量（直接 commit）；兼容用户已有的 {commit: …} 写法。
        let commit = entry
            .as_str()
            .or_else(|| entry.get("commit").and_then(Value::as_str))
            .ok_or_else(|| anyhow!("{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY}/{key} 没有 commit"))?;
        let expected = if key == PUNCT_BACKSLASH {
            PUNCT_BACKSLASH_COMMIT
        } else {
            PUNCT_COLON_COMMIT
        };
        if commit != expected {
            bail!("{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY}/{key} 的 commit 不是 {expected}");
        }
    }
    Ok(())
}

/// half_shape 的一条：标量形式（直接上屏，不进候选列表）。
///
/// 必须是标量而不是 "{commit: …}" 映射：rime-prelude/symbols.yaml 里
/// "'\\' : [ 、, '\\', ＼ ]" 是列表，schema 里用映射覆盖预设列表会让
/// librime 合并配置失败、整个方案部署不出来（0.6.20 容器 smoke 实测）。
/// 标量属于 ConfigValue，直接替换预设值，Rime 的 TranslateUniquePunct 会
/// 立刻上屏、不弹候选列表。
fn half_shape_entry(key: &str) -> String {
    let commit = if key == PUNCT_BACKSLASH {
        PUNCT_BACKSLASH_COMMIT
    } else {
        PUNCT_COLON_COMMIT
    };
    format!("{}: \"{commit}\"", yaml_key(key))
}

impl Doc {
    /// engine/translators 首位放 rimesync_vnum。
    fn ensure_translator(&mut self, value: &Value) -> bool {
        let Some(engine) = value.get("engine") else {
            return false;
        };
        if engine.get("translators").is_none() || self.is_flow_list("translators", 2) {
            return false;
        }
        self.remove_item("translators", 2, VNUM_TRANSLATOR);
        self.insert_item_front("translators", 2, VNUM_TRANSLATOR)
            || self.push_item("translators", 2, VNUM_TRANSLATOR)
    }

    /// engine/processors：recognizer 之后是 rimesync_vselect，再后面是 numpad。
    fn ensure_processors(&mut self, value: &Value) -> bool {
        let Some(engine) = value.get("engine") else {
            return false;
        };
        if engine.get("processors").is_none() || self.is_flow_list("processors", 2) {
            return false;
        }
        // 先摘掉同名项，再按目标位置插回去：顺序被用户改过也能纠正。
        self.remove_item("processors", 2, VSELECT_PROCESSOR);
        self.remove_item("processors", 2, NUMPAD_PROCESSOR);

        let anchors = ["recognizer", "key_binder", "speller"];
        let mut placed = false;
        for anchor in anchors {
            if !self.contains_item("processors", 2, anchor) {
                continue;
            }
            if anchor == "recognizer" {
                placed = self.insert_item_after("processors", 2, anchor, VSELECT_PROCESSOR);
            } else {
                placed = self.insert_item_before("processors", 2, anchor, VSELECT_PROCESSOR);
            }
            if placed {
                break;
            }
        }
        if !placed {
            placed = self.push_item("processors", 2, VSELECT_PROCESSOR);
        }
        if !placed {
            return false;
        }
        // numpad 必须紧跟 vselect。
        self.insert_item_after("processors", 2, VSELECT_PROCESSOR, NUMPAD_PROCESSOR)
    }

    /// recognizer/patterns/vnum。
    ///
    /// 幂等判据不能只看「recognizer/patterns 下有没有 vnum」这一个位置：vnum 可能
    /// 落在 recognizer 的其它层级（那些位置 Rime 不认——vnum 只在
    /// recognizer/patterns 下生效），但文件里确实已经有了一个 vnum 键。只认
    /// patterns 一个位置时就会再插一份，两个 vnum 落进同一个映射 → YAML 非法
    /// （duplicate entry with key "vnum"）→ 方案加载失败。所以这里的规则是：
    /// recognizer 子树里只留一份 vnum，且必须落在 recognizer/patterns 下；错位的
    /// 那份原样搬过去（值不改），已经在正确位置的那份一个字节都不动。
    fn ensure_vnum_pattern(&mut self, value: &Value) -> bool {
        let Some(recognizer) = value.get("recognizer") else {
            return false;
        };
        // 目标状态：vnum 已经在 recognizer/patterns 下。
        let has_proper = recognizer
            .get("patterns")
            .and_then(|patterns| patterns.get("vnum"))
            .is_some();
        // 错位状态：vnum 在 recognizer 块首（patterns 之前）。
        let has_misplaced = recognizer.get("vnum").is_some();
        if has_proper && !has_misplaced {
            return false;
        }
        let Some(block) = self.find_block("recognizer", 0) else {
            return false;
        };
        // flow 风格（recognizer: {…}）没法按行插，放弃。
        if is_flow_value(&self.lines[block.0]) {
            return false;
        }
        // recognizer 子键的实际缩进：方案文件的缩进宽度不是我们能定的（2 格、4 格
        // 都有人用），按实际值算才不会插出第二个 patterns 键。
        let child_indent = self.child_indent_of(block);
        let patterns_line = (block.0 + 1..block.1)
            .find(|&index| is_key_at(&self.lines[index], "patterns", child_indent));
        // patterns 是 flow 风格（patterns: {…}）就按行插不了：宁可不改，也不能
        // 造出第二个 patterns 键（那会让 YAML 直接非法）。
        if let Some(line) = patterns_line {
            if is_flow_value(&self.lines[line]) {
                return false;
            }
        }
        let misplaced_line = (block.0 + 1..block.1)
            .find(|&index| is_key_at(&self.lines[index], "vnum", child_indent));

        if !has_proper {
            // patterns 下还没有 vnum：补一份。错位的那份优先搬过来（值原样保留）。
            let entry = misplaced_line
                .map(|index| self.lines[index].trim_start().to_string())
                .unwrap_or_else(|| format!("vnum: '{VNUM_PATTERN}'"));
            match patterns_line {
                Some(line) => {
                    let Some(inner) = self.find_block_from("patterns", child_indent, line) else {
                        return false;
                    };
                    let indent = (inner.0 + 1..inner.1)
                        .find(|&index| !is_blank(&self.lines[index]))
                        .map(|index| indent_of(&self.lines[index]))
                        .unwrap_or(child_indent + 2);
                    let last = (inner.0 + 1..inner.1)
                        .rev()
                        .find(|&index| !is_blank(&self.lines[index]))
                        .unwrap_or(inner.0);
                    self.lines
                        .insert(last + 1, format!("{}{}", " ".repeat(indent), entry));
                }
                None => {
                    // recognizer 块里没有 patterns：建一个再放 vnum。
                    let at = block.0 + 1;
                    self.lines
                        .insert(at, format!("{}patterns:", " ".repeat(child_indent)));
                    self.lines
                        .insert(at + 1, format!("{}{}", " ".repeat(child_indent + 2), entry));
                }
            }
            self.changed = true;
        }
        // 错位的那份（recognizer 块首的 vnum）搬走或删掉：recognizer 子树里只留
        // 一份 vnum，否则升级路径上会出现两个 vnum 键。
        if misplaced_line.is_some() && self.remove_vnum_at(child_indent) {
            self.changed = true;
        }
        true
    }

    /// 块内子键的缩进：取块里第一行非空非注释的缩进；空块按 2 算（YAML 惯例）。
    fn child_indent_of(&self, block: (usize, usize)) -> usize {
        (block.0 + 1..block.1)
            .find(|&index| !is_blank(&self.lines[index]) && !is_comment(&self.lines[index]))
            .map(|index| indent_of(&self.lines[index]))
            .unwrap_or(2)
    }

    /// 删掉 recognizer 子键层上的那个 vnum 行（错位的那份）。
    fn remove_vnum_at(&mut self, indent: usize) -> bool {
        let Some(block) = self.find_block("recognizer", 0) else {
            return false;
        };
        let Some(index) =
            (block.0 + 1..block.1).find(|&index| is_key_at(&self.lines[index], "vnum", indent))
        else {
            return false;
        };
        self.lines.remove(index);
        true
    }

    /// punctuator 深合并（0.6.20）：保证 half_shape 里有
    /// "\\" -> 、 与 ":" -> ：（标量形式，直接上屏）；已有同键一律跳过。
    ///
    /// 三种形态：
    /// * 段缺失 → 追加 import_preset: default + half_shape；
    /// * 标量（punctuator: default）→ 规范化为映射并保留原 preset 名；
    /// * 已是映射 → 只补 half_shape 里缺的键（half_shape 缺失就新建）。
    fn ensure_punctuator(&mut self, value: &Value) -> PunctuatorPatch {
        let mut patch = PunctuatorPatch::default();
        match value.get(PUNCTUATOR_KEY) {
            None => self.append_punctuator_block(DEFAULT_PUNCTUATOR_PRESET, &mut patch),
            Some(Value::Mapping(mapping)) => {
                self.merge_punctuator_mapping(mapping, &mut patch);
            }
            Some(Value::String(preset)) => {
                let preset = preset.trim();
                let preset = if preset.is_empty() {
                    DEFAULT_PUNCTUATOR_PRESET
                } else {
                    preset
                };
                if !self.replace_scalar_punctuator(preset, &mut patch) {
                    patch.skipped.push(format!(
                        "{PUNCTUATOR_KEY}: {preset}（内联写法，无法安全改写）"
                    ));
                }
            }
            Some(_) => patch
                .skipped
                .push(format!("{PUNCTUATOR_KEY}: 既不是映射也不是标量，跳过")),
        }
        patch
    }

    /// 段缺失：在文件末尾追加完整的 punctuator 块。
    fn append_punctuator_block(&mut self, preset: &str, patch: &mut PunctuatorPatch) {
        let needs_blank = self
            .lines
            .last()
            .map(|line| !is_blank(line))
            .unwrap_or(false);
        if needs_blank {
            self.lines.push(String::new());
        }
        self.lines.push(format!("{PUNCTUATOR_KEY}:"));
        self.lines
            .push(format!("  {IMPORT_PRESET_KEY}: {}", yaml_scalar(preset)));
        self.lines.push(format!("  {HALF_SHAPE_KEY}:"));
        for key in [PUNCT_BACKSLASH, PUNCT_COLON] {
            self.lines.push(format!("    {}", half_shape_entry(key)));
            patch.mark(key);
        }
        self.changed = true;
    }

    /// 标量形态（punctuator: default）→ 映射，保留原 preset 名与行尾注释。
    fn replace_scalar_punctuator(&mut self, preset: &str, patch: &mut PunctuatorPatch) -> bool {
        let Some(index) = self
            .lines
            .iter()
            .position(|line| is_key_at(line, PUNCTUATOR_KEY, 0))
        else {
            return false;
        };
        if is_flow_value(&self.lines[index]) {
            return false;
        }
        let comment = self.lines[index]
            .split_once(" #")
            .map(|(_, tail)| format!(" #{}", tail))
            .unwrap_or_default();
        let mut replacement = vec![
            format!("{PUNCTUATOR_KEY}:{comment}"),
            format!("  {IMPORT_PRESET_KEY}: {}", yaml_scalar(preset)),
            format!("  {HALF_SHAPE_KEY}:"),
        ];
        for key in [PUNCT_BACKSLASH, PUNCT_COLON] {
            replacement.push(format!("    {}", half_shape_entry(key)));
            patch.mark(key);
        }
        self.lines.splice(index..=index, replacement);
        self.changed = true;
        true
    }

    /// 已是映射：只往 half_shape 里补缺的键。
    fn merge_punctuator_mapping(
        &mut self,
        mapping: &serde_yaml::Mapping,
        patch: &mut PunctuatorPatch,
    ) {
        let Some((start, end)) = self.find_block(PUNCTUATOR_KEY, 0) else {
            patch.skipped.push(format!("{PUNCTUATOR_KEY}:（找不到块）"));
            return;
        };
        if is_flow_value(&self.lines[start]) {
            patch
                .skipped
                .push(format!("{PUNCTUATOR_KEY}:（flow 写法，无法安全改写）"));
            return;
        }
        let half_key = Value::String(HALF_SHAPE_KEY.to_string());
        match mapping.get(&half_key) {
            Some(Value::Mapping(half)) => {
                let Some((hs_start, hs_end)) = self.find_block_from(HALF_SHAPE_KEY, 2, start + 1)
                else {
                    patch
                        .skipped
                        .push(format!("{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY}（找不到块）"));
                    return;
                };
                if hs_start >= end {
                    patch.skipped.push(format!(
                        "{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY}（不在 punctuator 块内）"
                    ));
                    return;
                }
                if is_flow_value(&self.lines[hs_start]) {
                    patch.skipped.push(format!(
                        "{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY}（flow 写法，无法安全改写）"
                    ));
                    return;
                }
                let indent = (hs_start + 1..hs_end)
                    .find(|&index| !is_blank(&self.lines[index]) && !is_comment(&self.lines[index]))
                    .map(|index| indent_of(&self.lines[index]))
                    .unwrap_or_else(|| indent_of(&self.lines[hs_start]) + 2);
                let mut at = hs_end;
                for key in [PUNCT_BACKSLASH, PUNCT_COLON] {
                    if half.get(Value::String(key.to_string())).is_some() {
                        patch.skipped.push(format!(
                            "{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY}/{key}（已存在，未覆盖）"
                        ));
                        continue;
                    }
                    self.lines.insert(
                        at,
                        format!("{}{}", " ".repeat(indent), half_shape_entry(key)),
                    );
                    at += 1;
                    patch.mark(key);
                    self.changed = true;
                }
            }
            Some(_) => patch
                .skipped
                .push(format!("{PUNCTUATOR_KEY}/{HALF_SHAPE_KEY} 不是映射，跳过")),
            None => {
                // punctuator 有、half_shape 没有：在 punctuator 块末尾补一整段。
                let base = indent_of(&self.lines[start]) + 2;
                let mut at = end;
                self.lines
                    .insert(at, format!("{}{HALF_SHAPE_KEY}:", " ".repeat(base)));
                at += 1;
                for key in [PUNCT_BACKSLASH, PUNCT_COLON] {
                    self.lines.insert(
                        at,
                        format!("{}{}", " ".repeat(base + 2), half_shape_entry(key)),
                    );
                    at += 1;
                    patch.mark(key);
                }
                self.changed = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml_text::item_name;

    /// 测试用临时目录（core 不带 tempfile 依赖，手写一份就够）。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path =
                env::temp_dir().join(format!("rime-sync-data-dirs-{}-{tag}", std::process::id()));
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

    fn paths(value: &[PathBuf]) -> Vec<String> {
        value
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    }

    #[test]
    fn registry_output_keeps_both_values() {
        let output = "\r\nHKEY_LOCAL_MACHINE\\Software\\WOW6432Node\\Rime\\Weasel\r\n    \
            InstallDir    REG_SZ    C:\\Program Files\\Rime\\weasel-0.17.4\r\n    \
            WeaselRoot    REG_SZ    C:\\Program Files\\Rime\r\n";
        let values = weasel_registry_from_output(output);
        assert_eq!(
            values.weasel_root,
            Some(PathBuf::from(r"C:\Program Files\Rime"))
        );
        assert_eq!(
            values.install_dir,
            Some(PathBuf::from(r"C:\Program Files\Rime\weasel-0.17.4"))
        );
    }

    #[test]
    fn registry_output_falls_back_to_install_dir() {
        let output = "    InstallDir    REG_SZ    D:\\Apps\\Rime  \r\n";
        let values = weasel_registry_from_output(output);
        assert_eq!(values.weasel_root, None);
        assert_eq!(values.install_dir, Some(PathBuf::from(r"D:\Apps\Rime")));
    }

    #[test]
    fn registry_output_without_values_is_empty() {
        assert_eq!(
            weasel_registry_from_output("HKEY_LOCAL_MACHINE\\Nope\r\n"),
            WeaselRegistryValues::default()
        );
        assert_eq!(
            weasel_registry_from_output("    WeaselRoot    REG_DWORD    0x1\r\n"),
            WeaselRegistryValues::default()
        );
    }

    #[test]
    fn weasel_version_parsing() {
        assert_eq!(weasel_version("weasel-0.17.4"), Some(vec![0, 17, 4]));
        assert_eq!(weasel_version("weasel-0.17.10"), Some(vec![0, 17, 10]));
        assert_eq!(weasel_version("weasel-"), None);
        assert_eq!(weasel_version("weasel-0.17.x"), None);
        assert_eq!(weasel_version("Rime"), None);
    }

    /// ② 注册表 WeaselRoot 命中：<WeaselRoot>\data 存在就直接用。
    #[test]
    fn weasel_root_data_dir_wins() {
        let tmp = TempDir::new("root-hit");
        let rime = tmp.path().join("Rime");
        fs::create_dir_all(rime.join("data")).expect("mkdir");
        let registry = WeaselRegistryValues {
            weasel_root: Some(rime.clone()),
            install_dir: None,
        };

        let resolution = resolve_windows(&registry, Some(tmp.path()));

        assert_eq!(resolution.dirs, vec![rime.join("data")]);
        assert!(resolution.error.is_none());
        assert!(
            resolution.candidates.iter().any(|item| {
                item.path == rime.join("data").display().to_string() && item.exists
            }),
            "候选里必须记下命中的目录：{:?}",
            resolution.candidates
        );
    }

    /// 227 实测的形状：WeaselRoot 是 C:\Program Files\Rime，data 在版本目录里。
    #[test]
    fn weasel_root_without_data_falls_back_to_version_dir() {
        let tmp = TempDir::new("root-glob");
        let rime = tmp.path().join("Rime");
        let version_dir = rime.join("weasel-0.17.4");
        fs::create_dir_all(version_dir.join("data")).expect("mkdir");
        let registry = WeaselRegistryValues {
            weasel_root: Some(rime.clone()),
            install_dir: None,
        };

        let resolution = resolve_windows(&registry, Some(tmp.path()));

        assert_eq!(resolution.dirs, vec![version_dir.join("data")]);
        assert!(
            resolution
                .candidates
                .iter()
                .any(|item| item.path == rime.join("data").display().to_string() && !item.exists),
            "不存在的 <WeaselRoot>\\data 也要记进候选：{:?}",
            resolution.candidates
        );
    }

    /// ③ WeaselRoot 缺失：用 InstallDir 通配 weasel-*，多个版本取最高。
    #[test]
    fn install_dir_glob_picks_highest_version() {
        let tmp = TempDir::new("install-glob");
        let install = tmp.path().join("Rime");
        for version in ["weasel-0.16.0", "weasel-0.17.4", "weasel-0.17.10"] {
            fs::create_dir_all(install.join(version).join("data")).expect("mkdir");
        }
        // 同名文件不算版本目录
        fs::write(install.join("weasel-0.18.0"), b"not a dir").expect("write");
        let registry = WeaselRegistryValues {
            weasel_root: None,
            install_dir: Some(install.clone()),
        };

        let resolution = resolve_windows(&registry, Some(tmp.path()));

        assert_eq!(
            paths(&resolution.dirs),
            vec![install
                .join("weasel-0.17.10")
                .join("data")
                .display()
                .to_string()]
        );
    }

    /// ④ 两个注册值都没有：默认 %ProgramFiles%\Rime 下也找一遍。
    #[test]
    fn default_program_files_layout_is_probed() {
        let tmp = TempDir::new("default-layout");
        let version_dir = tmp.path().join("Rime").join("weasel-0.17.4");
        fs::create_dir_all(version_dir.join("data")).expect("mkdir");

        let resolution = resolve_windows(&WeaselRegistryValues::default(), Some(tmp.path()));

        assert_eq!(resolution.dirs, vec![version_dir.join("data")]);
    }

    /// ⑤ 全部候选都不存在：错误文案要列出探测过的路径。
    #[test]
    fn no_candidate_reports_probed_paths() {
        let tmp = TempDir::new("all-missing");
        let resolution = resolve_windows(&WeaselRegistryValues::default(), Some(tmp.path()));

        assert!(resolution.dirs.is_empty());
        let error = resolution.error.expect("必须有可读原因");
        assert!(error.contains("未找到可用的 Rime data 目录"), "{error}");
        assert!(
            error.contains(&tmp.path().join("Rime").join("data").display().to_string()),
            "错误文案必须列出探测过的路径：{error}"
        );
        assert!(
            error.contains("default:%ProgramFiles%"),
            "错误文案要带候选来源：{error}"
        );
    }

    /// 平台默认目录作为候选记进报告（Linux 上不存在也照样列出来）。
    #[cfg(not(windows))]
    #[test]
    fn platform_default_dir_is_a_candidate() {
        if env::var_os(ENV_DATA_DIR).is_some() {
            return; // 别的测试/CI 显式指定过 data 目录，跳过
        }
        let resolution = resolve_data_dirs();
        assert!(
            resolution
                .candidates
                .iter()
                .any(|item| item.path == LINUX_DATA_DIR),
            "{:?} 里没有 {LINUX_DATA_DIR}",
            resolution.candidates
        );
    }

    #[test]
    fn env_override_parsing() {
        assert!(resolve_env(OsStr::new("")).dirs.is_empty());
        assert!(resolve_env(OsStr::new("0")).dirs.is_empty());

        let tmp = TempDir::new("env");
        let resolution = resolve_env(tmp.path().as_os_str());
        assert_eq!(resolution.dirs, vec![tmp.path().to_path_buf()]);
        assert_eq!(
            resolution
                .candidates
                .first()
                .map(|item| item.source.as_str()),
            Some("env:RIMESYNC_WEASEL_DATA")
        );

        // 不存在的目录被过滤掉，并给出可读原因
        let missing = resolve_env(OsStr::new("/nonexistent/rime-sync-test"));
        assert!(missing.dirs.is_empty());
        assert!(missing
            .error
            .expect("必须有可读原因")
            .contains("/nonexistent/rime-sync-test"));
    }

    const SCHEMA_FIXTURE: &str = "schema:\n  schema_id: luna_pinyin_simp\nengine:\n  processors:\n    - recognizer\n    - key_binder\n  translators:\n    - table_translator\n";

    /// 0.6.22：注入 RIMESYNC_VERSION 后，注释行必须用这个版本（CLI 与 GUI 同源）。
    #[test]
    fn env_version_is_used_for_marker() {
        let tmp = TempDir::new("env-version");
        let file = tmp.path().join("luna_pinyin_simp.schema.yaml");
        fs::write(&file, SCHEMA_FIXTURE).expect("write");
        let previous = env::var_os(ENV_VERSION);
        env::set_var(ENV_VERSION, "9.9.9-test");
        let version = resolve_version(None);
        let outcome = patch_file(&file, &version).expect("patch");
        match previous {
            Some(value) => env::set_var(ENV_VERSION, value),
            None => env::remove_var(ENV_VERSION),
        }
        assert_eq!(version, "9.9.9-test");
        assert_eq!(outcome, PatchOutcome::Patched);
        let text = fs::read_to_string(&file).expect("read");
        assert!(
            text.starts_with("# patched by rime-sync 9.9.9-test\n"),
            "{text}"
        );
        // 显式参数优先级最高（GUI 传 app version）。
        assert_eq!(resolve_version(Some("1.2.3")), "1.2.3");
    }

    #[cfg(not(windows))]
    fn readonly_files(
        tmp: &TempDir,
        tag: &str,
        files: &[(&str, &str)],
        original: &std::fs::Permissions,
    ) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let data = tmp.path().join(tag);
        fs::create_dir_all(&data).expect("mkdir");
        for (name, body) in files {
            fs::write(data.join(name), body).expect("write");
        }
        let mut readonly = original.clone();
        readonly.set_mode(0o555);
        fs::set_permissions(&data, readonly).expect("chmod");
        // root / 特殊文件系统下 chmod 挡不住写：跳过（没有可验证的降级场景）。
        let probe = data.join(".probe");
        if fs::write(&probe, b"x").is_ok() {
            let _ = fs::remove_file(&probe);
            fs::set_permissions(&data, original.clone()).expect("restore");
            return None;
        }
        Some(data)
    }

    #[cfg(not(windows))]
    fn readonly_dir(tmp: &TempDir, tag: &str, original: &std::fs::Permissions) -> Option<PathBuf> {
        readonly_files(
            tmp,
            tag,
            &[("luna_pinyin_simp.schema.yaml", SCHEMA_FIXTURE)],
            original,
        )
    }

    fn read_yaml(path: &Path) -> Value {
        let text =
            fs::read_to_string(path).unwrap_or_else(|error| panic!("读取 {path:?}: {error}"));
        serde_yaml::from_str(&text).unwrap_or_else(|error| panic!("解析 {path:?}: {error}"))
    }

    /// 0.6.23：/usr/share/rime-data 只读（普通用户）时不再往用户目录写方案文件
    /// （librime 只从共享目录读方案本体，写过去等于静默失效），改为生成用户级
    /// <schema>.custom.yaml；engine/processors 必须是「共享方案原列表 + 插入项」的
    /// 完整列表——custom.yaml 是 patch 语义，列表整段替换。
    #[cfg(not(windows))]
    #[test]
    fn readonly_data_dir_writes_custom_yaml() {
        let tmp = TempDir::new("custom");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_dir(&tmp, "data", &original) else {
            return;
        };
        let user = tmp.path().join("xdg").join("fcitx5").join("rime");
        fs::create_dir_all(&user).expect("mkdir");
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: vec![DataDirCandidate {
                path: data.display().to_string(),
                source: "test:readonly".to_string(),
                exists: true,
            }],
            error: None,
        };

        let report = patch_auto_with(
            resolution,
            vec![user.clone()],
            "9.9.9",
            FixSource::Explicit,
            &|_, _| DeployOutcome::Ok,
        );
        fs::set_permissions(&data, original.clone()).expect("restore");

        assert!(report.fallback, "{report:?}");
        assert_eq!(report.mode, MODE_CUSTOM);
        assert_eq!(report.data_dir, data.display().to_string());
        assert_eq!(report.deploy_triggered, DEPLOY_OK, "{report:?}");
        assert_eq!(report.version, "9.9.9");
        assert_eq!(report.patched(), 1, "{report:?}");
        // 降级成功：没用上全局目录，但也不该报「需要提权」。
        assert!(!report.needs_elevation());
        assert!(
            report
                .data_error
                .as_deref()
                .unwrap_or_default()
                .contains("custom.yaml"),
            "{:?}",
            report.data_error
        );
        assert!(report
            .data_candidates
            .iter()
            .any(|item| item.source == "fallback:user"));

        let custom = user.join("luna_pinyin_simp.custom.yaml");
        assert_eq!(
            report.custom_written,
            vec![custom.display().to_string()],
            "{report:?}"
        );
        let document = read_yaml(&custom);
        let patch = document
            .get("patch")
            .and_then(Value::as_mapping)
            .expect("patch 段");
        let names = |key: &str| list_at(patch, &["engine", key]).expect("engine 列表");
        // 原有处理器一个都不能丢，顺序也不能变（只在 recognizer 之后插入两项）。
        assert_eq!(
            names("processors"),
            vec![
                "recognizer",
                VSELECT_PROCESSOR,
                NUMPAD_PROCESSOR,
                "key_binder"
            ]
        );
        assert_eq!(
            names("translators"),
            vec![VNUM_TRANSLATOR, "table_translator"]
        );
        assert_eq!(
            get_path(patch, &["recognizer", "patterns", "vnum"]).and_then(Value::as_str),
            Some(VNUM_PATTERN)
        );
        let half = get_path(patch, &["punctuator", "half_shape"]).expect("half_shape");
        assert_eq!(
            half.get(PUNCT_BACKSLASH).and_then(Value::as_str),
            Some(PUNCT_BACKSLASH_COMMIT)
        );
        assert_eq!(
            half.get(PUNCT_COLON).and_then(Value::as_str),
            Some(PUNCT_COLON_COMMIT)
        );
        // 共享目录里的方案一个字节都没动。
        assert_eq!(
            fs::read_to_string(data.join("luna_pinyin_simp.schema.yaml")).expect("read"),
            SCHEMA_FIXTURE
        );
    }

    /// luna_pinyin_simp 这类方案自己只有 `__include: luna_pinyin.schema:/`，
    /// engine 全在父方案里；不展开 include 就会误判「没有 engine」，主方案降级失效。
    #[cfg(not(windows))]
    #[test]
    fn custom_yaml_expands_include_chain() {
        // luna_pinyin 不在保留清单里，但它是 luna_pinyin_simp 的 __include 基类，
        // 属于依赖闭包，必须一并生成 custom.yaml（否则主方案的补丁静默失效）。
        const BASE: &str = "schema:\n  schema_id: luna_pinyin\nengine:\n  processors:\n    - ascii_composer\n    - recognizer\n    - key_binder\n    - speller\n    - punctuator\n  translators:\n    - table_translator\n";
        const CHILD: &str =
            "schema:\n  schema_id: luna_pinyin_simp\n__include: luna_pinyin.schema:/\n";
        let tmp = TempDir::new("custom-include");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_files(
            &tmp,
            "data",
            &[
                ("luna_pinyin.schema.yaml", BASE),
                ("luna_pinyin_simp.schema.yaml", CHILD),
            ],
            &original,
        ) else {
            return;
        };
        let user = tmp.path().join("xdg").join("fcitx5").join("rime");
        fs::create_dir_all(&user).expect("mkdir");
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: Vec::new(),
            error: None,
        };

        let report = patch_auto_with(
            resolution,
            vec![user.clone()],
            "9.9.9",
            FixSource::Explicit,
            &|_, _| DeployOutcome::Ok,
        );
        fs::set_permissions(&data, original.clone()).expect("restore");

        assert_eq!(report.mode, MODE_CUSTOM, "{report:?}");
        assert_eq!(report.skipped_not_retained, 0, "{report:?}");
        assert_eq!(report.custom_written.len(), 2, "{report:?}");
        assert!(user.join("luna_pinyin.custom.yaml").is_file(), "{report:?}");
        let custom = user.join("luna_pinyin_simp.custom.yaml");
        assert!(custom.is_file(), "{report:?}");
        let document = read_yaml(&custom);
        let patch = document
            .get("patch")
            .and_then(Value::as_mapping)
            .expect("patch 段");
        assert_eq!(
            list_at(patch, &["engine", "processors"]).expect("processors"),
            vec![
                "ascii_composer",
                "recognizer",
                VSELECT_PROCESSOR,
                NUMPAD_PROCESSOR,
                "key_binder",
                "speller",
                "punctuator"
            ]
        );
        assert_eq!(
            list_at(patch, &["engine", "translators"]).expect("translators"),
            vec![VNUM_TRANSLATOR, "table_translator"]
        );
    }

    /// 已有 custom.yaml：合并而不是覆盖——用户自己的键、已有列表顺序都保留，
    /// 原文件另外留一份 .rimesync-orig。
    #[cfg(not(windows))]
    #[test]
    fn custom_yaml_merges_existing_user_file() {
        let tmp = TempDir::new("custom-merge");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_dir(&tmp, "data", &original) else {
            return;
        };
        let user = tmp.path().join("xdg").join("fcitx5").join("rime");
        fs::create_dir_all(&user).expect("mkdir");
        let custom = user.join("luna_pinyin_simp.custom.yaml");
        let existing = "schema: luna_pinyin_simp
patch:
  menu/page_size: 9
  engine/processors:
    - speller
    - punctuator
  punctuator/half_shape:
    \":\": \"用户冒号\"
";
        fs::write(&custom, existing).expect("write existing");
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: Vec::new(),
            error: None,
        };

        let report = patch_auto_with(
            resolution,
            vec![user.clone()],
            "9.9.9",
            FixSource::Explicit,
            &|_, _| DeployOutcome::Failed,
        );

        assert_eq!(report.mode, MODE_CUSTOM, "{report:?}");
        assert_eq!(report.deploy_triggered, DEPLOY_FAILED, "{report:?}");
        let document = read_yaml(&custom);
        // 顶层用户键与 patch 里的其它键都保留。
        assert_eq!(
            document.get("schema").and_then(Value::as_str),
            Some("luna_pinyin_simp")
        );
        let patch = document
            .get("patch")
            .and_then(Value::as_mapping)
            .expect("patch 段");
        assert_eq!(patch.get("menu/page_size").and_then(Value::as_u64), Some(9));
        // 基准列表来自用户自己的 custom.yaml（不是共享方案的），顺序保留。
        assert_eq!(
            list_at(patch, &["engine", "processors"]).expect("processors"),
            vec![VSELECT_PROCESSOR, NUMPAD_PROCESSOR, "speller", "punctuator"]
        );
        let half = get_path(patch, &["punctuator", "half_shape"]).expect("half_shape");
        // 用户写过的键不覆盖，缺的键才补。
        assert_eq!(
            half.get(PUNCT_COLON).and_then(Value::as_str),
            Some("用户冒号")
        );
        assert_eq!(
            half.get(PUNCT_BACKSLASH).and_then(Value::as_str),
            Some(PUNCT_BACKSLASH_COMMIT)
        );
        // 原文件备份（只留一份，内容是改写前的那份）。
        assert_eq!(
            fs::read_to_string(backup_path(&custom)).expect("backup"),
            existing
        );
        assert!(report
            .custom_written
            .iter()
            .any(|path| path == &custom.display().to_string()));

        // 再跑一次：已经是目标状态，不再改写、不再覆盖备份。
        let again = patch_auto_with(
            DataDirResolution {
                dirs: vec![data.clone()],
                candidates: Vec::new(),
                error: None,
            },
            vec![user],
            "9.9.9",
            FixSource::Explicit,
            &|_, _| DeployOutcome::Skipped,
        );
        fs::set_permissions(&data, original.clone()).expect("restore");
        assert!(again.custom_written.is_empty(), "{again:?}");
        assert_eq!(again.schemas_unchanged.len(), 1, "{again:?}");
    }

    /// 0.6.23：custom 降级只处理保留清单 + 依赖闭包里的方案。共享目录里残留的
    /// terra_pinyin / cangjie5 之类已裁掉的方案，一个 custom.yaml 都不该生成
    /// （否则用户目录被污染，prune 下次启动又把它删掉）。
    #[cfg(not(windows))]
    #[test]
    fn custom_mode_only_writes_retained_schemas() {
        let tmp = TempDir::new("custom-retained");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_files(
            &tmp,
            "data",
            &[
                ("luna_pinyin_simp.schema.yaml", SCHEMA_FIXTURE),
                ("wubi_pinyin.schema.yaml", SCHEMA_FIXTURE),
                ("wubi86.schema.yaml", SCHEMA_FIXTURE),
                ("terra_pinyin.schema.yaml", SCHEMA_FIXTURE),
                ("cangjie5.schema.yaml", SCHEMA_FIXTURE),
            ],
            &original,
        ) else {
            return;
        };
        let user = tmp.path().join("xdg").join("fcitx5").join("rime");
        fs::create_dir_all(&user).expect("mkdir");
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: Vec::new(),
            error: None,
        };

        let report = patch_auto_with(
            resolution,
            vec![user.clone()],
            "9.9.9",
            FixSource::Explicit,
            &|_, _| DeployOutcome::Ok,
        );
        fs::set_permissions(&data, original.clone()).expect("restore");

        assert_eq!(report.mode, MODE_CUSTOM, "{report:?}");
        assert_eq!(report.custom_written.len(), 3, "{report:?}");
        assert_eq!(report.skipped_not_retained, 2, "{report:?}");
        for id in ["luna_pinyin_simp", "wubi_pinyin", "wubi86"] {
            assert!(
                user.join(format!("{id}.custom.yaml")).is_file(),
                "{id} 在保留清单里，必须生成 custom.yaml：{report:?}"
            );
        }
        for id in ["terra_pinyin", "cangjie5"] {
            assert!(
                !user.join(format!("{id}.custom.yaml")).exists(),
                "{id} 不在保留清单里，不该生成 custom.yaml"
            );
            assert!(
                report.schemas_skipped.iter().any(|item| item.contains(id)),
                "{id} 要进日志：{:?}",
                report.schemas_skipped
            );
        }
        // 共享目录只读：一个字节都没动。
        assert_eq!(
            fs::read_to_string(data.join("terra_pinyin.schema.yaml")).expect("read"),
            SCHEMA_FIXTURE
        );
    }

    /// 0.6.23：启动自举写了新文件也只记 deploy=deferred——绝不调用外部命令，
    /// 否则 fcitx5-remote -r 会把用户正在用的输入法打断。
    #[cfg(not(windows))]
    #[test]
    fn bootstrap_source_defers_deploy_without_running_commands() {
        use std::cell::Cell;
        let tmp = TempDir::new("deploy-bootstrap");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_dir(&tmp, "data", &original) else {
            return;
        };
        let user = tmp.path().join("xdg").join("fcitx5").join("rime");
        fs::create_dir_all(&user).expect("mkdir");
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: Vec::new(),
            error: None,
        };
        let calls = Cell::new(0);
        let deploy = |_: &Path, _: &Path| {
            calls.set(calls.get() + 1);
            DeployOutcome::Ok
        };

        let report = patch_auto_with(
            resolution,
            vec![user.clone()],
            "9.9.9",
            FixSource::Bootstrap,
            &deploy,
        );
        fs::set_permissions(&data, original.clone()).expect("restore");

        assert_eq!(calls.get(), 0, "Bootstrap 不得触发任何外部命令：{report:?}");
        assert_eq!(report.deploy_triggered, DEPLOY_DEFERRED, "{report:?}");
        assert_eq!(report.custom_written.len(), 1, "{report:?}");
        assert!(
            report
                .data_error
                .as_deref()
                .unwrap_or_default()
                .contains("重新部署"),
            "{:?}",
            report.data_error
        );
    }

    /// Explicit（用户点「修复输入法」/ 手动跑 fix-ime）仍然触发一次部署，
    /// 并把用户目录与共享目录交给 runner。
    #[cfg(not(windows))]
    #[test]
    fn explicit_source_runs_deploy_once() {
        use std::cell::RefCell;
        let tmp = TempDir::new("deploy-explicit");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_dir(&tmp, "data", &original) else {
            return;
        };
        let user = tmp.path().join("xdg").join("fcitx5").join("rime");
        fs::create_dir_all(&user).expect("mkdir");
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: Vec::new(),
            error: None,
        };
        let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let deploy = |user_dir: &Path, shared_dir: &Path| {
            seen.borrow_mut()
                .push(format!("{}|{}", user_dir.display(), shared_dir.display()));
            DeployOutcome::Ok
        };

        let report = patch_auto_with(
            resolution,
            vec![user.clone()],
            "9.9.9",
            FixSource::Explicit,
            &deploy,
        );
        fs::set_permissions(&data, original.clone()).expect("restore");

        assert_eq!(report.deploy_triggered, DEPLOY_OK, "{report:?}");
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(
            seen[0],
            format!("{}|{}", user.display(), data.display()),
            "runner 拿到的必须是用户目录 + 共享目录"
        );
    }

    /// 没有用户目录可降级：不写任何文件，报告 skipped + 需 sudo。
    #[cfg(not(windows))]
    #[test]
    fn readonly_without_fallback_reports_skipped() {
        let tmp = TempDir::new("fallback-none");
        let original = fs::metadata(tmp.path()).expect("meta").permissions();
        let Some(data) = readonly_dir(&tmp, "data", &original) else {
            return;
        };
        let resolution = DataDirResolution {
            dirs: vec![data.clone()],
            candidates: Vec::new(),
            error: None,
        };

        let report = patch_auto_from(resolution, Vec::new(), "9.9.9", FixSource::Explicit);
        fs::set_permissions(&data, original.clone()).expect("restore");

        assert!(!report.fallback);
        assert_eq!(report.mode, MODE_SKIPPED);
        assert_eq!(report.deploy_triggered, DEPLOY_SKIPPED);
        assert_eq!(report.data_dir, "");
        assert!(report.custom_written.is_empty());
        assert!(report.patched() == 0);
        assert!(
            report
                .schemas_skipped
                .iter()
                .any(|item| item.contains("sudo")),
            "{:?}",
            report.schemas_skipped
        );
        assert!(report.needs_elevation());
        assert_eq!(
            fs::read_to_string(data.join("luna_pinyin_simp.schema.yaml")).expect("read"),
            SCHEMA_FIXTURE
        );
    }

    /// Explicit 下的部署顺序：先 fcitx5-remote -r，失败 / 不存在再退
    /// rime_deployer --build <用户目录> <共享目录>。
    #[cfg(not(windows))]
    #[test]
    fn deploy_order_tries_reload_then_builder() {
        use std::cell::RefCell;
        let user = PathBuf::from("/tmp/rime-user");
        let shared = PathBuf::from("/usr/share/rime-data");
        let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let run = |program: &str, args: &[&OsStr]| {
            seen.borrow_mut().push(
                std::iter::once(program)
                    .chain(args.iter().map(|arg| arg.to_str().unwrap_or("?")))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            if program == DEPLOY_RELOAD {
                RunResult::Failed
            } else {
                RunResult::Ok
            }
        };

        assert_eq!(deploy_outcome_with(&run, &user, &shared), DeployOutcome::Ok);
        assert_eq!(
            seen.into_inner(),
            vec![
                format!("{DEPLOY_RELOAD} -r"),
                format!(
                    "{DEPLOY_BUILDER} --build {} {}",
                    user.display(),
                    shared.display()
                ),
            ]
        );
    }

    /// 重载成功就不再多跑重建命令；两个命令都不存在 = skipped。
    #[cfg(not(windows))]
    #[test]
    fn deploy_outcome_stops_on_success_and_maps_missing() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let ok = |_: &str, _: &[&OsStr]| {
            calls.set(calls.get() + 1);
            RunResult::Ok
        };
        assert_eq!(
            deploy_outcome_with(&ok, Path::new("/u"), Path::new("/s")),
            DeployOutcome::Ok
        );
        assert_eq!(calls.get(), 1, "重载成功后不该再跑 rime_deployer");

        let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let missing = |program: &str, _: &[&OsStr]| {
            seen.borrow_mut().push(program.to_string());
            RunResult::Missing
        };
        assert_eq!(
            deploy_outcome_with(&missing, Path::new("/u"), Path::new("/s")),
            DeployOutcome::Skipped
        );
        assert_eq!(seen.into_inner().len(), 2);
    }

    #[test]
    fn item_name_handles_quotes_and_tickets() {
        assert_eq!(item_name("    - rimesync_vselect"), "rimesync_vselect");
        assert_eq!(
            item_name("    - \"table_translator@custom_phrase\""),
            "table_translator@custom_phrase"
        );
        assert_eq!(item_name("    - simplifier@zh_hans"), "simplifier@zh_hans");
        assert_eq!(item_name("not an item"), "");
    }
}
