# 方案丙：主程序统一体（输入法与同步程序合一）

> 状态：2026-11-15 用户拍板采用本文档的「方案丙」。
> 本文是设计文档，代码尚未全部落地；每阶段完成后在此更新进度。

## 一句话

**主程序是唯一的大脑，输入法引擎只是被指挥的执行器。**

用户看到的只有**一个程序**（托盘 + 设置界面 + 同步全在里面）；输入法引擎（Weasel DLL /
fcitx5-rime 插件）只管打字，不管同步。

## 为什么不是另外两种形态

| 形态 | 做法 | 为什么不选 |
|---|---|---|
| 甲：config-via-sync 单一产物 | 同步程序接管全部配置生产 | 必要但不充分——用户体感仍是两个程序 |
| 乙：同步入口内嵌进输入法 | WebDAV / 三方合并编进 librime | 同步代码跑在输入法进程里，一个解析 bug 可能让**输入法起不来、用户打不了字**；还要在 C++ 侧从零写 WebDAV + TLS，并新增「编译 Linux librime」的 CI job |
| **丙：主程序统一体** | 主程序统一生产配置、统一指挥重编译；同步逻辑留在 Rust 侧 | **采用**。拿到甲的结构性优势 + 搜狗式的使用体感，但风险不外溢到打字 |

### 关于「一个程序」的物理边界

搜狗输入法**也不是**一个进程：主程序（托盘/设置/云同步）+ 输入法引擎 DLL。Windows 的 TSF
要求输入法引擎必须是被每个输入框进程加载的 DLL，Linux 的 fcitx5 同理（插件跑在 fcitx5
守护进程里）。所以「所有代码进同一个进程」**物理上做不到**，能做到的是搜狗那种**体感上的
一个程序**——这正是方案丙的目标。

rime-sync 的这个骨架**大部分已经存在**：Tauri 应用已有托盘（`src-tauri/src/tray.rs`）、
输入法已由程序自举安装、进程内部署已实现（`crates/core/src/librime.rs` 的 `deploy`）。
缺的是把两边真正焊死。

## 三个核心设计

### ① 配置只由主程序生产

现状是**三个生产者**互相覆盖：

| 生产者 | 位置 | 产出 |
|---|---|---|
| 随包模板 | `packaging/rime/schemas/*.custom.yaml` | 用户目录 custom.yaml |
| data 补丁 | `crates/core/src/data_patch.rs` 的 `Doc::ensure_processors` | 发行版 schema.yaml |
| 用户文件修复 | `crates/core/src/managed_custom.rs` 的 `repair_processors` | 用户目录 custom.yaml |

`custom.yaml` 的 `patch.engine/processors` 是**整段替换**语义（librime 的 `patch:` 对列表
整段替换），所以一份能把另一份**顶掉**——这就是「多来源打架」。

方案丙下：用户不再手改 `custom.yaml`，改配置走 GUI，主程序重新生成产物。
**三个生产者变一个**，下列问题一次性消失：

- `managed_custom` 补 punct 要改三处（审计发现③）
- 随包模板顺序违反 `verify()` 自己的「vselect 紧跟 recognizer」硬校验
- 新设备首次同步继承旧设备缺 numpad 的配置

### ② 主程序统一指挥重编译

现状：「要不要重编译」的判断散落在好几个调用点，各自为政：

- `engine.rs` 的 transport 路径调 `trigger.deploy()`——Windows 上这只 taskkill
  WeaselServer，**不重编译**，但 `report.ok` 仍是 true ⇒「同步成功」却没生效
  （与已修过的 P0「fcitx5-remote 假成功」同形）。
- `ime_state.rs` 里 `rebuild_after_config_change` 的唯一调用点条件是 `pointer_changed`，
  所以 Linux 上「修完用户 custom.yaml」**不重建** ⇒ 报告好了、当场不生效；Windows 走进程
  内部署当场生效 ⇒ **两端不一致**。

方案丙下这条规则**只有一处**：主程序写完配置 → 统一走重建入口，两端行为一致。

### ③ 同步逻辑留在 Rust 侧

**不**把 WebDAV / 三方合并编进 librime。这正是方案丙比方案乙安全一个数量级的地方：
同步的 WebDAV bug 最坏是「同步失败」，不会变成「输入法起不来、用户打不了字」。

搜狗也是这么干的——云词库在主程序进程里，不在输入法 DLL 里。我们照抄搜狗的架构，
而不是照抄它的野心。

## 落地阶段

| 阶段 | 内容 | 顺带修掉的审计发现 | 状态 |
|---|---|---|---|
| **一** | 抽出单一的 processors 规格，让三条路径共用；统一模板与 Rust 侧顺序；补 punct | ③、模板顺序违规 | ✅ 已落地 |
| **二** | 统一重编译通道：所有配置变更走一处 | ②⑥ | ✅ 已落地 |
| **三** | 路径解析收敛为一份（shell 改调 `rime-sync-cli detect --json`） | ⑤⑬ | ✅ 已落地 |
| **四** | 界面合一：输入法内的同步入口 = 主程序同一界面；托盘统一 | ⑩ | ✅ 已落地 |
| **五** | Linux 两个 deb 合并成一个 | 交付体感 | ✅ 已落地（v0.6.33） |

阶段一、二、三**并行推进**（文件边界不重叠）。

### 阶段四：界面合一怎么落

输入法设置里的同步入口，现在只是**拉起另一个程序**（fcitx5 配置工具 / Weasel 菜单里的按钮）。
方案丙下它要变成**主程序的同一个界面**。

可行性已核实：主程序侧有 `tray.rs`（托盘）与 Tauri command 体系（`src-tauri/src/lib.rs` 已有
约 15 个 `#[tauri::command]`），入口侧只需要改成**唤起主程序并带上参数**（而不是拉起独立进程），
主程序收到参数后切到对应页面。

