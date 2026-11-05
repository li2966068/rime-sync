# Windows 打包（自编译小狼毫 + 先装输入法）

目标：一个安装包装完即有小狼毫，且安装顺序是「先装输入法、再装 Rime 同步」。

## 1. 文件

| 文件 | 作用 |
|---|---|
| `../../scripts/ime/build-weasel.ps1` | **主路径**：用 MSVC 从 `third_party/weasel` 编译 Weasel，产出安装包 |
| `../../scripts/ime/build-librime-win.ps1` | **0.6.9 起**：从 `third_party/librime`（vendored 1.17.0）自建 librime，把 `plugins/rimesync-extra` 合并进 rime.dll，产出 `lib64/lib/output/output\\Win32/include` 布局 |
| `fetch-weasel.ps1` | **离线兜底**：下载官方 `weasel-0.17.4.0-installer.exe`，校验大小与 SHA-256 |
| `vendor/` | 构建产物（`*.exe` 不入库）+ GPLv3 全文 + 来源说明 |
| `../nsis-hooks.nsh` | NSIS 钩子：PREINSTALL 检查系统版本 → **卸载旧版（同步程序 + 自建输入法）** → 检查/安装 WebView2 → 装输入法并记所有权；POSTINSTALL 建快捷方式 + 按新版无条件写配置（覆盖前备份）；PREUNINSTALL 卸载输入法（仅当是 rime-sync 装的） |
| `../nsis-hooks-offline.nsh` | 离线变体：`!define RIMESYNC_EMBED_WEBVIEW` 后 include 主钩子（内嵌 WebView2 Standalone） |
| `fetch-webview2.ps1` | 下载 WebView2 Evergreen **Standalone**（x64，约 130 MB）供离线包内嵌；3 次重试 + 体积/文件头校验 |
| `../../src-tauri/tauri.conf.json` | `bundle.resources` 只打包许可证/说明；`installMode = perMachine`；`webviewInstallMode = skip` |
| `../../src-tauri/tauri.offline.conf.json` | 离线包覆盖项：只改 `bundle.windows.nsis.installerHooks` 指向离线钩子 |

> 为什么 resources 里没有 `weasel-*.exe`：安装器用 NSIS 的 `File` 指令在**构建期**把
> 它内嵌进 setup.exe（见第 3 节），再放进 resources 会让同一个 12MB 文件进包两次。

## 2. 构建（CI 由 release.yml 的 windows job 执行）

```powershell
# 自编译（主路径）：先自建 librime（合并 rimesync-extra），再编 Weasel
pwsh -NoProfile -File scripts/ime/build-weasel.ps1
# 只重建 librime（CI 在编 Weasel 之前单独跑这一步，见下文「自建 librime」）
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1
# 只做前置检查，不下载/不编译
pwsh -NoProfile -File scripts/ime/build-weasel.ps1 -Check
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1 -Check

# 然后是 Tauri 打包
pnpm tauri build --bundles nsis
```

脚本会依次做：

1. **前置检查**：`weasel.sln`、`output/install.nsi`、`render.js`、`output/7z.exe`、
   `curl.exe`、`WinSparkle.dll`、`resource/weasel.ico` 等（`get-rime.ps1` 只在
   `-LibrimeSource prebuilt` 时才是必需项）。
2. **boost 1.84.0**：下载源码（`archives.boost.io`，用 vendored 的 `output/7z.exe` 解压），
   `bootstrap.bat` + `b2` 静态运行时构建 x64 与 x86 两份 `stage\lib`。
   CI 用 `actions/cache` 缓存 `deps/boost_1_84_0/{boost,stage}`，命中则整体跳过。
3. **librime（0.6.9 起默认自建）**：调 `build-librime-win.ps1` —— 按 pin 浅克隆 deps，
   用 librime 自带的 `build.bat` 编 x64 与 Win32（`BUILD_MERGED_PLUGINS=ON` 把
   `plugins/rimesync-extra` 合并进 rime.dll），再落到 `lib64/`、`lib/`、`output/`、
   `output/Win32/`、`include/`，opencc 数据放进 `output/data/opencc/`。
   布局已就绪且 rime.dll 含插件组件名时直接复用（`-BuildLibrime` 强制重编）。
   应急退路：`-LibrimeSource prebuilt -LibrimeTag 1.13.1` 走原来的 `get-rime.ps1`。
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
| librime | **1.17.0 自建**（`third_party/librime` + 合并 `rimesync-extra`） | 本仓库 vendored 源码 + 上游 deps（见下） |
| librime（应急） | 1.13.1 预编译 msvc 包 | rime/librime releases（`-LibrimeSource prebuilt`） |
| NSIS | 3.08 | 脚本自动安装 |

**Weasel 层没有 CMake**：Weasel 0.17.4 上游没有 Weasel 层的 CMakeLists.txt（官方是
`build.bat`/MSBuild 或 `xbuild.bat`/xmake）。CMake 只出现在 librime：0.6.9 起自建
librime 用的是上游的 `third_party/librime/build.bat`，它内部就是用 CMake + MSBuild 编
deps 与 librime，我们只负责喂 `env.bat` 与几个前缀变量（见下节）。

### 自建 librime（0.6.9 起）

**为什么必须自建**：上游预编译包里没有外部插件加载逻辑 —— librime 的
`plugins/PluginManager` 在 `_WIN32` 下还是 TODO（`current_module_path()` 直接返回空路径），
所以 `plugins/rimesync-extra`（v 模式数字转换、标点 Shift 备选、a-h 选词）只有以
`BUILD_MERGED_PLUGINS=ON`（librime 默认值）**合并进 rime.dll** 才对 Weasel 生效。
0.6.8 的 Windows 包用的是预编译 1.13.1，所以这三项在 Windows 上完全不生效（标点直出靠配置，
仍然生效）。

**deps pin**（`third_party/librime/.gitmodules` 声明但目录为空，脚本按 tag 浅克隆到
`third_party/librime/deps/<name>`；tag 已核对上游实际命名）：

| deps 目录 | 上游 | tag | 许可证 |
|---|---|---|---|
| `glog` | <https://github.com/google/glog> | `v0.6.0` | BSD-3-Clause |
| `leveldb` | <https://github.com/google/leveldb> | `1.23` | BSD-3-Clause |
| `yaml-cpp` | <https://github.com/jbeder/yaml-cpp> | `0.8.0` | MIT |
| `marisa-trie` | <https://github.com/s-yata/marisa-trie> | `v0.2.6` | BSD-2-Clause / LGPL-2.1 |
| `opencc` | <https://github.com/BYVoid/OpenCC> | `ver.1.1.7` | Apache-2.0 |
| `googletest` | <https://github.com/google/googletest> | `v1.14.0` | BSD-3-Clause（**默认不克隆**，见下） |

- `googletest` 只在 `-WithTests` 时真实克隆；`build.bat deps` 会无条件编它，所以默认给一个
  最小占位工程（`project(... NONE)` + 一条 `install(FILES)`）让它空转，编完即删。
- 只有 `leveldb` 声明了 submodule（gtest/benchmark），两者都在 `LEVELDB_BUILD_TESTS` /
  `LEVELDB_BUILD_BENCHMARKS` 打开时才 `add_subdirectory`，脚本两个都关掉，
  因此不需要 `--recurse-submodules`。

**env.bat 是唯一定制点**：`build.bat` 会无条件 `call env.bat`，而仓库里的
`env.bat.template` 把 `ARCH`/`CMAKE_GENERATOR`/`PLATFORM_TOOLSET` 硬编码成
`Win32` + VS2019 + v142（普通 `set`，会覆盖外部环境变量）。所以脚本自己生成
`third_party/librime/env.bat`：

```bat
set RIME_ROOT=%CD%
set BOOST_ROOT=<third_party\weasel\deps\boost_1_84_0>
set ARCH=x64            rem 或 Win32
set CMAKE_GENERATOR="Visual Studio 17 2022"
set PLATFORM_TOOLSET=v143
set BJAM_TOOLSET=msvc-14.3
```

