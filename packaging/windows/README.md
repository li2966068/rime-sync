# Windows 打包（自编译小狼毫 + 先装输入法）

目标：一个安装包装完即有小狼毫，且安装顺序是「先装输入法、再装 Rime 同步」。

## 1. 文件

| 文件 | 作用 |
|---|---|
| `../../scripts/ime/build-weasel.ps1` | **主路径**：用 MSVC 从 `third_party/weasel` 编译 Weasel，产出安装包 |
| `fetch-weasel.ps1` | **离线兜底**：下载官方 `weasel-0.17.4.0-installer.exe`，校验大小与 SHA-256 |
| `vendor/` | 构建产物（`*.exe` 不入库）+ GPLv3 全文 + 来源说明 |
| `../nsis-hooks.nsh` | NSIS 钩子：PREINSTALL 检查系统版本 → 检查/安装 WebView2 → 装输入法并记所有权；POSTINSTALL 建快捷方式；PREUNINSTALL 卸载输入法（仅当是 rime-sync 装的） |
| `../nsis-hooks-offline.nsh` | 离线变体：`!define RIMESYNC_EMBED_WEBVIEW` 后 include 主钩子（内嵌 WebView2 Standalone） |
| `fetch-webview2.ps1` | 下载 WebView2 Evergreen **Standalone**（x64，约 130 MB）供离线包内嵌；3 次重试 + 体积/文件头校验 |
| `../../src-tauri/tauri.conf.json` | `bundle.resources` 只打包许可证/说明；`installMode = perMachine`；`webviewInstallMode = skip` |
| `../../src-tauri/tauri.offline.conf.json` | 离线包覆盖项：只改 `bundle.windows.nsis.installerHooks` 指向离线钩子 |

> 为什么 resources 里没有 `weasel-*.exe`：安装器用 NSIS 的 `File` 指令在**构建期**把
> 它内嵌进 setup.exe（见第 3 节），再放进 resources 会让同一个 12MB 文件进包两次。

## 2. 构建（CI 由 release.yml 的 windows job 执行）

```powershell
# 自编译（主路径）
pwsh -NoProfile -File scripts/ime/build-weasel.ps1
# 只做前置检查，不下载/不编译
pwsh -NoProfile -File scripts/ime/build-weasel.ps1 -Check

# 然后是 Tauri 打包
pnpm tauri build --bundles nsis
```

脚本会依次做：

1. **前置检查**：`weasel.sln`、`output/install.nsi`、`get-rime.ps1`、`render.js`、
   `output/7z.exe`、`curl.exe`、`WinSparkle.dll`、`resource/weasel.ico` 等。
2. **boost 1.84.0**：下载源码（`archives.boost.io`，用 vendored 的 `output/7z.exe` 解压），
   `bootstrap.bat` + `b2` 静态运行时构建 x64 与 x86 两份 `stage\lib`。
   CI 用 `actions/cache` 缓存 `deps/boost_1_84_0/{boost,stage}`，命中则整体跳过。
3. **librime**：跑上游 `get-rime.ps1 -use dev -tag 1.13.1`，把 rime/librime 的
   Windows-msvc 预编译包（x64 + x86）落到 `lib64/`、`lib/`、`output/`、`output/Win32/`、
   `include/`，并把 opencc 数据放进 `output/data/opencc/`。
4. **数据脚手架**：复制 `LICENSE.txt`→`output/LICENSE.txt`、`README.md`→`output/README.txt`；
   plum 为空时生成一个只打印提示的 `output/rime-install.bat`（`install.nsi` 要求它存在）。
5. **weasel.props**：`cscript render.js` 把 `BOOST_ROOT` / `PLATFORM_TOOLSET=v143` /
   版本号写进 props。
6. **MSBuild**：`weasel.sln`，`Release|x64` + `Release|Win32`（32 位 TSF 组件，
   64 位系统上的 32 位程序要用；`install.nsi` 在编译期就 `File` 它们）。
