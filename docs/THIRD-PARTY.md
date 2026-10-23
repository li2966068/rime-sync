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
准备工作：复制 `LICENSE.txt` / `README.txt`、放置 librime 预编译件与 opencc 数据。

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
- 安装器把带时间戳的顺序写入 `$INSTDIR\install-ime-order.log`（静默安装的唯一可复核证据，
  卸载时删除），同时 `DetailPrint` 一份到安装日志。
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
- **用户数据始终保留**：`%APPDATA%\Rime`（词库、配置、同步备份）不在删除范围内。
  钩子里没有任何针对 `%APPDATA%` 的删除操作；Weasel 0.17.4 的卸载器同样只删安装目录、
  程序组与 `HKLM\Software\Rime`，不碰用户目录（见 `third_party/weasel/output/install.nsi`
  的 `Section "Uninstall"`）。rime-sync 自己的配置目录同样不在卸载清理范围内。

### 已知差异与离线兜底

- 自编译安装包不含 OpenCC 转换数据（`data\opencc`）：librime 的预编译包不提供该数据。
  构建期把上游 `install.nsi` 里两行 `File data\opencc\*` 改成 `/nonfatal`
  （生成 `output/install.rime-sync.nsi`，**不改 vendored 源码**），否则 NSIS 会因
  “通配符没有匹配文件”直接中止编译。简繁转换等依赖 OpenCC 的特性因此不可用。
- 自编译安装包默认只含 Weasel 自带数据（`weasel.yaml`、preview 配色、librime 的 opencc 数据），
  **不含输入方案**（方案数据由上游 `plum` 提供，vendored 树里 plum 为空）。
  需要完整方案时：用 `build-weasel.ps1 -WithRimeData` 重新构建，或走下面的离线兜底。
- `packaging/windows/fetch-weasel.ps1` 仍然保留：自编译失败、或把仓库变量
  `RIMESYNC_WEASEL_MODE` 设为 `fetch` 时，CI 回退到官方安装包（校验大小与 SHA-256）。
  该路径分发的是官方二进制，SHA-256 与官方发布页一致，CI 摘要会注明来源。
- 升级 Weasel：替换 `third_party/weasel` 的 vendored 版本，并同步
  `scripts/ime/build-weasel.ps1` 的 `$WeaselVersion` / `$ProductVersion` 与
  `packaging/nsis-hooks.nsh` 的 `RIMESYNC_WEASEL_INSTALLER` / `RIMESYNC_WEASEL_DIRVER`
  （后者是 Weasel 自己的安装目录名 `weasel-<ver>`，注意 0.17.4 与 0.17.4.0 的区别）。

## 2. Linux 侧的第三方 deb

deb 安装时会从 `packaging/linux/vendor/` 安装 fcitx5 / librime 等发行版软件包，
其版本、来源与许可证见 `packaging/linux/README.Debian` 与各包自带版权文件。

## 3. 其他

- 图标：见 `packaging/assets/README.md`。
- Rust / 前端依赖的许可证由 Cargo / pnpm 各自的元数据描述，未在此逐一列举。