随后每次 `build.bat` 调用只额外喂三个前缀变量（它们用 `if not defined`，外部传入优先）：

| 变量 | x64 | Win32 | 说明 |
|---|---|---|---|
| `build_dir` | `build\x64` | `build\win32` | 每个 deps 目录与 librime 根目录各一份 |
| `deps_install_prefix` | `build\deps-x64` | `build\deps-win32` | 静态 deps 的安装前缀（MSVC CRT/架构不能混用） |
| `rime_install_prefix` | `dist\x64` | `dist\Win32` | librime 的 `dist`（`lib\rime.dll`、`lib\rime.lib`、`include\*.h`） |

`build` 与 `dist` 都在 `third_party/librime/.gitignore` 里，仓库工作区不会多出未跟踪文件。

**断言（失败即 `throw`，不是 warning）**：

1. 两个 `rime.dll` 都含 `rimesync_vnum` / `rimesync_punct` / `rimesync_vselect` 明文
   （组件名是 registry 注册用的普通字符串常量，UTF-8 即 ASCII）；缺了就是插件没合进去 ——
   这时 Windows 上的 v 模式 / 标点 Shift 备选 / a-h 选词 会静默失效，正是 0.6.8 的状态。
2. 两个 `rime.dll` 的 PE 架构分别是 x64（`0x8664`）与 x86（`0x14c`）—— 防止 `ARCH` 被
   模板覆盖后把 x64 DLL 摆进 `output\Win32`（msbuild 不会报错，装上才发现 32 位程序加载失败）。
3. 摆进 Weasel 后重新断言一遍 5 个布局文件与两个 DLL 的组件名。
4. 构建记录写到 `third_party/librime/dist/LIBRIME-BUILD.json`（版本、boost、工具集、
   每个 dep 的 tag 与 commit、两个架构的 DLL 路径），并写进 job summary。

**缓存与耗时**（`release.yml` 的 `cache self-built librime (deps + build dirs)`）：

- 缓存 `third_party/librime/deps/*/build` 与 `third_party/librime/build`（MSVC 增量编译目录）；
  **不缓存** `dist/`：`--target install` 会立刻重建它，缓存它反而可能留下别的架构的 DLL。
- key 含 `librime-1.17.0` + 五个 deps 的 tag + `build-librime-win.ps1` 的哈希；
  `restore-keys` 按 `<os>-librime-1.17.0-` 兜底，所以改脚本不会每次都从零编（但会重新 configure）。
- 冷缓存（首跑 / deps pin 变更）：deps 两个架构约 18-25 分钟、librime 两个架构约 6-10 分钟；
  热缓存：约 2-4 分钟。job 超时因此从 120 提到 150 分钟，librime 步骤单独给 50 分钟。

**失败排查**：

| 现象 | 原因 / 处理 |
|---|---|
| `build.bat deps` 在 `deps\googletest` 报错 | 占位工程在 VS 生成器下没给出 install 目标；加 `-WithTests` 让它真实编 gtest，或手工 `cmake . -Bbuild` 看输出 |
| `deps/xxx 克隆失败` | 网络 / 代理；tag 名变更（`marisa-trie` 是 `v0.2.6`、OpenCC 是 `ver.1.1.7`，不是 `0.2.6`/`1.1.7`） |
| `PE 架构是 x64，期望 x86` | `env.bat` 被 `env.bat.template` 覆盖（`ARCH=Win32` 没生效）；删掉 `third_party/librime/env.bat` 重跑 |
| `搜不到插件组件名` | `build.bat librime` 的 cmake 输出里应出现 `Found plugin: rimesync-extra`；检查 `BUILD_MERGED_PLUGINS` 没被关掉、`plugins/rimesync-extra` 还在 |
| msbuild 链接 `rime.lib` 失败 | 布局没摆对（`lib64\rime.lib` 给 x64、`lib\rime.lib` 给 Win32）；跑 `build-librime-win.ps1 -Check` 看 `lib64/lib/output` 行 |
| librime 1.17 头文件与 Weasel 0.17.4 不兼容 | 头文件是随包编的（`include/` 来自同一份源码），编译期就能暴露；不要混用预编译 include |

**本地只编 librime（不编 Weasel）**：

```powershell
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1 -Check        # 只看 pin/boost/VS 状态
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1              # 全量：deps + librime + 摆进 Weasel
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1 -SkipDeps     # 复用已编好的 deps
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1 -Rebuild      # 从零重编（deps 也重编）
pwsh -NoProfile -File scripts/ime/build-librime-win.ps1 -NoDeploy     # 只编到 dist，不动 third_party/weasel
```

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
| 运行期：统一安装日志 | `%TEMP%\rime-sync-setup.log`（UTF-16LE，同一份内容 + 阶段名 `OS-CHECK` / `UNINSTALL-OLD` / `WEBVIEW-CHECK` / `WEBVIEW-INSTALL` / `IME-INSTALL` / `POSTINSTALL` / `DONE`；失败路径也写） |
| 运行期：安装日志 | `DetailPrint` 里的 `[rime-sync] [YYYY-MM-DD HH:MM:SS] 阶段=...` |
| 运行期：卸载旧版（0.6.11 / 0.6.14） | `%TEMP%\rime-sync-setup.log` 里的 `phase=UNINSTALL-OLD target=rime-sync exit= ms=`（无卸载器时 `result=none`）。0.6.14 起**不再有** `target=weasel`：输入法由 `phase=IME-CHECK` / `IME-REPAIR` / `WEASEL-ROLLBACK` 负责 |
| 运行期：配置备份（0.6.11） | 同上日志里的 `seed file=<name> result=backup backup=<path>` 与 `phase=POSTINSTALL seed done configs= backups=` |
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
| `cache self-built librime (deps + build dirs)` | 缓存 `third_party/librime/deps/*/build` 与 `third_party/librime/build`；不缓存 `dist/` |
| `build librime from vendored source (merged rimesync-extra)` | 跑 `build-librime-win.ps1`（deps 浅克隆 → build.bat deps → build.bat librime ×2 → 断言组件名 → 摆进 Weasel）；`continue-on-error` 只在 `build`/`fetch` 为真，`require` 下失败即 job 失败 |
| `build weasel installer from vendored source` | 调 `build-weasel.ps1 -LibrimeSource self`；`continue-on-error` 只在 `build`/`fetch` 模式为真，`require` 下失败即 job 失败 |
| `fetch upstream weasel installer (fallback / offline mode)` | `build` 模式自编译失败、或 `fetch` 模式时走官方安装包；**绝不**在 `require` 下运行 |
| `verify weasel vendor payload` | 必须有 `weasel-*.exe` 且 ≥ 10 MiB；`require` 下来源必须是自编译；写 `packaging/out/WEASEL-SOURCE.json` 并红字标注回退 |
| `assert rime.dll carries rimesync components` | 解开 `packaging/windows/vendor/weasel-*.exe`，断言每个 `rime.dll` 都含 `rimesync_vnum` / `rimesync_punct` / `rimesync_vselect`（与脚本内部断言双保险） |
| `verify-artifacts`（独立 job） | 解 `*-setup.exe` → `WeaselServer.exe` 搜 UTF-16LE「Rime 同步」；再解出 `rime.dll` 断言三个 `rimesync_*` 组件名；`require` 下缺失即失败，publish 不会跑 |
| `assert nsis setup embeds weasel` | 在线 setup ≥ 12 MiB（内嵌 Weasel） |
| `stash online nsis artifacts` | 在线包先挪到 `packaging/out/nsis-online/`（第二遍构建会写回同名文件） |
| `fetch WebView2 standalone payload` | `fetch-webview2.ps1`：3 次重试；< 100 MiB 或非 `MZ` 直接失败 |
| `build nsis (offline variant, embeds WebView2)` | `pnpm tauri build --bundles nsis --config src-tauri/tauri.offline.conf.json` |
| `rename and assert offline bundle` | 改名 `*-offline-setup.exe`（`.sig` 同步）+ 断言 ≥ 100 MiB |
| `collect` 的 Windows 断言 | 在线包 ≥ 12 MiB、离线包 ≥ 100 MiB、两者 `.sig` 都在 |
| `verify-artifacts` 的离线断言 | `--require-offline-setup`：存在性 + 体积 + 内嵌 WebView2/Weasel 两个载荷 |

