# SPEC-4 — v0.3 改进

## 适用范围
用户指令：继续 v0.3 改进。本轮覆盖：首同步保护、日志轮转、同步历史、配置 schema 迁移、词条级冲突统计、更新签名闭环、托盘常驻+开机自启、二进制瘦身。
**不做**（理由）：多 WebDAV 目标（架构改动大，需单独一轮）、Windows 实机验证（无环境）、fcitx5 apply 实机（缺依赖）。

## 文件所有权
| 代理 | 路径 |
|---|---|
| A core | `crates/**`、`crates/cli/**`、`docs/architecture.md` |
| B platform | `src-tauri/**`（含 tauri.conf.json、Cargo.toml、capabilities）、`docs/RELEASE-SIGNING.md`、`.gitignore` |
| 协调者 | SPEC-4.md、提交与推送 |

## A · core 需求

### A1 首同步保护（默认安全）
- 定义「首次接入」= 本地 `index.json` 不存在 **且** 远端 `sync/` 下存在其他设备目录或 `shared/` 非空
- 首次接入时：**只拉不推**，`Report.first_sync = true`、`Report.mode = <policy id>`（`preview` / `pull-only`）
- 保护是**粘性**的：policy ≠ `normal` 的每一轮 transport 都只读（不 ensure_base、不写 owner/shared、不上传），只有用户显式切 `normal` 才双向；只读轮次同样拉取 shared 文件与快照
- 配置新增 `first_sync_policy: "preview" | "pull-only" | "normal"`，默认 **preview**（等价 pull-only + 前端提示；用户显式切 normal 才双向）
- 严格保证：即使 pull-only 也不得写 shared、不得上传任何文件；临时 device 目录不得残留

### A2 日志轮转
- 单文件 5 MB，保留 3 份（`log.txt` → `log.1.txt` → `log.2.txt` → `log.3.txt`）
- 写入时按大小阈值触发，轮转失败不得中断同步

### A3 同步历史
- 每次 transport/apply 追加一条到 `history.jsonl`：`{ts, mode, ok, pulled, pushed, merged, conflicts, duration_ms, backup_id, error}`
- 上限 500 条（超出按行裁剪最旧）
- CLI：`history --limit N --json`

### A4 配置 schema 版本
- `config.json` 增加 `schema_version: 2`
- 迁移器链：v1（无字段/明文密码）→ v2（密码迁钥匙串、新增 first_sync_policy）
- 迁移必须幂等；无法迁移时保留原文件并返回明确错误，**不得静默丢配置**

### A5 词条级冲突统计（只统计，不自动合并）
- 对 `*.userdb.txt` 冲突：解析两侧行（tab 分隔），统计 `only_local` / `only_remote` / `both_changed` 条目数
- 写入 `Report.conflict_details[]` = `{path, only_local, only_remote, both_changed, sample: [..5 条]}`
- **禁止**自动合并 userdb 快照；统计失败时降级为 `null` 且不影响主流程

### A6 验收
1. `cargo test --workspace` ≥ 99 passed（不得下降）
2. fmt / clippy -D warnings 干净
3. 新增单测：首同步判定（4 种组合）、pull-only 不写远端、日志轮转触发、history 裁剪到 500、v1→v2 迁移幂等、userdb 统计 3 类计数
4. 用真实 OpenList 跑一次首次接入演练：远端先放一台"别的设备"的快照，本地无 index → 断言 `first_sync=true` 且**远端文件集合不变**（一个字节都没写）；测试后清理

## B · platform 需求

### B1 更新签名闭环
- 用 `cargo tauri signer generate`（或 tauri CLI）生成本地密钥对
- 将 **公钥** 写入 `tauri.conf.json` 的 `plugins.updater.pubkey`
- 私钥写入 `/home/ljj/rime-sync/.secrets/updater.key`（**必须 gitignore，绝不入库**）
- `docs/RELEASE-SIGNING.md` 说明：如何把私钥加入 GitHub Secrets（`TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`）、如何轮换、如何本地验证
- 校验 tauri.conf.json 仍是合法 JSON 且 pubkey 非占位符

### B2 托盘常驻 + 关窗隐藏
- Tauri v2 tray：菜单「打开主窗口 / 立即搬运 / 立即合并生效 / 查看冲突 / 退出」
- 关闭窗口 → 隐藏到托盘（配置项 `close_to_tray` 默认 true）；托盘「退出」才真正退出
- Linux 需 appindicator（已在依赖中），Windows 用原生托盘
- 托盘动作复用既有命令逻辑，不新增业务实现

### B3 开机自启
- 接入 `tauri-plugin-autostart`；命令 `cmd_get_autostart` / `cmd_set_autostart(enabled)`
- Linux 写 `~/.config/autostart`，Windows 写注册表

### B4 二进制瘦身
- `src-tauri/Cargo.toml` 增加 `[profile.release]`：`strip = true`、`lto = "thin"`、`codegen-units = 1`、`opt-level = "s"`（若与 tauri 冲突则去掉冲突项并说明）

### B5 验收
1. `cd src-tauri && cargo build` 成功（debug）
2. `cargo test --workspace` 仍 ≥99（不得影响 core）
3. Xvfb 冒烟：应用启动无 panic；关闭窗口后进程仍存活（托盘常驻证据）；托盘菜单可被枚举或日志可见
4. `cargo clippy -- -D warnings` 通过
5. 贴出 tauri.conf.json 中 pubkey 片段（**不要贴私钥**）与 `.secrets/` 的 gitignore 验证
6. **不执行 pnpm tauri build / 不产包**

## 通用约束
- 不 commit / 不 push（协调者统一提交）
- 生产路径禁止 unwrap/expect
- 真实 OpenList 凭据只在命令行使用，**不得写进任何文件**
