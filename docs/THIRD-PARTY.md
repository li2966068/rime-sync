# 第三方组件与许可证

本仓库自身以 MIT 发布（见根目录 `LICENSE`）。分发的安装包里还包含下列第三方组件。

## 1. 小狼毫 Weasel 0.17.4（由本项目构建，源码 vendored 在 third_party/weasel）

| 项 | 值 |
|---|---|
| 组件 | 小狼毫输入法 / Weasel（Rime 输入法引擎的 Windows 前端） |
| 版本 | 0.17.4（安装包内部版本号 0.17.4.0） |
| 构建来源 | 本仓库 `third_party/weasel`（= rime/weasel tag `0.17.4`，commit `9cc96e20dc71b80876b12f689bb5863c76c2a7ed`） |
| 构建脚本 | `scripts/ime/build-weasel.ps1`（release.yml 的 windows job 调用） |
| 版权 | Copyleft RIME Developers / 式恕堂 |
| 许可证 | GNU General Public License v3.0（`third_party/weasel/LICENSE.txt`） |
| 上游源码 | <https://github.com/rime/weasel/tree/0.17.4> |
| 上游发布页 | <https://github.com/rime/weasel/releases/tag/0.17.4> |
| 构建产物 | `packaging/windows/vendor/weasel-0.17.4.0-installer.exe`（不入库，CI 构建） |

### 分发方式（与 0.3.x 的区别）

- 0.3.x 及以前：官方发布的 `weasel-0.17.4.0-installer.exe` **原样内嵌**。
- 自本版本起：**用 vendored 的 Weasel 源码自己编译**安装包。

Weasel 源码未做功能性修改（`third_party/weasel` 是上游 tag 0.17.4 的源码快照，
差异见 `third_party/weasel/FORK.md`）。构建期只做上游 `output/install.nsi` 本来就要求的
准备工作：复制 `LICENSE.txt` / `README.txt`、放置 librime 预编译件与 opencc 数据、
把 `third_party/rime-data` 的方案数据平铺进 `output\data`（见第 2 节）。

构建链（与上游 release-ci 对齐）：

| 依赖 | 版本 | 说明 |
|---|---|---|
| Visual Studio | 2022（v143 / msvc-14.3） | 上游 `weasel.sln` 用 MSBuild 构建 |
| boost | 1.84.0 | b2 静态运行时（`stage\lib`） |
| librime | 预编译 msvc 包，默认 pin `1.13.1` | 由上游 `get-rime.ps1 -use dev` 拉取 |
| NSIS | 3.08 | 跑上游 `output/install.nsi` |

> 注意：Weasel 0.17.4 在 Weasel 层**没有 CMake 构建**（CMake 只用于 librime 子模块）；
> 官方脚本是 `build.bat`（MSBuild）/ `xbuild.bat`（xmake）。vendored 树里
> `librime/`、`plum/` 是空子模块，所以 librime 走上游发布的预编译包。

因为是我们自己编译的产物，SHA-256 **不再**与官方发布页一致；对应源码与构建步骤完全公开。

### GPLv3 合规

