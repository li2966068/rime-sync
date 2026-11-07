# Windows 输入法自举（B 方案）

从 0.6.23 之后的这一版起，Windows 安装器（NSIS）**只负责程序本体**，输入法
（小狼毫 / Weasel）的一切判断与安装都搬到程序里。本文说明为什么这么改、程序在
首次启动时到底做了什么、老用户升级会看到什么，以及怎么验证。

## 1. 为什么搬

- `/S` 静默安装时 Tauri 的 NSIS 钩子**根本不会执行**（0.6.19 实测：日志里连
  `phase=DONE` 都没有）。旧钩子还把「输入法就绪」当硬失败（退出码 5），于是
  静默升级必然装出「有同步程序、没有任何输入法」的机器。
- 钩子里的判据（注册表 + `WeaselServer.exe` + `rime.dll` 标记）、prebuilt 铺设、
  data 补丁、部署、自检各自实现，和程序里的 `ime_state::ensure` 结论经常互相矛盾。
- 结论：安装器只做它可靠的事（系统版本、WebView2、卸载旧程序、快捷方式），
  输入法交给**每次启动都会跑的同一个状态机**。

## 2. 职责划分

| 步骤 | 谁做 | 说明 |
|---|---|---|
| Windows 10 1803 门槛 | 安装器 | 低于门槛弹框 + 退出码 3 |
| WebView2 检查/安装 | 安装器 | 在线包只给下载地址；离线包内嵌 Standalone 静默安装 |
| 卸载旧版 rime-sync | 安装器 | `uninstall.exe /S _?=<dir>`，120 秒超时，失败只记日志 |
| 创建 `$INSTDIR\resources` | 安装器 | Tauri 随后把 resources 解压进去 |
| 开始菜单快捷方式 | 安装器 | 主程序 + `--setup` |
| **安装/修复 Weasel** | **程序** | `resources/weasel/weasel-0.17.4.0-installer.exe`，`/S /NOREBOOT`，300 秒超时 |
| **播 config 模板** | **程序** | `default.custom.yaml` / `weasel.custom.yaml` / `schemas/*.custom.yaml`，缺失才写 |
| **铺预编译 build** | **程序** | `resources/prebuilt`，缺才铺 |
| **patch data / 部署 / assess** | **程序** | 全部在 `crates/core/src/ime_state.rs` |
| 方案裁剪（prune） | 程序 | 启动自举时静默跑，`RIMESYNC_PRUNE_SCHEMAS=0` 关闭 |

钩子里**不再**出现：`StrStrW`、`rimesync_vnum`、`ensure-ime`、`check-ime`、
`RIMESYNC_PREBUILT`、`prune-schemas`、`WeaselServer.exe`、config 模板播种、
安装器自检与任何 IME 退出码/弹框。`packaging/windows/check-nsis-hooks.sh --strict`
会逐条断言这些字符串与函数都不存在。

## 3. 首次启动：`ensure(Mode::Install)`

入口是 `src-tauri/src/prebuilt_cmds.rs::bootstrap`，在 `setup` 里用**后台线程**
跑（不阻塞窗口）。顺序固定：

1. **输入法本体**（`crates/core/src/ime.rs`）
   - 读 `HKLM\Software\Rime\Weasel` 的 `WeaselRoot` / `InstallDir`（64/32 位视图），
     目录里必须有 `WeaselServer.exe`，同目录 `rime.dll` 必须能搜到
     `rimesync_vnum`（我们这套构建）。
   - 缺失 / 不是我们的构建 → 先 `taskkill /IM WeaselServer.exe /F`（只停进程，
     **不改用户数据**）→ 跑随包安装器 `/S /NOREBOOT`，最多等 300 秒。
   - 需要管理员时返回 `needs_elevation=true` 与可读错误（`请以管理员身份重新启动…`），
     **绝不静默失败**。
   - `RIMESYNC_INSTALL_IME=0` 可关闭自动安装（报告为 `disabled`，不算失败）。
2. **config 模板**（`ime_state::seed_templates`，仅 Windows 安装版）
   - `default.custom.yaml`：只有 Weasel 的 `data` 里真有 `wubi86.schema.yaml` 才播
     （否则部署会把缺失方案算成 failure、整次部署失败）。
   - `weasel.custom.yaml`：只影响外观，无条件播。
   - `schemas/<id>.custom.yaml`：`data` 里有 `<id>.schema.yaml` 才播。
   - **目标文件已存在就沿用**（不覆盖、不改名、不备份），与旧钩子语义一致。
3. **预编译产物**：`resources/prebuilt` → 用户目录（缺才铺；旧产物缺插件标记时整
   方案覆盖一次）。
4. **data 补丁 →（有改动才）部署 → 最终 assess**：结论只由最终 `assess` + 本次
   实际动作推出（`ok / noop / partial / failed`）。

