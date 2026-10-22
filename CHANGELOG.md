# 更新日志

本项目遵循语义化版本；日期为发布日（UTC+8）。

## v0.5.1 — 2026-10-22

修复 0.5.0 安装后「找不到同步设置」的两个打包缺陷：

1. **可执行位丢失**：离线安装器重打包同步 deb 时用 `chmod 0644` 抹平了所有文件，
   `/usr/bin/rime-sync-app` 变成不可执行，菜单项点了没反应。现在重打包会恢复
   `/usr/bin/*` 与 `/usr/lib/rime-sync/*` 为 0755。
2. **fcitx5 配置页未随包**：`Rime Sync` 的 addon（`rimesync.so` / `rimesync.conf` /
   `rime-sync-open-setup`）之前只为验证单独构建，没有进 IME deb。现在四个发行版的 IME
   包都会从源码编译并安装这个 addon，输入法设置里可直接编辑 WebDAV 参数。
## v0.5.0 — 2026-10-22

输入法不再是「打个包带上游 .deb」，而是**源码随仓库、每次一起编译**：

1. **源码 vendoring**：`third_party/librime`（1.17.0）、`third_party/fcitx5-rime`（5.0.16）、`third_party/weasel`（0.17.4）、
   `third_party/rime-data`（prelude/essay/luna-pinyin）随仓库走，每个目录有 FORK.md 记录上游 pin 与修改。
2. **四发行版离线安装器**：`rime-sync-installer_<ver>~{jammy,noble,bookworm,trixie}.run`，内含自编译 IME deb +
   同步 deb + 该发行版的依赖闭包；`--network none` 下四套全部实测通过（Loaded addon rime / deploy success / 候选「你好」）。
3. **安装顺序严格为输入法 → 同步程序**，安装器失败即停并保留日志；支持 `--uninstall`（先同步后输入法，保留用户数据）。
4. **fcitx5 配置页**：输入法设置里新增「Rime Sync」页，可直接编辑 WebDAV 地址/用户名/密码/远端目录/设备名，
   并有「打开 Rime 同步」按钮（ExternalOption）。
5. **Windows 自编译小狼毫**：CI 用 `third_party/weasel` 源码构建安装包，NSIS 在 PREINSTALL 阶段先装输入法
   （写 `install-ime-order.log` 作为顺序证据），再安装同步程序；失败可回退上游安装包。
6. **方案数据随包**：rime-prelude / rime-essay / luna_pinyin 打进 `/usr/share/rime-data`，装完即可输入。
## v0.4.0 — 2026-10-21

本轮把「输入法与同步」合成一个安装包，并去掉安装后自动弹窗：

1. **Linux 一体包（离线可装）**：deb 内置 fcitx5-rime 完整依赖闭包（82 个 .deb / 28 MB，来自 ubuntu 22.04），
   断网 `dpkg -i` 后台自动补齐输入法；输入法三件套放 `Recommends`，安装不阻塞（避免 dpkg 配置阶段的依赖死锁）。
2. **Windows 一体包**：NSIS 内置小狼毫（Weasel 0.17.4）安装包，安装时静默部署，随后创建「Rime 同步」与
   「同步设置」快捷方式；不再在安装完成后自动启动任何窗口。
3. **入口调整**：同步设置不再自动弹出，改为菜单入口 `rime-sync-app --setup`（Windows 同时放入 Weasel 程序组），
   带参数启动会直接进入同步设置向导，不带参数按普通启动。
4. **同步设置向导**：本机 Rime 目录/设备名、WebDAV 连接与测试（含写权限提示）、首次同步策略（默认 preview 只拉不推）、
   可选立即做一次首次同步并展示结果。
5. **Windows 黑框修复**：所有子进程（powershell 计量探测、WeaselDeployer 等）统一以 `CREATE_NO_WINDOW` 启动，
   后台静默运行，不再闪命令行窗口。
6. **发布流程**：CI 在构建前抓取 vendor 依赖与 Weasel 安装包，deb 产物做体积断言（< 20 MiB 直接失败），
   并新增 packaging 脚本守卫 job。
## v0.3.0 — 2026-10-21

本轮聚焦「首次接入安全 + 可运维性 + 桌面常驻」：

1. **首同步保护（默认安全，粘性）**：本地无 `index.json` 且远端已有别的设备或 `shared/` 数据时判定首次接入；
   `first_sync_policy` 默认 `preview`，只要不是 `normal`，每一轮 transport 都只拉不推——不建远端目录、
   不写 `owner.json`、不碰 `shared/`、不上传任何文件，同时照常拉取设备快照与 shared 文件。
   只有用户在界面点「切换为双向同步」才写入 `normal`。
2. **同步历史**：每次 transport/apply 追加 `history.jsonl`（ts/mode/ok/pulled/pushed/merged/conflicts/duration_ms/backup_id/error），
   上限 500 条；新增 `history --limit N --json`。
3. **日志轮转**：`log.txt` 单文件 5 MB，保留 3 份；轮转失败不影响同步。
4. **配置 schema v2**：新增 `schema_version` 与 `first_sync_policy`；v1（无版本号/明文密码）自动迁移到钥匙串，迁移幂等，
   遇到更高版本保留原文件并报错，不静默丢配置。
5. **词条级冲突统计**：对 `*.userdb.txt` 冲突解析两侧行，上报 `only_local` / `only_remote` / `both_changed` 与样本，
   只统计不自动合并；统计失败降级为 null 且不阻断同步。
6. **更新签名闭环**：`tauri.conf.json` 写入 minisign 公钥、updater 指向 GitHub Releases `latest.json`；
   私钥存 `.secrets/`（已 gitignore），`docs/RELEASE-SIGNING.md` 说明 GitHub Secrets 配置与轮换。