7. **NSIS 3.08**：找不到 `makensis` 时先试 `choco install nsis`，再退回 SourceForge 静默安装；
   先把 output/install.nsi 生成一份 output/install.rime-sync.nsi：脚本按 output/ 下实际存在的文件
   写入 !ifdef 宏（ARM 组件、OpenCC、data 的 txt/gram），再把原样 install.nsi 附在宏之后编译
   （/DWEASEL_VERSION=0.17.4 /DWEASEL_BUILD=0 /DPRODUCT_VERSION=0.17.4.0）。
8. **产物断言**：复制到 `vendor/weasel-0.17.4.0-installer.exe`，打印大小与 SHA-256，
   低于 `-MinInstallerMB`（默认 8 MiB）直接失败；写了 `GITHUB_STEP_SUMMARY`（CI 摘要）。

> **可选载荷（为什么构建期生成脚本）**
> 上游 install.nsi 对 ARM/ARM64 二进制、data 的 txt/gram、OpenCC 数据用硬性 File，通配符没匹配到就中止。
> 我们按文件实际存在与否生成 !ifdef 宏，再原样附上上游脚本：不改 vendored 源码，也不会因缺组件失败。
> 随包方案数据来自 third_party/rime-data（9 个 recipe、16 个方案），见 docs/THIRD-PARTY.md 第 2 节。

### 依赖版本

| 依赖 | 版本 | 来源 |
|---|---|---|
| Visual Studio | 2022 + v143（msvc-14.3） | windows-latest 自带 |
| boost | 1.84.0 | 脚本下载源码 + b2 自建 |
| librime | 1.13.1（预编译 msvc 包，可 `-LibrimeTag` 覆盖） | rime/librime releases |
| NSIS | 3.08 | 脚本自动安装 |

**没有 CMake 步骤**：Weasel 0.17.4 上游没有 Weasel 层的 CMakeLists.txt（官方是
`build.bat`/MSBuild 或 `xbuild.bat`/xmake）；CMake 只属于 librime 子模块，而这里用
librime 的官方预编译包，因此不需要 CMake。

### CI 前置：vendored 源码必须入库

`third_party/weasel` 目前在工作区里是**未跟踪**目录。要让 CI 自编译，必须先入库：

```sh
git add -f third_party/weasel          # -f 是必须的，见下
```

vendored 树自带的上游 `.gitignore` 忽略了 `output/*.exe`、`output/*.txt`、`output/*.bat`、
`deps/`，而 `install.nsi` 恰恰要 `File` 这些文件（`7z.exe`、`curl.exe`、
`7-zip-license.txt`、`rime-install-config.bat` …）。脚本的前置检查会明确列出缺失项并提示
`git add -f`。

## 3. 安装顺序的实现（证据点）

Tauri 的 NSIS 模板在 `Section Install` 里的顺序是：

```nsis
SetOutPath $INSTDIR
!insertmacro NSIS_HOOK_PREINSTALL     ; ← 我们在这里装输入法
File "${MAINBINARYSRCPATH}"          ; ← 这里才开始拷 rime-sync
File ...resources...
!insertmacro NSIS_HOOK_POSTINSTALL    ; ← 只建快捷方式
```

因此 **PREINSTALL 阶段读不到 `$INSTDIR\resources`**（resources 是后面才解压的）。
做法：

1. 钩子里用 `${__FILEDIR__}`（Tauri 把 `installerHooks` canonicalize 成绝对路径再
   `!include`，所以它等于 `<repo>\packaging`）定位
   `windows\vendor\weasel-0.17.4.0-installer.exe`；
