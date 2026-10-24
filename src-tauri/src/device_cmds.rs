//! 远端设备：读 \`sync/*/owner.json\`，注销时删除整个设备目录。

use std::fs;
use std::path::Path;

use anyhow::Context;
use rime_sync_core::config::{self, Config};
use rime_sync_core::engine;
use rime_sync_core::plan;
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
    pub is_self: bool,
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
            DeviceView {
                is_self: row.id == local_id,
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
            reachable: false,
        });
    }
    devices.sort_by(|left, right| {
        right
            .is_self
            .cmp(&left.is_self)
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
        rows.push(RawDevice { id, owner });
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

#[tauri::command]
pub fn cmd_revoke_device(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RevokeView, String> {
    let (config, client) = dav_client(&app, &state.0)?;
    let id = id.trim().to_string();
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id == "."
        || id == ".."
        || id.starts_with('.')
    {
        return Err("设备 ID 无效".to_string());
    }
    let rime = detect_rime(&config).ok().map(|detection| detection.path);
    if id == local_remote_id(rime.as_deref(), &config) {
        return Err("不能注销本机设备".to_string());
    }
    let rel = format!("sync/{id}");
    plan::validate_rel_path(&rel).map_err(|error| format!("设备路径无效：{error:#}"))?;
    // 服务端 DELETE 集合未必递归：逐文件/逐目录删除后再删目录。
    remote_tree::delete_tree(&client, &rel).map_err(|error| format!("注销设备失败：{error:#}"))?;
    push_log(&state.0, format!("已注销远端设备 {id}"));
    Ok(RevokeView { id, removed: true })
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
    let client = DavClient::new(
        &config.webdav.url,
        &config.webdav.username,
        &config.webdav.password,
        &config.webdav.remote_dir,
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
}

/// 迁移需要的外部效果：改 installation_id、重命名本地/远端 sync 目录、保存标签。
/// 抽成 trait 便于单测断言步骤顺序与回滚（远端复制/删除沿用 remote_tree）。
pub(crate) trait MigrationIo {
    fn set_installation_id(&self, id: &str) -> anyhow::Result<()>;
    fn local_dir_exists(&self, name: &str) -> bool;
    fn rename_local_dir(&self, from: &str, to: &str) -> anyhow::Result<()>;
    fn remote_dir_exists(&self, rel: &str) -> anyhow::Result<bool>;
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
/// 本地与远端都可能没有旧目录（已清理 / 从未上传）：此时不报错，只改
/// installation_id 与标签，并在结果里标记 skipped / remote_renamed。
/// 破坏性的删除放在最后：写 config 之前的任何失败都能把前三步完整回滚。
pub(crate) fn run_migration(
    io: &impl MigrationIo,
    requested_old: &str,
    local_id: &str,
    label: &str,
    new_id: &str,
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
    plan::validate_rel_path(&src).map_err(|error| format!("旧设备路径无效：{error:#}"))?;
    plan::validate_rel_path(&dst).map_err(|error| format!("新设备路径无效：{error:#}"))?;

    // 预检：任何改动之前先确认两端都不会被覆盖。
    if io.local_dir_exists(new_id) {
        return Err(format!("本地已存在 sync/{new_id}，迁移已取消（不会覆盖）"));
    }
    if io
        .remote_dir_exists(&dst)
        .map_err(|error| format!("检查目标目录失败：{error:#}"))?
    {
        return Err(format!("远端已存在 {dst}，迁移已取消（不会覆盖）"));
    }
    let remote_src = io
        .remote_dir_exists(&src)
        .map_err(|error| format!("检查旧目录失败：{error:#}"))?;
    let local_src = io.local_dir_exists(&old);

    let skipped = !local_src && !remote_src;
    let mut steps = vec![step(
        "CHECK",
        true,
        if requested_old.is_empty() || requested_old == local_id {
            format!("{src} -> {dst}（device_id 标签：{label}）")
        } else {
            format!("旧值 {requested_old} 按当前 installation_id {local_id} 处理：{src} -> {dst}")
        },
    )];

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

    if let Err(error) = io.save_label(new_id) {
        let cleanup = if remote_src {
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
            "保存配置失败：{error:#}{}{}{}",
            undo_note(&format!("删除 {dst}"), cleanup),
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
        copied_files: copied.files,
        copied_dirs: copied.dirs,
        removed: removed.removed,
        freed_bytes: removed.freed_bytes,
        steps,
        remote_renamed: remote_src,
        skipped,
        warnings,
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
#[tauri::command]
pub fn cmd_migrate_device(
    app: AppHandle,
    state: State<'_, AppState>,
    old: String,
    new: String,
) -> Result<MigrateView, String> {
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

    let view = run_migration(&io, &old, &local_id, &config.device_id, &new)?;
    let head = if view.skipped {
        format!(
            "设备身份已更新：{} -> {}（无需迁移目录）",
            view.old_id, view.new_id
        )
    } else if view.remote_renamed {
        format!(
            "设备身份已迁移：{} -> {}（{} 个文件）",
            view.old_id, view.new_id, view.copied_files
        )
    } else {
        format!(
            "设备身份已迁移：{} -> {}（仅本地目录，远端无旧目录）",
            view.old_id, view.new_id
        )
    };
    push_log(&state.0, head);
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
            },
            RawDevice {
                id: "5095".to_string(),
                owner: Some(owner_json("5095", 20)),
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
        fail_at: Option<&'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl RecordingMigration {
        fn new(local: &[&str], remote: &[&str]) -> Self {
            Self {
                local_dirs: RefCell::new(local.iter().map(|dir| dir.to_string()).collect()),
                remote_dirs: RefCell::new(remote.iter().map(|dir| dir.to_string()).collect()),
                fail_at: None,
                calls: RefCell::new(Vec::new()),
            }
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

        fn copy_remote_dir(&self, src: &str, dst: &str) -> anyhow::Result<CopyReport> {
            self.record(format!("copy:{src}->{dst}"))?;
            self.remote_dirs.borrow_mut().push(dst.to_string());
            Ok(CopyReport { files: 3, dirs: 1 })
        }

        fn delete_remote_dir(&self, rel: &str) -> anyhow::Result<DeleteReport> {
            self.record(format!("delete:{rel}"))?;
            self.remote_dirs.borrow_mut().retain(|dir| dir != rel);
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
        let view =
            run_migration(&io, "label-old", "old-id", "label-old", "new-id").expect("migrate");
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
    fn migrate_copy_failure_rolls_back_local_and_installation_id() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id"])
            .failing("copy:sync/old-id->sync/new-id");
        let error =
            run_migration(&io, "old-id", "old-id", "label", "new-id").expect_err("must fail");
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
        let error =
            run_migration(&io, "old-id", "old-id", "label", "new-id").expect_err("must fail");
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
        let view = run_migration(&io, "old-id", "old-id", "label", "new-id")
            .expect("migration itself succeeds");
        assert_eq!(view.warnings.len(), 1);
        assert!(view.warnings[0].contains("删除失败"), "{:?}", view.warnings);
        assert_eq!(
            view.steps.last().map(|step| (step.step.as_str(), step.ok)),
            Some(("DELETE", false))
        );
    }

    #[test]
    fn migrate_rejects_existing_targets_and_same_name() {
        let remote_taken = RecordingMigration::new(&["old-id"], &["sync/old-id", "sync/new-id"]);
        let error = run_migration(&remote_taken, "old-id", "old-id", "label", "new-id")
            .expect_err("must refuse to overwrite");
        assert!(error.contains("远端已存在"), "{error}");
        assert!(
            remote_taken.calls.borrow().is_empty(),
            "预检失败不得改动任何东西"
        );

        let local_taken = RecordingMigration::new(&["old-id", "new-id"], &["sync/old-id"]);
        let error = run_migration(&local_taken, "old-id", "old-id", "label", "new-id")
            .expect_err("must refuse to overwrite");
        assert!(error.contains("本地已存在"), "{error}");
        assert!(local_taken.calls.borrow().is_empty());

        let same = RecordingMigration::new(&["old-id"], &["sync/old-id"]);
        let error =
            run_migration(&same, "old-id", "old-id", "label", "old-id").expect_err("must refuse");
        assert!(error.contains("相同"), "{error}");
        assert!(same.calls.borrow().is_empty());
    }

    /// 0.6.0 的真实故障：UI 传的 old 是 device_id 标签（5095），远端目录其实是
    /// installation_id；迁移必须按 installation_id 找到旧目录，而不是报「不存在」。
    #[test]
    fn migrate_uses_installation_id_even_when_old_value_is_device_label() {
        let io = RecordingMigration::new(&["old-id"], &["sync/old-id"]);
        let view = run_migration(&io, "5095", "old-id", "5095", "new-id").expect("migrate");
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
        let view = run_migration(&io, "old-id", "old-id", "label", "new-id").expect("migrate");
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
        let view = run_migration(&io, "5095", "old-id", "5095", "new-id").expect("migrate");
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
        let view = run_migration(&io, "old-id", "old-id", "label", "new-id").expect("migrate");
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
}
