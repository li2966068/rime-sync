//! 共享状态目录的解析 + 旧目录的一次性迁移。
//!
//! 为什么要统一到一处：桌面端原来把状态（index.json / retry.json / 锁）与备份放在
//! **自己的应用配置目录**（Linux `~/.config/com.rimesync.app`、Windows
//! `%APPDATA%\com.rimesync.app`），而 CLI 用的是 `~/.config/rime-sync`。
//! 两端各存一份基线 ⇒ 同一台机器上「桌面端刚改过的文件」在 CLI 眼里还是旧的，
//! 合并时就被当成冲突 —— 这正是那些莫名其妙的假冲突。
//!
//! 统一之后：状态目录 = `~/.config/rime-sync`（Windows `%APPDATA%\rime-sync`），
//! 备份根 = 它的 `backups/` 子目录，与 CLI 完全同一处。
//!
//! **配置文件本身的位置不动**：桌面端的 config.json 仍在应用配置目录里
//! （搬配置文件等于丢用户配置），只搬状态与备份，而且只搬一次。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// 共享目录名（与 CLI 的配置目录名一致）。
pub const SHARED_DIR_NAME: &str = "rime-sync";

/// 旧目录里要搬走的**文件**。
///
/// 只列白名单：旧目录里还有 config.json（用户的配置，不能动）与
/// install-warning.json（Linux postinst 写的，应用仍然从旧目录读它），
/// 以及将来可能出现的其它文件 —— 未列名的条目一律留在原地，绝不误搬。
pub const MIGRATED_FILES: &[&str] = &[
    // 基线与同步状态：两端共用同一份才不会再有假冲突。
    "index.json",
    "retry.json",
    "sync.lock",
    "history.jsonl",
    "last-conflicts.json",
    // 桌面端自己的状态。
    "platform.json",
    "bootstrap.log",
    // 日志与崩溃标记：搬过去才能保住「上次崩了」的现场。
    "log.txt",
    "crash.txt",
    // 轮转出来的历史日志（log.1.txt / log.2.txt …）。
    "log.1.txt",
    "log.2.txt",
    "log.3.txt",
];

/// 旧目录里要搬走的**目录**。
pub const MIGRATED_DIRS: &[&str] = &[
    // 还原点：两端共用同一批备份。
    "backups",
    // 引擎的基线缓存与设备配置目录（<state_dir>/profiles/<id> → <state_dir>/sync/<id>）。
    "base", "profiles", "sync",
];

/// 旧目录里**不搬**的条目（留在原地，搬了就等于丢用户配置）。
const KEPT_FILES: &[&str] = &["config.json", "install-warning.json"];

/// 读一个非空的环境变量（空串按没设处理）。
fn env_path(name: &str) -> Option<PathBuf> {
    let value = env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// 共享状态目录：桌面端与 CLI 的 index.json（基线）、备份都放在这里。
///
/// 规则与 CLI（crates/cli/src/main.rs）**逐条一致**，两端才能真的落在同一处：
/// 1. `RIME_SYNC_CONFIG`：CLI 的状态目录就是配置文件所在目录，桌面端跟着走；
/// 2. `XDG_CONFIG_HOME` → `$XDG_CONFIG_HOME/rime-sync`；
/// 3. `HOME` → `$HOME/.config/rime-sync`（与 core 的默认配置路径同规则）；
/// 4. `APPDATA` → `%APPDATA%\rime-sync`（Windows；不写死盘符，用环境变量）。
///
/// 没有第 5 条：解析不出来就是错误，让调用方明确报错，
/// 绝不悄悄落到某个临时目录 —— 那样两端又会算到不同的基线上去。
pub fn shared_state_dir() -> Result<PathBuf, String> {
    if let Some(config) = env_path("RIME_SYNC_CONFIG") {
        if let Some(parent) = config.parent() {
            if !parent.as_os_str().is_empty() {
                return Ok(parent.to_path_buf());
            }
        }
    }
    if let Some(dir) = env_path("XDG_CONFIG_HOME") {
        return Ok(dir.join(SHARED_DIR_NAME));
    }
    if let Some(home) = env_path("HOME") {
        return Ok(home.join(".config").join(SHARED_DIR_NAME));
    }
    if let Some(dir) = env_path("APPDATA") {
        return Ok(dir.join(SHARED_DIR_NAME));
    }
    Err("无法定位用户配置目录：XDG_CONFIG_HOME / HOME / APPDATA 都没有设置".to_string())
}

/// 备份根：状态目录下的 `backups/`（与 CLI 的 `app_data/backups` 同一处）。
pub fn backup_root(state_dir: &Path) -> PathBuf {
    state_dir.join("backups")
}

/// 一次迁移的结果。
#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    /// 已经搬到新位置的条目。
    pub moved: Vec<String>,
    /// 新位置已有同名条目而跳过的（不覆盖新位置）。
    pub skipped: Vec<String>,
    /// 搬失败的条目与原因。
    pub failures: Vec<(String, String)>,
}