在 CI 上确认自编译成功的检查项：

1. `build librime from vendored source (merged rimesync-extra)` 为绿色，日志里有
   `[librime/x64] ... rimesync_vnum / rimesync_punct / rimesync_vselect 全部命中` 与
   `[librime/Win32] ...` 各一条；
2. `build weasel installer from vendored source` 步骤为绿色，日志末行有
   `[ok] ...\weasel-0.17.4.0-installer.exe` 与 `sha256=...`；
3. `verify weasel vendor payload` 打印 `weasel source: self-built from third_party/weasel @ 0.17.4`，
   且 `WEASEL-SOURCE.json` 的 `librime` 字段是 `self-built 1.17.0 (merged rimesync-extra)`；
4. `assert rime.dll carries rimesync components` 打印 `rime.dll 组件齐全`；
5. makensis 日志里**没有** `vendored weasel installer not found` 告警；
6. `assert nsis setup embeds weasel` 打印的 MiB 数 ≥ 15；
7. 真机静默安装后 `%ProgramFiles%\rime-sync\install-ime-order.log` 含
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
- 只回退 librime（保留自编译 Weasel，本地应急）：
  `pwsh -File scripts/ime/build-weasel.ps1 -LibrimeSource prebuilt -LibrimeTag 1.13.1`。
  布局/头文件一致，能链接能装，但日志红字说明「不含输入增强插件」：v 模式 / 标点 Shift 备选 /
  a-h 选词 不生效，`WEASEL-SOURCE.json` 的 `librime` 会记成 `prebuilt 1.13.1 (no plugins)`。
  **CI 里这条路径被 `ci.yml` 的 guard 明令禁止**（release.yml 不许出现 `-LibrimeTag`）：发布版必须带插件。
- 两条路径产出的文件名一致（`weasel-0.17.4.0-installer.exe`），钩子与 resources 布局不用改。
- 产物验收：`bash .github/scripts/verify-artifacts.sh --dir dist`（严格）或
  `--weasel-mode build`（放行回退但红字告警）。

## 6. 无法在本机验证的点（必须靠 CI / 真机）

- 本机（Linux 开发机）没有 MSVC / NSIS：`build-weasel.ps1` / `build-librime-win.ps1` 的
  **编译链路**只能靠 CI 首跑验证（人工审读 + `-Check` 静态检查）。语法有 CI 守卫：
  `ci.yml` 的 `packaging-scripts` job 里 `parse Windows packaging ps1 (syntax only)` 用
  PowerShell 自带的 `[Parser]::ParseFile` 解析 `scripts/ime/*.ps1` 与
  `packaging/windows/*.ps1`，`parse inline PowerShell in release.yml` 解析所有
  `shell: pwsh` 步骤的内联脚本。
- 自建 librime 的 `build.bat deps` 里 googletest 的占位工程只在 Linux 上验过 configure+install
  （CMake 3.31 + Makefiles）；VS 生成器下的同一行为需要 CI 首跑确认（失败时用 `-WithTests` 绕过）。
- 插件在 Windows 上**是否真的生效**（v 模式、标点 Shift 备选、a-h 选词）需要真机打字验证：
  二进制里能搜到组件名只证明它被编进去了，不证明 Weasel 的 schema 配置引用到了它们。
- Weasel 在无交互的 CI 会话里安装是否完整注册 TSF（可能需重启 explorer）未验证。
- `get-rime.ps1`（只在 `-LibrimeSource prebuilt` 时用到）依赖 `api.github.com`：
  限流/网络问题会失败（CI 传 `GITHUB_TOKEN` 缓解）。自建路径不联网拉 release，
  只做 5 个 deps 的 `git clone --depth 1`。
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
| 安装日志 | `$INSTDIR\install-ime-order.log` | 另加 `%TEMP%\rime-sync-setup.log`（UTF-16LE，阶段名 `OS-CHECK` / `UNINSTALL-OLD` / `WEBVIEW-CHECK` / `WEBVIEW-INSTALL` / `IME-INSTALL` / `POSTINSTALL` / `DONE`；失败路径同样写） |
| Windows 版本门槛 | 无 | 低于 Windows 10 1803（10.0.17134）：弹框说明并 Abort |

**任何路径都不再联网下载**：在线包只把地址打印在弹框里，离线包用内嵌的 Standalone 安装器。

### 8.1 两个变体（都是「Weasel + rime-sync」一个包）

0.6.6 起 Windows 与 Linux 一致：一个安装包 = 输入法本体 + 同步工具，再分全量 / 非全量。
两个 exe **都内嵌 Weasel 安装器**，安装顺序都是 PREINSTALL 装 Weasel（`phase=IME-INSTALL`）
→ Tauri 装 rime-sync 本体 → POSTINSTALL 建快捷方式。

```text
rime-sync_<ver>_x64-setup.exe          非全量（默认）：缺 WebView2 时弹框给地址，不下载
rime-sync_<ver>_x64-offline-setup.exe  全量：内嵌 WebView2 Standalone + Weasel，全程不联网
```

非全量包只省掉 WebView2 运行时（约 130 MiB）；Weasel 与 rime-sync 本体两个包都有。
受限代理/内网机器用全量包，或给非全量包加 `/SKIPWEBVIEW`（运行库已由域内分发时）。

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

---

## 9. 开机自启动（0.6.7）

设置页「开机自启动」写入**用户级** Run 键，`perMachine` 安装也不需要管理员：

- 键：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值名 `RimeSync`，
  数据 `"<安装目录>\rime-sync-app.exe" --autostart`（路径加引号，兼容空格与中文）。
- 关闭开关删除该值；卸载程序不主动清理它（值指向已删除的 exe 时，登录启动会失败并被系统忽略）。
- `--autostart` 启动只建托盘、不显示主窗口；点托盘图标或托盘菜单「打开主窗口」才显示。
- 注册表写入失败（组策略/杀软拦截）时，设置页显示后端返回的 `reg.exe` 错误原文。

输出示例（0 warning 0 error；离线包体积比在线包正好大一个假载荷 = 确实内嵌了 Standalone）：