- 许可证全文随安装包分发：`C:\Program Files\rime-sync\resources\GPL-3.0.txt`
  （仓库副本 `packaging/windows/vendor/GPL-3.0.txt`）；
  Weasel 自己的 `LICENSE.txt` 也由它的安装器装进 `C:\Program Files\Rime\weasel-0.17.4\`。
- **对应源码（Corresponding Source）**：本仓库 `third_party/weasel`（tag 0.17.4 源码快照）
  + 构建脚本 `scripts/ime/build-weasel.ps1` + 本文件的构建说明；
  上游源码同样可从 <https://github.com/rime/weasel/tree/0.17.4> 获取。
- 组件来源、构建环境与版本见 `packaging/windows/vendor/WEASEL-NOTICE.txt`
  （安装到 `resources\WEASEL-NOTICE.txt`）。

### 安装与卸载

- **安装顺序：先装小狼毫，再装同步程序。**
  构建期用 NSIS `File` 把 Weasel 安装器内嵌进 rime-sync 的 setup.exe；运行时的
  `PREINSTALL` 钩子（在 Tauri 拷贝主程序与 resources **之前**）把它解到 `$PLUGINSDIR`，
  `ExecWait ... /S` 等安装结束，然后 Tauri 才拷贝 rime-sync 自己的程序文件。
- 安装器把带时间戳的顺序写入 `$INSTDIR\install-ime-order.log`（静默安装的可复核证据，
  卸载时删除），并**同时**追加到 `%TEMP%\rime-sync-setup.log`（UTF-16LE，阶段名
  `OS-CHECK` / `WEBVIEW-CHECK` / `WEBVIEW-INSTALL` / `IME-INSTALL` / `POSTINSTALL` / `DONE`，
  安装结束后仍在，失败路径也写）。
- 安装前先检查系统版本（≥ Windows 10 1803）与 WebView2 运行时（见第 3 节），
  再装小狼毫；三步都在 Tauri 拷贝主程序之前。
- 只有检测不到小狼毫时才静默安装，已安装则跳过（不覆盖用户版本）。
- **卸载 rime-sync 默认连小狼毫一起卸载，但只卸「rime-sync 装的」那个**：
  - 安装前先探测小狼毫是否已存在。原本不存在、由本次安装装上时，安装器在
    `HKLM\Software\rime-sync`（64 位视图）记录所有权标记：
    `BundledWeasel=1`、`WeaselRoot=<程序目录>`、`WeaselUninstall=<Weasel 的 UninstallString>`、
    `BundledAt=<时间戳>`。卸载时按标记决定是否连小狼毫一起卸。
  - 安装前就有小狼毫（用户自己装的，或更早的 rime-sync 没留标记）→ **不写标记**，卸载时保留。
  - 卸载调用链：先跑标记里的 `UninstallString`（追加 `/S`，用 `nsExec::ExecToLog` 等它结束）；
    退出码非 0 / 启动失败 / 超时则退回 `<WeaselRoot>\uninstall.exe /S`（与第一条相同则不重跑）；
    两条都失败只写日志，**不阻断** rime-sync 自己的卸载。无论成功失败都删除标记。
  - 保留选项：`uninstall.exe /KEEPWEASEL`（或环境变量 `RIMESYNC_KEEP_WEASEL=1`）跳过小狼毫卸载，
    并把所有权交还用户（删除标记）。
  - 卸载日志：`%TEMP%\rime-sync-uninstall.log`（不在 `$INSTDIR` 里，卸载后仍可复核）。
- **用户数据默认保留**：`%APPDATA%\Rime`（词库、配置、同步备份）与 `%APPDATA%\com.rimesync.app`
  （rime-sync 配置/备份）不在默认删除范围内。只有卸载时显式确认才删：交互模式在 MessageBox
  里选「是」（默认「否」），静默模式必须带 `/PURGEDATA`；删除严格限定这两条路径，失败只写
  `%TEMP%\rime-sync-uninstall.log`、不中止卸载。Weasel 0.17.4 的卸载器本身只删安装目录、
  程序组与 `HKLM\Software\Rime`，不碰用户目录（见 `third_party/weasel/output/install.nsi`
  的 `Section "Uninstall"`）。

### 已知差异与离线兜底

- 自编译安装包的 NSIS 载荷按「文件实际存在」生成宏：ARM/ARM64 组件、OpenCC 数据、data/*.txt、data/*.gram
  各自用 !ifdef 包住，构建期生成 output/install.rime-sync.nsi（宏前言 + 原样 install.nsi，不改 vendored 源码）。
  缺哪个可选组件都不会让 makensis 中止；data/*.yaml 仍是硬性 File，缺了会红。
  （早期版本用把 OpenCC 两行改成 /nonfatal 的做法，现已废弃。）
- 自编译安装包内置 `third_party/rime-data` 里的全部方案（见第 2 节）：安装器把
  `*.yaml` 平铺进 `C:\Program Files\Rime\weasel-0.17.4\data\`，`essay.txt` 一并放入。
  `-WithRimeData` 仍可用 plum 追加更多方案（plum 在 vendored 树里为空，默认关闭）。
- `packaging/windows/fetch-weasel.ps1` 仍然保留：自编译失败、或把仓库变量
  `RIMESYNC_WEASEL_MODE` 设为 `fetch` 时，CI 回退到官方安装包（校验大小与 SHA-256）。
  该路径分发的是官方二进制，SHA-256 与官方发布页一致，CI 摘要会注明来源。
- 升级 Weasel：替换 `third_party/weasel` 的 vendored 版本，并同步
  `scripts/ime/build-weasel.ps1` 的 `$WeaselVersion` / `$ProductVersion` 与
  `packaging/nsis-hooks.nsh` 的 `RIMESYNC_WEASEL_INSTALLER` / `RIMESYNC_WEASEL_DIRVER`
  （后者是 Weasel 自己的安装目录名 `weasel-<ver>`，注意 0.17.4 与 0.17.4.0 的区别）。

## 2. 随包的 Rime 方案数据（third_party/rime-data）

`rime-inputmethod`（Linux，装到 `/usr/share/rime-data`）与自编译的小狼毫（Windows，
装到 `data\`）都在安装包内直接附带下列方案数据，安装后无需联网即可 deploy 并输入中文。
每个目录只保留上游的 `*.yaml` / `*.txt` / `LICENSE` / `AUTHORS` / `README.md`；
`Makefile`、`scripts/`、`check.py`、`.github/` 等不入库。

| 目录 | 上游仓库 | 提交（master 快照） | 许可证 | 文件数 | 字节数 |
| --- | --- | --- | --- | --- | --- |
| rime-prelude | <https://github.com/rime/rime-prelude> | `082425ea0684bca36474415d4a0e8db9b016487e` | LGPL-3.0 | 7 | 43,963 |
| rime-essay | <https://github.com/rime/rime-essay> | `054920de4f54c9e5994276a96a4fc2a35cb51aa3` | LGPL-3.0 | 5 | 5,895,745 |
| rime-luna-pinyin | <https://github.com/rime/rime-luna-pinyin> | `56b934b099dfbeab842320f13aa8b461a6ab3e42` | LGPL-3.0 | 10 | 907,752 |
| rime-cangjie | <https://github.com/rime/rime-cangjie> | `52d90a1b1312e74042b38c1cbc8142defbc53171` | LGPL-3.0 | 10 | 811,576 |
| rime-stroke | <https://github.com/rime/rime-stroke> | `1e8fff9b9494ddec23b0cbc526bcfd8171a6fd48` | LGPL-3.0 | 5 | 3,407,589 |
| rime-terra-pinyin | <https://github.com/rime/rime-terra-pinyin> | `723e51bc266cf9464530c1ddedb856aa18e3da34` | LGPL-3.0 | 6 | 1,814,794 |
| rime-bopomofo | <https://github.com/rime/rime-bopomofo> | `6085c9a38a4a728047862b33d67eee18aa86f3b9` | LGPL-3.0 | 8 | 28,186 |
| rime-wubi | <https://github.com/rime/rime-wubi> | `152a0d3f3efe40cae216d1e3b338242446848d07` | LGPL-3.0 | 7 | 2,302,709 |
| rime-pinyin-simp | <https://github.com/rime/rime-pinyin-simp> | `0c6861ef7420ee780270ca6d993d18d4101049d0` | Apache-2.0 | 4 | 1,279,668 |

合计 62 个文件、16,491,982 字节。2026-10-24 新增的 6 个 recipe（cangjie / stroke /
terra-pinyin / bopomofo / wubi / pinyin-simp）共 40 个文件、9,644,522 字节（约 9.20 MiB）。

打包与守卫：

- 三个构建入口（`build-weasel.ps1`、`container-build.sh`、`package-ime-deb.sh`）
  都遍历 `third_party/rime-data` 下所有含 `*.yaml` / `*.txt` 的目录
  （`rime-essay` 只有 `essay.txt`），把 `*.yaml` 与 `*.txt` 平铺到目标目录；新增方案只需加一个目录。
- `rime-prelude/default.yaml` 的 `schema_list` 只列随包方案（当前 16 个）。
  Windows 构建缺一个就 throw，deb 打包用 shell 断言；`packaging/rime/default.custom.yaml`
  同步列出同样的 16 个（Windows 与 Linux 的推荐用户配置都来自这一个文件）。
- `rime-bopomofo/detenele.schema.yaml`（实验方案「動態能力注音」）随文件入库，但不列入
  `schema_list`。
- 官方小狼毫 0.17.4 的 data **不含五笔**；`rime-wubi`（wubi86 / wubi_pinyin / wubi_trad）
  是本项目额外新增的。
- `rime-pinyin-simp`（`pinyin_simp`，袖珍简化字拼音，Apache-2.0）随文件入库但**不列入**
  `schema_list`：它是 wubi86 / wubi_pinyin / wubi_trad 的 `dependencies`，部署时由依赖链自动构建
  （实测生成 `pinyin_simp.table.bin` / `.reverse.bin`），因此五笔的拼音反查可用：
  wubi86 敲 `znihao` 出「你好」（实测）。

许可证：8 个 recipe 为 LGPL-3.0（各目录 `LICENSE` 与上游一致，SHA-256
`da7eabb7bafdf7d3ae5e9f223aa5bdc1eece45ac569dc21b3b037520b4464768`）；
`rime-pinyin-simp` 为 Apache-2.0（SHA-256
`cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`）。
deb 把每个 recipe 的 `LICENSE` 装到 `/usr/share/doc/rime-inputmethod/copyright.<repo>`；
Windows 安装包把数据装进 `C:' + chr(92) + 'Program Files' + chr(92) + 'Rime' + chr(92) + 'weasel-0.17.4' + chr(92) + 'data' + chr(92) + '`，不额外投放单独的
许可证文件——对应源码、提交与许可证见上表及仓库内 `third_party/rime-data/<repo>/LICENSE`。

## 3. Microsoft Edge WebView2 运行时（Windows，0.6.5 起）

rime-sync 的界面是 WebView（Tauri v2），Windows 上需要 Microsoft Edge WebView2 Evergreen
运行时。它不是本仓库的代码，也不随仓库入库：

| 项 | 值 |
|---|---|
| 组件 | Microsoft Edge WebView2 Runtime（Evergreen，x64） |
| 提供方 | Microsoft Corporation |
| 许可证 | 随安装器分发的 Microsoft 许可条款（安装器自带 EULA，本项目不再单独分发文本） |
| 获取方式（用户手动） | <https://go.microsoft.com/fwlink/?linkid=2124703>（在线引导器，约 1.5 MB，安装时联网） |
| 获取方式（内网/离线） | <https://go.microsoft.com/fwlink/?linkid=2124701>（**Standalone**，约 130 MB） |

**分发策略（0.6.5 的变更）**

- 0.6.4 及以前：`tauri.conf.json` 用 `webviewInstallMode = embedBootstrapper`，
  安装器在缺运行时的机器上**静默联网**下载约 130 MB；受限代理下反复重试并卡死在
  `WebView2Setup`，无提示、无日志。
- 0.6.5 起：`webviewInstallMode = skip`，改由项目自己的 NSIS 钩子处理：
  - 在线包（`rime-sync_<ver>_x64-setup.exe`）：**不下载**。缺运行时则弹框中英说明 +
    官方下载地址，由用户选择"继续（我已装好）/ 退出安装"；
  - 离线包（`rime-sync_<ver>_x64-offline-setup.exe`）：构建期用 `File` 内嵌
    `MicrosoftEdgeWebView2RuntimeInstallerX64.exe`，运行期 `ExecWait '/silent /install'`，
    全程不联网。该 Standalone 安装器由 CI 的 `packaging/windows/fetch-webview2.ps1`
    从微软官方链接下载后打进包内，**不入库**（`packaging/windows/vendor/*.exe`）。
- 检测方式：`HKLM`/`HKCU` × 64/32 位视图读
  `SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}\pv`，
  另用程序文件 `Microsoft\EdgeWebView\Application\*\msedgewebview2.exe` 兜底。
- 用户可跳过：`/SKIPWEBVIEW`（缺运行时也不再阻断）；`/SKIPIME` 跳过内嵌小狼毫安装。
  详见 `packaging/windows/README.md` 第 8 节。

## 4. Linux 侧的第三方 deb


deb 安装时会从 `packaging/linux/vendor/` 安装 fcitx5 / librime 等发行版软件包，
其版本、来源与许可证见 `packaging/linux/README.Debian` 与各包自带版权文件。

## 5. 其他

- 图标：见 `packaging/assets/README.md`。
- Rust / 前端依赖的许可证由 Cargo / pnpm 各自的元数据描述，未在此逐一列举。