impl MigrationReport {
    /// 什么都没发生（既没搬成也没失败）⇒ 重复启动时的正常状态。
    pub fn is_noop(&self) -> bool {
        self.moved.is_empty() && self.skipped.is_empty() && self.failures.is_empty()
    }

    /// 一行可读摘要（写进日志，让用户看得见发生了什么）。
    pub fn summary(&self) -> String {
        if self.is_noop() {
            return "状态目录已是最新，无需迁移".to_string();
        }
        let mut parts = Vec::new();
        if !self.moved.is_empty() {
            parts.push(format!("已迁移 {} 项", self.moved.len()));
        }
        if !self.skipped.is_empty() {
            parts.push(format!("跳过 {} 项（新位置已有）", self.skipped.len()));
        }
        if !self.failures.is_empty() {
            parts.push(format!("失败 {} 项", self.failures.len()));
        }
        parts.join("，")
    }

    /// 失败原因的可读文本（每行一条）。
    pub fn failure_lines(&self) -> Vec<String> {
        self.failures
            .iter()
            .map(|(name, reason)| format!("{name}：{reason}"))
            .collect()
    }
}

/// 这个条目要不要搬。
fn should_migrate(name: &str) -> bool {
    if KEPT_FILES.contains(&name) {
        return false;
    }
    MIGRATED_FILES.contains(&name) || MIGRATED_DIRS.contains(&name)
}

/// 把旧目录里的状态与备份**一次性**搬到共享目录。
///
/// 规则：
/// - 旧目录不存在 ⇒ 什么都不做（没升级过的用户不受影响）；
/// - 新位置已有同名条目 ⇒ 跳过，**绝不覆盖**（新位置是共享基线，
///   覆盖它等于毁掉另一端已经算好的基线）；
/// - 搬失败 ⇒ 记进报告继续往下走，**绝不因为迁移失败让应用起不来**；
/// - 重复调用是空操作（搬过的条目已经不在旧目录里了）。
///
/// 旧目录与新目录相同 ⇒ 直接返回（没什么可搬的）。
pub fn migrate_legacy_state(legacy: &Path, shared: &Path) -> MigrationReport {
    let mut report = MigrationReport::default();
    if legacy == shared {
        return report;
    }
    let entries = match fs::read_dir(legacy) {
        Ok(entries) => entries,
        Err(error) => {
            // 旧目录读不动（权限/已被删/不是目录）：放弃迁移，应用照常用新目录。
            report.failures.push((
                legacy.display().to_string(),
                format!("读取旧目录失败：{error}"),
            ));
            return report;
        }
    };
    if let Err(error) = fs::create_dir_all(shared) {
        report.failures.push((
            shared.display().to_string(),
            format!("创建共享目录失败：{error}"),
        ));
        return report;
    }
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.failures.push((
                    legacy.display().to_string(),
                    format!("读取目录项失败：{error}"),
                ));
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if !should_migrate(&name) {
            continue;
        }
        let source = legacy.join(&name);
        let target = shared.join(&name);
        if target.exists() {
            // 新位置已有 ⇒ 不覆盖。旧的那份留在原地，用户想手动处理也不拦着。
            report.skipped.push(name);
            continue;
        }
        match move_entry(&source, &target) {
            Ok(()) => report.moved.push(name),
            Err(reason) => report.failures.push((name, reason)),
        }
    }
    report
}

/// 搬一个文件或目录：先改名（同一文件系统内最快），失败再退回「复制 + 删除」。
fn move_entry(source: &Path, target: &Path) -> Result<(), String> {
    match fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            // 跨设备（EXDEV）等情况下 rename 不可用，退回复制。
            let copied = if source.is_dir() {
                copy_dir(source, target)
            } else {
                fs::copy(source, target)
                    .map(|_| ())
                    .map_err(|error| format!("复制失败：{error}"))
            };
            match copied {
                Ok(()) => {
                    // 复制成功才算搬完；删不掉旧文件不算失败（旧文件留在原地也无害）。
                    let _ = remove_entry(source);
                    Ok(())
                }
                Err(copy_error) => Err(format!("改名失败（{error}），{copy_error}")),
            }
        }
    }
}

/// 递归复制目录。
fn copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|error| format!("创建目录失败：{error}"))?;
    let entries = fs::read_dir(source).map_err(|error| format!("读取目录失败：{error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取目录项失败：{error}"))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|error| format!("复制失败：{error}"))?;
        }
    }
    Ok(())
}

/// 删除文件或目录（删不掉就算了，不影响迁移结果）。
fn remove_entry(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|error| format!("删除失败：{error}"))
}