关键约束：**入口侧不含同步逻辑**（`third_party/librime/plugins/rimesync-extra/` 里只有
numpad / punct / vnum / vselect 四个处理器，没有任何 WebDAV 代码），所以这一步是纯调用改动，
不触碰 C++ 侧。

**已落地（阶段四）**：

- 后端新增 `--panel <页面>` 启动参数（白名单：connection / sync / conflicts / devices /
  settings）与 `cmd_take_panel_request` 命令；页面名进白名单才生效，外部参数不能决定
  前端切到哪个视图。
- 接入 `tauri-plugin-single-instance`：主程序已在跑时，第二次启动（输入法设置页按钮、
  开始菜单项）把参数**转交给已有实例**，由它切页并把窗口带到前台，第二次启动随即退出
  —— 不再出现「两个窗口」。
- 前端 `init()` 取一次启动参数（切页），并在运行时监听 `panel://request` / `setup://request`
  事件（第二次启动走这条路）。
- 顺手修掉托盘「查看冲突」点了没反应的缺陷：托盘此前只 emit `tray://action` 而前端
  没有监听方，现在前端接上了。
- 入口侧改用 `--panel sync --setup`：切到同步页，同时保留「未配置时仍开设置向导」。
  Linux 的 `rime-sync-open-setup.sh` 与 Windows 开始菜单「同步设置」都已改。

### 阶段五：Linux 两个 deb 合并

**已落地（v0.6.33）**：两个 deb 由 `packaging/linux/merge-debs.sh` 合成单个
`rime-sync` 包（`Provides: fcitx5-rime`，`Conflicts/Replaces` 覆盖旧包名与发行版
`fcitx5-rime`/`librime1*`/`rime-data*`），输入法判定用的 shim 随包下发
（`/usr/lib/rime-sync/ime-detect-shim.sh`）。

下面是合并前的可行性分析（保留作背景）：现状（已核实）：`.run` 自解包后是**两个独立的 deb**——

- 同步程序 deb：Tauri bundler 产出（`src-tauri/tauri.conf.json` 的 `bundle.linux.deb`），
  维护脚本由 `packaging/linux/prepare-sync-deb.sh` 重打包
- 输入法 deb：`scripts/ime/package-ime-deb.sh` 产出，包名 `rime-inputmethod`，
  `Provides: fcitx5-rime`、`Conflicts/Replaces` 掉发行版的 `fcitx5-rime`/`librime1*`

合并的**可行做法**：把同步程序打进输入法 deb（同包），或让输入法 deb `Depends` 同步 deb。

**但有个硬约束**：`rime-inputmethod` 与发行版 `fcitx5-rime`/`librime-data` 的
`Conflicts`/`Replaces` 关系（2026-10-22 记忆：Debian 13 上发行版 `rime-data-*` 与自带
`rime-data` 冲突，尚未在装过发行版 rime-data 的机器上验过）——把同步程序塞进这个包会
让它的卸载语义与 `rime-inputmethod` 绑死，**用户单独卸载同步程序时可能连带卸掉输入法**。

⇒ 阶段五当时**先不做**，等阶段一~四落地、发行链稳定后再单独评估。Windows 侧不受影响
（本来就是单个 setup.exe）。

> **更新（v0.6.33）**：阶段五已按上面的思路落地并发布验证——合并后的单个 `rime-sync`
> 包在 Debian 13 上实测可装可卸，输入法载荷完整，离线 `.run` 内 269 个 vendor deb 的
> sha256 全部校验通过。上面这段「硬约束」是合并**之前**的判断，留作背景。

### 与审计清单的关系

审计出的问题按「会不会被方案丙重写」分三档：

- **合并前必修**（地基 + 数据安全，方案丙不会推翻）：数据安全（误删远端、拉取覆盖无备份、
  restore 快照被吞）、路径解析、错误处理（退避清零、落盘被吞、prune 静默跳过、GUI 静默改
  配置、密码 chmod）、CI 护栏。
- **合并后修**（在会被重写的链上，现在做白做）：随包模板顺序、AppImage 随包产物覆盖、
  `verify()` 的 punct 位置校验（与阶段一一起做掉）。
- **第三批**（测试可信度）：CI 加 Windows 测试类型检查、桌面端单测接 CI、`ensure → patch_auto_in`
  集成覆盖、让测试不再空转。

## 不做什么（红线）

- **不**把 WebDAV / HTTP / TLS 引入 C++ 侧。
- **不**动搜狗词库的归属——它只在用户本机，绝不进仓库、绝不进发布产物。
- **不**改 `device_local_keys`（仍是 `["schema_list"]`）。
- **不**改 `first_sync_policy` 的默认值（两端仍是 `normal`）。
- **不**在任何阶段里自动推送 GitHub 或打 tag——等用户明确指令。

## 已验证的硬事实（修法的地基）

- Linux 上**只有 `rime_deployer --build` 会重编译**；`fcitx5-remote -r` 与 DBus
  `ReloadConfig` 都不会（build/ 产物 mtime 不变、引擎日志零新增）。228 真机实测多次。
- Linux 正确顺序：**停 fcitx5 → `rime_deployer --build` → 起 fcitx5**。
- Windows 上**进程内部署**（`librime::deploy`）才会重编译。
- `custom.yaml` 的 `patch.engine/processors` 是整段替换语义 ⇒ 多来源必然互相顶掉。
- fcitx5 引擎自己拒绝相对路径的 `UserDataDirPath` 并回落到默认目录
  （`third_party/fcitx5-rime/src/rimeengine.cpp`）；rime-sync 与 shell 侧此前照单全收。