7. **托盘常驻 + 开机自启**：托盘菜单「打开主窗口 / 立即搬运 / 立即合并生效 / 查看冲突 / 退出」；关窗默认隐藏到托盘；
   接入 autostart 插件（Linux 写 `~/.config/autostart`，Windows 写注册表）。
8. **发布瘦身**：release profile 打开 `strip` / `lto=thin` / `codegen-units=1` / `opt-level=s`。
9. **测试**：119 个测试通过（109 单元 + first_sync + 5 read_only + spec2_flow），fmt/clippy 干净；
   前端补 zh/en 文案与只读提示。
## v0.2.0 — 2026-10-20

本轮的 14 项改进：

1. **备份与回滚**：新增 `BackupManager`——apply 前自动把 `sync/**/*.userdb.txt`、`*.custom.yaml`、
   `*.dict.yaml`、词表 `*.txt`、`user.yaml` 复制到 `<app_data>/backups/<时间戳>/` 并写入
   `manifest.json`（相对路径、sha256、大小、原因、版本）；备份失败则中止 apply，宁可不合并。
   支持 `list` / `restore` / `prune`（默认保留 10 份），restore 前自动再建一份 `pre-restore` 备份。

2. **密码进系统钥匙串**：新增 `SecretStore`（`KeyringStore` + 0600 `FileStore` 兜底）与
   `password_ref` 字段，配置里不再持久化明文密码；旧配置的 `password` 字段会自动迁移进钥匙串并从
   JSON 删除；`--dav-pass` 只在内存使用。

3. **残留锁回收**：锁文件记录 `{pid, hostname, started_at, boot_id}`；pid 不存活、boot_id/启动时间不匹配
   或超过 6 小时即判定为 stale 并接管，RAII `LockGuard` 保证释放，`status` 展示锁持有者。

4. **YAML 段落级合并**：`merge_yaml_patch` 对 `*.custom.yaml` 的 `patch:` 映射做 key 级三向合并，
   不同 key 互不干扰；同 key 三方差分冲突保留本地并生成 conflict 副本；YAML 非法或结构异常时
   自动回落文件级合并，不中断同步。

5. **事件驱动同步**：新增 `watch`（notify + 3s 防抖），忽略 `build/`、`*.userdb/`、`*.bin` 与自身写入
   造成的回声；只发事件不直接调 engine，避免与手动操作抢锁。

6. **服务商预设与 AList 适配**：`Preset`（AList / 坚果云 / Nextcloud / 群晖 / 自定义）提供默认路径、
   URL 归一化（补 scheme、去尾斜杠、AList 补 5244 端口与 `/dav`）和按响应头识别 AList。

7. **WebDAV 写权限探测**：`check_write()` 依次 MKCOL → PUT 1 字节 → DELETE，失败时把 HTTP 状态码
   翻译成中文可执行建议（403 + AList → 提示勾选 Webdav Read / Webdav Manage 与存储写入/删除权限）。

8. **CLI 新命令**：`backup`、`backups`、`rollback <id>`、`watch`、`probe`，全部支持 `--json`，
   便于脚本化与排障。

9. **开源许可**：根目录补齐 MIT `LICENSE`（版权人 li2966068，2026），Cargo / Tauri 元数据统一为 MIT。

10. **CI 拆分与门禁**：删除旧的 `build.yml`，新增 `ci.yml`（push / PR / 手动）——`cargo fmt --check`、
    `cargo clippy -D warnings`、`cargo test --workspace`、`tsc --noEmit`；并把浏览器测试收进同一份门禁：
    **rclone E2E job**（本地起 WebDAV → 首轮上传 → 第二轮字节级幂等 → 断言远端无 `*.userdb`、
    `shared/` 下无 `*.userdb.txt`、无 `*.bin`）。

11. **发布流水线**：新增 `release.yml`（tag `v*` 或手动指定 tag）——Linux 出 `deb` + `AppImage`，
    Windows 出 `NSIS` + **便携版 zip**；publish job 汇总产物、生成 updater 用的 `latest.json`
    （从各 `*.sig` 提取签名与下载地址）并上传 Release。

12. **打包配置**：Windows `webviewInstallMode = embedBootstrapper`（内嵌 WebView2 引导器，内网/离线可装）；
    Linux `targets = ["deb", "appimage"]`；`createUpdaterArtifacts = true` + updater 占位 endpoints/pubkey；
    `packaging/portable-zip.ps1` 生成解压即用的便携包。

13. **fcitx5 配置页**：新增 `packaging/fcitx5-addon/`（C++ + CMake）——Qt 配置页含 WebDAV 地址 / 用户名 /
    密码 / 远端目录与「打开 Rime 同步」按钮，直接读写 `~/.config/rime-sync/config.json`（与主程序共享），
    并提供最小 fcitx5 addon 共享库。

14. **文档与安装体验**：README 重写（真实截图、AList 权限勾选表、两种模式、更新密钥替换步骤）；
    USER-GUIDE 新增 AList 章节与 403 权限排查；NSIS 钩子在开始菜单创建「Rime 同步」并尝试归入
    Weasel 程序组，卸载时清理快捷方式；同步更新 `packaging/README.md` 的产物清单与体积预期。

### 兼容性

- 配置格式向后兼容：旧 `config.json` 的明文 `password` 会在首次读取时迁移进钥匙串。
- 远端目录结构不变（`shared/` + `sync/<device_id>/`），v0.1.x 客户端可继续与 v0.2.0 互通。
- 红线不变：`*.userdb/` 目录永不同步。