```text
ok   static  22 条结构断言（卸载命令/120s 超时/失败不 Abort/备份命名）
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

## 10. 升级即全新安装（0.6.11）

每个新版本都按「先卸载旧版 → 全新安装」执行：升级不再沿用旧文件，也不再因为「检测到
已存在」而跳过任何一步。**0.6.14 起只卸载同步程序**：输入法本体不再「先卸再装」，而是先
判就绪、未就绪才改名留证 + 重装（第 11 节）。安装器按新版本无条件创建目录与文件，
覆盖前保留备份。

### 10.1 卸载顺序（`NSIS_HOOK_PREINSTALL`）

| 步骤 | 动作 | 失败时 |
|---|---|---|
| 0 | `RimeSyncCheckWindows`：系统版本 ≥ Win10 1803 | 弹框 + `Abort`。**只读检查放在卸载之前**：先卸载再中止会把用户晾在「旧版没了、新版没装」的半路 |
| 1 | `taskkill /IM rime-sync-app.exe /F`、`taskkill /IM WeaselServer.exe /F` | 忽略（进程本来就不在时 taskkill 返回 128），避免旧卸载器删不掉被占用的 exe/dll 而白等到超时 |
| 2 | 跑旧版卸载器：`"<$INSTDIR>\uninstall.exe" /S _?=$INSTDIR`。`$INSTDIR\uninstall.exe` 不在时枚举 `HKLM\...\Uninstall`（32 位视图优先，再 64 位），`DisplayName` 以 `rime-sync` 开头或 `Publisher` 是 `rime-sync` 即命中 | 只写日志（`exit=` / `result=none` / `timeout`），继续安装 |
| 3 | 清掉所有权标记（`BundledWeasel` / `WeaselRoot` / `WeaselUninstall` / `BundledAt`），新版装完再重新写 | 忽略 |

> **0.6.14 起这里不再卸小狼毫**（0.6.11 的「步骤 3 跑小狼毫卸载器」已删除）。原因见第 11 节：
> 先把 Weasel 卸掉再判「是否就绪」，残留的 TSF/注册表会把判定骗成「已就绪」→ 跳过安装 →
> 机器上最终没有输入法。Weasel 的替换统一由 `RimeSyncInstallWeaselPre` 的
> 「判定 → 改名 → 重装 → 回滚」负责。

* 等待用 `nsExec::ExecToStack /TIMEOUT=120000`（`RIMESYNC_UNINSTALL_TIMEOUT_MS`）。
  `ExecWait` 没有超时参数，卸载器一卡住安装器就永远停在那儿；nsExec 到点结束进程并返回
  `timeout`，退出码照样进日志。**任何一步失败都只记日志，绝不 Abort。**
* 旧版卸载器（0.6.13 及以前）自己的 `PREUNINSTALL` 会卸掉「我们装的」小狼毫 —— 那是已经
  发出去的代码，改不了。所以 0.6.14 在卸载**之后**再判一次就绪：它真被删了 → 重装。
* 卸载之后就是全新安装：Weasel 先（`RimeSyncInstallWeaselPre`）、同步程序后（Tauri 拷主程序）。
  0.6.14 起输入法这一步由「唯一硬条件 + 先判定后动手」驱动（第 11 节）：已就绪 → 跳过安装
  （`action=skip reason=exe-present`）；未就绪 → 改名留证 + 重装，复查失败**先回滚再硬失败**。

### 10.2 配置文件：按新版无条件写，覆盖前备份

`POSTINSTALL` 的 `RimeSyncSeedRimeConfig` 规则（0.6.12 起）：

| 目标 | 规则 |
|---|---|
| `%APPDATA%\Rime\default.custom.yaml` | 目录不存在就创建；**已存在就沿用**，不覆盖。仍要求 Weasel 的 `data` 里有 `wubi86.schema.yaml`（模板的 `schema_list` 列了五笔，缺方案会让 librime 整次部署失败） |
| `%APPDATA%\Rime\weasel.custom.yaml` | 已存在就沿用；缺失才写（只影响外观：候选横向排列） |
| `%APPDATA%\Rime\<schema>.custom.yaml` | 遍历 `$INSTDIR\resources\schemas\*.custom.yaml`，**逐个**要求 Weasel `data` 里有 `<schema>.schema.yaml`，否则跳过；已存在则沿用 |

**为什么改成「沿用」**：卸载旧版时不会删除 `%APPDATA%\Rime`，升级后用户自己的改造
（以及同步下来的设置）必须原样保留；安装器只补缺失的文件。想强制刷新某个配置，删掉
对应文件再装一次即可（安装器会重新写入新版）。

日志（`%TEMP%\rime-sync-setup.log`，UTF-16LE）：

```text
[rime-sync] 2026-11-02 20:00:00 phase=UNINSTALL-OLD target=rime-sync exit=0 ms=3120 source=(instdir)
[rime-sync] 2026-11-02 20:00:03 phase=UNINSTALL-OLD target=weasel exit=0 ms=2410
[rime-sync] 2026-11-02 20:00:05 phase=POSTINSTALL seed begin version=0.6.12 policy=reuse-existing
[rime-sync] 2026-11-02 20:00:05 seed file=default.custom.yaml result=reuse
[rime-sync] 2026-11-02 20:00:05 seed file=luna_pinyin.custom.yaml result=new
[rime-sync] 2026-11-02 20:00:05 phase=POSTINSTALL seed done configs=1 reused=15 version=0.6.12
[rime-sync] 2026-11-02 20:00:06 phase=DONE setup finished configs=1 reused=15 weasel-kept=0
```

### 10.3 用户数据一律不动

写入清单只有上面三类 `*.custom.yaml`。下列内容**既不读也不写、不改名、不删除**：

* 用户词典：`*.userdb`、`*.userdb.txt`、`*.userdb.kct` 等；
* 词表与状态：`*.txt`、`custom_phrase.txt`、`installation.yaml`、`user.yaml`；
* `%APPDATA%\com.rimesync.app`（同步程序自己的配置与同步状态）；
* 本次改动没有新增任何 `Delete` / `RMDir` / 通配删除动作。

卸载器默认同样保留 `%APPDATA%\Rime`（只有交互卸载选「是」或静默卸载带 `/PURGEDATA`
才删，见第 7 节）。

### 10.4 想让它重新写一遍新版配置

```powershell
# 只删要刷新的那几个文件，再装一次新版（安装器只补缺失，不会覆盖其它文件）
Remove-Item "$env:APPDATA\Rime\luna_pinyin.custom.yaml","$env:APPDATA\Rime\default.custom.yaml" -ErrorAction SilentlyContinue
```

装完让小狼毫重新部署（托盘「重新部署」或 `WeaselDeployer.exe /deploy`）即可。

### 10.5 本机能验到哪一步（真机仍需复核）

已验：`bash packaging/windows/check-nsis-hooks.sh --strict` —— 两套钩子 0 warning 0 error，
外加 98 条静态结构断言（卸载命令带引号 + `_?=`、120/300/20 秒超时、就绪判定只认 exe、
`tsf=` 只许出现在 `phase=TSF-EVIDENCE`、先判定再卸/装、`Rime.bak-` 改名留证、回滚改回原名、
UNINSTALL-OLD 不再卸 Weasel、自检硬失败退出码 5、`SetShellVarContext current` 播种、失败不 `Abort`）。
断言本身做过反向验证（`RIMESYNC_HOOKS=<临时副本> bash … --strict`）：把 `tsf` 塞回判定行、去掉
exe 判定、删掉回滚 `Rename`、让 UNINSTALL-OLD 又卸 Weasel、把 preflight 挪到卸载之后、删掉自检的
`SetErrorLevel 5`、允许空目录改名 —— 每一条都让脚本立即变红（见第 11.6 节）。编译只能证明语法与结构，**运行期行为只能在真机复核**：

1. 装 0.6.10 → 再装 0.6.11：旧 `uninstall.exe /S _?=` 静默跑完、`rime-sync-app.exe` 被
   taskkill、日志里 `exit=0`；
2. 旧版带自建小狼毫：升级时**不再**在 UNINSTALL-OLD 里卸它；就绪则只跳过安装
   （`action=skip reason=exe-present`），旧卸载器真删了它就重装并重新写标记；
3. 卸载器卡住（把 `uninstall.exe` 换成 `cmd /c pause`）→ 120 秒后 `timeout`，安装继续；
4. `%APPDATA%\Rime` 里的 `*.userdb*`、`*.txt` 升级后原样还在；
5. 手改过 `weasel.custom.yaml` → 升级后**原样保留**（日志 result=reuse），改动不被覆盖；
6. 删掉 `luna_pinyin.custom.yaml` 再装 → 日志 result=new，新版被写入（只补缺失）；
7. 0.6.14 回归场景（机 A：`C:\Program Files\Rime` 空目录、`HKLM\Software\WOW6432Node\Rime\Weasel`
   缺失、TSF 残留）→ 日志 `phase=IME-CHECK stage=preflight ready=no reason=empty-dir`、
   `phase=IME-REPAIR rename skipped`、安装器重装后 `phase=IME-INSTALL ready=yes`、
   `phase=SELF-CHECK … build=updated`；`tsf=yes` 只出现在 `phase=TSF-EVIDENCE` 行；
8. 删掉 `HKLM\Software\Rime\Weasel` 但保留非空程序目录 → `rename ok` 到 `Rime.bak-<ts>`、
   重装成功；若安装器失败（换成 `cmd /c exit 1` 的桩构建）→ `WEASEL-INSTALL failed … rollback=ok`、
   `Rime.bak-*` 被改回 `Rime`、安装器退出码 5（静默不弹框，交互弹框）；
9. 桩构建 + 空目录 → `rollback=none`（没有备份就不假装回滚过），退出码照样 5；
10. `/SKIPIME` 安装 → 日志 `phase=IME-CHECK skip=1 switch=/SKIPIME`，结束提示说明本次没装输入法；
11. 装完把 `WeaselServer.exe` 删掉再手工跑一次自检（或直接跑安装包）→
    `phase=SELF-CHECK weasel-ready=no … default-exe=no`、`phase=SETUP failed … exit=5`、
    交互安装弹 STOP 框、`phase=DONE` 不出现；
12. 装完检查 `$env:APPDATA\Rime` 里有模板，而 `C:\ProgramData\Rime` 里没有（0.6.12 的 bug）。

## 11. Weasel 就绪判定、重装与回滚（0.6.14）

0.6.13 有两个连锁问题，合起来在真机上把输入法装没了（机 A：`C:\Program Files\Rime` 空目录、
`HKLM\Software\WOW6432Node\Rime\Weasel` 不存在、没有 `WeaselServer` 进程、TSF 残留还在）：

1. **顺序错**：`UNINSTALL-OLD` 先把自建 Weasel 卸掉，之后才判「是否就绪」；
2. **判据太宽**：卸完之后 TSF 文本服务注册、注册表残留还在，判定把它们当成证据 →
   `action=skip` → 内嵌安装器根本没跑 → 机器上没有输入法。
   （机 B 是另一种形态：注册表在、`WeaselServer` 跑在 `SessionId=0`、部署从不生效。）

0.6.14 的改法：

* `UNINSTALL-OLD` **不再卸 Weasel**，只卸旧版 rime-sync（第 10.1 节）；
* `RimeSyncWeaselReady` 只剩**一个硬条件**：注册表值指向的目录里真有 `WeaselServer.exe`；
* TSF / 进程证据**只写日志**（`phase=TSF-EVIDENCE`），`tsf=` 不出现在任何判定行；
* 未就绪才改名 + 重装；复查仍不就绪 → **回滚**把旧目录改回来，日志 `rollback=ok|failed`。

### 11.1 判定条件（`RimeSyncWeaselReady`）

| 条件 | 怎么判 | 缺失时 |
|---|---|---|
| 注册表值存在 | `HKLM\Software\Rime\Weasel` 的 `InstallDir` **或** `WeaselRoot` 非空。Weasel 的安装器是 32 位 NSIS，值通常落在 `WOW6432Node`，所以 64 位与 32 位视图都读 | 未就绪，原因 `registry-missing` |
| 程序目录里真有服务程序 | 该值指向的目录里存在 `WeaselServer.exe`。`InstallDir` 写的是父目录 `<Program Files>\Rime`，真正的程序目录是下面的 `weasel-<ver>`；直接指向程序目录、父目录 + 随包版本、父目录 + 任意 `weasel-*` 三种布局都认 | 未就绪，原因 `no-WeaselServer.exe` |
| 目录不为空 | 注册表值指向的目录存在，但一个文件都没有 | 未就绪，原因 `empty-dir`（0.6.14 新增，专门识别机 A 的形态） |

**绝不算证据的东西**（0.6.13 就是被它们骗的）：

* TSF 文本服务注册（`HKLM\SOFTWARE\Microsoft\CTF\TIP\{…}`、`HKLM\SOFTWARE\Classes\CLSID\{…}\InprocServer32`）；
* `WeaselServer.exe` 是否在跑（`SessionId=0` 的僵尸进程也算「在跑」，不代表能用）。

这两项由 `RimeSyncWeaselLogEvidence` 写成一行
`phase=TSF-EVIDENCE tsf= … proc= … note=log-only-not-used-for-readiness`，只供排查，
**不参与任何分支**。静态断言 `assert_tsf_log_only` 保证 `tsf=` 只出现在这一行。

判定结果落在 `phase=IME-CHECK`：

```text
[rime-sync] 2026-11-04 10:00:02 phase=IME-CHECK stage=preflight ready=yes reason=ok root=C:\Program Files\Rime\weasel-0.17.4 source=HKLM\Software\Rime\Weasel\InstallDir(view=32) action=pending-decision
[rime-sync] 2026-11-04 10:00:02 phase=TSF-EVIDENCE tsf=yes tsfdll=C:\Program Files\Rime\weasel-0.17.4\WeaselTSF.dll proc=running note=log-only-not-used-for-readiness
[rime-sync] 2026-11-04 10:00:03 phase=IME-CHECK ready=no reason=empty-dir root= regdir=C:\Program Files\Rime action=repair
[rime-sync] 2026-11-04 10:00:03 phase=IME-CHECK ready=yes root=C:\Program Files\Rime\weasel-0.17.4 source=HKLM\Software\Rime\Weasel\InstallDir(view=32) proc=running action=skip reason=exe-present
```

### 11.2 顺序（`NSIS_HOOK_PREINSTALL`）

```text
RimeSyncCheckWindows             只读：系统版本门槛
RimeSyncWeaselPreflight          只读：判定就绪 → phase=IME-CHECK stage=preflight
RimeSyncEnsureWebView2
RimeSyncUninstallOldVersion      只卸旧 rime-sync（绝不碰 Weasel）
CreateDirectory $INSTDIR\resources
RimeSyncInstallWeaselPre         再判一次（旧卸载器可能删掉 Weasel）→ 决定 skip / repair
  ├─ ready=1 → action=skip reason=exe-present     （不动现有 Weasel）
  └─ ready=0 → taskkill → rename Rime.bak-<ts> → installer /S /NOREBOOT → 复查
                 └─ 仍不就绪 → 回滚 → WEASEL-INSTALL failed + SetErrorLevel 5 + Abort
