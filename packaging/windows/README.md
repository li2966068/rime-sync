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
| 运行期：卸载旧版（0.6.11） | `%TEMP%\rime-sync-setup.log` 里的 `phase=UNINSTALL-OLD target=rime-sync exit= ms=` / `target=weasel exit= ms=`（无卸载器时 `result=none`） |
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
已存在」而跳过任何一步。输入法本体与同步程序都卸载，安装器按新版本无条件创建目录与文件，
覆盖前保留备份。

### 10.1 卸载顺序（`NSIS_HOOK_PREINSTALL`）

| 步骤 | 动作 | 失败时 |
|---|---|---|
| 0 | `RimeSyncCheckWindows`：系统版本 ≥ Win10 1803 | 弹框 + `Abort`。**只读检查放在卸载之前**：先卸载再中止会把用户晾在「旧版没了、新版没装」的半路 |
| 1 | `taskkill /IM rime-sync-app.exe /F`、`taskkill /IM WeaselServer.exe /F` | 忽略（进程本来就不在时 taskkill 返回 128），避免旧卸载器删不掉被占用的 exe/dll 而白等到超时 |
| 2 | 跑旧版卸载器：`"<$INSTDIR>\uninstall.exe" /S _?=$INSTDIR`。`$INSTDIR\uninstall.exe` 不在时枚举 `HKLM\...\Uninstall`（32 位视图优先，再 64 位），`DisplayName` 以 `rime-sync` 开头或 `Publisher` 是 `rime-sync` 即命中 | 只写日志（`exit=` / `result=none` / `timeout`），继续安装 |
| 3 | 跑小狼毫卸载器：仅当 `HKLM\Software\rime-sync` 的 `BundledWeasel=1`。命令依次取标记里的 `WeaselUninstall` → `WeaselRoot\uninstall.exe` → 现场探测 | 只写日志，继续安装 |
| 4 | 清掉所有权标记（`BundledWeasel` / `WeaselRoot` / `WeaselUninstall` / `BundledAt`），新版装完再重新写 | 忽略 |

* 等待用 `nsExec::ExecToStack /TIMEOUT=120000`（`RIMESYNC_UNINSTALL_TIMEOUT_MS`）。
  `ExecWait` 没有超时参数，卸载器一卡住安装器就永远停在那儿；nsExec 到点结束进程并返回
  `timeout`，退出码照样进日志。**任何一步失败都只记日志，绝不 Abort。**
* 旧版卸载器自己也会跑 `PREUNINSTALL`（同样只卸「我们装的」小狼毫、默认保留用户数据），
  所以步骤 3 通常空跑；它兜的是「`rime-sync` 目录被手删、标记还在」这种情况。
* 卸载之后就是全新安装：Weasel 先（`RimeSyncInstallWeaselPre`）、同步程序后（Tauri 拷主程序）。
  若步骤 2/3 失败、旧 Weasel 仍在，安装阶段**仍然跳过**输入法（硬装会覆盖用户正在用的输入法），
  但会记 `phase=IME-INSTALL skip existing weasel root=... reason=still-present-after-uninstall`，
  并在结束提示里弹框要求手动卸载（静默安装只写日志）。

### 10.2 配置文件：按新版无条件写，覆盖前备份

`POSTINSTALL` 的 `RimeSyncSeedRimeConfig` 不再「存在就跳过」：

| 目标 | 规则 |
|---|---|
| `%APPDATA%\Rime\default.custom.yaml` | 目录不存在就创建；已存在 → 先改名备份再写新版。仍要求 Weasel 的 `data` 里有 `wubi86.schema.yaml`（模板的 `schema_list` 列了五笔，缺方案会让 librime 整次部署失败） |
| `%APPDATA%\Rime\weasel.custom.yaml` | 无条件写（只影响外观：候选横向排列） |
| `%APPDATA%\Rime\<schema>.custom.yaml` | 遍历 `$INSTDIR\resources\schemas\*.custom.yaml`，**逐个**要求 Weasel `data` 里有 `<schema>.schema.yaml`，否则跳过该文件 |

备份命名：

* `<file>.bak-<版本>`，例如 `default.custom.yaml.bak-0.6.11`；
* 版本取 `PRODUCTVERSION`（Tauri 模板的版本号 = `src-tauri/tauri.conf.json` 的 `version`）；
  没有该宏的构建退回内嵌小狼毫版本（`RIMESYNC_WEASEL_DIRVER`，当前 `0.17.4`）；
