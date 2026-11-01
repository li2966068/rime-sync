//! 远端设备：读 \`sync/*/owner.json\`，注销时删除整个设备目录。

use std::fs;
use std::path::Path;

use anyhow::Context;
use rime_sync_core::config::{self, Config};
use rime_sync_core::dedupe;
use rime_sync_core::engine;
use rime_sync_core::plan;
use rime_sync_core::profile::{self, Profile};
use rime_sync_core::rime_dir;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::remote_tree::{self, CopyReport, DeleteReport};
use crate::{config_path, dav_client, detect_rime, ensure_config, push_log, AppState};

#[derive(Serialize)]
pub struct DeviceView {
    pub id: String,
    pub hostname: String,
    pub os: String,
    pub session: String,
    pub heartbeat_ts: u64,
    /// 目录名 == 本机 installation_id。
    pub is_self: bool,
    /// 旧版目录：目录名 != 本机 ID，且没有 owner.json 或 owner.json 的 device_id
    /// 就等于目录名（0.6.0 及更早用 device_id 当目录名），是「历史目录」候选。
    pub legacy: bool,
    pub reachable: bool,
}

#[derive(Serialize)]
pub struct RevokeView {
    pub id: String,
    pub removed: bool,
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::to_string)
}

/// 远端目录名（= 本机身份）：installation.yaml 的 installation_id，缺失时回退
/// device_id。device_id 只是 owner.json 里的人类可读标签，不再决定目录名。
pub(crate) fn local_remote_id(rime: Option<&Path>, config: &Config) -> String {
    match rime {
        Some(dir) => rime_dir::local_sync_id(dir, &config.device_id),
        None => config.device_id.clone(),
    }
}

/// sync/ 下一行的原始数据：目录名 + 可选的 owner.json。
struct RawDevice {
    id: String,
    owner: Option<serde_json::Value>,
    /// sync/<id> 目录是否存在；只出现在旧布局 profiles/<id> 下的设备为 false。
    has_sync_dir: bool,
}

/// 纯函数：目录名与 owner.json → 设备页视图。is_self 只按 installation_id 判定，
/// 旧版 device_id 目录会作为「另一台设备」出现（可在设备页注销）。
fn build_devices(
    rows: Vec<RawDevice>,
    local_id: &str,
    self_owner: Option<&serde_json::Value>,
) -> Vec<DeviceView> {
    let mut devices: Vec<DeviceView> = rows
        .into_iter()
        .map(|row| {
            let owner = row.owner.as_ref();
            let field = |key: &str| owner.and_then(|value| text_field(value, key));
            // 只出现在旧布局 profiles/<id> 下的设备不是历史目录；sync/ 里没有
            // owner.json 的空目录则按遗留候选处理。
            let legacy = row.id != local_id
                && row.has_sync_dir
                && owner
                    .and_then(|value| text_field(value, "device_id"))
                    .map(|label| label == row.id)
                    .unwrap_or(true);
            DeviceView {
                is_self: row.id == local_id,
                legacy,
                hostname: field("hostname").unwrap_or_else(|| "—".to_string()),
                os: field("os").unwrap_or_else(|| "—".to_string()),
                session: field("session").unwrap_or_default(),
                heartbeat_ts: owner
                    .and_then(|value| value.get("heartbeat_ts"))
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                reachable: owner.is_some(),
                id: row.id,
            }
        })
        .collect();
    // 本机身份目录可能还没在远端创建（改了名称但没跑同步）：补一条占位行。
    if !devices.iter().any(|device| device.id == local_id) {
        let field = |key: &str| {
            self_owner
                .and_then(|value| text_field(value, key))
                .unwrap_or_else(|| "—".to_string())
        };
        devices.push(DeviceView {
            id: local_id.to_string(),
            hostname: field("hostname"),
            os: field("os"),
            session: String::new(),
            heartbeat_ts: 0,
            is_self: true,
            legacy: false,
            reachable: false,
        });
    }
    // 本机在最前，其余在线设备按心跳排序，历史目录垫底（不遮挡正在用的设备）。
    devices.sort_by(|left, right| {
        right
            .is_self
            .cmp(&left.is_self)
            .then(left.legacy.cmp(&right.legacy))
            .then(right.heartbeat_ts.cmp(&left.heartbeat_ts))
            .then(left.id.cmp(&right.id))
    });
    devices
}

/// 设备页入口：本机身份按 installation_id 计算（diagnostics 导出也走这里）。
pub(crate) fn collect_devices(
    config: &Config,
    client: &DavClient,
) -> Result<Vec<DeviceView>, String> {
    let rime = detect_rime(config).ok().map(|detection| detection.path);
    let local_id = local_remote_id(rime.as_deref(), config);
    collect_devices_for(config, &local_id, client)
}

fn collect_devices_for(
    config: &Config,
    local_id: &str,
    client: &DavClient,
) -> Result<Vec<DeviceView>, String> {
    let mut rows = Vec::new();
    for entry in client
        .propfind("sync", 1)
        .map_err(|error| format!("读取远端设备失败：{error:#}"))?
    {
        if !entry.is_dir {
            continue;
        }
        let id = entry.path.rsplit('/').next().unwrap_or("").to_string();
        if id.is_empty() || id == "sync" || id.starts_with('.') {
            continue;
        }
        if plan::validate_rel_path(&format!("sync/{id}")).is_err() {
            continue;
        }
        let owner = client
            .get(&format!("sync/{id}/owner.json"))
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        rows.push(RawDevice {
            id,
            owner,
            has_sync_dir: true,
        });
    }
    // 新布局的配置目录在 sync/<id>/config 下，上面的 sync/ 扫描已经覆盖；这里只补
    // 0.6.4 开发期间的旧布局 profiles/<id>（迁移失败或设备没再同步的残留），让它们
    // 仍能在设备页被看到并注销。
    let legacy_root = profile::LEGACY_PROFILES_DIR;
    let known: std::collections::BTreeSet<String> = rows.iter().map(|row| row.id.clone()).collect();
    if let Ok(entries) = client.propfind(legacy_root, 1) {
        for entry in entries {
            if !entry.is_dir || !entry.path.starts_with(&format!("{legacy_root}/")) {
                continue;
            }
            let id = entry.path.rsplit('/').next().unwrap_or("").to_string();
            if !profile::is_legacy_profile_dir_name(&id)
                || known.contains(&id)
                || plan::validate_rel_path(&entry.path).is_err()
            {
                continue;
            }
            rows.push(RawDevice {
                id,
                owner: None,
                has_sync_dir: false,
            });
        }
    }
    Ok(build_devices(
        rows,
        local_id,
        engine::owner_json(config).ok().as_ref(),
    ))
}

#[tauri::command]
pub fn cmd_list_devices(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<DeviceView>, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    collect_devices(&config, &client)
}

/// 注销 / 重置的路径闸门：本机目录必须带上与本机 ID 完全一致的 `confirm`。
pub(crate) fn revoke_target(id: &str, self_id: &str, confirm: Option<&str>) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id == "."
        || id == ".."
        || id.starts_with('.')
    {
        return Err("设备 ID 无效".to_string());
    }
    if id == self_id && confirm.map(str::trim) != Some(self_id) {
        // 本机目录 = 当前 installation_id：删掉它等于重置远端身份，必须二次确认。
        return Err(format!(
            "sync/{id} 是本机目录（installation_id）；要重置远端身份，确认值必须等于 {self_id}"
        ));
    }
    Ok(())
}

/// 注销要删的远端路径：sync/<id> 与其配置目录 sync/<id>/config。
/// 本机目录的配置目录被删除闸门挡住（自删必须先重置身份）。
pub(crate) fn revoke_delete_targets(id: &str, self_id: &str) -> Result<Vec<String>, String> {
    let profile = Profile::new(id).map_err(|error| format!("设备路径无效：{error:#}"))?;
    let sync_rel = profile.remote_sync();
    plan::validate_rel_path(&sync_rel).map_err(|error| format!("设备路径无效：{error:#}"))?;
    let mut targets = vec![sync_rel];
    let config_rel = profile.remote_root();
    if dedupe::validate_profile_delete_path(&config_rel, self_id).is_ok() {
        targets.push(config_rel);
    }
    Ok(targets)
}

fn resolve_ids(config: &Config) -> String {
    let rime = detect_rime(config).ok().map(|detection| detection.path);
    local_remote_id(rime.as_deref(), config)
}

#[tauri::command]
pub fn cmd_revoke_device(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    confirm: Option<String>,
) -> Result<RevokeView, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    let id = id.trim().to_string();
    let self_id = resolve_ids(&config);
    revoke_target(&id, &self_id, confirm.as_deref())?;
    // [设备目录, 配置目录]：配置在 sync/<id>/config（正常情况下随设备目录一起删掉），
    // 再显式删一次作为兜底，避免服务端把 config/ 当特殊子集合跳过时留下孤儿目录。
    let targets = revoke_delete_targets(&id, &self_id)?;
    let rel = targets[0].clone();
    // 服务端 DELETE 集合未必递归：逐文件/逐目录删除后再删目录。
    remote_tree::delete_tree(&client, &rel).map_err(|error| format!("注销设备失败：{error:#}"))?;
    if let Some(profile_rel) = targets.get(1) {
        if !client
            .propfind(profile_rel, 0)
            .unwrap_or_default()
            .is_empty()
        {
            remote_tree::delete_tree(&client, profile_rel)
                .map_err(|error| format!("注销设备配置目录失败：{error:#}"))?;
        }
    }
    push_log(&state.0, format!("已注销远端设备 {id}"));
    Ok(RevokeView { id, removed: true })
}

