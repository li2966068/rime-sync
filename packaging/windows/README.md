# Windows 打包（自编译小狼毫 + 先装输入法）

目标：一个安装包装完即有小狼毫，且安装顺序是「先装输入法、再装 Rime 同步」。

## 1. 文件

| 文件 | 作用 |
|---|---|
| `../../scripts/ime/build-weasel.ps1` | **主路径**：用 MSVC 从 `third_party/weasel` 编译 Weasel，产出安装包 |
| `fetch-weasel.ps1` | **离线兜底**：下载官方 `weasel-0.17.4.0-installer.exe`，校验大小与 SHA-256 |
| `vendor/` | 构建产物（`*.exe` 不入库）+ GPLv3 全文 + 来源说明 |
| `../nsis-hooks.nsh` | NSIS 钩子：PREINSTALL 装输入法并记所有权、POSTINSTALL 建快捷方式、PREUNINSTALL 卸载输入法（仅当是 rime-sync 装的） |
| `../../src-tauri/tauri.conf.json` | `bundle.resources` 只打包许可证/说明；`installMode = perMachine` |

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
   先把 `output/install.nsi` 生成一份 `output/install.rime-sync.nsi`（只把两行 OpenCC 数据的
   `File` 改成 `/nonfatal`，见下），再编译它
   （`/DWEASEL_VERSION=0.17.4 /DWEASEL_BUILD=0 /DPRODUCT_VERSION=0.17.4.0`）。
8. **产物断言**：复制到 `vendor/weasel-0.17.4.0-installer.exe`，打印大小与 SHA-256，
   低于 `-MinInstallerMB`（默认 8 MiB）直接失败；写了 `GITHUB_STEP_SUMMARY`（CI 摘要）。

> **OpenCC 数据（构建期改脚本的原因）**
> librime 的 Windows 预编译包不含 `share/opencc`（那是 librime 构建时生成的），
> vendored 树里 `librime/` 又是空子模块。上游 `install.nsi` 对
> `File "data\opencc\*.json"` / `*.ocd*` 用的是**硬性** File（通配符没匹配到就中止编译），
> 所以构建期生成一份只把这两行改成 `/nonfatal` 的 `install.rime-sync.nsi` 再编译，
> **不改 vendored 源码**。后果：自编译安装包没有 OpenCC 转换数据（简繁转换等特性不可用），
> 与「不含输入方案」同属已知差异，见 `docs/THIRD-PARTY.md`。

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
| 运行期：顺序 + 退出码 | `%ProgramFiles%\rime-sync\install-ime-order.log`（UTF-8/ANSI 单行记录，含 `phase=PREINSTALL`、`exit=0`、`phase=POSTINSTALL`） |
| 运行期：安装日志 | `DetailPrint` 里的 `[rime-sync] [YYYY-MM-DD HH:MM:SS] 阶段=PREINSTALL ...` |
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
| `build weasel installer from vendored source` | 调 `build-weasel.ps1`，`continue-on-error`（失败不直接红） |
| `fetch upstream weasel installer (fallback / offline mode)` | 自编译失败或 `RIMESYNC_WEASEL_MODE=fetch` 时走官方安装包 |
| `verify weasel vendor payload` | 必须有 `weasel-*.exe` 且 ≥ 10 MiB；摘要记录来源 |
| `assert nsis setup embeds weasel` | 最终 setup ≥ 15 MiB |

在 CI 上确认自编译成功的检查项：

1. `build weasel installer from vendored source` 步骤为绿色，日志末行有
   `[ok] ...\weasel-0.17.4.0-installer.exe` 与 `sha256=...`；
2. `verify weasel vendor payload` 打印 `weasel source: self-built from third_party/weasel @ 0.17.4`；
3. makensis 日志里**没有** `vendored weasel installer not found` 告警；
4. `assert nsis setup embeds weasel` 打印的 MiB 数 ≥ 15；
5. 真机静默安装后 `%ProgramFiles%\rime-sync\install-ime-order.log` 含
   `phase=PREINSTALL weasel installer exit=0`，且 `phase=POSTINSTALL` 在其后。

## 5. 回退方案（自编译失败）

任选其一：