（之后 Tauri 才 File 主程序与 resources）
```

为什么要判两次：第一次是「动手之前到底是什么状态」的证据；第二次才是决定 —— 旧版卸载器
（0.6.13 及以前）会在自己的 `PREUNINSTALL` 里卸载 Weasel。第二次判定用的是**卸载之后**的
真实状态，所以不可能再被「已卸掉、但残留还在」骗过。

### 11.3 未就绪时的重装与回滚

| 步骤 | 动作 | 失败时 |
|---|---|---|
| 1 | `taskkill /IM WeaselServer.exe /F` | 忽略（进程不在时 taskkill 返回非 0） |
| 2 | 把残留目录改名成 `<原目录>.bak-YYYYMMDD-HHMMSS`。候选顺序：注册表里读到的目录 → `%ProgramFiles%\Rime` → `%ProgramFiles(x86)%\Rime`。**仅当目录非空且 `WeaselServer.exe` 真的在**才改名；空目录 / 缺 exe 只写 `phase=IME-REPAIR rename skipped dir=… reason=empty-or-no-WeaselServer.exe` | 只记日志（`rename failed`），继续装；这时没有备份，后面只能 `rollback=none` |
| 3 | 跑内嵌的 `weasel-0.17.4.0-installer.exe /S /NOREBOOT`，`nsExec::ExecToStack /TIMEOUT=300000`；退出码与耗时进日志。`/NOREBOOT` 防它 `SetRebootFlag` 后静默重启机器 | 继续下一步（退出码不是充分条件，就绪判定才是） |
| 4 | 再判一次 `RimeSyncWeaselReady`（注册表 + 真 exe） | 仍不就绪 → 第 5 步 |
| 5 | **回滚**：`Rime.bak-<ts>` 改回 `Rime`。安装器若已建了同名目录，先把它改名成 `Rime.incomplete-<ts>` 留证，不覆盖新证据。日志 `phase=WEASEL-ROLLBACK result=ok\|failed`；`ok` 的判据是改回后目录里又能找到 `WeaselServer.exe` | 改名失败 → `result=failed`，照样继续硬失败 |
| 6 | 硬失败：`WEASEL-INSTALL failed reason=… installer-exit=… rollback=ok\|failed`、`SetErrorLevel 5`、`Abort`；交互安装额外弹框 | — |

`rollback=ok` 只说明**文件回来了**：注册表可能仍然缺失（这时 `RimeSyncWeaselReady` 依旧是
未就绪），下次安装会再走一遍修复。所以 `rollback=ok` 与「输入法可用」不是同义词。

硬失败的后果：旧版 rime-sync 已按「升级即全新安装」卸掉、新版本也没装上，但 **Weasel 的旧目录
已被改回原名**，所以机器至少回到装之前的状态（不会像 0.6.13 那样变成「什么都没有」）。
唯一的逃生开关仍是 `/SKIPIME`（只装同步程序、不动输入法），此时 POSTINSTALL 会弹框说明。

修复成功后写 `HKLM\Software\rime-sync` 的所有权标记（`BundledWeasel=1` 等），卸载时据此决定
是否连小狼毫一起卸。

### 11.4 日志样例

已就绪 → 跳过安装（`tsf` 只在证据行）：

```text
[rime-sync] 2026-11-04 10:00:02 phase=IME-CHECK stage=preflight ready=yes reason=ok root=C:\Program Files\Rime\weasel-0.17.4 source=HKLM\Software\Rime\Weasel\InstallDir(view=32) action=pending-decision
[rime-sync] 2026-11-04 10:00:02 phase=TSF-EVIDENCE tsf=yes tsfdll=C:\Program Files\Rime\weasel-0.17.4\WeaselTSF.dll proc=running note=log-only-not-used-for-readiness
[rime-sync] 2026-11-04 10:00:03 phase=UNINSTALL-OLD target=rime-sync exit=0 ms=3120 source=(instdir)
[rime-sync] 2026-11-04 10:00:03 phase=IME-CHECK ready=yes root=C:\Program Files\Rime\weasel-0.17.4 source=HKLM\Software\Rime\Weasel\InstallDir(view=32) proc=running action=skip reason=exe-present
[rime-sync] 2026-11-04 10:00:05 phase=SELF-CHECK weasel-ready=1 root=C:\Program Files\Rime\weasel-0.17.4 install-dir=C:\Program Files\Rime default-exe=yes configdir=C:\Users\me\AppData\Roaming\Rime configs=2 reused=1 deploy=0 ran=1 build=updated warnings=0
[rime-sync] 2026-11-04 10:00:05 phase=DONE setup finished configs=2 backups=0 weasel-kept=0 weasel-ready=1 install-dir=C:\Program Files\Rime default-exe=yes deploy=0 build=updated warnings=0
```

未就绪 → 改名 → 重装成功：

```text
[...] phase=IME-CHECK ready=no reason=registry-missing root= regdir=C:\Program Files\Rime action=repair
[...] phase=IME-REPAIR rename ok from=C:\Program Files\Rime to=C:\Program Files\Rime.bak-20261104-100004 exe=C:\Program Files\Rime\weasel-0.17.4
[...] phase=IME-INSTALL weasel installer exit=0 ms=41200 backup=C:\Program Files\Rime.bak-20261104-100004
[...] phase=IME-INSTALL ready=yes root=C:\Program Files\Rime\weasel-0.17.4 source=HKLM\Software\Rime\Weasel\InstallDir(view=32) proc=stopped backup=C:\Program Files\Rime.bak-20261104-100004
```

空目录（机 A 的形态）→ 不改名 → 重装；失败时 `rollback=none`：

```text
[...] phase=IME-CHECK ready=no reason=empty-dir root= regdir=C:\Program Files\Rime action=repair
[...] phase=IME-REPAIR rename skipped dir=C:\Program Files\Rime reason=empty-or-no-WeaselServer.exe
[...] phase=IME-INSTALL weasel installer exit=1603 ms=5120 backup=
[...] WEASEL-INSTALL failed reason=registry-missing installer-exit=1603 rollback=none backup= orig= root= regdir=C:\Program Files\Rime proc=stopped
```

有备份但重装失败 → 回滚（`Rime.bak-<ts>` 改回 `Rime`）：

```text
[...] phase=IME-REPAIR rename ok from=C:\Program Files\Rime to=C:\Program Files\Rime.bak-20261104-100004 exe=C:\Program Files\Rime\weasel-0.17.4
[...] phase=IME-INSTALL weasel installer exit=0 ms=3000 backup=C:\Program Files\Rime.bak-20261104-100004
[...] phase=WEASEL-ROLLBACK result=ok from=C:\Program Files\Rime.bak-20261104-100004 to=C:\Program Files\Rime exe=C:\Program Files\Rime\weasel-0.17.4
[...] WEASEL-INSTALL failed reason=no-WeaselServer.exe installer-exit=0 rollback=ok backup=C:\Program Files\Rime.bak-20261104-100004 orig=C:\Program Files\Rime root= regdir=C:\Program Files\Rime proc=stopped
```

### 11.5 手工排查命令（PowerShell / cmd）

```powershell
# 1) 注册表到底有没有值（32/64 位视图都看一眼；Weasel 是 32 位安装器）
reg query "HKLM\SOFTWARE\Rime\Weasel" /v InstallDir /reg:32
reg query "HKLM\SOFTWARE\Rime\Weasel" /v WeaselRoot /reg:32
reg query "HKLM\SOFTWARE\Rime\Weasel" /v InstallDir /reg:64