#[derive(Serialize)]
pub struct ResetRemoteDirView {
    pub id: String,
    pub removed: usize,
    pub freed_bytes: u64,
}

/// 重置远端身份：删除 sync/<当前 installation_id>（含 owner.json）。
/// 下次同步会按同一 ID 重新创建目录并写 owner.json，本地数据不受影响。
#[tauri::command]
pub fn cmd_reset_local_remote_dir(
    app: AppHandle,
    state: State<'_, AppState>,
    confirm: String,
) -> Result<ResetRemoteDirView, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    let self_id = resolve_ids(&config);
    if confirm.trim() != self_id {
        return Err(format!(
            "确认值与本机目录 sync/{self_id} 不一致，已取消（不会删除任何远端数据）"
        ));
    }
    let rel = format!("sync/{self_id}");
    plan::validate_rel_path(&rel).map_err(|error| format!("设备路径无效：{error:#}"))?;
    let report = remote_tree::delete_tree(&client, &rel)
        .map_err(|error| format!("重置远端身份失败（删除 {rel}）：{error:#}"))?;
    push_log(
        &state.0,
        format!("已重置远端身份：删除 {rel}（{} 项）", report.removed),
    );
    Ok(ResetRemoteDirView {
        id: self_id,
        removed: report.removed,
        freed_bytes: report.freed_bytes,
    })
}

#[derive(Serialize)]
pub struct LocalIdentityView {
    pub device_id: String,
    /// 远端目录名（= local_sync_id）；缺 installation.yaml 时回退 device_id。
    pub installation_id: String,
    pub differs: bool,
}

/// 设置页提示用：显示名称（label）与本机远端目录名是否不同。
#[tauri::command]
pub fn cmd_local_identity(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LocalIdentityView, String> {
    let config = ensure_config(&app, &state.0)?;
    let installation_id = resolve_ids(&config);
    Ok(LocalIdentityView {
        differs: installation_id != config.device_id,
        device_id: config.device_id.clone(),
        installation_id,
    })
}

#[derive(Serialize)]
pub struct PublishIdentityView {
    pub device_id: String,
    pub remote_path: String,
    pub wrote: bool,
}

/// 身份登记只用到三个 WebDAV 写操作，抽成 trait 便于单测断言调用顺序。
pub(crate) trait IdentityDav {
    fn ensure_base(&self) -> anyhow::Result<()>;
    fn ensure_dir(&self, rel: &str) -> anyhow::Result<()>;
    fn put(&self, rel: &str, bytes: &[u8]) -> anyhow::Result<()>;
}

struct DavIdentity<'a>(&'a DavClient);

impl IdentityDav for DavIdentity<'_> {
    fn ensure_base(&self) -> anyhow::Result<()> {
        self.0.ensure_base()
    }

    fn ensure_dir(&self, rel: &str) -> anyhow::Result<()> {
        self.0.ensure_dir(rel)
    }

    fn put(&self, rel: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.0.put(rel, bytes, None).map(|_| ())
    }
}

pub(crate) fn require_webdav(config: &Config) -> Result<(), String> {
    if config.webdav.url.trim().is_empty() {
        return Err("尚未配置 WebDAV 地址，无法登记远端设备".to_string());
    }
    Ok(())
}

/// 只登记身份，不搬运任何快照 / shared 文件：ensure_base → sync → sync/<id> → owner.json。
/// 返回实际执行的步骤（单测据此断言顺序）。
pub(crate) fn publish_identity_with(
    dav: &impl IdentityDav,
    device_id: &str,
    owner: &[u8],
) -> Result<Vec<String>, String> {
    // 与 transport 使用同一套路径约束（不套用更严的「新 ID」语法校验：
    // 旧配置里的 device_id 只要传输可用，就必须能登记）。
    if device_id.is_empty()
        || device_id.contains('/')
        || device_id.contains('\\')
        || device_id.starts_with('.')
    {
        return Err(format!("设备 ID 无效：{device_id}"));
    }
    let remote = format!("sync/{device_id}");
    plan::validate_rel_path(&remote).map_err(|error| format!("设备路径无效：{error:#}"))?;

    let mut steps = Vec::new();
    dav.ensure_base()
        .map_err(|error| format!("登记远端身份失败（根目录）：{error:#}"))?;
    steps.push("ensure_base".to_string());
    dav.ensure_dir("sync")
        .map_err(|error| format!("登记远端身份失败（sync 目录）：{error:#}"))?;
    steps.push("ensure_dir:sync".to_string());
    dav.ensure_dir(&remote)
        .map_err(|error| format!("登记远端身份失败（{remote}）：{error:#}"))?;
    steps.push(format!("ensure_dir:{remote}"));
    let owner_path = format!("{remote}/owner.json");
    dav.put(&owner_path, owner)
        .map_err(|error| format!("登记远端身份失败（{owner_path}）：{error:#}"))?;
    steps.push(format!("put:{owner_path}"));
    Ok(steps)
}

/// 把本机身份登记到远端 sync/<installation_id>/owner.json（幂等、只写一个文件）。
/// 目录名用 installation_id，owner.json 里的 device_id 仍是人类可读标签。
#[tauri::command]
pub fn cmd_publish_identity(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PublishIdentityView, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    let rime = detect_rime(&config).ok().map(|detection| detection.path);
    let view = publish_identity(&config, rime.as_deref(), &client)?;
    push_log(&state.0, format!("已登记远端设备身份 {}", view.remote_path));
    Ok(view)
}

pub(crate) fn publish_identity(
    config: &Config,
    rime: Option<&Path>,
    client: &DavClient,
) -> Result<PublishIdentityView, String> {
    require_webdav(config)?;
    let owner =
        engine::owner_bytes(config).map_err(|error| format!("生成设备身份失败：{error:#}"))?;
    let remote_id = local_remote_id(rime, config);
    let steps = publish_identity_with(&DavIdentity(client), &remote_id, &owner)?;
    Ok(PublishIdentityView {
        device_id: config.device_id.clone(),
        remote_path: format!("sync/{remote_id}"),
        wrote: !steps.is_empty(),
    })
}

#[derive(Serialize)]
pub struct DeviceIdCheck {
    pub id: String,
    /// 语法可用（不负责远端是否已有同名目录）。
    pub ok: bool,
    pub error: Option<String>,
    /// 远端是否已存在 sync/<id>；未配置 WebDAV 时为 None。
    pub remote_exists: Option<bool>,
    /// 远端探测失败不影响本地校验，单独回报。
    pub remote_error: Option<String>,
}