- **让 CI 自动兜底**：什么都不用做。自编译步骤失败时下一步自动跑
  `fetch-weasel.ps1`，摘要里注明 `upstream official installer`。
- **强制走官方安装包**：把仓库变量 `RIMESYNC_WEASEL_MODE` 设为 `fetch`
  （Settings → Secrets and variables → Actions → Variables），或本地
  `pwsh -File packaging/windows/fetch-weasel.ps1` 后再 `pnpm tauri build --bundles nsis`。
- 两条路径产出的文件名一致（`weasel-0.17.4.0-installer.exe`），
  钩子与 resources 布局不用改。

## 6. 无法在本机验证的点（必须靠 CI / 真机）

- 本机（Linux 开发机）没有 MSVC / NSIS / pwsh：`build-weasel.ps1` 只做了人工审读，
  **PowerShell 语法检查与整链验证在 CI**。本地可用的替代检查：
  `pwsh -NoProfile -Command "[void][scriptblock]::Create((Get-Content -Raw scripts/ime/build-weasel.ps1))"`。
- Weasel 在无交互的 CI 会话里安装是否完整注册 TSF（可能需重启 explorer）未验证。
- `get-rime.ps1` 依赖 `api.github.com`：限流/网络问题会失败（CI 传 `GITHUB_TOKEN` 缓解）。
- boost 源码下载（`archives.boost.io`，约 120MB）可能慢；缓存命中可跳过。
- 自编译安装包不含输入方案（plum 为空子模块）；需要方案时用 `-WithRimeData`
  或走官方安装包，见 `docs/THIRD-PARTY.md`。

## 7. 卸载顺序（实现与证据点）

`NSIS_HOOK_PREUNINSTALL` 是 Tauri `Section Uninstall` 的第一条语句（此时 `$INSTDIR` 还在），
顺序是「先卸小狼毫（仅当是我们装的）→ 再删 rime-sync 自己的快捷方式/日志」。

| 阶段 | 行为 |
|---|---|
| 所有权 | 只在「安装前没有小狼毫、内嵌安装器退出码 0」时写 `HKLM\Software\rime-sync`：`BundledWeasel=1`、`WeaselRoot`、`WeaselUninstall`、`BundledAt`（64 位视图） |
| 判断 | 读不到 `BundledWeasel=1` → 保留小狼毫，只清理 rime-sync 自己 |
| 保留选项 | `uninstall.exe /KEEPWEASEL` 或环境变量 `RIMESYNC_KEEP_WEASEL=1`：跳过小狼毫卸载并删除标记（所有权交还用户） |
| 第 1 次尝试 | `nsExec::ExecToLog '$R9 /S'`，`$R9` = 标记里的 UninstallString（Weasel 自己的卸载器，键在 32 位视图 `...\Uninstall\Weasel`） |
| 第 2 次尝试 | 退出码非 0 / error / timeout 时 `nsExec::ExecToLog '"$R8\uninstall.exe" /S'`；与第 1 次同一条命令则不重跑 |
| 失败处理 | 只 `DetailPrint` + 写日志，**不 Abort**，rime-sync 继续卸载；标记照样删除 |
| 用户数据 | 钩子不删 `%APPDATA%\Rime` 与 rime-sync 配置；Weasel 卸载器也不删（`third_party/weasel/output/install.nsi` 的 `Section "Uninstall"`） |

真机验证点（本机 Linux 只能验 NSIS 语法，见第 6 节）：

1. 干净系统装 rime-sync → `reg query HKLM\Software\rime-sync` 有 4 个值；卸载 → 小狼毫消失、
   `%APPDATA%\Rime` 仍在、`HKLM\Software\rime-sync` 被删空。
2. 先手动装 Weasel 再装 rime-sync → 没有标记；卸载 → Weasel 仍在。
3. `uninstall.exe /KEEPWEASEL` → Weasel 仍在，标记被删。
4. 卸载时把 `<WeaselRoot>\uninstall.exe` 改名 → 第 1 次尝试报 error、第 2 次找不到文件，
   日志里是 `weasel uninstall failed` / `weasel uninstaller not found`，rime-sync 仍被正常卸载。
5. 卸载后再装一次 rime-sync → 会重新装小狼毫并重新写标记。