# 2) 程序目录里有没有服务程序与部署器
Test-Path "$env:ProgramFiles\Rime\weasel-0.17.4\WeaselServer.exe"
Test-Path "$env:ProgramFiles\Rime\weasel-0.17.4\WeaselDeployer.exe"

# 3) 服务在不在跑（SessionId=0 的那种「在跑」不代表能用）
Get-Process WeaselServer -ErrorAction SilentlyContinue | Select-Object Id,SessionId

# 4) TSF 文本服务有没有注册（只作证据，不参与判定）
reg query "HKLM\SOFTWARE\Microsoft\CTF\TIP\{A3F4CDED-B1E9-41EE-9CA6-7B4D0DE6CB0A}" /s

# 5) 部署产物是不是新的
Get-Item "$env:APPDATA\Rime\build\luna_pinyin.schema.yaml" | Select-Object LastWriteTime,Length

# 6) 安装器留下的备份 / 回滚残留
Get-ChildItem "$env:ProgramFiles" -Filter "Rime.bak-*" | Select-Object Name,LastWriteTime
Get-ChildItem "$env:ProgramFiles" -Filter "Rime.incomplete-*" | Select-Object Name,LastWriteTime

# 7) 一把看全安装器日志里与输入法相关的行
Get-Content "$env:TEMP\rime-sync-setup.log" | Select-String -Pattern "IME-CHECK|IME-REPAIR|IME-INSTALL|TSF-EVIDENCE|WEASEL|SELF-CHECK"
```

想强制重来一次：删掉（或改回原名）`C:\Program Files\Rime`，再运行安装包；卸载残留输入法
也可以直接走「控制面板 → 程序和功能 → 小狼毫输入法」。

### 11.6 本机（Linux）能验到哪一步

`bash packaging/windows/check-nsis-hooks.sh --strict` 会跑 98 条结构断言 + 编译两套钩子
（0 warning 0 error）。反向验证的做法（不动工作区文件）：

```bash
cp packaging/nsis-hooks.nsh /tmp/mut.nsh
# 手工改坏一处，例如把 tsf 塞回判定行，或删掉 rollback 的 Rename，或去掉自检的 SetErrorLevel 5
RIMESYNC_HOOKS=/tmp/mut.nsh bash packaging/windows/check-nsis-hooks.sh --strict   # 必须 rc=1
```

已逐条验过会变红的改动：`tsf` 混进判定行、就绪判定不再要求 exe、删掉回滚 `Rename`、
UNINSTALL-OLD 又卸 Weasel、预检挪到卸载之后、删掉自检 `SetErrorLevel 5`、允许空目录改名、
回滚失败假报 `ok`。运行期行为（注册表、进程、弹框、真实安装器退出码）仍只能在真机复核。

## 12. 安装自检输出（0.6.14）

`POSTINSTALL` 末尾固定跑一次 `RimeSyncSelfCheck`（在写配置之后）：

1. 再判一次 `RimeSyncWeaselReady`（注册表 + 真 exe）；TSF/进程证据另起一行 `phase=TSF-EVIDENCE`；
2. **硬性检查 A**：`HKLM\Software\Rime\Weasel\InstallDir`（64 位或 32 位视图）必须有值；
3. **硬性检查 B**：`<Program Files>\Rime\weasel-*\WeaselServer.exe` 必须存在
   （`%ProgramFiles%` 与 `%ProgramFiles(x86)%` 两处都扫，命中一个就够）；
4. 报配置目标目录与写入数量（`configs=` 新写入、`reused=` 沿用）；
5. 调一次 `WeaselDeployer.exe /deploy`（`nsExec`，最多 20 秒 / `RIMESYNC_DEPLOY_TIMEOUT_MS`），
   然后比较 `%APPDATA%\Rime\build\luna_pinyin.schema.yaml` 的写入时间。

`weasel-ready` 字段：

| 值 | 含义 |
|---|---|
| `1` | 注册表 + `WeaselServer.exe` 都在（判定就绪） |
| `0` | 未就绪（`reason=` 给出 `registry-missing` / `no-WeaselServer.exe` / `empty-dir`） |

**自检不通过 = 安装失败**（0.6.14 起，第 2/3 项是硬性检查）：

| 触发条件 | 日志 | 交互安装 | 退出码 |
|---|---|---|---|
| `weasel-ready=0` | `phase=SELF-CHECK weasel-ready=no ready=0 reason=…` | 弹 `MB_ICONSTOP` 说明 | 5 |
| 注册表 `InstallDir` 为空（`install-dir=` 为空） | 同上，`install-dir=` 为空 | 同上 | 5 |
| 默认布局没有 `weasel-*\WeaselServer.exe`（`default-exe=no`） | 同上，`default-exe=no` | 同上 | 5 |

不 `Abort` 是刻意的：程序已经装好，用户要的是日志与提示；失败靠 **退出码 5 +
`phase=SETUP failed`** 表达（这时不会写 `phase=DONE`）。静默安装只写日志，不弹框。

`/SKIPIME` 是唯一例外：用户明确要求「本次不动输入法」，所以自检只写
`phase=SELF-CHECK warning=weasel-not-ready-skipped … skip=1 switch=/SKIPIME`，
不改退出码（否则逃生开关本身就会被判失败）。

`build` 字段的取值：

| 值 | 含义 |
|---|---|
| `updated` | 部署后文件时间前进了（正常路径） |
| `generated` | 部署前不存在、部署后出现（首次部署成功） |
| `stale` | 文件在，但时间没变 → 部署没生效（只告警） |
| `missing` | 部署后仍然没有这个文件 → 部署失败（只告警） |
| `unknown` | 没有调用部署（输入法未就绪等） |
| `no-deployer` | 就绪目录里没有 `WeaselDeployer.exe`（记 `deploy=no-deployer` 并告警） |

日志样例（`%TEMP%\rime-sync-setup.log`）：

```text
[rime-sync] 2026-11-04 10:00:07 phase=POSTINSTALL seed begin version=0.6.14 policy=reuse-existing dir=C:\Users\me\AppData\Roaming\Rime context=current
[rime-sync] 2026-11-04 10:00:07 seed file=default.custom.yaml result=reuse
[rime-sync] 2026-11-04 10:00:07 seed file=weasel.custom.yaml result=new
[rime-sync] 2026-11-04 10:00:07 seed file=luna_pinyin.custom.yaml result=new
[rime-sync] 2026-11-04 10:00:07 phase=POSTINSTALL seed done dir=C:\Users\me\AppData\Roaming\Rime configs=2 reused=1 version=0.6.14
[rime-sync] 2026-11-04 10:00:08 phase=SELF-CHECK deploy exit=0
[rime-sync] 2026-11-04 10:00:09 phase=SELF-CHECK weasel-ready=1 root=C:\Program Files\Rime\weasel-0.17.4 source=HKLM\Software\Rime\Weasel\InstallDir(view=32) proc=running install-dir=C:\Program Files\Rime default-exe=yes configdir=C:\Users\me\AppData\Roaming\Rime configs=2 reused=1 deploy=0 ran=1 build=updated warnings=0
[rime-sync] 2026-11-04 10:00:09 phase=DONE setup finished configs=2 backups=0 weasel-kept=0 weasel-ready=1 install-dir=C:\Program Files\Rime default-exe=yes deploy=0 build=updated warnings=0
```

硬失败（自检没通过）：

```text
[rime-sync] 2026-11-04 10:00:09 phase=SELF-CHECK weasel-ready=no ready=0 reason=empty-dir root= install-dir= default-exe=no expected=C:\Program Files\Rime\weasel-*\WeaselServer.exe
[rime-sync] 2026-11-04 10:00:09 phase=SETUP failed stage=SELF-CHECK exit=5 weasel-ready=0 install-dir= default-exe=no root= warnings=1
```

非静默安装会额外弹框：

* 自检通过 → 信息框（就绪目录、配置目录与数量、部署与 `build` 状态、日志路径）；
* 自检硬失败 → `MB_ICONSTOP` 框（`ready` / `install-dir` / `default-exe` 三个值 + 日志路径 +
  「安装已标记为失败（退出码 5）」）。

非致命告警的形态：

```text
[...] phase=SELF-CHECK warning=build-not-updated state=stale file=C:\Users\me\AppData\Roaming\Rime\build\luna_pinyin.schema.yaml
```

* 判成 `stale` 不一定是坏事：没改任何配置时，部署产物的内容与时间戳本来就可能不变；
* 但 `missing` 说明部署确实失败：先看 `WeaselDeployer.exe /deploy` 的退出码，再看
  `%APPDATA%\Rime\build` 是否可写；
* 自检不修复任何东西：修复与回滚在 PREINSTALL（第 11 节），这里只负责把状态说清楚并把
  失败变成可见的退出码。

---

## 13. 预编译 build 产物兜底（0.6.15）

### 13.1 什么时候才用得上

实测机器 192.168.61.227（官方 Weasel 0.17.4 与我们自编译包都一样）：跑
`WeaselDeployer.exe /deploy` 返回 0，但 `%APPDATA%\Rime\` 下的
`user.yaml` / `installation.yaml` / `build\` 全停在旧日期，手动起
`WeaselServer.exe` 也一样。这种机器上「本机部署」永远不产出 `build\`，
装完官方安装器的结果是**有输入法、打不出字**。

0.6.15 的兜底：安装包自带一份别处已经部署好的 `build\`，安装时直接铺进用户目录。
先在**能部署的机器**（我们的构建容器）里生成，随安装包分发，不依赖目标机的部署能力。

### 13.2 包内位置与判据

| 位置 | 内容 |
|---|---|
| `$INSTDIR\resources\prebuilt\build\` | `luna_pinyin` / `wubi86` 的 `*.schema.yaml` + `*.prism.bin` + `*.table.bin` + `*.reverse.bin`，依赖方案的产物（`stroke`、`pinyin_simp`），以及 `default.yaml` |
| `$INSTDIR\resources\prebuilt\config\` | 生成这套产物时用的 `default.custom.yaml`（`schema_list` 已裁剪到两套方案）+ 两个 `<schema>.custom.yaml` |
| `$INSTDIR\resources\prebuilt\MANIFEST.json` | 文件清单（大小 / sha256）、方案表、体积、rime 版本 |

体积：这套产物 `du -sh` 约 29 MB，安装包会相应增大（`.bin` 本身已是压缩过的
字典/表，NSIS 再压收益很小）。预算 60 MB，超了生成脚本会自动退化成只带 `luna_pinyin`。

判据与 Linux 侧完全一致：`%APPDATA%\Rime\build\luna_pinyin.schema.yaml`
**存在**，且文件里同时出现 `rimesync_vnum` 与 `rimesync_punct`。
只认「文件在」不行——官方部署出来的同名文件没有这两个插件标记，设备上跑的是别的配置。

### 13.3 安装顺序（`RimeSyncSelfCheck`）

1. `RimeSyncWeaselReady` + 硬性检查（注册表 `InstallDir`、默认布局 `WeaselServer.exe`）——不变；
2. 调一次 `WeaselDeployer.exe /deploy`（20 秒超时）——不变；
3. **新增** `RimeSyncEnsurePrebuiltBuild`：
   - 判据满足 → `PREBUILT ok source=deploy`，什么都不做；
   - 不满足且 `RIMESYNC_PREBUILT=0` → `PREBUILT skipped reason=disabled-by-env`；
   - 不满足 → 逐个文件把 `prebuilt\build\*` 铺到 `%APPDATA%\Rime\build\`、
     `prebuilt\config\*.yaml` 铺到 `%APPDATA%\Rime\`（**存在即跳过**，
     `RIMESYNC_PREBUILT=force` 才覆盖）→ 复查判据：
     - 满足 → `PREBUILT ok source=prebuilt copied=<n> kept=<m> escalated=0`；
     - 仍不满足 → **只对 `build\` 升级成覆盖模式再铺一遍**（`phase=PREBUILT retry
       reason=probe-after-skip-only scope=build-only`）。这一步是必须的：用户目录里
       本来就有旧的 `build\luna_pinyin.schema.yaml`（没有插件标记、日期停在旧部署），
       「存在即跳过」会把它留下，兜底就白做了。`build\` 是部署器随时重写的派生数据，
       覆盖它是安全的；`config\*.yaml`（用户配置）**绝不参与这一遍**；
     - 重铺后仍不满足 → `PREBUILT failed reason=probe-after-copy … escalated=1` +
       告警（`warnings+1`），**不 Abort、不改退出码**；
   - 安装包里没有 `prebuilt\build` → `PREBUILT failed reason=payload-missing` + 告警。
4. 自检汇总行新增 `build-source=deploy|prebuilt|none`（`phase=SELF-CHECK` 与
   `phase=DONE` 都有）。兜底成功时不再报 `warning=build-not-updated`。

日志样例（部署坏掉的机器）：

```text
[rime-sync] 2026-11-05 10:00:08 phase=SELF-CHECK deploy exit=0
[rime-sync] 2026-11-05 10:00:08 phase=PREBUILT begin mode=auto src=C:\Program Files\rime-sync\resources\prebuilt
[rime-sync] 2026-11-05 10:00:09 phase=PREBUILT ok source=prebuilt copied=20 kept=0 mode=auto dir=C:\Users\me\AppData\Roaming\Rime\build
[rime-sync] 2026-11-05 10:00:09 phase=SELF-CHECK weasel-ready=1 … build=missing build-source=prebuilt prebuilt=ok copied=20 kept=0 warnings=0
[rime-sync] 2026-11-05 10:00:09 phase=DONE setup finished … deploy=0 build=missing build-source=prebuilt warnings=0
```

复查失败的形态（只告警）：

```text
[rime-sync] 2026-11-05 10:00:09 phase=PREBUILT failed reason=probe-after-copy copied=0 kept=0 file=C:\Users\me\AppData\Roaming\Rime\build\luna_pinyin.schema.yaml
```

### 13.4 关闭 / 强制覆盖

| 目的 | 做法 |
|---|---|
| 关闭兜底 | 装之前设环境变量 `RIMESYNC_PREBUILT=0`（`set RIMESYNC_PREBUILT=0` 后运行安装包；静默装用 `cmd /c "set RIMESYNC_PREBUILT=0 && rime-sync_*_x64-setup.exe /S"`） |
| 强制覆盖用户目录里已有的产物 | `RIMESYNC_PREBUILT=force`（同上方式设置）；也可以直接手工覆盖，见下 |
| 手工铺一次（不重装） | `Copy-Item "$env:ProgramFiles\rime-sync\resources\prebuilt\build\*" "$env:APPDATA\Rime\build\" -Force`，再把 `prebuilt\config\*.yaml` 拷到 `%APPDATA%\Rime\` |
| 不要这份产物 | 从 `%APPDATA%\Rime\build\` 里删掉 `luna_pinyin.*` / `wubi86.*` 再重新部署 |

默认行为是**只补不改**：用户目录里已经有的文件（用户自己的配置、他自己部署出来的
产物）一律沿用，不覆盖、不备份、不改名。

### 13.5 产物怎么来的（仓库内生成，已入库）

```bash
# 生成（容器里用 rime_deployer 真实部署；容器没有 deployer 时用 build/ime-staging 里的）
scripts/ime/gen-prebuilt-build.py