/// 设备 ID 校验：语法（非空 / ≤64 / [a-z0-9-] / 非 . .. / 无路径分隔）+ 远端占位检查。
#[tauri::command]
pub fn cmd_check_device_id(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<DeviceIdCheck, String> {
    let id = id.trim().to_string();
    if let Err(error) = config::validate_device_id(&id) {
        return Ok(DeviceIdCheck {
            id,
            ok: false,
            error: Some(format!("{error:#}")),
            remote_exists: None,
            remote_error: None,
        });
    }
    let config = ensure_config(&app, &state.0)?;
    let rime = detect_rime(&config).ok().map(|detection| detection.path);
    let local_id = local_remote_id(rime.as_deref(), &config);
    if config.webdav.url.trim().is_empty() || local_id == id {
        return Ok(DeviceIdCheck {
            id,
            ok: true,
            error: None,
            remote_exists: None,
            remote_error: None,
        });
    }
    let client = DavClient::with_proxy(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
        Some(&config.proxy),
    )
    .map_err(|error| format!("WebDAV 配置无效：{error:#}"))?;
    match client.propfind(&format!("sync/{id}"), 0) {
        Ok(entries) => Ok(DeviceIdCheck {
            id,
            ok: true,
            error: None,
            remote_exists: Some(!entries.is_empty()),
            remote_error: None,
        }),
        Err(error) => Ok(DeviceIdCheck {
            id,
            ok: true,
            error: None,
            remote_exists: None,
            remote_error: Some(format!("{error:#}")),
        }),
    }
}

#[derive(Debug, Serialize)]
pub struct MigrateStep {
    pub step: String,
    pub ok: bool,
    pub detail: String,
}

/// 目标目录 owner.json 的摘要（冲突提示与 replace 判定用）。
#[derive(Debug, Clone, Serialize)]
pub struct MigrateOwner {
    pub device_id: Option<String>,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub session: Option<String>,
    pub heartbeat_ts: u64,
}

impl MigrateOwner {
    fn from_value(value: &serde_json::Value) -> Self {
        Self {
            device_id: text_field(value, "device_id"),
            hostname: text_field(value, "hostname"),
            os: text_field(value, "os"),
            session: text_field(value, "session"),
            heartbeat_ts: value
                .get("heartbeat_ts")
                .and_then(|item| item.as_u64())
                .unwrap_or(0),
        }
    }
}

/// 在线窗口：owner.json 的心跳在 15 分钟内才算「有设备在用」。
pub(crate) const ONLINE_WINDOW_SECS: u64 = 15 * 60;

fn is_online(heartbeat_ts: u64, now: u64) -> bool {
    heartbeat_ts > 0 && now.saturating_sub(heartbeat_ts) <= ONLINE_WINDOW_SECS
}

/// 迁移预检看到的目标目录状态（目录名 = 新 ID）。
#[derive(Debug, Clone, Default)]
pub(crate) struct RemoteDirInfo {
    pub exists: bool,
    pub snapshots: usize,
    /// owner.json 的心跳仍在窗口内，且 owner 不是本机标签 → 别的设备可能正在用。
    pub in_use: bool,
    pub owner: Option<MigrateOwner>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrateConflict {
    pub target: String,
    pub target_snapshot_count: usize,
    pub target_owner: Option<MigrateOwner>,
    /// 目录名 = owner.json 里的 device_id（0.6.0 及更早的命名），或目录里没有 owner.json。
    pub target_legacy: bool,
    pub target_in_use: bool,
    /// 允许 mode=replace：是遗留目录且没有其他在线设备在用。
    pub can_replace: bool,
    /// can_replace=false 时给人看的原因。
    pub replace_blocked_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MigrateView {
    pub old_id: String,
    pub new_id: String,
    pub copied_files: usize,
    pub copied_dirs: usize,
    pub removed: usize,
    pub freed_bytes: u64,
    pub steps: Vec<MigrateStep>,
    /// 远端旧目录存在并已完成复制（false = 只改了本地目录与名称）。
    pub remote_renamed: bool,
    /// 本地与远端都没有旧目录：无需搬运，只改了 installation_id 与标签。
    pub skipped: bool,
    /// 迁移已完成、但旧目录清理失败等非致命问题（同时写入日志）。
    pub warnings: Vec<String>,
    /// 本次实际模式：direct（目标不存在）/ cancel / merge / replace。
    pub mode: String,
    /// 预检发现目标已存在且 mode=cancel 时的结构化状态；其余情况为 null。
    pub conflict: Option<MigrateConflict>,
    /// merge 模式从目标目录补进源目录的快照数。
    pub merged_files: usize,
}

/// 目标目录已存在时的处理方式；cancel 只回报冲突、不改动任何东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrateMode {
    Cancel,
    Merge,
    Replace,
}

impl MigrateMode {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "" | "cancel" => Ok(Self::Cancel),
            "merge" => Ok(Self::Merge),
            "replace" => Ok(Self::Replace),
            other => Err(format!(
                "未知迁移模式：{other}（可用 cancel / merge / replace）"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Merge => "merge",
            Self::Replace => "replace",
        }
    }
}

fn conflict_for(target: &str, info: &RemoteDirInfo) -> MigrateConflict {
    let name = target.rsplit('/').next().unwrap_or("");
    // 0.6.0 及更早用 device_id 当目录名：owner.json 的 device_id 与目录名相同。
    // 没有 owner.json 的目录也按遗留处理（只有目录、没有身份登记）。
    let target_legacy = info
        .owner
        .as_ref()
        .and_then(|owner| owner.device_id.as_deref())
        .map(|label| label == name)
        .unwrap_or(true);
    let blocked = if info.in_use {
        Some(format!(
            "{target} 里仍有其他在线设备的心跳（device_id：{}），不能删除后迁移",
            info.owner
                .as_ref()
                .and_then(|owner| owner.device_id.clone())
                .unwrap_or_else(|| "未知".to_string())
        ))
    } else if !target_legacy {
        Some(format!(
            "{target} 是另一台设备的目录（目录名不等于 owner.json 的 device_id），不能删除后迁移"
        ))
    } else {
        None
    };
    MigrateConflict {
        target: target.to_string(),
        target_snapshot_count: info.snapshots,
        target_owner: info.owner.clone(),
        target_legacy,
        target_in_use: info.in_use,
        can_replace: info.exists && blocked.is_none(),
        replace_blocked_reason: blocked,
    }
}

/// 冲突目录的内容摘要：失败/冲突提示要说清目标里有什么。
fn conflict_detail(conflict: &MigrateConflict) -> String {
    let owner = match &conflict.target_owner {
        Some(owner) => format!(
            "owner.json：device_id={} / hostname={} / 心跳 {}",
            owner.device_id.clone().unwrap_or_else(|| "—".to_string()),
            owner.hostname.clone().unwrap_or_else(|| "—".to_string()),
            owner.heartbeat_ts
        ),
        None => "没有 owner.json".to_string(),
    };
    format!(
        "{} 已存在（{} 个 *.userdb.txt，{owner}）",
        conflict.target, conflict.target_snapshot_count
    )
}

/// 迁移需要的外部效果：改 installation_id、重命名本地/远端 sync 目录、保存标签。
/// 抽成 trait 便于单测断言步骤顺序与回滚（远端复制/删除沿用 remote_tree）。
pub(crate) trait MigrationIo {
    fn set_installation_id(&self, id: &str) -> anyhow::Result<()>;
    fn local_dir_exists(&self, name: &str) -> bool;
    fn rename_local_dir(&self, from: &str, to: &str) -> anyhow::Result<()>;
    fn remote_dir_exists(&self, rel: &str) -> anyhow::Result<bool>;
    fn inspect_remote_dir(&self, rel: &str) -> anyhow::Result<RemoteDirInfo>;
    /// 把 from 里 into 缺失的 *.userdb.txt 复制进 into（同名保留 into 的版本）。
    fn merge_missing_snapshots(&self, from: &str, into: &str) -> anyhow::Result<usize>;
    fn copy_remote_dir(&self, src: &str, dst: &str) -> anyhow::Result<CopyReport>;
    fn delete_remote_dir(&self, rel: &str) -> anyhow::Result<DeleteReport>;
    fn save_label(&self, new: &str) -> anyhow::Result<()>;
}

fn step(name: &str, ok: bool, detail: String) -> MigrateStep {
    MigrateStep {
        step: name.to_string(),
        ok,
        detail,
    }
}

fn undo_note<T>(action: &str, result: anyhow::Result<T>) -> String {
    match result {
        Ok(_) => format!("；已{action}"),
        Err(error) => format!("；{action}失败：{error:#}"),
    }
}

/// 统一标识的迁移：
/// 1. installation.yaml 的 installation_id 旧 → 新（远端目录名的唯一来源）；
/// 2. 本地 sync/<旧> → sync/<新>；
/// 3. 远端 sync/<旧> → sync/<新>（复制后删旧）；
/// 4. config.device_id = 新（只是 owner.json 与 UI 里的标签）。
///
/// 目标 sync/<新> 已存在时按 mode 处理：
/// - cancel：不改动任何东西，返回结构化冲突（前端弹三选一）；
/// - merge：先把目标里缺的 *.userdb.txt 补进源（同名以源为准），再改名并删源；
/// - replace：仅当目标是没有在线设备在用的遗留目录时，先删目标再改名。
///
/// 本地与远端都可能没有旧目录（已清理 / 从未上传）：此时不报错，只改
/// installation_id 与标签，并在结果里标记 skipped / remote_renamed。
/// 破坏性的删除放在最后：写 config 之前的任何失败都能把前三步完整回滚。
pub(crate) fn run_migration(
    io: &impl MigrationIo,
    requested_old: &str,
    local_id: &str,
    label: &str,
    new_id: &str,
    mode: MigrateMode,
) -> Result<MigrateView, String> {
    let new_id = new_id.trim();
    config::validate_device_id(new_id).map_err(|error| format!("新设备名称无效：{error:#}"))?;
    let local_id = local_id.trim();
    let requested_old = requested_old.trim();
    // 旧目录名一律取 local_sync_id()（installation_id）。UI 传来的 old 是 device_id
    // 标签，只用于诊断信息、不参与路径计算——0.6.0 在 sync/<标签> 找不到旧目录
    // 就直接报错，正是这里用错了标识。
    let old = local_id.to_string();
    if old == new_id {
        return Err("新旧设备名称相同".to_string());
    }
    let src = format!("sync/{old}");
    let dst = format!("sync/{new_id}");
    // 0.6.4 起每台设备另有一个配置 profile：迁移时必须与 sync 目录一起改名。
    let old_profile = Profile::new(&old).map_err(|error| format!("旧配置目录无效：{error:#}"))?;
    let new_profile = Profile::new(new_id).map_err(|error| format!("新配置目录无效：{error:#}"))?;
    let profile_src = old_profile.remote_root();
    let profile_dst = new_profile.remote_root();
    plan::validate_rel_path(&src).map_err(|error| format!("旧设备路径无效：{error:#}"))?;
    plan::validate_rel_path(&dst).map_err(|error| format!("新设备路径无效：{error:#}"))?;
    plan::validate_rel_path(&profile_src).map_err(|error| format!("旧配置目录无效：{error:#}"))?;
    plan::validate_rel_path(&profile_dst).map_err(|error| format!("新配置目录无效：{error:#}"))?;

    // 预检：任何改动之前先确认两端都不会被覆盖。
    if io.local_dir_exists(new_id) {
        return Err(format!("本地已存在 sync/{new_id}，迁移已取消（不会覆盖）"));
    }
    if io
        .remote_dir_exists(&profile_dst)
        .map_err(|error| format!("检查新配置目录失败：{error:#}"))?
    {
        return Err(format!("远端已存在 {profile_dst}，迁移已取消（不会覆盖）"));
    }
    let remote_src = io
        .remote_dir_exists(&src)
        .map_err(|error| format!("检查旧目录失败：{error:#}"))?;
    let remote_profile_src = io
        .remote_dir_exists(&profile_src)
        .map_err(|error| format!("检查旧配置目录失败：{error:#}"))?;
    let local_src = io.local_dir_exists(&old);
    let skipped = !local_src && !remote_src && !remote_profile_src;

    let mut steps = vec![step(
        "CHECK",
        true,
        if requested_old.is_empty() || requested_old == local_id {
            format!("{src} -> {dst}（device_id 标签：{label}）")
        } else {
            format!("旧值 {requested_old} 按当前 installation_id {local_id} 处理：{src} -> {dst}")
        },
    )];

    let target = io
        .inspect_remote_dir(&dst)
        .map_err(|error| format!("检查目标目录 {dst} 失败：{error:#}"))?;
    let conflict = if target.exists {
        Some(conflict_for(&dst, &target))
    } else {
        None
    };
    let mut merged_files = 0usize;

    if let Some(info) = conflict.as_ref() {
        match mode {
            MigrateMode::Cancel => {
                let detail = conflict_detail(info);
                steps.push(step("CONFLICT", true, detail.clone()));
                return Ok(MigrateView {
                    old_id: old,
                    new_id: new_id.to_string(),
                    copied_files: 0,
                    copied_dirs: 0,
                    removed: 0,
                    freed_bytes: 0,
                    steps,
                    remote_renamed: false,
                    skipped,
                    warnings: vec![detail],
                    mode: MigrateMode::Cancel.as_str().to_string(),
                    conflict: Some(info.clone()),
                    merged_files: 0,
                });
            }
            MigrateMode::Merge => {
                if remote_src {
                    merged_files = io.merge_missing_snapshots(&dst, &src).map_err(|error| {
                        format!("合并 {dst} -> {src} 失败（未做任何改动）：{error:#}")
                    })?;
                    steps.push(step(
                        "MERGE",
                        true,
                        format!(
                            "把 {dst} 里缺少的 {merged_files} 个 *.userdb.txt 补进 {src}（同名以源为准）"
                        ),
                    ));
                } else {
                    steps.push(step(
                        "MERGE",
                        true,
                        format!("远端无 {src}，保留目标目录 {dst}（不再搬运）"),
                    ));
                }
            }
            MigrateMode::Replace => {
                if !info.can_replace {
                    return Err(format!(
                        "不能删除 {} 后迁移：{}",
                        dst,
                        info.replace_blocked_reason
                            .clone()
                            .unwrap_or_else(|| "目标目录不是可删除的遗留目录".to_string())
                    ));
                }
                let report = io.delete_remote_dir(&dst).map_err(|error| {
                    format!("删除目标目录 {dst} 失败（未做任何改动）：{error:#}")
                })?;
                steps.push(step(
                    "REPLACE",
                    true,
                    format!(
                        "删除目标目录 {dst}（{} 项，释放 {} 字节）",
                        report.removed, report.freed_bytes
                    ),
                ));
            }
        }
    }

    io.set_installation_id(new_id)
        .map_err(|error| format!("写入 installation.yaml 失败，未做任何改动：{error:#}"))?;
    steps.push(step(
        "SET_ID",
        true,
        format!("installation_id: {old} -> {new_id}"),
    ));

    if local_src {
        if let Err(error) = io.rename_local_dir(&old, new_id) {
            let undo = io.set_installation_id(&old);
            return Err(format!(
                "重命名本地 sync/{old} 失败：{error:#}{}",
                undo_note(&format!("把 installation_id 改回 {old}"), undo)
            ));
        }
        steps.push(step(
            "LOCAL_RENAME",
            true,
            format!("sync/{old} -> sync/{new_id}"),
        ));
    } else {
        steps.push(step(
            "LOCAL_RENAME",
            true,
            format!("本地无 sync/{old}，跳过"),
        ));
    }

    let mut copied = CopyReport::default();
    if remote_src {
        match io.copy_remote_dir(&src, &dst) {
            Ok(report) => {
                copied = report;
                steps.push(step(
                    "COPY",
                    true,
                    format!(
                        "{src} -> {dst}（{} 个文件 / {} 个目录）",
                        report.files, report.dirs
                    ),
                ));
            }
            Err(error) => {
                let cleanup = io.delete_remote_dir(&dst);
                let local_undo = if local_src {
                    io.rename_local_dir(new_id, &old)
                } else {
                    Ok(())
                };
                let id_undo = io.set_installation_id(&old);
                return Err(format!(
                    "复制远端目录 {src} -> {dst} 失败：{error:#}{}{}{}",
                    undo_note(&format!("删除 {dst}"), cleanup),
                    undo_note(&format!("把本地目录改回 sync/{old}"), local_undo),
                    undo_note(&format!("把 installation_id 改回 {old}"), id_undo)
                ));
            }
        }
    } else {
        steps.push(step("COPY", true, format!("远端无 {src}，跳过")));
    }

    let mut copied_profiles = CopyReport::default();
    if remote_profile_src {
        match io.copy_remote_dir(&profile_src, &profile_dst) {
            Ok(report) => {
                copied_profiles = report;
                steps.push(step(
                    "PROFILE_COPY",
                    true,
                    format!(
                        "{profile_src} -> {profile_dst}（{} 个文件 / {} 个目录）",
                        report.files, report.dirs
                    ),
                ));
            }
            Err(error) => {
                let profile_cleanup = io.delete_remote_dir(&profile_dst);
                let sync_cleanup = if remote_src {
                    io.delete_remote_dir(&dst)
                } else {
                    Ok(DeleteReport::default())
                };
                let local_undo = if local_src {
                    io.rename_local_dir(new_id, &old)
                } else {
                    Ok(())
                };
                let id_undo = io.set_installation_id(&old);
                return Err(format!(
                    "复制配置目录 {profile_src} -> {profile_dst} 失败：{error:#}{}{}{}{}",
                    undo_note(&format!("删除 {profile_dst}"), profile_cleanup),
                    undo_note(&format!("删除 {dst}"), sync_cleanup),
                    undo_note(&format!("把本地目录改回 sync/{old}"), local_undo),
                    undo_note(&format!("把 installation_id 改回 {old}"), id_undo)
                ));
            }
        }
    }

    if let Err(error) = io.save_label(new_id) {
        let cleanup = if remote_src {
            io.delete_remote_dir(&dst)
        } else {
            Ok(DeleteReport::default())
        };
        let profile_cleanup = if remote_profile_src {
            io.delete_remote_dir(&profile_dst)
        } else {
            Ok(DeleteReport::default())
        };
        let local_undo = if local_src {
            io.rename_local_dir(new_id, &old)
        } else {
            Ok(())
        };
        let id_undo = io.set_installation_id(&old);
        return Err(format!(
            "保存配置失败：{error:#}{}{}{}{}",
            undo_note(&format!("删除 {dst}"), cleanup),
            undo_note(&format!("删除 {profile_dst}"), profile_cleanup),
            undo_note(&format!("把本地目录改回 sync/{old}"), local_undo),
            undo_note(&format!("把 installation_id 改回 {old}"), id_undo)
        ));
    }
    steps.push(step("CONFIG", true, format!("device_id（标签）= {new_id}")));

    let mut removed = DeleteReport::default();
    let mut warnings = Vec::new();
    if remote_src {
        match io.delete_remote_dir(&src) {
            Ok(report) => {
                removed = report;
                steps.push(step(
                    "DELETE",
                    true,
                    format!(
                        "删除 {src}（{} 项，释放 {} 字节）",
                        report.removed, report.freed_bytes
                    ),
                ));
            }
            Err(error) => {
                let detail = format!(
                    "已改名为 {new_id}，但旧远端目录 {src} 删除失败：{error:#}；请在设备页注销它或稍后重试"
                );
                steps.push(step("DELETE", false, detail.clone()));
                warnings.push(detail);
            }
        }
    }

    if remote_profile_src {
        match io.delete_remote_dir(&profile_src) {
            Ok(report) => {
                steps.push(step(
                    "PROFILE_DELETE",
                    true,
                    format!(
                        "删除 {profile_src}（{} 项，释放 {} 字节）",
                        report.removed, report.freed_bytes
                    ),
                ));
            }
            Err(error) => {
                let detail = format!(
                    "已改名为 {new_id}，但旧配置目录 {profile_src} 删除失败：{error:#}；可稍后重试"
                );
                steps.push(step("PROFILE_DELETE", false, detail.clone()));
                warnings.push(detail);
            }
        }
    }

    // 0.6.0 的 publish 曾用 device_id 建过远端目录（只含 owner.json）：迁移后它
    // 已不代表本机，但这里只提示、不删远端数据（用户可在设备页注销——is_self
    // 已不再按 device_id 判定，那条行不会再被当成「本机」）。
    let legacy = label.trim();
    if !legacy.is_empty() && legacy != old && legacy != new_id {
        let legacy_rel = format!("sync/{legacy}");
        if io.remote_dir_exists(&legacy_rel).unwrap_or(false) {
            let detail = format!(
                "{legacy_rel} 是旧版 device_id 目录（仅登记信息），可在设备页注销；不影响同步"
            );
            steps.push(step("LEGACY", true, detail.clone()));
            warnings.push(detail);
        }
    }

    Ok(MigrateView {
        old_id: old,
        new_id: new_id.to_string(),
        copied_files: copied.files + copied_profiles.files,
        copied_dirs: copied.dirs + copied_profiles.dirs,
        removed: removed.removed,
        freed_bytes: removed.freed_bytes,
        steps,
        remote_renamed: remote_src || remote_profile_src,
        skipped,
        warnings,
        mode: if conflict.is_some() {
            mode.as_str().to_string()
        } else {
            "direct".to_string()
        },
        conflict: None,
        merged_files,
    })
}

/// 真实 IO：installation.yaml、本地 sync 目录、WebDAV、config.json。
struct DavMigration<'a> {
    rime_dir: &'a Path,
    sync_dir: &'a Path,
    client: &'a DavClient,
    config: &'a Config,
    config_path: &'a Path,
}

impl MigrationIo for DavMigration<'_> {
    fn set_installation_id(&self, id: &str) -> anyhow::Result<()> {
        rime_dir::set_installation_id(self.rime_dir, id)
    }

    fn local_dir_exists(&self, name: &str) -> bool {
        self.sync_dir.join(name).is_dir()
    }

    fn rename_local_dir(&self, from: &str, to: &str) -> anyhow::Result<()> {
        let src = self.sync_dir.join(from);
        if !src.is_dir() {
            return Ok(());
        }
        let dst = self.sync_dir.join(to);
        fs::rename(&src, &dst)
            .with_context(|| format!("重命名 {} -> {} 失败", src.display(), dst.display()))
    }

    fn remote_dir_exists(&self, rel: &str) -> anyhow::Result<bool> {
        Ok(!self.client.propfind(rel, 0)?.is_empty())
    }

    fn inspect_remote_dir(&self, rel: &str) -> anyhow::Result<RemoteDirInfo> {
        if self.client.propfind(rel, 0)?.is_empty() {
            return Ok(RemoteDirInfo::default());
        }
        let entries = remote_tree::walk(
            self.client,
            rel,
            remote_tree::MAX_DEPTH,
            remote_tree::MAX_ENTRIES,
        )?;
        let snapshots = entries
            .iter()
            .filter(|entry| !entry.is_dir && entry.path.ends_with(".userdb.txt"))
            .count();
        let owner = self
            .client
            .get(&format!("{rel}/owner.json"))
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .map(|value| MigrateOwner::from_value(&value));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let in_use = owner.as_ref().is_some_and(|owner| {
            owner.device_id.as_deref() != Some(self.config.device_id.as_str())
                && is_online(owner.heartbeat_ts, now)
        });
        Ok(RemoteDirInfo {
            exists: true,
            snapshots,
            in_use,
            owner,
        })
    }

    fn merge_missing_snapshots(&self, from: &str, into: &str) -> anyhow::Result<usize> {
        self.client.ensure_dir(into)?;
        remote_tree::merge_missing_snapshots(self.client, from, into)
    }

    fn copy_remote_dir(&self, src: &str, dst: &str) -> anyhow::Result<CopyReport> {
        remote_tree::copy_tree(self.client, src, dst)
    }

    fn delete_remote_dir(&self, rel: &str) -> anyhow::Result<DeleteReport> {
        remote_tree::delete_tree(self.client, rel)
    }

    fn save_label(&self, new: &str) -> anyhow::Result<()> {
        let mut next = self.config.clone();
        next.device_id = new.to_string();
        next.save(self.config_path)
    }
}