* 同名备份已存在则追加 `-1`/`-2`…（`default.custom.yaml.bak-0.6.11-1`）；
* 备份改名失败（文件被占用/只读）→ **不覆盖**，日志记 `result=failed`：宁可不更新，也不毁掉
  用户手改的配置；复制失败会把备份改回原名。

日志（`%TEMP%\rime-sync-setup.log`，UTF-16LE）：

```text
[rime-sync] 2026-11-02 20:00:00 phase=UNINSTALL-OLD target=rime-sync exit=0 ms=3120 source=(instdir)
[rime-sync] 2026-11-02 20:00:03 phase=UNINSTALL-OLD target=weasel exit=0 ms=2410
[rime-sync] 2026-11-02 20:00:05 phase=POSTINSTALL seed begin version=0.6.11 policy=backup-overwrite
[rime-sync] 2026-11-02 20:00:05 seed file=default.custom.yaml result=backup backup=C:\Users\me\AppData\Roaming\Rime\default.custom.yaml.bak-0.6.11
[rime-sync] 2026-11-02 20:00:05 seed file=luna_pinyin.custom.yaml result=new backup=
[rime-sync] 2026-11-02 20:00:05 phase=POSTINSTALL seed done configs=16 backups=1 version=0.6.11
[rime-sync] 2026-11-02 20:00:06 phase=DONE setup finished configs=16 backups=1 weasel-kept=0
```

`阶段=DONE` 的 `DetailPrint` 与日志都会写「已按新版刷新 N 个配置文件（备份 N 个）」。

### 10.3 用户数据一律不动

写入清单只有上面三类 `*.custom.yaml`。下列内容**既不读也不写、不改名、不删除**：

* 用户词典：`*.userdb`、`*.userdb.txt`、`*.userdb.kct` 等；
* 词表与状态：`*.txt`、`custom_phrase.txt`、`installation.yaml`、`user.yaml`；
* `%APPDATA%\com.rimesync.app`（同步程序自己的配置与同步状态）；
* 本次改动没有新增任何 `Delete` / `RMDir` / 通配删除动作。

卸载器默认同样保留 `%APPDATA%\Rime`（只有交互卸载选「是」或静默卸载带 `/PURGEDATA`
才删，见第 7 节）。

### 10.4 怎么回滚备份

```powershell
# 看有哪些备份
Get-ChildItem "$env:APPDATA\Rime" -Filter *.bak-*
# 回滚单个文件：删掉新版，把备份改回原名
Remove-Item "$env:APPDATA\Rime\default.custom.yaml"
Rename-Item "$env:APPDATA\Rime\default.custom.yaml.bak-0.6.11" "default.custom.yaml"
# 回滚本版本的全部备份
Get-ChildItem "$env:APPDATA\Rime" -Filter *.bak-0.6.11* | ForEach-Object {
  Move-Item -Force $_.FullName ($_.FullName -replace '\.bak-[^\\]+$', '')
}
```

回滚后让小狼毫重新部署（托盘「重新部署」或 `WeaselDeployer.exe /deploy`）才生效。

### 10.5 本机能验到哪一步（真机仍需复核）

已验：`bash packaging/windows/check-nsis-hooks.sh --strict` —— 两套钩子 0 warning 0 error，
外加 22 条静态结构断言（卸载命令带引号 + `_?=`、120 秒超时、失败不 `Abort`、备份命名、
DONE 报数）。编译只能证明语法与结构，**运行期行为只能在真机复核**：

1. 装 0.6.10 → 再装 0.6.11：旧 `uninstall.exe /S _?=` 静默跑完、`rime-sync-app.exe` 被
   taskkill、日志里 `exit=0`；
2. 旧版带自建小狼毫：`BundledWeasel=1` 分支命中，小狼毫被卸、新版重装后再写标记；
3. 卸载器卡住（把 `uninstall.exe` 换成 `cmd /c pause`）→ 120 秒后 `timeout`，安装继续；
4. `%APPDATA%\Rime` 里的 `*.userdb*`、`*.txt` 升级后原样还在；
5. 手改过 `weasel.custom.yaml` → 升级后出现 `.bak-<版本>`，新版生效；
6. 备份名冲突（已有 `.bak-0.6.11`）→ 生成 `.bak-0.6.11-1`。
