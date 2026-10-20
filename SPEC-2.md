# SPEC-2 — rime-sync 改进规格（P0–P3）

> 前提：既有实现已通过验收（cargo test 35 passed、E2E 幂等、红线零泄漏）。
> 本轮为增量改造，**不得破坏既有测试与红线**。

## 0. 解释性约定

用户要求「暂时不执行编译动作」，理解为：**不产出安装包、不触发 GitHub Actions**。
本地 `cargo check / cargo test / cargo clippy / pnpm build` 属于必要正确性验证，**必须执行**。

## 1. 文件所有权（严格，避免并行冲突）

| 代理 | 独占路径 |
|---|---|
| **S1 core** | `crates/core/**`、`crates/cli/**`、`docs/architecture.md` |
| **S2 engineering** | `LICENSE`、`README.md`、`CHANGELOG.md`、`.github/**`、`packaging/**`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` |
| **S3 ui**（后续） | `src-tauri/src/**`、`src/**`、`package.json`、`vite.config.ts`、`tsconfig.json` |
| 协调者 | `PLAN.md`、`SPEC-2.md` |

## 2. S1 · core 需求

### 2.1 备份与回滚（P0）
- `backup.rs`：`BackupManager`
  - `create(rime_dir, app_data_dir, reason) -> BackupId`：把 `<rime_dir>/sync/**/*.userdb.txt`、根下 `*.custom.yaml`、`*.dict.yaml`、`*.txt`、`user.yaml` 复制到 `<app_data>/backups/<YYYYmmdd-HHMMSS>/`，附 `manifest.json`（相对路径、sha256、size、creation reason、app version）
  - `list() -> Vec<BackupInfo>`、`restore(&BackupId)`（先校验 manifest 中 sha256，再覆盖写回）、`prune(keep: usize)`（默认保留 10 份）
  - `restore` 前必须再自动建一份 `pre-restore` 备份（防止回滚本身造成损失）
- `engine.rs`：`apply()` 在**任何写操作之前**调用 `BackupManager::create`；备份失败则中止 apply 并返回明确错误（宁可不合并）
- `Report` 增加 `backup_id: Option<String>`

### 2.2 密码进系统钥匙串（P0）
- `secret.rs`：`trait SecretStore { get/set/delete }`
  - `KeyringStore`（`keyring` crate，service = "rime-sync"，account = webdav username）
  - `FileStore` 兜底（0600），用于无 Secret Service 的 headless 环境
  - `detect()`：优先 Keyring，不可用则回落 FileStore，并在结果里标明所用后端
- `config.rs`：新增 `password_ref: PasswordRef`（`Keyring` / `File` / `None`），**不再持久化明文密码**；
  - 兼容迁移：读取旧配置里的 `password` 字段 → 写入钥匙串 → 从 JSON 删除该字段
- CLI 支持 `--dav-pass` 只在内存使用，不落盘

### 2.3 残留锁回收（P0）
- `lock.rs`：锁文件内容 `{pid, hostname, started_at, boot_id}`
  - `acquire()`：若锁存在且（pid 不存活 / 同 PID 但 started_at 不匹配 / 超过 6h）→ 判为 stale，记录日志后接管
  - RAII `LockGuard` 释放；`Drop` 失败不 panic
- `engine` 使用新锁；`status` 命令展示锁状态与持有者

### 2.4 YAML 段落级合并（P3）
- `merge3.rs` 增加 `merge_yaml_patch(base, local, remote) -> Result<MergeOutcome>`
  - 仅对 `*.custom.yaml`：解析 YAML，对 `patch:` 映射做 key 级三向合并
  - 同一 key 三方差分冲突 → 保留本地并生成 conflict 副本（沿用现有命名）
  - 解析失败或结构非预期 → 自动回落文件级合并（不得报错中断）
- 现有 10 个合并单测必须保持通过，并新增：key 级互不干扰合并、同 key 冲突、YAML 非法回落

### 2.5 事件驱动（P1）
- `watch.rs`：基于 `notify` crate
  - `Watcher::start(rime_dir, kinds, debounce=3s) -> (JoinHandle, Receiver<WatchEvent>)`
  - 忽略 `build/`、`*.userdb/`、`*.bin`、`sync/` 自身写入造成的回声（用写入期静音窗口避免自触发循环）
  - 只发事件，不直接调用 engine（避免与手动操作竞争）
- CLI 增加 `watch` 子命令（前台运行，收到事件跑 transport）

### 2.6 服务商预设与 AList 适配（P1）
- `presets.rs`：
  ```rust
  pub enum Preset { Alist, Jianguoyun, Nextcloud, Synology, Custom }
  impl Preset {
      pub fn default_path(&self) -> &'static str;   // /dav , /dav , /remote.php/dav/ , /webdav
      pub fn normalize(&self, raw: &str) -> String; // 补全 scheme / 去掉尾斜杠 / 补默认端口 5244(AList)
      pub fn detect(headers: &Headers) -> Option<Preset>;
  }
  ```
  - AList 识别依据：`Server` 头含 `alist`，或 `/dav` 路径 + `WWW-Authenticate: Basic`
- `webdav.rs` 增加 `check_write() -> Result<WriteProbe>`：
  - 依次 MKCOL 临时目录、PUT 1 字节对象、DELETE 清理
  - 失败时把 HTTP 状态映射为中文可执行建议（403 + AList → 提示开启 `Webdav Read`/`Webdav Manage` 及上传/删除权限）

### 2.7 CLI 新命令
`backup`、`backups`、`rollback <id>`、`watch`、`probe`（写权限探测）。
全部支持 `--json`。

## 3. S2 · 工程与发布需求

### 3.1 LICENSE
根目录 MIT LICENSE，版权人 `li2966068`，年份 2026。

### 3.2 CI 拆分
- `.github/workflows/ci.yml`（push / PR）：`cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test --workspace`、`pnpm tsc --noEmit`、**rclone E2E job**（起本地 WebDAV，跑 transport 两轮验幂等 + userdb 红线）
- `.github/workflows/release.yml`（tag `v*` 或手动）：linux 出 `deb` + `AppImage`，windows 出 `nsis` + **portable zip**
- 删除或改造现有 `build.yml`（避免三份重复）

### 3.3 打包配置
- `tauri.conf.json`：
  - windows `webviewInstallMode: embedBootstrapper`（国内离线可装）
  - linux targets = ["deb","appimage"]
  - `bundle.createUpdaterArtifacts: true` + updater 插件 endpoints 占位
- `packaging/`：
  - `portable-zip.ps1`（Windows 便携版打包脚本）
  - `fcitx5-addon/`：C++ addon 源码 + CMakeLists + README
    - addon 提供一个配置页：WebDAV 地址/用户名/密码/远端目录 + 「打开 Rime 同步」按钮
    - 数据落在 `~/.config/rime-sync/config.json`（与主程序共享）
    - **不要求本轮编译通过**，但源码与构建说明必须完整
  - `nsis-hooks.nsh`：开始菜单 `Rime 同步` 快捷方式，并归入 Weasel 程序组

### 3.4 文档
- `README.md`：项目定位、截图占位、Linux/Windows 安装、AList 配置（含权限勾选表）、两种模式说明、开发构建
- `CHANGELOG.md`：v0.2.0 记录本轮 14 项
- 更新 `docs/USER-GUIDE.md`：新增 AList 章节与权限排查

### 3.5 约束
- 不执行 `pnpm tauri build`、不产出安装包、不触发 Actions
- YAML 必须能 `yaml.safe_load`；shell 脚本必须 `bash -n` 通过

## 4. S3 · UI 需求（第三轮）
冲突页（并排 diff + 一键取舍）、设备管理页（列 owner.json + 注销）、首次运行向导（4 步）、备份/回滚页、服务商预设下拉 + AList 权限提示、诊断包导出、中英文切换、watcher 开关与状态显示。