/// 改设备名称：installation_id 与本地/远端 sync 目录一起改名，device_id 只是标签。
/// mode 决定目标目录已存在时的处理：cancel（默认，只回报冲突）/ merge / replace。
#[tauri::command]
pub fn cmd_migrate_device(
    app: AppHandle,
    state: State<'_, AppState>,
    old: String,
    new: String,
    mode: Option<String>,
) -> Result<MigrateView, String> {
    let mode = MigrateMode::parse(mode.as_deref().unwrap_or("cancel"))?;
    let (config, client) = dav_client(&app, &state.0)?;
    let rime = detect_rime(&config)?.path;
    let sync_dir = rime_dir::sync_dir(&rime);
    let local_id = rime_dir::local_sync_id(&rime, &config.device_id);
    let path = config_path(&app)?;
    let io = DavMigration {
        rime_dir: &rime,
        sync_dir: &sync_dir,
        client: &client,
        config: &config,
        config_path: &path,
    };

    let view = run_migration(&io, &old, &local_id, &config.device_id, &new, mode)?;
    let head = if let Some(conflict) = view.conflict.as_ref() {
        format!(
            "迁移预检发现 {} 已存在（{} 个快照），已暂停等待选择处理方式",
            conflict.target, conflict.target_snapshot_count
        )
    } else if view.skipped {
        format!(
            "设备身份已更新：{} -> {}（无需迁移目录）",
            view.old_id, view.new_id
        )
    } else if view.remote_renamed {
        format!(
            "设备身份已迁移：{} -> {}（{} 个文件，模式 {}）",
            view.old_id, view.new_id, view.copied_files, view.mode
        )
    } else {
        format!(
            "设备身份已迁移：{} -> {}（仅本地目录，远端无旧目录）",
            view.old_id, view.new_id
        )
    };
    push_log(&state.0, head);
    if view.merged_files > 0 {
        push_log(
            &state.0,
            format!("合并模式：从目标目录补齐 {} 个快照", view.merged_files),
        );
    }
    for warning in &view.warnings {
        push_log(&state.0, warning.clone());
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 记录调用顺序的假 WebDAV；可在指定步骤注入失败。
    struct RecordingDav {
        fail_at: Option<&'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl RecordingDav {
        fn new() -> Self {
            Self {
                fail_at: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn failing(step: &'static str) -> Self {
            Self {
                fail_at: Some(step),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn record(&self, call: String) -> anyhow::Result<()> {
            let fail = self.fail_at.is_some_and(|step| step == call.as_str());
            self.calls.borrow_mut().push(call);
            if fail {
                anyhow::bail!("mock network failure");
            }
            Ok(())
        }
    }

    impl IdentityDav for RecordingDav {
        fn ensure_base(&self) -> anyhow::Result<()> {
            self.record("ensure_base".to_string())
        }

        fn ensure_dir(&self, rel: &str) -> anyhow::Result<()> {
            self.record(format!("ensure_dir:{rel}"))
        }

        fn put(&self, rel: &str, _bytes: &[u8]) -> anyhow::Result<()> {
            self.record(format!("put:{rel}"))
        }
    }

    #[test]
    fn publish_identity_without_webdav_is_explicit_error() {
        let config = Config::default();
        let error = require_webdav(&config).expect_err("empty url must fail");
        assert!(error.contains("尚未配置 WebDAV"), "{error}");
    }

    #[test]
    fn publish_identity_calls_ensure_base_then_sync_then_device_then_owner() {
        let dav = RecordingDav::new();
        let steps = publish_identity_with(&dav, "device-new", br#"{"device_id":"device-new"}"#)
            .expect("publish");
        assert_eq!(
            steps,
            vec![
                "ensure_base".to_string(),
                "ensure_dir:sync".to_string(),
                "ensure_dir:sync/device-new".to_string(),
                "put:sync/device-new/owner.json".to_string(),
            ]
        );
        // 调用序列与返回的步骤完全一致（没有额外的读/搬运操作）。
        assert_eq!(*dav.calls.borrow(), steps);
    }

    #[test]
    fn publish_identity_stops_on_first_failure() {
        let dav = RecordingDav::failing("ensure_dir:sync/device-new");
        let error = publish_identity_with(&dav, "device-new", b"{}").expect_err("must fail");
        assert!(error.contains("sync/device-new"), "{error}");
        assert_eq!(
            *dav.calls.borrow(),
            vec![
                "ensure_base".to_string(),
                "ensure_dir:sync".to_string(),
                "ensure_dir:sync/device-new".to_string(),
            ]
        );
    }

    #[test]
    fn publish_identity_rejects_invalid_device_id() {
        let dav = RecordingDav::new();
        let error = publish_identity_with(&dav, "../escape", b"{}").expect_err("must fail");
        assert!(error.contains("设备 ID 无效"), "{error}");
        assert!(dav.calls.borrow().is_empty());
    }

    #[test]
    fn owner_bytes_matches_write_owner_fields() {
        let config = Config {
            device_id: "device-new".to_string(),
            ..Config::default()
        };
        let owner: serde_json::Value =
            serde_json::from_slice(&engine::owner_bytes(&config).unwrap()).expect("owner json");
        assert_eq!(owner["device_id"], "device-new");
        assert!(owner["hostname"].is_string());
        assert_eq!(owner["os"], std::env::consts::OS);
        assert!(owner["session"]
            .as_str()
            .unwrap_or("")
            .starts_with("device-new-"));
        assert!(owner["heartbeat_ts"].as_u64().unwrap_or(0) > 0);
    }

    fn temp_rime(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rime-sync-devcmds-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn local_remote_id_prefers_installation_id_over_device_label() {
        let dir = temp_rime("local-id");
        fs::write(
            dir.join("installation.yaml"),
            "installation_id: \"e6169df8-0000-4000-8000-000000000000\"\n",
        )
        .expect("write");
        let config = Config {
            device_id: "5095".to_string(),
            ..Config::default()
        };
        assert_eq!(
            local_remote_id(Some(&dir), &config),
            "e6169df8-0000-4000-8000-000000000000",
            "远端目录名必须来自 installation_id"
        );
        fs::remove_file(dir.join("installation.yaml")).expect("remove");
        assert_eq!(
            local_remote_id(Some(&dir), &config),
            "5095",
            "没有 installation.yaml 时才回退 device_id"
        );
        assert_eq!(local_remote_id(None, &config), "5095");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn publish_targets_installation_id_directory_with_label_in_owner() {
        let config = Config {
            device_id: "5095".to_string(),
            ..Config::default()
        };
        let owner = engine::owner_bytes(&config).expect("owner");
        let dav = RecordingDav::new();
        let steps = publish_identity_with(&dav, "e6169df8-0000-4000-8000-000000000000", &owner)
            .expect("publish");
        assert_eq!(
            steps.last().map(String::as_str),
            Some("put:sync/e6169df8-0000-4000-8000-000000000000/owner.json"),
            "远端目录名必须是 installation_id"
        );
        let parsed: serde_json::Value = serde_json::from_slice(&owner).expect("owner json");
        assert_eq!(parsed["device_id"], "5095", "device_id 只是人类可读标签");
    }

    fn owner_json(device: &str, heartbeat: u64) -> serde_json::Value {
        serde_json::json!({
            "device_id": device,
            "hostname": "host-a",
            "os": "linux",
            "session": format!("{device}-1"),
            "heartbeat_ts": heartbeat,
        })
    }

    #[test]
    fn is_self_matches_installation_id_not_device_label() {
        let rows = vec![
            RawDevice {
                id: "e6169df8-uuid".to_string(),
                owner: Some(owner_json("5095", 10)),
                has_sync_dir: true,
            },
            RawDevice {
                id: "5095".to_string(),
                owner: Some(owner_json("5095", 20)),
                has_sync_dir: true,
            },
        ];
        let self_owner = owner_json("5095", 0);
        let devices = build_devices(rows, "e6169df8-uuid", Some(&self_owner));
        assert_eq!(devices.len(), 2, "远端已有本机目录时不再补占位行");
        assert_eq!(devices[0].id, "e6169df8-uuid", "本机行排在最前");
        assert!(devices[0].is_self);
        assert!(devices[0].reachable);
        let legacy = devices
            .iter()
            .find(|device| device.id == "5095")
            .expect("legacy row");
        assert!(!legacy.is_self, "旧 device_id 目录不能再被认成本机");
    }

    #[test]
    fn placeholder_row_uses_installation_id() {
        let devices = build_devices(Vec::new(), "e6169df8-uuid", Some(&owner_json("5095", 0)));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "e6169df8-uuid");
        assert!(devices[0].is_self);
        assert!(!devices[0].reachable);
        assert_eq!(
            devices[0].hostname, "host-a",
            "占位行沿用本机 owner.json 字段"
        );
    }

    /// 记录迁移动作的假 IO；可在指定动作注入失败。
    struct RecordingMigration {
        local_dirs: RefCell<Vec<String>>,
        remote_dirs: RefCell<Vec<String>>,
        /// (目录, 快照文件名)
        snapshots: RefCell<Vec<(String, String)>>,
        /// (目录, owner.json 的 device_id)
        owners: RefCell<Vec<(String, String)>>,
        /// 仍被在线设备使用的目录（owner 心跳在窗口内）。
        in_use: RefCell<Vec<String>>,
        fail_at: Option<&'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl RecordingMigration {
        fn new(local: &[&str], remote: &[&str]) -> Self {
            Self {
                local_dirs: RefCell::new(local.iter().map(|dir| dir.to_string()).collect()),
                remote_dirs: RefCell::new(remote.iter().map(|dir| dir.to_string()).collect()),
                snapshots: RefCell::new(Vec::new()),
                owners: RefCell::new(Vec::new()),
                in_use: RefCell::new(Vec::new()),
                fail_at: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_snapshots(self, dir: &str, names: &[&str]) -> Self {
            for name in names {
                self.snapshots
                    .borrow_mut()
                    .push((dir.to_string(), (*name).to_string()));
            }
            self
        }

        fn with_owner(self, dir: &str, device_id: &str) -> Self {
            self.owners
                .borrow_mut()
                .push((dir.to_string(), device_id.to_string()));
            self
        }

        fn with_in_use(self, dir: &str) -> Self {
            self.in_use.borrow_mut().push(dir.to_string());
            self
        }

        fn snapshot_names(&self, dir: &str) -> Vec<String> {
            let mut names: Vec<String> = self
                .snapshots
                .borrow()
                .iter()
                .filter(|(owner, _)| owner == dir)
                .map(|(_, name)| name.clone())
                .collect();
            names.sort();
            names
        }

        fn failing(mut self, action: &'static str) -> Self {
            self.fail_at = Some(action);
            self
        }

        fn record(&self, call: String) -> anyhow::Result<()> {
            let fail = self.fail_at.is_some_and(|action| action == call.as_str());
            self.calls.borrow_mut().push(call);
            if fail {
                anyhow::bail!("mock migration failure");
            }
            Ok(())
        }
    }

    impl MigrationIo for RecordingMigration {
        fn set_installation_id(&self, id: &str) -> anyhow::Result<()> {
            self.record(format!("set_id:{id}"))
        }

        fn local_dir_exists(&self, name: &str) -> bool {
            self.local_dirs.borrow().iter().any(|dir| dir == name)
        }

        fn rename_local_dir(&self, from: &str, to: &str) -> anyhow::Result<()> {
            self.record(format!("rename_local:{from}->{to}"))?;
            let mut dirs = self.local_dirs.borrow_mut();
            if let Some(index) = dirs.iter().position(|dir| dir == from) {
                dirs.remove(index);
            }
            dirs.push(to.to_string());
            Ok(())
        }

        fn remote_dir_exists(&self, rel: &str) -> anyhow::Result<bool> {
            Ok(self.remote_dirs.borrow().iter().any(|dir| dir == rel))
        }

        fn inspect_remote_dir(&self, rel: &str) -> anyhow::Result<RemoteDirInfo> {
            let exists = self.remote_dirs.borrow().iter().any(|dir| dir == rel);
            let names = self.snapshot_names(rel);
            let owner =
                self.owners
                    .borrow()
                    .iter()
                    .find(|(dir, _)| dir == rel)
                    .map(|(_, device_id)| MigrateOwner {
                        device_id: Some(device_id.clone()),
                        hostname: Some("host-b".to_string()),
                        os: Some("linux".to_string()),
                        session: None,
                        heartbeat_ts: 1_700_000_000,
                    });
            Ok(RemoteDirInfo {
                exists,
                snapshots: names.len(),
                in_use: self.in_use.borrow().iter().any(|dir| dir == rel),
                owner,
            })
        }

        fn merge_missing_snapshots(&self, from: &str, into: &str) -> anyhow::Result<usize> {
            self.record(format!("merge:{from}->{into}"))?;
            let source = self.snapshot_names(from);
            let mut snapshots = self.snapshots.borrow_mut();
            let mut copied = 0usize;
            for name in source {
                let exists = snapshots
                    .iter()
                    .any(|(dir, existing)| dir == into && *existing == name);
                if !exists {
                    snapshots.push((into.to_string(), name));
                    copied += 1;
                }
            }
            Ok(copied)
        }

        fn copy_remote_dir(&self, src: &str, dst: &str) -> anyhow::Result<CopyReport> {
            self.record(format!("copy:{src}->{dst}"))?;
            self.remote_dirs.borrow_mut().push(dst.to_string());
            // 复制语义：dst 已有的同名文件被 src 覆盖，src 的其它文件补进 dst。
            let source = self.snapshot_names(src);
            let mut snapshots = self.snapshots.borrow_mut();
            for name in source {
                let exists = snapshots
                    .iter()
                    .any(|(dir, existing)| dir == dst && *existing == name);
                if !exists {
                    snapshots.push((dst.to_string(), name));
                }
            }
            Ok(CopyReport { files: 3, dirs: 1 })
        }

        fn delete_remote_dir(&self, rel: &str) -> anyhow::Result<DeleteReport> {
            self.record(format!("delete:{rel}"))?;
            self.remote_dirs.borrow_mut().retain(|dir| dir != rel);
            self.snapshots.borrow_mut().retain(|(dir, _)| dir != rel);
            Ok(DeleteReport {
                removed: 2,
                freed_bytes: 42,
            })
        }

        fn save_label(&self, new: &str) -> anyhow::Result<()> {
            self.record(format!("save:{new}"))
        }
    }

    fn migrate_steps(view: &MigrateView) -> Vec<&str> {
        view.steps.iter().map(|step| step.step.as_str()).collect()
    }

    #[test]
    fn migrate_renames_installation_id_local_and_remote_in_order() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id"]);
        // UI 传的是 device_id 标签（label-old），旧值按当前 installation_id 处理。
        let view = run_migration(
            &io,
            "label-old",
            "old-id",
            "label-old",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("migrate");
        assert_eq!(view.old_id, "old-id");
        assert_eq!(view.new_id, "new-id");
        assert_eq!(
            migrate_steps(&view),
            vec![
                "CHECK",
                "SET_ID",
                "LOCAL_RENAME",
                "COPY",
                "CONFIG",
                "DELETE"
            ]
        );
        assert!(view.remote_renamed, "远端旧目录存在时必须复制成功");
        assert!(!view.skipped);
        assert_eq!(
            *io.calls.borrow(),
            vec![
                "set_id:new-id".to_string(),
                "rename_local:old-id->new-id".to_string(),
                "copy:sync/old-id->sync/new-id".to_string(),
                "save:new-id".to_string(),
                "delete:sync/old-id".to_string(),
            ],
            "必须先改 installation_id、迁移目录，最后才删旧目录"
        );
        assert!(view.warnings.is_empty());
        assert_eq!(view.copied_files, 3);
        assert_eq!(view.removed, 2);
    }

    #[test]
    fn migrate_renames_profile_alongside_sync_directory() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/old-id/config"]);
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("migrate");
        assert_eq!(
            migrate_steps(&view),
            vec![
                "CHECK",
                "SET_ID",
                "LOCAL_RENAME",
                "COPY",
                "PROFILE_COPY",
                "CONFIG",
                "DELETE",
                "PROFILE_DELETE"
            ]
        );
        let calls = io.calls.borrow().clone();
        assert!(
            calls
                .iter()
                .any(|call| call == "copy:sync/old-id/config->sync/new-id/config"),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|call| call == "delete:sync/old-id/config"),
            "{calls:?}"
        );
        assert!(view.remote_renamed, "profile 也是远端数据");
        assert!(!io
            .remote_dirs
            .borrow()
            .contains(&"sync/old-id/config".to_string()));
        assert!(io
            .remote_dirs
            .borrow()
            .contains(&"sync/new-id/config".to_string()));
    }

    /// 注销必须同时覆盖 sync/<id> 与它的配置目录 sync/<id>/config。
    #[test]
    fn revoke_covers_device_dir_and_its_config() {
        let targets = revoke_delete_targets("dev-old", "self").expect("targets");
        assert_eq!(
            targets,
            vec![
                "sync/dev-old".to_string(),
                "sync/dev-old/config".to_string()
            ]
        );
        // 本机目录的配置目录被自删闸门挡住（重置身份另有入口）。
        let self_targets = revoke_delete_targets("self", "self").expect("self targets");
        assert_eq!(self_targets, vec!["sync/self".to_string()]);
        assert!(revoke_delete_targets("../evil", "self").is_err());
    }

    #[test]
    fn migrate_copy_failure_rolls_back_local_and_installation_id() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id"])
            .failing("copy:sync/old-id->sync/new-id");
        let error = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect_err("must fail");
        assert!(error.contains("已删除 sync/new-id"), "{error}");
        assert!(
            error.contains("已把 installation_id 改回 old-id"),
            "{error}"
        );
        assert_eq!(
            *io.calls.borrow(),
            vec![
                "set_id:new-id".to_string(),
                "rename_local:old-id->new-id".to_string(),
                "copy:sync/old-id->sync/new-id".to_string(),
                "delete:sync/new-id".to_string(),
                "rename_local:new-id->old-id".to_string(),
                "set_id:old-id".to_string(),
            ]
        );
        assert_eq!(*io.local_dirs.borrow(), vec!["old-id".to_string()]);
    }

    #[test]
    fn migrate_config_failure_rolls_back_everything() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id"]).failing("save:new-id");
        let error = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect_err("must fail");
        assert!(error.contains("保存配置失败"), "{error}");
        assert_eq!(
            *io.calls.borrow(),
            vec![
                "set_id:new-id".to_string(),
                "rename_local:old-id->new-id".to_string(),
                "copy:sync/old-id->sync/new-id".to_string(),
                "save:new-id".to_string(),
                "delete:sync/new-id".to_string(),
                "rename_local:new-id->old-id".to_string(),
                "set_id:old-id".to_string(),
            ],
            "config 写入失败时，远端旧目录还没删，可以完整回滚"
        );
        assert_eq!(*io.remote_dirs.borrow(), vec!["sync/old-id".to_string()]);
    }

    #[test]
    fn migrate_delete_failure_keeps_new_identity_and_warns() {
        let io =
            RecordingMigration::new(&["old-id"], &["sync/old-id"]).failing("delete:sync/old-id");
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("migration itself succeeds");
        assert_eq!(view.warnings.len(), 1);
        assert!(view.warnings[0].contains("删除失败"), "{:?}", view.warnings);
        assert_eq!(
            view.steps.last().map(|step| (step.step.as_str(), step.ok)),
            Some(("DELETE", false))
        );
    }

    #[test]
    fn migrate_existing_target_returns_conflict_without_touching_anything() {
        let remote_taken = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/new-id"])
            .with_snapshots("sync/new-id", &["a.userdb.txt", "c.userdb.txt"])
            .with_owner("sync/new-id", "new-id");
        let view = run_migration(
            &remote_taken,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("conflict is not an error");
        let conflict = view.conflict.as_ref().expect("conflict state");
        assert_eq!(conflict.target, "sync/new-id");
        assert_eq!(conflict.target_snapshot_count, 2);
        assert_eq!(
            conflict
                .target_owner
                .as_ref()
                .and_then(|owner| owner.device_id.as_deref()),
            Some("new-id"),
            "冲突提示要带目标 owner"
        );
        assert!(
            conflict.target_legacy,
            "目录名 = owner.device_id 时算遗留目录"
        );
        assert!(conflict.can_replace);
        assert_eq!(view.mode, "cancel");
        assert_eq!(migrate_steps(&view), vec!["CHECK", "CONFLICT"]);
        assert!(
            remote_taken.calls.borrow().is_empty(),
            "预检冲突不得改动任何东西"
        );
        assert_eq!(
            remote_taken.snapshot_names("sync/new-id"),
            vec!["a.userdb.txt".to_string(), "c.userdb.txt".to_string()]
        );

        let local_taken = RecordingMigration::new(&["old-id", "new-id"], &["sync/old-id"]);
        let error = run_migration(
            &local_taken,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect_err("must refuse to overwrite");
        assert!(error.contains("本地已存在"), "{error}");
        assert!(local_taken.calls.borrow().is_empty());

        let same = RecordingMigration::new(&["old-id"], &["sync/old-id"]);
        let error = run_migration(
            &same,
            "old-id",
            "old-id",
            "label",
            "old-id",
            MigrateMode::Cancel,
        )
        .expect_err("must refuse");
        assert!(error.contains("相同"), "{error}");
        assert!(same.calls.borrow().is_empty());
    }

    /// 0.6.0 的真实故障：UI 传的 old 是 device_id 标签（5095），远端目录其实是
    /// installation_id；迁移必须按 installation_id 找到旧目录，而不是报「不存在」。
    #[test]
    fn migrate_uses_installation_id_even_when_old_value_is_device_label() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id"]);
        let view = run_migration(&io, "5095", "old-id", "5095", "new-id", MigrateMode::Cancel)
            .expect("migrate");
        assert_eq!(view.old_id, "old-id");
        assert!(
            view.steps[0].detail.contains("按当前 installation_id"),
            "{:?}",
            view.steps[0]
        );
        assert_eq!(
            *io.calls.borrow(),
            vec![
                "set_id:new-id".to_string(),
                "rename_local:old-id->new-id".to_string(),
                "copy:sync/old-id->sync/new-id".to_string(),
                "save:new-id".to_string(),
                "delete:sync/old-id".to_string(),
            ],
            "不得再按 sync/5095 找旧目录"
        );
    }

    /// 远端旧目录不存在（已清理 / 从未上传）但本地有：只重命名本地，不报错。
    #[test]
    fn migrate_without_remote_source_renames_local_only() {
        let io = RecordingMigration::new(&["old-id"], &[]);
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("migrate");
        assert!(!view.skipped);
        assert!(
            !view.remote_renamed,
            "远端没有旧目录时标记 remote_renamed=false"
        );
        assert_eq!(
            migrate_steps(&view),
            vec!["CHECK", "SET_ID", "LOCAL_RENAME", "COPY", "CONFIG"]
        );
        assert_eq!(
            *io.calls.borrow(),
            vec![
                "set_id:new-id".to_string(),
                "rename_local:old-id->new-id".to_string(),
                "save:new-id".to_string(),
            ],
            "没有远端旧目录时不得复制/删除"
        );
    }

    /// 本地与远端都没有旧目录：视为无需迁移，只改 installation_id 与标签。
    /// 0.6.0 遗留的 sync/<device_id> 目录只提示、不删远端数据。
    #[test]
    fn migrate_reports_legacy_device_id_folder_without_deleting_it() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/5095"]);
        let view = run_migration(&io, "5095", "old-id", "5095", "new-id", MigrateMode::Cancel)
            .expect("migrate");
        assert_eq!(
            migrate_steps(&view),
            vec![
                "CHECK",
                "SET_ID",
                "LOCAL_RENAME",
                "COPY",
                "CONFIG",
                "DELETE",
                "LEGACY"
            ]
        );
        assert_eq!(view.warnings.len(), 1);
        assert!(
            view.warnings[0].contains("sync/5095"),
            "{:?}",
            view.warnings
        );
        assert!(
            io.remote_dirs.borrow().iter().any(|dir| dir == "sync/5095"),
            "不得自动删除遗留目录"
        );
    }

    #[test]
    fn migrate_without_any_source_is_skipped_not_an_error() {
        let io = RecordingMigration::new(&[], &[]);
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("migrate");
        assert!(view.skipped, "两端都没有旧目录时必须标记 skipped");
        assert!(!view.remote_renamed);
        assert_eq!(
            migrate_steps(&view),
            vec!["CHECK", "SET_ID", "LOCAL_RENAME", "COPY", "CONFIG"]
        );
        assert_eq!(
            *io.calls.borrow(),
            vec!["set_id:new-id".to_string(), "save:new-id".to_string()]
        );
    }

    /// 本机目录（installation_id）可以注销，但必须带上完全一致的 confirm。
    #[test]
    fn revoke_allows_self_only_with_matching_confirm() {
        assert!(
            revoke_target("5095", "e6169df8", None).is_ok(),
            "非本机目录不需要 confirm"
        );
        let error = revoke_target("e6169df8", "e6169df8", None).expect_err("self needs confirm");
        assert!(error.contains("确认值"), "{error}");
        let error =
            revoke_target("e6169df8", "e6169df8", Some("other")).expect_err("wrong confirm");
        assert!(error.contains("确认值"), "{error}");
        assert!(
            revoke_target("e6169df8", "e6169df8", Some(" e6169df8 ")).is_ok(),
            "confirm 允许首尾空格"
        );
        assert!(
            revoke_target("../escape", "e6169df8", Some("../escape")).is_err(),
            "路径穿越必须被拒绝"
        );
        assert!(revoke_target("", "e6169df8", Some("")).is_err());
    }

    /// merge：目标有 A、源有 B → 迁移后目标 = A ∪ B，且同名以源为准。
    #[test]
    fn migrate_merge_mode_keeps_target_and_source_snapshots() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/new-id"])
            .with_snapshots("sync/old-id", &["b.userdb.txt", "both.userdb.txt"])
            .with_snapshots("sync/new-id", &["a.userdb.txt", "both.userdb.txt"])
            .with_owner("sync/new-id", "new-id");
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Merge,
        )
        .expect("merge");
        assert_eq!(view.mode, "merge");
        assert!(view.conflict.is_none(), "merge 后不再返回冲突");
        assert_eq!(view.merged_files, 1, "只有目标独有的 a.userdb.txt 需要补齐");
        assert!(
            view.steps
                .iter()
                .any(|step| step.step == "MERGE" && step.ok),
            "{:?}",
            view.steps
        );
        assert_eq!(
            io.snapshot_names("sync/new-id"),
            vec![
                "a.userdb.txt".to_string(),
                "b.userdb.txt".to_string(),
                "both.userdb.txt".to_string()
            ],
            "结果必须是 A + B（同名只留一份）"
        );
        assert!(
            !io.remote_dirs
                .borrow()
                .iter()
                .any(|dir| dir == "sync/old-id"),
            "迁移完成后旧远端目录要删掉"
        );
        let calls = io.calls.borrow();
        let merge_at = calls
            .iter()
            .position(|call| call.starts_with("merge:"))
            .expect("merge call");
        let set_at = calls
            .iter()
            .position(|call| call.starts_with("set_id:"))
            .expect("set_id call");
        let copy_at = calls
            .iter()
            .position(|call| call.starts_with("copy:"))
            .expect("copy call");
        assert!(
            merge_at < set_at && merge_at < copy_at,
            "补齐快照必须在改身份与复制之前：{calls:?}"
        );
    }

    /// replace：目标存在且是遗留目录 → 先删目标再改名；目标的快照不保留。
    #[test]
    fn migrate_replace_mode_deletes_target_then_renames() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/new-id"])
            .with_snapshots("sync/old-id", &["b.userdb.txt"])
            .with_snapshots("sync/new-id", &["a.userdb.txt"])
            .with_owner("sync/new-id", "new-id");
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Replace,
        )
        .expect("replace");
        assert_eq!(view.mode, "replace");
        assert_eq!(view.merged_files, 0);
        let calls = io.calls.borrow().clone();
        let delete_at = calls
            .iter()
            .position(|call| call == "delete:sync/new-id")
            .expect("target delete");
        let set_at = calls
            .iter()
            .position(|call| call.starts_with("set_id:"))
            .expect("set_id");
        assert!(delete_at < set_at, "先删目标再改身份：{calls:?}");
        assert_eq!(
            io.snapshot_names("sync/new-id"),
            vec!["b.userdb.txt".to_string()],
            "删掉目标后只剩源快照"
        );
    }

    /// replace 的安全边界：目标仍被其他在线设备使用 → 拒绝，且不改动任何东西。
    #[test]
    fn migrate_replace_refuses_target_used_by_online_device() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/new-id"])
            .with_owner("sync/new-id", "new-id")
            .with_in_use("sync/new-id");
        let error = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Replace,
        )
        .expect_err("must refuse");
        assert!(error.contains("不能删除"), "{error}");
        assert!(io.calls.borrow().is_empty());
    }

    /// 目标目录名 ≠ owner.device_id（另一台设备的现代目录）时同样不允许 replace。
    #[test]
    fn migrate_replace_refuses_target_owned_by_another_device() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/new-id"])
            .with_owner("sync/new-id", "work-pc");
        let view = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Cancel,
        )
        .expect("conflict");
        let conflict = view.conflict.expect("conflict");
        assert!(!conflict.target_legacy);
        assert!(!conflict.can_replace);
        assert!(conflict
            .replace_blocked_reason
            .unwrap_or_default()
            .contains("另一台设备"));
        let error = run_migration(
            &io,
            "old-id",
            "old-id",
            "label",
            "new-id",
            MigrateMode::Replace,
        )
        .expect_err("must refuse");
        assert!(error.contains("另一台设备"), "{error}");
        assert!(io.calls.borrow().is_empty());
    }

    /// 设备页徽标：旧 device_id 目录与无 owner.json 的目录都算 legacy。
    #[test]
    fn legacy_marks_old_device_id_directories() {
        let rows = vec![
            RawDevice {
                id: "e6169df8-uuid".to_string(),
                owner: Some(owner_json("laptop", 10)),
                has_sync_dir: true,
            },
            RawDevice {
                id: "jdk".to_string(),
                owner: Some(owner_json("jdk", 10)),
                has_sync_dir: true,
            },
            RawDevice {
                id: "ghost".to_string(),
                owner: None,
                has_sync_dir: true,
            },
            RawDevice {
                id: "work-pc".to_string(),
                owner: Some(owner_json("work-pc-2", 10)),
                has_sync_dir: true,
            },
        ];
        let devices = build_devices(rows, "e6169df8-uuid", None);
        let find = |id: &str| devices.iter().find(|device| device.id == id).expect(id);
        assert!(find("e6169df8-uuid").is_self);
        assert!(!find("e6169df8-uuid").legacy);
        assert!(find("jdk").legacy, "目录名 = owner.device_id → 历史目录");
        assert!(find("ghost").legacy, "没有 owner.json → 历史目录候选");
        assert!(
            !find("work-pc").legacy,
            "目录名是 installation_id、device_id 只是标签 → 正常设备"
        );

        // 0.6.4 旧布局：只有 profiles/<id> 的设备也会出现在列表里，不能被误标成历史目录。
        let profile_only = build_devices(
            vec![RawDevice {
                id: "profile-only".to_string(),
                owner: None,
                has_sync_dir: false,
            }],
            "e6169df8-uuid",
            None,
        );
        let row = profile_only
            .iter()
            .find(|device| device.id == "profile-only")
            .expect("profile-only row");
        assert!(!row.is_self);
        assert!(!row.legacy, "只出现在 profiles/ 下的设备不是历史目录");
    }
}
