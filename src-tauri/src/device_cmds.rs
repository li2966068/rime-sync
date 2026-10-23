//! 远端设备：读 \`sync/*/owner.json\`，注销时删除整个设备目录。

use rime_sync_core::config::{self, Config};
use rime_sync_core::plan;
use rime_sync_core::webdav::DavClient;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::remote_tree;
use crate::{config_path, dav_client, ensure_config, push_log, AppState};

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

pub(crate) fn collect_devices(
    config: &Config,
    client: &DavClient,
) -> Result<Vec<DeviceView>, String> {
    let mut devices = Vec::new();
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
        let mut hostname = "—".to_string();
        let mut os = "—".to_string();
        let mut session = String::new();
        let mut heartbeat_ts = 0u64;
        let mut reachable = false;
        if let Some(owner) = owner.as_ref() {
            reachable = true;
            hostname = text_field(owner, "hostname").unwrap_or_else(|| "—".to_string());
            os = text_field(owner, "os").unwrap_or_else(|| "—".to_string());
            session = text_field(owner, "session").unwrap_or_default();
            heartbeat_ts = owner
                .get("heartbeat_ts")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
        }
        devices.push(DeviceView {
            is_self: id == config.device_id,
            id,
            hostname,
            os,
            session,
            heartbeat_ts,
            reachable,
        });
    }
    devices.sort_by(|left, right| {
        right
            .is_self
            .cmp(&left.is_self)
            .then(right.heartbeat_ts.cmp(&left.heartbeat_ts))
            .then(left.id.cmp(&right.id))
    });
    Ok(devices)
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
    if id == config.device_id {
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
    if config.webdav.url.trim().is_empty() || config.device_id == id {
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

#[derive(Serialize)]
pub struct MigrateStep {
    pub step: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Serialize)]
pub struct MigrateView {
    pub old_id: String,
    pub new_id: String,
    pub copied_files: usize,
    pub copied_dirs: usize,
    pub removed: usize,
    pub freed_bytes: u64,
    pub steps: Vec<MigrateStep>,
}

/// 逐目录 / 逐文件把 sync/<old> 复制成 sync/<new>，全部成功后才删除旧目录；
/// 复制阶段失败会删除已复制的新目录（回滚），旧目录始终保留。
#[tauri::command]
pub fn cmd_migrate_device(
    app: AppHandle,
    state: State<'_, AppState>,
    old: String,
    new: String,
) -> Result<MigrateView, String> {
    let (mut config, client) = dav_client(&app, &state.0)?;
    let old = old.trim().to_string();
    let new = new.trim().to_string();
    config::validate_device_id(&new).map_err(|error| format!("新设备 ID 无效：{error:#}"))?;
    if old != config.device_id {
        return Err(format!(
            "旧设备 ID 与当前配置不一致（当前 {}）",
            config.device_id
        ));
    }
    // 旧 ID 来自本机配置，但删除是破坏性操作：仍然要求它是单个安全路径段。
    if old.is_empty()
        || old.contains('/')
        || old.contains('\\')
        || old == "."
        || old == ".."
        || old.starts_with('.')
    {
        return Err("旧设备 ID 不是有效的目录名".to_string());
    }
    if old == new {
        return Err("新旧设备 ID 相同".to_string());
    }
    let src = format!("sync/{old}");
    let dst = format!("sync/{new}");
    plan::validate_rel_path(&src).map_err(|error| format!("旧设备路径无效：{error:#}"))?;
    plan::validate_rel_path(&dst).map_err(|error| format!("新设备路径无效：{error:#}"))?;

    let source = client
        .propfind(&src, 0)
        .map_err(|error| format!("检查旧设备目录失败：{error:#}"))?;
    if source.is_empty() {
        return Err(format!("远端设备目录 {src} 不存在"));
    }
    let target = client
        .propfind(&dst, 0)
        .map_err(|error| format!("检查目标设备目录失败：{error:#}"))?;
    if !target.is_empty() {
        return Err(format!("远端已存在 {dst}，迁移已取消（不会覆盖）"));
    }

    let mut steps = vec![MigrateStep {
        step: "CHECK".to_string(),
        ok: true,
        detail: format!("{src} -> {dst}"),
    }];

    let copy = match remote_tree::copy_tree(&client, &src, &dst) {
        Ok(report) => report,
        Err(error) => {
            let rollback = remote_tree::delete_tree(&client, &dst);
            let detail = match rollback {
                Ok(_) => format!("复制失败：{error:#}；已回滚 {dst}"),
                Err(rollback_error) => {
                    format!("复制失败：{error:#}；回滚 {dst} 也失败：{rollback_error:#}")
                }
            };
            push_log(&state.0, format!("设备迁移失败：{detail}"));
            return Err(format!("迁移失败，旧目录 {src} 已保留：{detail}"));
        }
    };
    steps.push(MigrateStep {
        step: "COPY".to_string(),
        ok: true,
        detail: format!("{} 个文件 / {} 个目录", copy.files, copy.dirs),
    });

    let removed = match remote_tree::delete_tree(&client, &src) {
        Ok(report) => report,
        Err(error) => {
            push_log(
                &state.0,
                format!("设备迁移：新目录 {dst} 已复制，但删除旧目录 {src} 失败：{error:#}"),
            );
            return Err(format!(
                "新目录 {dst} 已复制完成，但旧目录 {src} 删除失败：{error:#}。本机设备 ID 未修改，请手动清理旧目录后重试。"
            ));
        }
    };
    steps.push(MigrateStep {
        step: "DELETE".to_string(),
        ok: true,
        detail: format!(
            "删除 {src}（{} 项，释放 {} 字节）",
            removed.removed, removed.freed_bytes
        ),
    });

    config.device_id = new.clone();
    if let Err(error) = config.save(&config_path(&app)?) {
        return Err(format!(
            "远端目录已迁移为 {dst}，但写入本机配置失败：{error:#}。请手动把设备 ID 改为 {new}。"
        ));
    }
    steps.push(MigrateStep {
        step: "CONFIG".to_string(),
        ok: true,
        detail: format!("device_id = {new}"),
    });
    push_log(
        &state.0,
        format!("设备 ID 已迁移：{old} -> {new}（{} 个文件）", copy.files),
    );
    Ok(MigrateView {
        old_id: old,
        new_id: new,
        copied_files: copy.files,
        copied_dirs: copy.dirs,
        removed: removed.removed,
        freed_bytes: removed.freed_bytes,
        steps,
    })
}
