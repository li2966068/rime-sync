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
//! 3. recognizer/patterns 增加 vnum: '^[vV][0-9.]*$'
//! 4. punctuator/half_shape 深合并 "\\" -> 、 与 ":" -> ：（0.6.20；只在缺失时注入）
//! 5. 文件顶部插入 # patched by rime-sync <version>
//!
//! 写盘纪律：先备份 <file>.rimesync-orig（只做一次），补丁后用 serde_yaml 重新
//! 解析确认合法、核对目标状态，再按原换行风格写成 UTF-8（不带 BOM）。权限不足时
//! 返回可读错误（提示以管理员身份重试），不 panic。

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_yaml::Value;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// 原文件备份后缀：只建一次，重复执行不覆盖。
pub const BACKUP_SUFFIX: &str = ".rimesync-orig";
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
/// 显式指定 data 目录（多个用系统路径分隔符）；空串或 0 = 关闭补丁。测试靠它隔离。
pub const ENV_DATA_DIR: &str = "RIMESYNC_WEASEL_DATA";
/// Linux 的 data 目录。
pub const LINUX_DATA_DIR: &str = "/usr/share/rime-data";
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
}

/// 一批 data 目录的补丁结果。
#[derive(Debug, Clone, Default, Serialize)]
pub struct DataPatchReport {
    /// 实际打补丁的 data 目录（0 或 1 个；找不到就是空）。
    pub data_dirs: Vec<String>,
    /// 实际使用的 data 目录；空串 = 没找到可用目录。
    pub data_dir: String,
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
    /// 备份所在目录（写第一个备份的 data 目录）；没写过备份时为空。
    pub backup_dir: String,
    /// 可读的失败原因（权限不足时带提权提示）。
    pub errors: Vec<String>,
}

impl DataPatchReport {
    /// 本次写入的方案个数，就是 CLI 报告里的 data_patched。
    pub fn patched(&self) -> usize {
        self.schemas_patched.len()
    }

    /// 是否需要提示用户以管理员身份重试。
    pub fn needs_elevation(&self) -> bool {
        self.errors.iter().any(|error| error.contains("管理员"))
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

/// 解析 data 目录并打补丁（CLI / 设置页 / 启动自举统一走这里）。
pub fn patch_auto(version: &str) -> DataPatchReport {
    let resolution = resolve_data_dirs();
    let mut report = patch_dirs(&resolution.dirs, version);
    report.data_candidates = resolution.candidates;
    report.data_error = resolution.error;
    report
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
    let mut report = DataPatchReport::default();
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
                    }
                }
                Err(error) => report.errors.push(format!("{}：{error:#}", file.display())),
            }
        }
    }
    report.data_dir = report.data_dirs.first().cloned().unwrap_or_default();
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
    let bytes = fs::read(path).with_context(|| format!("读取 {}", path.display()))?;
    let raw = String::from_utf8(bytes.clone()).map_err(|_| anyhow!("文件不是 UTF-8，跳过"))?;
    // 目标是 UTF-8 无 BOM：输入带 BOM 时去掉，写回也不写 BOM。
    let text = raw.strip_prefix(BOM).unwrap_or(&raw);
    let value: Value = serde_yaml::from_str(text)
        .with_context(|| format!("{} 不是合法 YAML，跳过", path.display()))?;

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
    let parsed: Value = serde_yaml::from_str(&patched)
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

/* ------------------------------ 文本编辑 ------------------------------ */

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// 这一行是不是「缩进 indent 的 key:」（键后面可以直接带值）。
fn is_key_at(line: &str, key: &str, indent: usize) -> bool {
    if is_blank(line) || is_comment(line) || indent_of(line) != indent {
        return false;
    }
    let rest = &line[indent..];
    let Some(after) = rest.strip_prefix(key) else {
        return false;
    };
    after.starts_with(':')
}

/// 这一行的值是 flow 风格（以 { 或 [ 开头）。正则里的 [0-9] 不算。
fn is_flow_value(line: &str) -> bool {
    let Some((_, value)) = line.split_once(':') else {
        return false;
    };
    let value = value.trim();
    value.starts_with('{') || value.starts_with('[')
}

/// 列表项的名字：- rimesync_vselect / - "table_translator@custom_phrase"。
fn item_name(line: &str) -> String {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('-') else {
        return String::new();
    };
    rest.trim()
        .trim_matches(|ch| ch == '\'' || ch == '"')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|ch| ch == '\'' || ch == '"')
        .to_string()
}

/// YAML 键的写法：双引号 + 反斜杠转义（反斜杠键写成 "\\"）。
fn yaml_key(key: &str) -> String {
    format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
}

/// 简单标量的写法：普通标识符原样，其余用双引号包起来。
fn yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
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

struct Doc {
    lines: Vec<String>,
    eol: String,
    trailing_newline: bool,
    /// 本次是否真的动过行（用于区分 Unchanged / Skipped）。
    changed: bool,
}

impl Doc {
    fn parse(text: &str) -> Self {
        let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = text.ends_with('\n');
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
            .collect();
        if trailing_newline {
            lines.pop();
        }
        Self {
            lines,
            eol: eol.to_string(),
            trailing_newline,
            changed: false,
        }
    }

    fn render(&self) -> String {
        let body = self.lines.join(&self.eol);
        if self.trailing_newline {
            format!("{body}{}", self.eol)
        } else {
            body
        }
    }

