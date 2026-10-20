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
| NSIS（`embedBootstrapper`） | **约 5–10MB** | 安装包内嵌 WebView2 引导器，安装时仍可离线启动、联网补运行时；**未实测** |
| NSIS（`offlineInstaller`） | 约 130–140MB | 完整离线运行时，官方文档称额外约 127MB；**未实测** |
| 便携 zip | 与 NSIS 安装后的程序本体相当（约 10–20MB） | 不含 WebView2；**未实测** |
| deb | 约 4–10MB | 估算，未实测（不含 WebKitGTK，走系统依赖） |
| AppImage | 约 8–20MB | 估算，未实测；不开 `bundleMediaFramework` 可省 GStreamer |
| `AppImage.tar.gz` 更新包 | 与 AppImage 相当 + 压缩 | 由 `createUpdaterArtifacts` 生成 |

> 首次 release 跑完请用 artifact 实际大小回填本表。

## 4. 运行时依赖

### Windows

- WebView2 Runtime：Win10 1803+ / Win11 通常自带。`embedBootstrapper` 会把引导器放进安装包，
  目标机联网时自动补齐；完全离线分发需改 `offlineInstaller` 或单独分发 Evergreen Standalone。
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
- 卸载时删除上述快捷方式与空的「Rime 同步」程序组；**不删** `%APPDATA%\rime-sync` 配置与备份。

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