# CI 校验：清单 / sha256 / 插件标记 / 绝对路径泄漏 / 体积 / 打包接线（不需要 docker）
scripts/ime/gen-prebuilt-build.py --check
```

* 输入：`third_party/rime-data`（平铺成共享数据目录）+ `packaging/rime/schemas/*.custom.yaml`
  + 裁剪过 `schema_list` 的 `default.custom.yaml`；
* 输出：`packaging/rime/prebuilt/`（随 `tauri.conf.json` 的 resources 映射进
  `resources/prebuilt`）；
* 体积：两套合计 **28.3 MB**（预算 60 MB；超了脚本会退化成只带 `luna_pinyin`）；
* 自检：生成完会把产物当成「已铺好的用户目录」再跑一次 —— 不部署、只用产物建会话并
  打出「你好」（`rime-smoke --no-deploy`）再跑一次 `rime_deployer`，两条都过才写出；
* 什么时候要重新生成：动了 `third_party/rime-data` 或 `packaging/rime/schemas/*.custom.yaml`。
  `--check` 会在 CI 里把「产物与 schema 不一致」当失败。

> 产物由 librime 1.17.0 部署（`Rime::Table/4.0` 格式，librime 标注的最低兼容版本就是 4.0）。
> 官方 Weasel 0.17.4 带的是 librime 1.13.1，理论上同一格式可直接加载，但**没有在
> 1.13.1 的运行时上实测过**——真机验收时请确认「装完直接能打字」。

### 13.6 程序自举兜底（0.6.16）

0.6.15 的实测还发现：`/S` 静默安装时 NSIS 钩子**根本没执行**（`%TEMP%\rime-sync-setup.log`
里连 `phase=DONE` 都没有），所以「安装器负责铺产物」这条路本身不可靠。0.6.16 起把同样的
逻辑搬进程序：**安装器钩子不跑时由程序自举兜底**。

* 应用启动（tauri `setup`）后在后台线程 `probe_build`：`none` / `deploy`（有文件但没
  插件标记）→ `install_prebuilt` 铺一遍；`prebuilt` 直接跳过。失败只写应用日志
  （`[bootstrap] …`），不弹窗、不阻塞启动；
* 设置页「修复输入法 / 强制覆盖」= `cmd_fix_ime`，与启动自举同一套
  `crates/core/src/prebuilt.rs`；
* CLI：`rime-sync-cli fix-ime [--force] [--json] [--payload <DIR>]`；
* 判据与铺入规则（缺才铺 / build 第二次机会 / config 永不覆盖）与安装器完全一致；
* 日志：应用日志 + `<Rime 用户目录>\.rimesync-prebuilt.log`。

详见 `docs/PREBUILT-BOOTSTRAP.md`。

