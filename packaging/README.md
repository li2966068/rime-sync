# 打包说明（packaging）

本目录放**打包脚本、安装器钩子、fcitx5 addon 与占位资源**，不参与 Rust 编译；
真正的 bundle 配置在 [`../src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json)。

| 文件 | 作用 |
|---|---|
| `portable-zip.ps1` | 把 release 产物打成 Windows 便携 zip（解压即用，不写注册表） |
| `nsis-hooks.nsh` | NSIS 钩子：开始菜单「Rime 同步」，并尝试归入 Weasel 程序组；卸载时清理 |
| `fcitx5-addon/` | Qt 配置页 + 最小 fcitx5 addon（C++ / CMake） |
| `assets/` | 图标等占位资源说明 |

---

## 1. 产物形态

| 平台 | 产物 | 默认文件名 | 来源 |
|---|---|---|---|
| Linux x64 | Debian 包 | `rime-sync_<ver>_amd64.deb` | `bundle.targets` 含 `deb` |
| Linux x64 | AppImage | `rime-sync_<ver>_amd64.AppImage` | `bundle.targets` 含 `appimage` |
| Linux x64 | 更新包 | `rime-sync_<ver>_amd64.AppImage.tar.gz` + `.sig` | `createUpdaterArtifacts = true` |
| Windows x64 | NSIS 安装包 | `rime-sync_<ver>_x64-setup.exe` + `.sig` | `pnpm tauri build --bundles nsis` |
| Windows x64 | 便携版 | `rime-sync_<ver>_x64-portable.zip` | `portable-zip.ps1` |

`bundle.targets` 固定写成 `["deb", "appimage"]`（Linux）。Windows 构建必须显式传
`--bundles nsis`，避免在 Windows 上尝试 deb 目标：

```bash
pnpm tauri build --bundles nsis
pwsh -File packaging/portable-zip.ps1 -TargetDir src-tauri/target/release -OutDir packaging/out
```

## 2. CI 分工

| 工作流 | 触发 | 产物 |
|---|---|---|
| `.github/workflows/ci.yml` | push（master/main）、PR、手动 | 不产包；跑 fmt / clippy / test / tsc / **rclone E2E** |
| `.github/workflows/release.yml` | tag `v*`、手动指定 tag | 上面整张表 + `latest.json` |

release 的两个构建 job：

- `linux`（ubuntu-22.04）：`pnpm tauri build --bundles deb,appimage`
- `windows`（windows-latest）：`pnpm tauri build --bundles nsis` → 便携 zip

`publish` job 下载两边 artifact，用 `.github/scripts/make-latest-json.py` 从 `*.sig` 生成
`latest.json`，再 `gh release upload` 上传（含 `--clobber`，可重复跑）。

发布前还有两个"功能入口断言"，任一失败 publish 都不会跑：

- `verify-artifacts` job：`.github/scripts/verify-artifacts.sh`——解 deb 检查
  `rime.so` 里的 `/usr/bin/rime-sync-status`、`zh_CN` 译文、可执行位；解 `*-setup.exe` 检查
  `WeaselServer.exe` 里有没有「Rime 同步」。`ci.yml` 每次 push/PR 跑它的 `--selftest`。
- `windows` job 的 `verify weasel vendor payload`：来源必须是自编译（`require` 模式），
  否则写 `WEASEL-SOURCE.json` 并在 summary / 发布说明红字标注。

### 必需的 secrets

| Secret | 用途 | 缺失后果 |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | 更新包签名私钥内容 | release 构建失败（`createUpdaterArtifacts = true` 强制要求） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 私钥口令 | 无口令私钥可留空 |

生成密钥：`pnpm tauri signer generate -w ~/.tauri/rime-sync.key`，公钥填进
`tauri.conf.json` 的 `plugins.updater.pubkey`；替换 endpoints 的步骤见根 README「自动更新」。

## 3. 体积预期

| 产物 | 预期体积 | 说明 |
|---|---|---|
| NSIS 在线包（`webviewInstallMode = skip`） | **约 12–15MB** | 内嵌 Weasel 安装器；WebView2 由钩子检测，缺了只弹框给下载地址（**不联网**） |
| NSIS 离线包（`*-offline-setup.exe`） | 约 140–160MB | 另内嵌 WebView2 Evergreen Standalone（≈130MB）+ Weasel（≈12MB）；断言下限 100MB |
| 便携 zip | 与 NSIS 安装后的程序本体相当（约 10–20MB） | 不含 WebView2；**未实测** |
| deb | 约 4–10MB | 估算，未实测（不含 WebKitGTK，走系统依赖） |
| AppImage | 约 8–20MB | 估算，未实测；不开 `bundleMediaFramework` 可省 GStreamer |
| `AppImage.tar.gz` 更新包 | 与 AppImage 相当 + 压缩 | 由 `createUpdaterArtifacts` 生成 |

> 首次 release 跑完请用 artifact 实际大小回填本表。

## 4. 运行时依赖

### Windows

- WebView2 Runtime：Win10 1803+ / Win11 通常自带。0.6.5 起 `webviewInstallMode = skip`，
  安装器**不再联网下载**：在线包缺运行时会弹框给出官方地址（不下载），离线包内嵌
  Evergreen Standalone 静默安装。参数 `/SKIPWEBVIEW` 可跳过检查（受限代理/域内已推送时用），
  详见 `windows/README.md` 第 8 节；下载地址与分发说明见 `docs/THIRD-PARTY.md` 第 3 节。
- 无额外 VC++ 运行库要求（Tauri 产物静态链接，**推断**，未逐一验证）。
- 未签名安装包会触发 SmartScreen「未知发布者」：更多信息 → 仍要运行。

### Linux（deb 已声明，AppImage 需系统提供）

```
libwebkit2gtk-4.1-0
libgtk-3-0
libayatana-appindicator3-1   # 仅启用托盘时需要
```

deb 的 `depends` 已写入 `tauri.conf.json` 的 `bundle.linux.deb.depends`。AppImage 在部分发行版
需要 FUSE2：装 `libfuse2`，或用 `--appimage-extract-and-run` 绕过（**本仓库未逐发行版验证**）。

## 5. NSIS 快捷方式（nsis-hooks.nsh）

- 安装后创建 `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Rime 同步\Rime 同步.lnk`
  （同时受 `bundle.windows.nsis.startMenuFolder` 控制，二者一致）。
- 若检测到 `小狼毫` 或 `Weasel` 程序组，再在其中放一份快捷方式；不新建、不改动 Weasel 自身文件。
- 卸载时删除上述快捷方式与空的「Rime 同步」程序组；用户数据默认**不删**
  （`%APPDATA%\Rime`、`%APPDATA%\com.rimesync.app`）。交互卸载会弹窗询问是否删除（默认否），
  静默卸载只有带 `/PURGEDATA` 才删，详见 `windows/README.md` 第 7 节。
- **默认连小狼毫一起卸载**：安装时若本机原本没有 Weasel，会在 `HKLM\Software\rime-sync` 写
  `BundledWeasel=1`（含 WeaselRoot / UninstallString），卸载时据此静默运行 Weasel 卸载器；
  原本就有 Weasel 的用户不受影响。需要保留输入法时用 `uninstall.exe /KEEPWEASEL`
  （或设 `RIMESYNC_KEEP_WEASEL=1`）。用户词库 `%APPDATA%\Rime` 默认保留，只有卸载时显式
  确认删除（交互选「是」或 `/S /PURGEDATA`）才会删。

## 6. 便携版（portable-zip.ps1）

```powershell
pwsh -File packaging/portable-zip.ps1                       # 读 tauri.conf.json 的版本号
pwsh -File packaging/portable-zip.ps1 -Version 0.2.0
```

生成 `packaging/out/rime-sync_<ver>_x64-portable/`（暂存目录）与同名 `.zip`，内含
`rime-sync.exe` + `README-便携版.txt`。便携版与安装版共用 `%APPDATA%\rime-sync`，不要同时运行。

## 7. 构建前置

| 工具 | 版本 | 备注 |
|---|---|---|
| Node | 22 LTS | 前端（Vite + TS） |
| pnpm | 10 | `corepack enable` 或 `npm i -g pnpm@10` |
| Rust | stable（≥1.77.2） | Tauri v2 要求 |
| 平台依赖 | 见下 | 只在对应平台需要 |

```bash
# Linux（Ubuntu 22.04 / Debian 12 基线）
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
                        patchelf xdg-utils file

# Windows：MSVC「Desktop development with C++」+ WebView2 Runtime
```

## 8. 安装后首次配置

1. 启动应用 → 确认 Rime 目录（Windows `%APPDATA%\Rime`，Linux `~/.local/share/fcitx5/rime`）。
2. 填 WebDAV：URL / 账号 / 应用专用密码 / 远端目录（默认 `rime-sync`；AList 要带挂载前缀）。
3. 「测试连接」应返回 207 或 200。
4. 先跑 **transport**，确认远端出现 `shared/` 与 `sync/<device_id>/`。
5. 需要真正合并生效时再 **apply**（Linux 会临时停 fcitx5）。

详细步骤与 AList 权限表见 [`../docs/USER-GUIDE.md`](../docs/USER-GUIDE.md)。

## 9. 代码签名

当前**不签名**：Windows 会显示 SmartScreen 警告，Linux 无签名校验。若后续分发：

- Windows：EV/OV 证书 + `signtool`（Tauri `bundle.windows.certificateThumbprint` 等）；
- Linux：仅 deb 可加 `debsigs`，AppImage 无内建签名；
- 更新包签名（`.sig`）与代码签名是两件事：前者用于自动更新校验，必须配置。

## 10. 图标资源（assets/）

图标二进制未入库，`src-tauri/icons/` 里是已知可用的一套。需要重新生成时：

```bash
pnpm tauri icon packaging/assets/source-icon.png   # 需要自备 1024x1024 源图
```

许可与来源要求见 `packaging/assets/README.md`。

## 11. 回退策略

打包链路里所有"失败后自动降级"的行为（Weasel 自编译失败回退上游官方包、离线依赖
安装失败回退自带 .deb……）都受 [`../docs/FALLBACK-POLICY.md`](../docs/FALLBACK-POLICY.md) 约束：

> 任何 fallback 必须同时做到：**写日志、在 UI 或发布说明可见、可用参数关闭。**

落到本目录的具体位置：

| 位置 | 说明 |
|---|---|
| `windows/README.md` §5 | `RIMESYNC_WEASEL_MODE=require/build/fetch` 的行为表 |
| `linux/postinst` | 依赖安装最终失败 → `~/.config/com.rimesync.app/install-warning.json`（应用诊断页红条 + 重试） |
| `.github/scripts/verify-artifacts.sh` | 把"入口必须存在"变成断言；`--selftest` 证明缺少入口时会红 |
| `.github/workflows/release.yml` | tag 发布强制 `require`；回退时 summary 与发布说明红字 |

## 12. Linux 离线安装器：判定逻辑与故障排查

`packaging/linux/install.sh` 负责 `.run` 自解压包里的
「vendor 依赖 → rime-inputmethod → rime-sync」三段安装。0.6.0 起它只对
**自己解包的包**负责：

| 行为 | 实现 |
|---|---|
| 只补缺失、不升级系统库 | dpkg 状态为 `ii` 的包直接跳过；只有 `iU/iF/rc/未安装` 才解包 |
| 多架构系统更保守 | `dpkg --print-foreign-architectures` 非空时，同名包任一架构已装且版本不同就不碰（避免 `libbrotli1:amd64` 与 `libbrotli1:i386` 版本不一致） |
| 只配置自己解包的包 | `dpkg --configure <pkg:arch>...`，不再 `dpkg --configure -a`；无关的半配置包不影响判定 |
| 失败判定 | 只看「安装前是 ii 或本来不存在、现在却不是 ii」的包；安装前就坏的包只提示 |
| 可重复执行 | 旧版已装时直接 `dpkg -i --force-confdef --force-confold` 覆盖升级，不需要先卸载 |
| 需要升级系统库时 | `sudo ./install.sh --upgrade-vendor`（默认关闭） |

失败时安装器会打印：未配置的包名与 dpkg 原文尾部、`dpkg --audit` 的系统范围
清单、本次改动过的包（安装前 → 现在）以及可执行的修复命令。安装前的 dpkg
快照写在 `/var/log/rime-sync/dpkg-before-install.txt`，完整日志在
`/var/log/rime-sync/install.log`。

多架构版本冲突的处置（用户现场：`libbrotli1:amd64` 被升到 `deb13u1`、`i386`
还停在 `b7`，dpkg 报 `cannot be configured because libbrotli1:i386 is at a
different version`）：

```bash
sudo apt --fix-broken install          # 有网络：让 apt 统一各架构版本
sudo apt purge '.*:i386'               # 不需要 32 位兼容时
sudo dpkg --remove-architecture i386
sudo dpkg --configure libbrotli1:amd64 # 只修被点名的那一个
sudo dpkg --audit                      # 确认系统整体状态
```

修好后重跑安装器即可（已配好的依赖不会重复解包）。

### 容器验收矩阵

```bash
packaging/linux/test/install-matrix.sh                # 全部用例
packaging/linux/test/install-matrix.sh upgrade        # 只跑升级用例
```

每个用例都是全新容器、`--network none` 真离线：

| 用例 | 场景 | 断言 |
|---|---|---|
| `normal` | debian-13 干净系统 | 两个包都装上、rime_deployer 能构建、dpkg.log 0 次 upgrade |
| `broken-unrelated` | 预置一个无关的半配置包（postinst 返回 1） | 安装仍成功；那个包保持原状且不出现在 install.log |
| `multiarch` | 启用 i386 + libbrotli1 两架构同版本，离线库里该包更高 | 不升级 `libbrotli1:amd64`、安装成功、0 次 upgrade |
| `prebroken-multiarch` | 已处于"上次安装把 amd64 解包成新版、i386 仍是旧版"的坏状态 | 退出码 3 但措辞明确，输出 dpkg 原文 + 多架构说明 + 修复命令 + 责任归属 |
| `upgrade` | 先装旧版（0.4.0 IME + 0.3.0 sync），再装当前产物 | 版本原地升级、旧版独有路径已清理、用户数据哈希不变 |
| `single-arch` | debian-12（无任何外部架构） | 安装成功且不触发多架构分支 |

产物默认取本地已有的 `.deb`（`build/deb/debian-*/`、`packaging/out/`），可用
`IME_DEB_NEW` / `SYNC_DEB_NEW` 等环境变量指向 CI 新产物。仓库里的
`packaging/linux/vendor/` 是历史快照（对应 IME 0.4.x）；IME 0.5.x 新增的
`libqt5core5t64` / `libpcre2-16-0` / `libdouble-conversion3` 由
`EXTRA_DEBS`（默认 `packaging/linux/out/test-extra-debs/<distro>/`）补入，
正式离线包由 `fetch-vendor-debs.sh` 现算闭包不会缺；缺任何依赖时脚本会在预检
阶段直接点名。