    /// 顶部标记；已有 # patched by rime-sync 就不再插（不追版本号）。
    fn ensure_marker(&mut self, version: &str) -> bool {
        if self
            .lines
            .iter()
            .any(|line| line.trim_start().starts_with(MARKER_PREFIX))
        {
            return false;
        }
        // YAML 指令（%YAML / %TAG）必须留在最前面。
        let at = self
            .lines
            .iter()
            .position(|line| !line.starts_with('%'))
            .unwrap_or(0);
        self.lines.insert(at, format!("{MARKER_PREFIX} {version}"));
        self.changed = true;
        true
    }

    fn find_block_from(&self, key: &str, indent: usize, from: usize) -> Option<(usize, usize)> {
        let start =
            (from..self.lines.len()).find(|&index| is_key_at(&self.lines[index], key, indent))?;
        let mut end = start + 1;
        while end < self.lines.len() {
            let line = &self.lines[end];
            if is_blank(line) || is_comment(line) {
                end += 1;
                continue;
            }
            if indent_of(line) <= indent {
                break;
            }
            end += 1;
        }
        // 块尾的空行/同级注释不算块内容。
        while end > start + 1 {
            let line = &self.lines[end - 1];
            if is_blank(line) || (is_comment(line) && indent_of(line) <= indent) {
                end -= 1;
            } else {
                break;
            }
        }
        Some((start, end))
    }

    fn find_block(&self, key: &str, indent: usize) -> Option<(usize, usize)> {
        self.find_block_from(key, indent, 0)
    }

    fn item_lines(&self, block: (usize, usize)) -> Vec<usize> {
        (block.0 + 1..block.1)
            .filter(|&index| {
                let line = &self.lines[index];
                !is_comment(line) && line.trim_start().starts_with("- ")
            })
            .collect()
    }

    /// 列表项的统一缩进：取现有项；空列表时比键多 2 格。
    fn item_indent(&self, block: (usize, usize), items: &[usize]) -> usize {
        items
            .first()
            .map(|&index| indent_of(&self.lines[index]))
            .unwrap_or_else(|| indent_of(&self.lines[block.0]) + 2)
    }

    /// 列表是不是 flow 风格（processors: [a, b]）。这种没法按行插，直接放弃。
    fn is_flow_list(&self, key: &str, indent: usize) -> bool {
        let Some((start, _)) = self.find_block(key, indent) else {
            return false;
        };
        is_flow_value(&self.lines[start])
    }

    fn remove_item(&mut self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let victims: Vec<usize> = self
            .item_lines(block)
            .into_iter()
            .filter(|&index| item_name(&self.lines[index]) == item)
            .collect();
        if victims.is_empty() {
            return false;
        }
        for index in victims.into_iter().rev() {
            self.lines.remove(index);
        }
        self.changed = true;
        true
    }

    fn insert_item_after(&mut self, key: &str, indent: usize, anchor: &str, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        let line_indent = self.item_indent(block, &items);
        let Some(index) = items
            .iter()
            .find(|&&index| item_name(&self.lines[index]) == anchor)
            .copied()
        else {
            return false;
        };
        self.lines
            .insert(index + 1, format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn insert_item_before(&mut self, key: &str, indent: usize, anchor: &str, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        let line_indent = self.item_indent(block, &items);
        let Some(index) = items
            .iter()
            .find(|&&index| item_name(&self.lines[index]) == anchor)
            .copied()
        else {
            return false;
        };
        self.lines
            .insert(index, format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn insert_item_front(&mut self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        if items.is_empty() {
            return false;
        }
        let line_indent = self.item_indent(block, &items);
        self.lines
            .insert(items[0], format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn push_item(&mut self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        let items = self.item_lines(block);
        let line_indent = self.item_indent(block, &items);
        let at = items.last().map(|&index| index + 1).unwrap_or(block.0 + 1);
        self.lines
            .insert(at, format!("{}- {item}", " ".repeat(line_indent)));
        self.changed = true;
        true
    }

    fn contains_item(&self, key: &str, indent: usize, item: &str) -> bool {
        let Some(block) = self.find_block(key, indent) else {
            return false;
        };
        self.item_lines(block)
            .into_iter()
            .any(|index| item_name(&self.lines[index]) == item)
    }

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
    fn ensure_vnum_pattern(&mut self, value: &Value) -> bool {
        let Some(recognizer) = value.get("recognizer") else {
            return false;
        };
        if recognizer
            .get("patterns")
            .and_then(|patterns| patterns.get("vnum"))
            .is_some()
        {
            return false;
        }
        let Some((start, end)) = self.find_block("recognizer", 0) else {
            return false;
        };
        // flow 风格（recognizer: {…}）没法按行插，放弃。
        if is_flow_value(&self.lines[start]) {
            return false;
        }
        let patterns = (start + 1..end).find(|&index| is_key_at(&self.lines[index], "patterns", 2));
        match patterns {
            Some(line) => {
                let Some(block) = self.find_block_from("patterns", 2, line) else {
                    return false;
                };
                let last = (block.0 + 1..block.1)
                    .rev()
                    .find(|&index| !is_blank(&self.lines[index]))
                    .unwrap_or(block.0);
                if is_flow_value(&self.lines[last]) {
                    // patterns: {} 之类的 flow 映射，放弃。
                    return false;
                }
                self.lines
                    .insert(last + 1, format!("    vnum: '{VNUM_PATTERN}'"));
                self.changed = true;
                true
            }
            None => {
                // recognizer 块里没有 patterns：插在块首。
                self.lines.insert(start + 1, "  patterns:".to_string());
                self.lines
                    .insert(start + 2, format!("    vnum: '{VNUM_PATTERN}'"));
                self.changed = true;
                true
            }
        }
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
