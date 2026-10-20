# 打包说明（packaging）

本目录只放**打包相关文档与占位资源**，不参与编译；真正的 bundle 配置在 `src-tauri/tauri.conf.json`
（由应用壳子模块维护）。平台触发机制的证据见 [`../docs/platform-notes.md`](../docs/platform-notes.md)。

## 1. 产物形态

| 平台 | 产物 | 默认文件名 | 安装方式 |
|---|---|---|---|
| Windows 10/11 x64 | NSIS 安装包 | `rime-sync_<version>_x64-setup.exe` | 双击安装；卸载走"应用和功能" |
| Linux x64 (deb 系) | Debian 包 | `rime-sync_<version>_amd64.deb` | `sudo apt install ./rime-sync_<version>_amd64.deb` |
| Linux x64（通用） | AppImage | `rime-sync_<version>_amd64.AppImage` | `chmod +x` 后直接运行 |

CI 产出（`.github/workflows/build.yml`）以 artifact 形式上传：
`rime-sync-linux`（deb + AppImage）、`rime-sync-windows`（NSIS `.exe`）。

## 2. 体积预期

| 产物 | 预期体积 | 说明 / 来源 |
|---|---|---|
| NSIS `-setup.exe`（WebView2 默认 `downloadBootstrapper`） | **约 3–8MB** | 安装时联网下载 WebView2；Tauri 文档：该模式额外体积 0MB。整包体积为估算，**未实测** |
| NSIS（`offlineInstaller`） | **约 130–140MB** | 文档：额外 ~127MB，离线分发用 |
| NSIS（`fixedVersion`） | **约 185MB** | 文档：额外 ~180MB，不推荐 |
| deb | **约 4–10MB** | 估算，未实测（不含 WebKitGTK，依赖系统包） |
| AppImage | **约 8–20MB** | 估算，未实测；不开 `bundleMediaFramework` 可省 GStreamer |

> 体积数字中"WebView2 额外体积"来自官方文档表格（<https://v2.tauri.app/distribute/windows-installer/>），
> 整包体积为经验估算——首次 CI 跑完请以 artifact 实际大小回填本节。

## 3. 运行时依赖

### Windows
- WebView2 Runtime：Win10 1803+ / Win11 随系统提供；否则安装包会按 `webviewInstallMode` 处理
  （默认下载安装，需联网）。
- 无额外 VC++ 运行库要求（Tauri 产物静态链接，**推断**）。
- 未签名安装包会触发 SmartScreen "未知发布者"，需要"更多信息 → 仍要运行"。

### Linux（deb 声明 / AppImage 需系统提供）
- `libwebkit2gtk-4.1-0`
- `libgtk-3-0`
- `libayatana-appindicator3-1`（仅当启用系统托盘）
- 建议在 `bundle > linux > deb > depends` 中补齐，模板：

```json
{
  "bundle": {
    "targets": ["deb", "appimage", "nsis"],
    "linux": {
      "deb": { "depends": ["libwebkit2gtk-4.1-0", "libgtk-3-0"] }
    }
  }
}
```

AppImage 在部分发行版需要 FUSE2 才能直接挂载运行（`--appimage-extract-and-run` 可绕过）；
**该项未在本仓库验证**，验证方法：在目标机执行 `./rime-sync_*.AppImage`，若报 fuse 错误则装
`libfuse2` 或改用 `--appimage-extract-and-run`。

## 4. 构建前置

| 工具 | 版本 | 备注 |
|---|---|---|
| Node | 22 LTS | 前端（Vite + TS） |
| pnpm | 10 | `corepack enable` 或 `npm i -g pnpm@10` |
| Rust | stable（≥1.77.2） | Tauri v2 要求 |
| 平台依赖 | 见下 | 只在对应平台需要 |

```bash
# Linux（Ubuntu 22.04 / Debian 12 基线）
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev \
                        patchelf xdg-utils file

# Windows：安装 "Desktop development with C++"（MSVC）+ WebView2 Runtime（Win11 自带）

# 本地出包
pnpm install
pnpm tauri build          # 产物在 src-tauri/target/release/bundle/
```

CI（`.github/workflows/build.yml`）：push tag `v*` 或手动 `workflow_dispatch`，两个 job 并行
（`ubuntu-22.04` / `windows-latest`），缓存用 `swatinem/rust-cache`（`./src-tauri -> target`）。

## 5. 安装后首次配置

1. 启动应用 → "Rime 目录"自动探测失败时手动选：
   - Windows：`%APPDATA%\Rime`（若注册表 `HKCU\Software\Rime\Weasel\RimeUserDir` 有值则以它为准）
   - Linux：`~/.local/share/fcitx5/rime`（fcitx5-rime）或 `~/.config/ibus/rime`（ibus-rime）
2. 填 WebDAV：URL / 账号 / **应用专用密码** / 远端目录（默认 `rime-sync`）。
3. 点"测试连接"，应返回 `207` 或 `200`。
4. 先跑一次 **transport**（只搬文件，不打断输入），确认远端出现 `shared/` 与 `sync/<device_id>/`。
5. 需要让新词库真正生效时，再点 **apply**（Linux 会临时停 fcitx5，输入会中断数秒）。

## 6. 代码签名

当前**不签名**：Windows 会显示 SmartScreen 警告，Linux 无签名校验。若后续分发：
- Windows 用 EV/OV 证书 + `signtool`（Tauri `bundle.windows.certificateThumbprint` 等）；
- Linux 仅 deb 可加 `debsigs`，AppImage 无内建签名。

## 7. 资源占位（packaging/assets/）

图标与安装器资源目前在 `assets/` 只有说明文档，二进制未入库。生成方式：

```bash
# 需要一张 1024x1024 的源图（PNG）
pnpm tauri icon packaging/assets/source-icon.png
# 产物会写到 src-tauri/icons/（32x32.png, 128x128.png, 128x128@2x.png, icon.icns, icon.ico ...）
```

许可与来源要求见 `packaging/assets/README.md`。