首次安装时 `%APPDATA%\Rime` 可能还不存在：`ensure(Mode::Install)` 会创建它，
`src-tauri` 在 `detect_rime` 失败时也会回退到 `%APPDATA%\Rime`（仅 Windows）。

### 3.1 失败时的交互

- **阻塞式对话框**（后台线程里 `blocking_show_with_result`）：
  「输入法自举未完成…」+ 最近一步 + 「去修复 / Repair」「关闭 / Close」。
  选「去修复」会把主窗口带到前台。开机自启（`--autostart`）时**不弹**：
  开机就弹模态框等于每次开机都被打断，失败信息留在设置页红条里。
- **自举进行中**：开始自举即发布 `result="running"` 的状态（最长可能 300 秒：
  Weasel 安装器超时），界面据此显示「在忙」而不是一片空白。
- **排他锁**：自举与「修复输入法」共用同一把锁（`FIX_IME_BUSY`）。
  自举占着锁的时候点「修复」立刻返回 busy（不排队、不并发跑第二次
  `ensure` —— 两个 Weasel 安装器同时写 `%APPDATA%\Rime` 会写坏 build/）。
  「以管理员重启」拉起的新实例带 `--repair-ime`：启动就跑一遍修复，
  最多等旧实例退出（60 秒）再接手。
- **设置页红条**（`#ime-warning`，来源 `cmd_ime_status` 与 `ime://bootstrap` 事件）：
  显示最近一次自举结论、是否缺管理员权限；按钮：
  - **修复**：跑 `cmd_fix_ime`（同一个 `ime_state::ensure` + `ime::ensure`）；
  - **以管理员重启**：`cmd_restart_elevated` → `powershell Start-Process -Verb RunAs`，
    成功后当前实例退出。
- 排障日志：`%APPDATA%\com.rimesync.app\bootstrap.log`（含 `ime check …` 与
  `ensure action=weasel …`）。

## 4. 安装完成后会不会自动启动

| 安装方式 | 完成页 | 会不会启动程序 | 输入法何时装好 |
|---|---|---|---|
| 交互式（双击 setup.exe） | 显示，「运行 rime-sync」**默认勾选** | 会 | 首次启动的同一分钟内（后台自举） |
| 静默 `/S` | 不显示 | **不会** | 用户首次手动启动，或开机自启 `--autostart` 触发时 |

`/S` 安装不会启动程序是 NSIS 的固有行为（没有完成页）；这也是为什么钩子不再做
输入法：静默场景下它本来就不可靠。

## 5. 老用户升级路径

1. 旧版（≤ 0.6.23）卸载器会**连带卸掉小狼毫**；新版安装器**不会装回**。
2. 因此从「升级完成」到「首次启动程序」之间，机器上**没有输入法**（用户已接受）；
   这段时间不影响安装本身。
3. 首次启动程序时会用随包安装器装回输入法。**这一步需要管理员权限**：
   - 以管理员身份运行 rime-sync（右键 → 以管理员身份运行），或
   - 在设置页点「以管理员重启」。
4. 装回后：config 模板缺失才补、prebuilt 缺才铺、data 补丁与部署按需执行；
   `%APPDATA%\Rime` 里的用户词库、词表、自定义配置**全程不动**。

## 6. 卸载时的行为

- rime-sync 的卸载器**不再连带卸载小狼毫**（输入法是用户级应用，由用户自己装卸）。
  旧版留下的 `HKLM\Software\rime-sync` 所有权标记会在升级时兜底删除。
- 仍会询问「是否同时删除本地用户数据」（默认否；静默卸载需 `/PURGEDATA`），
  删除范围只有 `%APPDATA%\Rime` 与 `%APPDATA%\com.rimesync.app`。

## 7. 验证

~~~bash
# 1) 静态断言 + 两套钩子编译（0 warning / -WX）
bash packaging/windows/check-nsis-hooks.sh --strict

# 2) wine 运行期：静默安装不报错、不写任何 IME 日志、卸载询问/清理照常
bash packaging/windows/test-nsis-hooks-wine.sh

# 3) 真 Windows（CI windows-e2e）：模拟装完后首次启动
#    ensure-ime --mode install → result=ok 且 build 含 rimesync_vnum
~~~

## 8. 相关开关

| 环境变量 | 作用 |
|---|---|
| `RIMESYNC_INSTALL_IME=0` | 只检查不自动安装输入法（报告 `disabled`；输入法确实不在时状态**不算成功** —— 用户手上没有输入法必须看得见） |
| `RIMESYNC_PREBUILT=0` | 关闭预编译产物自举（输入法本体仍会装） |
| `RIMESYNC_PREBUILT=force` | 强制覆盖 build/ |
| `RIMESYNC_PREBUILT_DIR=<dir>` | 指定预编译产物目录（测试用） |
| `RIMESYNC_WEASEL_DATA=<dir>` | 指定 Weasel 的 data 目录（测试用） |
| `RIMESYNC_PRUNE_SCHEMAS=0` | 关闭非保留方案清理 |