2. 在 `PREINSTALL` 里用 NSIS `File` 指令把它解到 `$PLUGINSDIR\rime-sync-weasel\`；
3. `ExecWait '"...\weasel-....exe" /S' $0` 等输入法装完，再 `SetOutPath $INSTDIR` 交还
   给 Tauri 继续拷程序文件。

可复核的证据：

| 证据 | 位置 |
|---|---|
| 编译期：内嵌缺失告警 | makensis 输出里的 `rime-sync: vendored weasel installer not found`（`!warning`） |
| 运行期：顺序 + 退出码 | `%ProgramFiles%\rime-sync\install-ime-order.log`（单行记录，含 `phase=IME-INSTALL`、`exit=0`、`phase=POSTINSTALL`） |
| 运行期：统一安装日志 | `%TEMP%\rime-sync-setup.log`（UTF-16LE，同一份内容 + 阶段名 `OS-CHECK` / `WEBVIEW-CHECK` / `WEBVIEW-INSTALL` / `IME-INSTALL` / `POSTINSTALL` / `DONE`；失败路径也写） |
| 运行期：安装日志 | `DetailPrint` 里的 `[rime-sync] [YYYY-MM-DD HH:MM:SS] 阶段=...` |
| 产物体积 | 最终 `*-setup.exe` ≥ 15 MiB（CI 断言；内嵌的 Weasel 安装器约 12 MiB） |

静默安装看不到 DetailPrint，所以顺序日志写文件；卸载时删除该文件。

卸载相关的证据：

| 证据 | 位置 |
|---|---|
| 所有权标记 | `HKLM\Software\rime-sync` 的 `BundledWeasel` / `WeaselRoot` / `WeaselUninstall` / `BundledAt` |
| 安装日志 | `%ProgramFiles%\rime-sync\install-ime-order.log` 里的 `phase=PREINSTALL ownership marker written` |
| 卸载日志 | `%TEMP%\rime-sync-uninstall.log`（卸载后仍在，逐行带 `phase=PREUNINSTALL`） |

## 4. CI（release.yml · windows job）

| 步骤 | 作用 |
|---|---|
| `cache weasel build deps (boost 1.84.0)` | 缓存 `third_party/weasel/deps/boost_1_84_0/{boost,stage}` |
| `resolve weasel mode` | 解析 `RIMESYNC_WEASEL_MODE`：tag 推送强制 `require`；手动按 `workflow_dispatch.weasel_mode` |
| `build weasel installer from vendored source` | 调 `build-weasel.ps1`；`continue-on-error` 只在 `build`/`fetch` 模式为真，`require` 下失败即 job 失败 |
| `fetch upstream weasel installer (fallback / offline mode)` | `build` 模式自编译失败、或 `fetch` 模式时走官方安装包；**绝不**在 `require` 下运行 |
| `verify weasel vendor payload` | 必须有 `weasel-*.exe` 且 ≥ 10 MiB；`require` 下来源必须是自编译；写 `packaging/out/WEASEL-SOURCE.json` 并红字标注回退 |
| `verify-artifacts`（独立 job） | 解 `*-setup.exe` → `WeaselServer.exe`，搜 UTF-16LE「Rime 同步」；`require` 下缺失即失败，publish 不会跑 |
| `assert nsis setup embeds weasel` | 在线 setup ≥ 12 MiB（内嵌 Weasel） |
| `stash online nsis artifacts` | 在线包先挪到 `packaging/out/nsis-online/`（第二遍构建会写回同名文件） |
| `fetch WebView2 standalone payload` | `fetch-webview2.ps1`：3 次重试；< 100 MiB 或非 `MZ` 直接失败 |
| `build nsis (offline variant, embeds WebView2)` | `pnpm tauri build --bundles nsis --config src-tauri/tauri.offline.conf.json` |
| `rename and assert offline bundle` | 改名 `*-offline-setup.exe`（`.sig` 同步）+ 断言 ≥ 100 MiB |
| `collect` 的 Windows 断言 | 在线包 ≥ 12 MiB、离线包 ≥ 100 MiB、两者 `.sig` 都在 |
| `verify-artifacts` 的离线断言 | `--require-offline-setup`：存在性 + 体积 + 内嵌 WebView2/Weasel 两个载荷 |

在 CI 上确认自编译成功的检查项：

1. `build weasel installer from vendored source` 步骤为绿色，日志末行有
   `[ok] ...\weasel-0.17.4.0-installer.exe` 与 `sha256=...`；
2. `verify weasel vendor payload` 打印 `weasel source: self-built from third_party/weasel @ 0.17.4`；
3. makensis 日志里**没有** `vendored weasel installer not found` 告警；
4. `assert nsis setup embeds weasel` 打印的 MiB 数 ≥ 15；
5. 真机静默安装后 `%ProgramFiles%\rime-sync\install-ime-order.log` 含
   `phase=PREINSTALL weasel installer exit=0`，且 `phase=POSTINSTALL` 在其后。

## 5. 回退方案（自编译失败）

规则见 [`../../docs/FALLBACK-POLICY.md`](../../docs/FALLBACK-POLICY.md)：回退必须
**写日志 + 对外可见 + 可用参数关闭**。v0.6.0 就是在这里静默回退到上游官方包，
结果 Windows 版没有「Rime 同步…」入口却照样发布。

`RIMESYNC_WEASEL_MODE`（`release.yml` 的 `windows` job）：

| 模式 | 何时 | 自编译失败时 | 可见性 |
|---|---|---|---|
| `require` | **tag 推送默认**；手动显式选择 | **job 失败，不回退，不发布** | — |
| `build` | 手动默认（仓库变量 `RIMESYNC_WEASEL_MODE` 缺省值） | 回退上游官方包 | job summary `> [!CAUTION]` + 发布说明顶部红字 + `WEASEL-SOURCE.json` |
| `fetch` | 手动显式选择（离线/应急） | 直接用上游官方包 | 同上 |

- 手动触发：Actions → release → Run workflow → `weasel_mode` 选 `require` / `build` / `fetch`。
- 本地等价：`pwsh -File packaging/windows/fetch-weasel.ps1` 后再 `pnpm tauri build --bundles nsis`
  （本地构建没有 summary，必须自己记得这一版没有入口）。
- 两条路径产出的文件名一致（`weasel-0.17.4.0-installer.exe`），钩子与 resources 布局不用改。
- 产物验收：`bash .github/scripts/verify-artifacts.sh --dir dist`（严格）或
  `--weasel-mode build`（放行回退但红字告警）。

## 6. 无法在本机验证的点（必须靠 CI / 真机）

- 本机（Linux 开发机）没有 MSVC / NSIS / pwsh：`build-weasel.ps1` 只做了人工审读，
  **PowerShell 语法检查与整链验证在 CI**。本地可用的替代检查：
  `pwsh -NoProfile -Command "[void][scriptblock]::Create((Get-Content -Raw scripts/ime/build-weasel.ps1))"`。
- Weasel 在无交互的 CI 会话里安装是否完整注册 TSF（可能需重启 explorer）未验证。
- `get-rime.ps1` 依赖 `api.github.com`：限流/网络问题会失败（CI 传 `GITHUB_TOKEN` 缓解）。
- boost 源码下载（`archives.boost.io`，约 120MB）可能慢；缓存命中可跳过。
- 自编译安装包不含输入方案（plum 为空子模块）；需要方案时用 `-WithRimeData`
  或走官方安装包，见 `docs/THIRD-PARTY.md`。

- 自编译安装包内置 third_party/rime-data 的 16 个方案（拼音/五笔/仓颉/笔画/地球/注音）；
  需要更多方案时用 -WithRimeData 走 plum，见 docs/THIRD-PARTY.md。
- 0.6.5 新增：WebView2 的真实探测/安装/弹框只做了 makensis 编译期验证，
  受限代理真机、静默安装中止路径、`%TEMP%\rime-sync-setup.log` 的 UTF-16LE 追加
  （BOM 只写一次）都要在真机复核，见第 8 节。

## 7. 卸载

`NSIS_HOOK_PREUNINSTALL` 是 Tauri `Section Uninstall` 的第一条语句（此时 `$INSTDIR` 还在），
顺序是「先问是否删本地用户数据（默认否）→ 卸小狼毫（仅当是我们装的）→ 删 rime-sync 自己的
快捷方式/日志 → 最后按选择删 `%APPDATA%\Rime` 与 `%APPDATA%\com.rimesync.app`」。

| 阶段 | 行为 |
|---|---|
| 所有权 | 只在「安装前没有小狼毫、内嵌安装器退出码 0」时写 `HKLM\Software\rime-sync`：`BundledWeasel=1`、`WeaselRoot`、`WeaselUninstall`、`BundledAt`（64 位视图） |
| 判断 | 读不到 `BundledWeasel=1` → 保留小狼毫，只清理 rime-sync 自己 |
| 保留选项 | `uninstall.exe /KEEPWEASEL` 或环境变量 `RIMESYNC_KEEP_WEASEL=1`：跳过小狼毫卸载并删除标记（所有权交还用户） |
| 第 1 次尝试 | `nsExec::ExecToLog '$R9 /S'`，`$R9` = 标记里的 UninstallString（Weasel 自己的卸载器，键在 32 位视图 `...\Uninstall\Weasel`） |
| 第 2 次尝试 | 退出码非 0 / error / timeout 时 `nsExec::ExecToLog '"$R8\uninstall.exe" /S'`；与第 1 次同一条命令则不重跑 |
| 失败处理 | 只 `DetailPrint` + 写日志，**不 Abort**，rime-sync 继续卸载；标记照样删除 |
| 用户数据（默认） | 默认**保留** `%APPDATA%\Rime` 与 `%APPDATA%\com.rimesync.app`；Weasel 卸载器也不删它们（`third_party/weasel/output/install.nsi` 的 `Section "Uninstall"`） |
| 删数据选项 | 交互卸载弹 `MessageBox MB_YESNO\|MB_ICONEXCLAMATION`「是否同时删除本地用户数据？」，默认「否」；静默卸载只在命令行带 `/PURGEDATA` 时删（`IfSilent` + `${GetOptions}`） |
| 删除范围 | 只有 `%APPDATA%\Rime` 与 `%APPDATA%\com.rimesync.app` 两条（父目录不碰）；先 `RMDir /r` 再 `IfFileExists` 复查，失败只写日志、不 Abort |
| 删数据日志 | `%TEMP%\rime-sync-uninstall.log` 里的 `purge-userdata=yes|no`、`purge-userdata removed|failed <path>` |

删数据的目标目录取**进程环境变量** `%APPDATA%`（`ReadEnvStr`），
不用 NSIS 的 `$APPDATA`：perMachine 卸载器执行过 `SetShellVarContext all`，
shell 变量可能被映射到「所有用户」上下文。读不到 `%APPDATA%` 时一条都不删。

真机验证点（本机 Linux 只能验 NSIS 语法，见第 6 节）：

1. 干净系统装 rime-sync → `reg query HKLM\Software\rime-sync` 有 4 个值；卸载 → 小狼毫消失、
   `%APPDATA%\Rime` 仍在、`HKLM\Software\rime-sync` 被删空。
2. 先手动装 Weasel 再装 rime-sync → 没有标记；卸载 → Weasel 仍在。
3. `uninstall.exe /KEEPWEASEL` → Weasel 仍在，标记被删。
4. 卸载时把 `<WeaselRoot>\uninstall.exe` 改名 → 第 1 次尝试报 error、第 2 次找不到文件，
   日志里是 `weasel uninstall failed` / `weasel uninstaller not found`，rime-sync 仍被正常卸载。
5. 卸载后再装一次 rime-sync → 会重新装小狼毫并重新写标记。
6. 交互卸载（直接跑 uninstaller.exe）→ 出现「是否同时删除本地用户数据？」；点「否」→
   `%APPDATA%\Rime` 与 `%APPDATA%\com.rimesync.app` 都在，日志是 `purge-userdata=no`；
   点「是」→ 两条目录都没了，日志是 `purge-userdata=yes source=prompt`。
7. 静默卸载：`uninstall.exe /S` → 用户数据全保留（日志 `purge-userdata=no`）；
   `uninstall.exe /S /PURGEDATA` → 两条目录都被删（日志 `purge-userdata=yes source=/PURGEDATA`）。
8. 把 `%APPDATA%\Rime` 里的文件用别的进程占住（或设只读）再 `/S /PURGEDATA` →
   日志出现 `purge-userdata failed <path>`，卸载本身仍正常结束（不 Abort）。

## 8. 受限代理 / 内网安装（0.6.5）

0.6.4 及以前 `bundle.windows.webviewInstallMode` 是
`{ "type": "embedBootstrapper", "silent": true }`：机器上没有 WebView2 时，安装器会
**静默联网**下载约 130 MB 运行时。走受限代理时装不上、反复重试，安装器就卡在
`WebView2Setup`，没有提示也没有日志（0.6.4 用户实测）。

0.6.5 的改动：

| 项 | 0.6.4 | 0.6.5 |
|---|---|---|
| Tauri `webviewInstallMode` | `embedBootstrapper`（静默联网下载） | `skip`（Tauri 完全不碰 WebView2） |
| WebView2 检测 | 无 | PREINSTALL 第一步：`HKLM`/`HKCU` × 64/32 位视图读 `EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}\pv`，另用 `EdgeWebView\Application\*\msedgewebview2.exe` 兜底 |
| 未安装时 | 静默联网下载，无提示 | ① 离线包 → 内嵌 Standalone 静默安装；② 带 `/SKIPWEBVIEW` → 只写日志继续；③ 在线包 → 弹框（中英按 `$LANGUAGE`）给下载地址，继续或退出（退出 = Abort） |
| 安装日志 | `$INSTDIR\install-ime-order.log` | 另加 `%TEMP%\rime-sync-setup.log`（UTF-16LE，阶段名 `OS-CHECK` / `WEBVIEW-CHECK` / `WEBVIEW-INSTALL` / `IME-INSTALL` / `POSTINSTALL` / `DONE`；失败路径同样写） |
| Windows 版本门槛 | 无 | 低于 Windows 10 1803（10.0.17134）：弹框说明并 Abort |

**任何路径都不再联网下载**：在线包只把地址打印在弹框里，离线包用内嵌的 Standalone 安装器。

### 8.1 两个变体

```text
rime-sync_<ver>_x64-setup.exe          在线包（默认）：缺 WebView2 时弹框给地址，不下载
rime-sync_<ver>_x64-offline-setup.exe  离线包：内嵌 WebView2 Standalone + Weasel，全程不联网
```

离线包由 `src-tauri/tauri.offline.conf.json` 选用 `packaging/nsis-hooks-offline.nsh`：
该文件第一行 `!define RIMESYNC_EMBED_WEBVIEW`，然后 include 主钩子 —— 其余逻辑完全相同。

### 8.2 安装器参数

| 参数 | 作用 |
|---|---|
| `/SKIPWEBVIEW` | 缺 WebView2 也不阻断：写日志后继续（应用可能起不来，用户自己装好再运行） |
| `/SKIPIME` | 跳过内嵌小狼毫安装（已有输入法、或不想让安装器碰输入法）；比"已存在就跳过"更强制 |
| `/S` | 静默安装。**静默 + 缺 WebView2 + 没给 `/SKIPWEBVIEW` → 直接中止**（不弹框、不联网），避免无人值守时装出跑不起来的程序 |
| `/KEEPWEASEL` / `/PURGEDATA` | 卸载参数，见第 7 节 |

```powershell
# 内网机器：WebView2 已由域内软件分发推送过
rime-sync_0.6.5_x64-setup.exe /S /SKIPWEBVIEW /SKIPIME
```

### 8.3 手动装 WebView2（受限代理）

1. 在能上网的机器下载 **Evergreen Standalone**（约 130 MB）：
   <https://go.microsoft.com/fwlink/?linkid=2124701>
   （在线引导器 <https://go.microsoft.com/fwlink/?linkid=2124703> 只有 1.5 MB，但它自己还要联网，
   受限代理下同样会卡 —— 内网请用 Standalone。）
2. 拷到目标机器，管理员运行：`MicrosoftEdgeWebView2RuntimeInstallerX64.exe /silent /install`。
3. 复查（有 `pv` 即已装）：
   ```bat
   reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" /v pv
   ```
4. 再运行 rime-sync 安装包（或直接用离线包）。

### 8.4 离线包怎么打（CI）

`release.yml` 的 windows job 现在打两遍 NSIS：

1. `pnpm tauri build --bundles nsis` → 在线包；`stash online nsis artifacts` 把它挪到
   `packaging/out/nsis-online/`（否则会被第二遍的同名产物覆盖）。
2. `pwsh -File packaging/windows/fetch-webview2.ps1`：下载 Standalone 到
   `packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe`
   （3 次重试；< 100 MiB 或文件头不是 `MZ` 直接失败，防止拿到在线引导器/错误页）。
3. `pnpm tauri build --bundles nsis --config src-tauri/tauri.offline.conf.json`
   → 第二遍只换 `installerHooks`。
4. `rename and assert offline bundle`：改名 `*-offline-setup.exe`（`.sig` 同步改名），
   断言 ≥ 100 MiB；随后 `collect`（资产名 + 体积）与 `verify-artifacts.sh
   --require-offline-setup`（解包查 WebView2 + Weasel 两个载荷）各再兜一层。

本机等价（Windows + NSIS）：

```powershell
pwsh -File packaging/windows/fetch-webview2.ps1
pnpm tauri build --bundles nsis --config src-tauri/tauri.offline.conf.json
```

体积：在线包约 12–15 MiB（内嵌 Weasel）；离线包约 140–160 MiB
（WebView2 Standalone ≈ 130 MiB + Weasel ≈ 12 MiB，NSIS LZMA 压缩后；断言下限取 100 MiB）。

> 离线包**不进** `latest.json`：`.github/scripts/make-latest-json.py` 显式跳过
> `-offline-setup.exe`，updater 只把在线包当更新目标。

### 8.5 本机（Linux）能验到哪一步

`packaging/windows/check-nsis-hooks.sh` 会现场生成一个最小 harness（桩掉 Tauri 模板的
`PRODUCTNAME` / `MAINBINARYNAME` / `MAINBINARYSRCPATH` 与两个 `NSIS_HOOK_*` 插入点，
`vendor/` 下放假载荷），用 `makensis -WX`（告警即错误）分别编译在线与离线钩子：

```sh
bash packaging/windows/check-nsis-hooks.sh            # 没有 makensis 时打印 SKIP
bash packaging/windows/check-nsis-hooks.sh --strict   # 没有 makensis 视为失败
```

输出示例（0 warning 0 error；离线包体积比在线包正好大一个假载荷 = 确实内嵌了 Standalone）：

```text
ok   online     144568 bytes  warnings=0
ok   offline   1193178 bytes  warnings=0
delta     1048610 bytes（离线包 - 在线包；假载荷 1048576 字节，应≈它）
check-nsis-hooks: 两套钩子均 0 warning 0 error
```

离线 harness 缺 payload 时，编译期应当出现
`rime-sync: offline build ... without WebView2 payload` 告警（构建缺陷的编译期信号）。

真机仍需复核（本机无法验证）：注册表 4 种视图组合在真实机器上的命中、
Standalone 静默安装的退出码/重启提示、弹框按钮语义、静默中止的退出码、
`%TEMP%\rime-sync-setup.log` 的 UTF-16 BOM 只写一次。

