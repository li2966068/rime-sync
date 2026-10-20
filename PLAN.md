# rime-sync — 跨平台 Rime 词库/配置 WebDAV 同步工具

## 1. 目标与边界

目标：Windows(Weasel 小狼毫) + Linux(fcitx5-rime / ibus-rime) 用 **同一个** Tauri 应用，
带 GUI 让用户手动配置**自己的** WebDAV（URL / 账号 / 密码 / 远端目录），双向同步：
- 用户词库（`*.userdb` 的快照 `*.userdb.txt`）
- 配置与词典源（`*.custom.yaml` / `*.schema.yaml` / `*.dict.yaml` / `*.txt` 词表）

非目标：不做服务端、不做账号体系、不同步 `*.userdb/` 目录本体（LevelDB，二进制）。

## 2. 硬约束（写进代码与测试，不得违反）

1. `*.userdb/` 任何文件**禁止**上传/下载/复制。
2. 远端 `shared/` 下**禁止**出现 `*.userdb.txt`（librime 会把 sync 目录下每个子目录当成一台设备去合并）。
3. `installation.yaml` **不同步**（每台设备的 id/路径私有）。
4. 同步顺序永远是 pull → merge → push，禁止只 push。
5. `installation_id` 每台设备唯一；远端 `owner.json` 做原子抢占与 session 校验。
6. 配置文件权限 0600（含 WebDAV 密码）。

## 3. 两种模式（关键设计）

| 模式 | 做什么 | 触发 | 是否打断输入 |
|---|---|---|---|
| **transport** | 只搬文件：pull 远端快照/shared，push 本地变化 | 定时器（默认 10 分钟）/ 手动 | 否 |
| **apply** | 触发 librime 真正合并 + 部署生效 | **仅手动**（用户点按钮） | 是（Linux 需临时停 fcitx5） |

原因：Linux 端 fcitx5-rime 常驻时持有 userdb 的 LevelDB 锁，直接跑 `rime_dict_manager` 会失败。
所以 apply 流程（Linux）= 停 fcitx5 → `rime_dict_manager --sync` → `rime_deployer --build` → 启 fcitx5。
Windows 端无锁冲突，用 Weasel 的 IPC/Deployer 命令。

## 4. 工作区结构

```
rime-sync/
├─ Cargo.toml                 # [workspace] members = crates/core, crates/cli, src-tauri
├─ crates/core/               # rime-sync-core（纯逻辑，可单测，不依赖 Tauri）
│   └─ src/{lib,config,rime_dir,webdav,plan,merge3,index,platform,engine}.rs
├─ crates/cli/                # rime-sync-cli（headless，用于 E2E 验收）
├─ src-tauri/                 # Tauri v2 薄壳 + 命令
├─ src/                       # Vite + TypeScript 前端（无框架）
├─ packaging/                 # 打包说明/资源
├─ scripts/dev-dav.sh         # 本地 WebDAV 测试服务（rclone serve webdav）
├─ docs/platform-notes.md     # 平台触发机制证据
└─ .github/workflows/build.yml
```

## 5. 远端布局

```
<remote_dir>/
├─ sync/<device_id>/owner.json          # {device_id, hostname, os, session, heartbeat_ts}
├─ sync/<device_id>/<dict>.userdb.txt   # Rime 原生快照，靠 librime 合并
└─ shared/manifest.json                 # {path -> {sha256, size, mtime, etag}}
    shared/files/<相对 rime 根目录的路径>
```

本地映射：
- 远端 `sync/<id>/` → `<rime_dir>/sync/<id>/`（Rime 原生 sync 目录，`--sync` 自动扫描）
- 远端 `shared/files/` → 三向合并后写入 `<rime_dir>/<相对路径>`
- 本机私有状态 → `<app_config_dir>/`（config.json / index.json / sync.lock / log）

## 6. 配置 schema（config.json）

```json
{
  "webdav": { "url": "", "username": "", "password": "", "remote_dir": "rime-sync" },
  "rime_dir": null,
  "device_id": "",
  "transport_interval_minutes": 10,
  "sync_kinds": { "userdict": true, "custom_yaml": true, "dict_sources": true },
  "exclude_globs": [],
  "stop_fcitx5_during_apply": true
}
```
`device_id` 为空时生成 `<hostname>-<6位随机>` 并写回。

## 7. 平台适配（trait）

```rust
pub trait RimeTrigger {
    fn sync_userdb(&self, rime_dir: &Path) -> Result<()>;
    fn deploy(&self, rime_dir: &Path) -> Result<()>;
}
```
- **Linux**：`rime_dict_manager --sync` + `rime_deployer --build <rime_dir> <shared_data_dir>`（来自 `librime-bin`）；
  apply 前可选停止 fcitx5（`fcitx5-remote -e` 或 `pkill -f fcitx5`），结束后 `fcitx5 -d` 重启。
- **Windows**：Weasel 的 `WeaselDeployer.exe /sync`（或 IPC `/sync`）+ `/deploy`；
  取不到时降级为「仅 transport」并在 UI 明确告警。

## 8. Tauri 命令契约（前端只依赖这些）

```
cmd_get_config()                 -> Config
cmd_set_config(config)           -> Config
cmd_detect_rime_dir()            -> { path, source }   # 自动探测 + 环境变量覆盖
cmd_test_connection()            -> { ok, latency_ms, server, dav_class }
cmd_list_remote()                -> { devices: [...], shared: [...] }
cmd_transport()                  -> Report              # 只搬文件
cmd_apply()                      -> Report              # 合并 + 部署 + push
cmd_cancel()
cmd_get_log(limit)               -> string[]
```
事件：`sync://progress` `{phase, current, total, message}`；`sync://done` `Report`。

`Report` 字段：`{ pulled, pushed, merged, conflicts: [...], skipped: [...], duration_ms, ok, error }`。

## 9. 文件分类规则

| 类别 | 匹配 | 动作 |
|---|---|---|
| 用户词库快照 | `sync/<id>/*.userdb.txt` | 走 sync 树，不解析 |
| 自定义配置 | `*.custom.yaml`, `default.custom.yaml`, `user.yaml` | shared + 三向合并 |
| 词典源 | `*.dict.yaml`, `*.txt`（词表） | shared + 三向合并 |
| 引擎产物 | `build/`, `*.bin`, `*.prism.bin`, `*.table.bin`, `*.reverse.bin`, `*.gram` | **排除** |
| LevelDB | `*.userdb/`, `*.userdb` | **排除（硬红线）** |
| 私有 | `installation.yaml`, `sync/` 根、`.rime-sync/` | **排除** |

三向合并：基线 `index.json` 记 `{path: {sha256, etag, mtime}}`。
- 只有一边变 → 取变的一边
- 两边都变且内容不同 → 保留本地，另存 `<name>.conflict-<device>-<ts>`，写入 `conflicts` 上报
- 远端删除 + 本地未改 → 删除；远端删除 + 本地改过 → 保留本地并告警

## 10. 代码规范

- Rust 2021，`anyhow` 错误链，禁止 `unwrap()` 于生产路径（测试可）
- 所有网络 IO 加超时（默认 30s）与重试（3 次，指数退避）
- 日志走 `tracing`，写文件 + 内存环形缓冲（供 UI 读取）
- 不引入巨型依赖；WebDAV 客户端自己用 `reqwest` 实现 PROPFIND/GET/PUT/DELETE/MKCOL，不引 webdav crate（避免维护死包）
- 单测覆盖：分类规则、红线断言、三向合并 5 种组合、路径穿越防护
