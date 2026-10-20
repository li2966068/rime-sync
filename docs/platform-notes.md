# 平台触发机制事实证据（platform-notes）

> 目的：回答"在一个 Tauri 应用里，如何真正触发 Rime 的用户词库合并与部署"。
> 约定：每条结论都带来源；**已实测**=本机（Debian 13 headless，librime 1.13.1）跑过并贴了输出；**源码推断**=读源码/文档得出但未在该平台执行。
> 证据固定到 commit：rime/weasel `82b50c9e9b9ee0bd31357db6f25ddc804546c7f7`（2026-10-08）。

## 0. 结论速查表

| # | 问题 | 结论 | 证据等级 |
|---|---|---|---|
| 1 | `WeaselDeployer.exe /sync` 是否存在 | **存在**，语义=调用 `rime_api->sync_user_data()`，含维护模式 + join | 源码（L145-147、L344-383） |
| 2 | Weasel IPC 是否有 sync 请求类型 | **不存在**。`WEASEL_IPC_COMMAND` 全部 17 个枚举值中无 sync；通知类型只有 deploy/schema/option | 源码（WeaselIPC.h L18-36，RimeWithWeasel.cpp L677-720） |
| 3 | 官方托盘"同步用户资料"怎么做的 | 就是 ShellExecute `WeaselDeployer.exe /sync`（SW_HIDE），不走 IPC | 源码（WeaselServerApp.cpp L81-83、.h L20-26） |
| 4 | `/deploy` 会不会顺带同步 userdb | **不会**。`UpdateWorkspace()` 只 `rime->deploy()`，无 sync 调用 | 源码（Configurator.cpp L150-190） |
| 5 | 传错开关会怎样 | **静默打开 GUI 设置窗口并返回 0**（不是报错退出） | 源码（WeaselDeployer.cpp L149-150） |
| 6 | Linux 能否在 fcitx5/rime 引擎运行时 `rime_dict_manager --sync` | **不能**：LevelDB LOCK 被引擎独占，sync 打不开库 | **已实测**（见 §3.3） |
| 7 | sync 失败会返回非 0 吗 | **不会**：`--sync` 失败时退出码仍为 0，只能看 stderr/产物 | **已实测**（见 §3.4） |
| 8 | sync 快照写到哪 | `<user_data_dir>/sync/<installation_id>/*.userdb.txt`；无 installation.yaml 时目录叫 `unknown` | **已实测**（见 §3.5） |
| 9 | Windows 安装包形态 | NSIS `-setup.exe`（默认 WebView2 downloadBootstrapper，0MB，需联网） | 官方文档（§4.1） |
| 10 | Linux 产物与运行时依赖 | deb + AppImage；deb 依赖 `libwebkit2gtk-4.1-0`、`libgtk-3-0`（托盘再加 appindicator） | 官方文档（§4.2） |

## 1. Weasel：WeaselDeployer.exe 的开关全集

来源：<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/WeaselDeployer/WeaselDeployer.cpp#L19-L48>

```cpp
// WeaselDeployer.cpp:34-45（无资源字符串时的 help）
L"Usage: WeaselDeployer.exe [options]\n"
L"/? or /help\t\t- Show this help message\n"
L"/deploy\t\t- Update Workspace\n"
L"/dict\t\t- Manage dictionary\n"
L"/dict list\t\t- List user dictionaries\n"
L"/dict backup <dict>\t- Backup a user dictionary\n"
L"/dict restore <file>\t- Restore a user dictionary snapshot\n"
L"/dict export <dict> <file>\t- Export a user dictionary\n"
L"/dict import <dict> <file>\t- Import a user dictionary\n"
L"/sync\t\t- Sync user data\n"
L"/install\t\t- Install Weasel (Initial deployment)"
```

分派逻辑：<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/WeaselDeployer/WeaselDeployer.cpp#L118-L151>

| 开关 | 行为 | 源码行 |
|---|---|---|
| 无参数 | `configurator.Run(false)` → 打开设置 GUI | L132-134 |
| `/deploy` | `configurator.UpdateWorkspace()` → 维护模式 + `rime->deploy()` + 退出维护 | L136-138 → Configurator.cpp L150-190 |
| `/sync` | `configurator.SyncUserData()` → 维护模式 + `rime->sync_user_data()` + `join_maintenance_thread()` | L145-147 → Configurator.cpp L344-383 |
| `/install` | `configurator.Run(installing=true)` → 首次部署 | L149-150 |
| `/dict list\|backup\|restore\|export\|import` | 词库管理 CLI（`IsDictionaryCli` 要求 `argc > 2`） | L50-52、L140-143 |
| `/?` `/help` | 帮助（MessageBox） | L124-127 |
| 其他任意参数 | **落到 `configurator.Run(false)`：静默弹 GUI，返回 0** | L149-150 |

调用方必须知道的三个细节（均源码可查）：

1. **单实例互斥**：进程级 `CreateMutex(L"WeaselDeployerExclusiveMutex")`，第二个实例直接返回 1（WeaselDeployer.cpp L100-107）；内部操作还各有一个 `L"WeaselDeployerMutex"`（Configurator.cpp L151/L345）。→ 不要把 `/sync` 设为高频定时任务。
2. **`/sync` 是同步阻塞的**：`sync_user_data()` 后会 `rime->join_maintenance_thread()`（Configurator.cpp L368-373），进程退出即完成。→ 用 `Command::status()` 等待即可，不必轮询。
3. **退出码**：`/sync` 失败返回 1（Configurator.cpp L368-372）；互斥被占返回 1；参数错误返回 0（弹 GUI）。→ 只看退出码不够，还要检查 `sync/<installation_id>/*.userdb.txt` 的 mtime。

### 1.1 `/sync` 的内部实现（决定了它为什么能在 WeaselServer 常驻时工作）

<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/WeaselDeployer/Configurator.cpp#L344-L383>

```cpp
int Configurator::SyncUserData() {
  HANDLE hMutex = CreateMutex(NULL, TRUE, L"WeaselDeployerMutex");   // L345
  ...
  weasel::Client client;
  if (client.Connect()) {                                            // L361
    client.StartMaintenance();                                       // L363
  }
  { RimeApi* rime = rime_get_api();
    if (!rime->sync_user_data()) { ... return 1; }                    // L368-372
    rime->join_maintenance_thread();                                  // L373
  }
  CloseHandle(hMutex);
  if (client.Connect()) client.EndMaintenance();                      // L378-381
}
```

`StartMaintenance()` 的 IPC 实现是 `WEASEL_IPC_START_MAINTENANCE`（WeaselClientImpl.cpp L159-162），服务端处理为 `Finalize()`（释放 rime 会话与 userdb 句柄）：
<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/RimeWithWeasel/RimeWithWeasel.cpp#L479-L491>

```cpp
void RimeWithWeaselHandler::StartMaintenance() { m_session_status_map.clear(); Finalize(); _UpdateUI(0); }
void RimeWithWeaselHandler::EndMaintenance()   { if (m_disabled) { Initialize(); _UpdateUI(0); } m_session_status_map.clear(); }
```

→ 这正是 Windows 端"无锁冲突"的机制：维护模式让 server 先放手，deployer 再独占 userdb。**fcitx5 没有对应的维护通道**，所以 Linux 只能停进程（§3.6）。

## 2. Weasel IPC：没有 sync 语义

### 2.1 请求类型全集

来源：<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/include/WeaselIPC.h#L18-L36>

```cpp
enum WEASEL_IPC_COMMAND {
  WEASEL_IPC_ECHO = (WM_APP + 1),
  WEASEL_IPC_START_SESSION, WEASEL_IPC_END_SESSION, WEASEL_IPC_PROCESS_KEY_EVENT,
  WEASEL_IPC_SHUTDOWN_SERVER, WEASEL_IPC_FOCUS_IN, WEASEL_IPC_FOCUS_OUT,
  WEASEL_IPC_UPDATE_INPUT_POS, WEASEL_IPC_START_MAINTENANCE, WEASEL_IPC_END_MAINTENANCE,
  WEASEL_IPC_COMMIT_COMPOSITION, WEASEL_IPC_CLEAR_COMPOSITION, WEASEL_IPC_TRAY_COMMAND,
  WEASEL_IPC_SELECT_CANDIDATE_ON_CURRENT_PAGE, WEASEL_IPC_HIGHLIGHT_CANDIDATE_ON_CURRENT_PAGE,
  WEASEL_IPC_CHANGE_PAGE, WEASEL_IPC_LAST_COMMAND
};
```

**没有 `SYNC` / `DEPLOY` 请求**。管道名 `L"WeaselNamedPipe"`（同文件 L10），传输结构 `PipeMessage{Msg,wParam,lParam}`（L43-47），`WEASEL_IPC_TRAY_COMMAND` 只能复现托盘菜单点击，托盘菜单项本身又是 ShellExecute 外部进程（§2.3），绕一圈仍是 deployer。

### 2.2 通知（notify）类型只有三种

来源：<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/RimeWithWeasel/RimeWithWeasel.cpp#L677-L720>

```cpp
if (m_message_type == "deploy")      { start/success/failure → 提示气泡 }
else if (m_message_type == "schema") { 方案名 }
else if (m_message_type == "option") { ascii_mode / full_shape / 其他 }
```

→ 即使能注入通知，语义也只是"显示提示"，不能触发同步。

### 2.3 官方"同步用户资料"菜单的真实路径

<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/WeaselServer/WeaselServerApp.cpp#L68-L96>

```cpp
m_server.AddMenuHandler(ID_WEASELTRAY_DEPLOY, std::bind(execute, dir / L"WeaselDeployer.exe", L"/deploy"));   // L72-74
m_server.AddMenuHandler(ID_WEASELTRAY_SETTINGS, std::bind(execute, dir / L"WeaselDeployer.exe", L""));       // L75-77
m_server.AddMenuHandler(ID_WEASELTRAY_DICT_MANAGEMENT, ... L"/dict");                                        // L78-80
m_server.AddMenuHandler(ID_WEASELTRAY_SYNC, std::bind(execute, dir / L"WeaselDeployer.exe", L"/sync"));       // L81-83
```

`execute()` 用 `ShellExecuteW`，对 deployer 传 `SW_HIDE`（WeaselServerApp.h L20-26）：
<https://github.com/rime/weasel/blob/82b50c9e9b9ee0bd31357db6f25ddc804546c7f7/WeaselServer/WeaselServerApp.h#L20-L26>

→ **官方自己也是"起子进程"**，不是 IPC。我们的 Tauri 应用照做即可，不需要复刻管道协议。

## 3. Linux：librime 命令行与真实约束

环境：Debian 13 (trixie)，`librime-bin 1.13.1+dfsg1-1`，`librime-dev 1.13.1`，`fcitx5-rime 5.1.10-2`，`rclone v1.60.1`。

### 3.1 librime-bin 提供什么（已实测）

```text
$ dpkg -L librime-bin | grep /usr/bin
/usr/bin/debian-rime-processor
/usr/bin/rime_deployer
/usr/bin/rime_dict_manager
/usr/bin/rime_patch
/usr/bin/rime_table_decompiler
```

`rime_dict_manager` 无参数时的用法输出（**已实测**）：

```text
options:
        -l|--list
        -s|--sync
        -b|--backup dict_name
        -r|--restore xxx.userdb.txt
        -e|--export dict_name export.txt
        -i|--import dict_name import.txt
```

`rime_deployer` 用法（**已实测**，节选）：

```text
--add-schema <schema_id>...
--build [user_data_dir] [shared_data_dir] [staging_dir]
        Build and deploy Rime data.
        If unspecified, user_data_dir and shared_data_dir defaults to the working directory.
--compile <x.schema.yaml> [user_data_dir] [shared_data_dir] [staging_dir]
--set-active-schema <schema_id>
```

**关键行为（已实测）**：`rime_dict_manager` 的 user_data_dir 就是**当前工作目录**——在 `/tmp/rtest/user` 下执行 `--import luna_pinyin word.txt`，生成的是 `/tmp/rtest/user/luna_pinyin.userdb/`。→ 调用时必须 `current_dir(rime_dir)`，不要指望环境变量。

`rime_deployer --build` 冒烟（**已实测**）：

```text
$ cd /tmp/rtest/user && rime_deployer --build /tmp/rtest/user /usr/share/rime-data
$ ls /tmp/rtest/user
build/  user.yaml
$ ls build | head -5
bopomofo.prism.bin  bopomofo.schema.yaml  cangjie5.reverse.bin  default.yaml  luna_pinyin.prism.bin
```

→ `--build` 只更新 `build/`（`*.bin` 引擎产物）与 `user.yaml`，**不产生/不合并 `*.userdb`**；两者是不同步骤，apply 必须两个都跑。

### 3.2 用 librime 生成一份真实 userdb（供测试）

```bash
printf 'ni\t你\t1\n' > /tmp/word.txt
cd /tmp/rtest/user && rime_dict_manager --import luna_pinyin /tmp/word.txt
# → imported 1 entries. 并生成 luna_pinyin.userdb/{CURRENT,LOCK,LOG,MANIFEST-*,*.log,*.ldb}
```

### 3.3 LevelDB 锁：引擎存活时 `--sync` 必然失败（**已实测**）

测试设计：写一个 30 行的 C 程序（`librime-dev` + `-lrime`）初始化引擎、`create_session()`、`select_schema("luna_pinyin")`、然后 `sleep(20)` 模拟输入法常驻；期间用 `fcntl.lockf` 探 LOCK，并跑一次 `rime_dict_manager --sync`。

```text
session_created=1
holding
== try lockf on userdb LOCK while engine alive ==
LOCK_BLOCKED: [Errno 11] Resource temporarily unavailable
== fd check (/proc/<pid>/fd) ==
lrwx------ 8 -> /tmp/rtest/user/luna_pinyin.userdb/LOCK
l-wx------ 9 -> /tmp/rtest/user/luna_pinyin.userdb/000015.log
== sync while engine alive ==
sync dir: sync
user id: 35cdfd89-5363-46f2-9808-08ac516be575
E... level_db.cc:259] Error opening db 'luna_pinyin': IO error: lock /tmp/rtest/user/./luna_pinyin.userdb/LOCK: Resource temporarily unavailable
E... user_dict_manager.cc:198] failed to merge snapshot file: sync/unknown/luna_pinyin.userdb.txt
E... level_db.cc:273] Error opening db 'luna_pinyin' read-only.
E... user_dict_manager.cc:204] error backing up user dict 'luna_pinyin'.
E... user_dict_manager.cc:220] failed synchronizing 1/1 user dicts.
== sync after engine exit ==
sync dir: sync
user id: 35cdfd89-...
```

结论（**能/不能/条件**）：
- **不能**：只要有一个存活的 librime 引擎（fcitx5-rime / ibus-rime / 任何 `RimeApi::create_session` 的进程）打开过该 userdb，`rime_dict_manager --sync` 就失败，报 `Resource temporarily unavailable`。
- **条件**：引擎进程退出（或至少其 rime 会话被销毁、LevelDB 关闭）后，同一条命令立即成功。
- 对 fcitx5-rime 的映射：fcitx5-rime 是 librime 的宿主（addon 常驻、会话随输入法生命周期存在），因此"fcitx5 在跑 → 持锁"属**源码+机制推断**；本机 headless 未能让 fcitx5 可靠加载 rime addon（见 §3.7），故"在真实 fcitx5 会话上复现"标记为**未验证**。桌面上的验证方法：

```bash
# 1) 找出谁持有 userdb 的 LOCK（二选一）
sudo lsof +D ~/.local/share/fcitx5/rime 2>/dev/null | grep LOCK
fuser -v ~/.local/share/fcitx5/rime/luna_pinyin.userdb/LOCK
# 2) 复现失败
cd ~/.local/share/fcitx5/rime && rime_dict_manager --sync    # 期望看到 Resource temporarily unavailable
# 3) fcitx5-remote -e 之后再跑一次，期望成功
```

### 3.4 陷阱：`--sync` 失败时退出码仍是 0（**已实测**）

```text
rime_dict_manager --sync ... ; echo "sync_exit=$?"
sync_exit=0        # 即使上面打印了 failed synchronizing 1/1 user dicts
```

→ 实现里**不能**靠 `ExitStatus` 判断成功。可靠判据（按优先级）：
1. stderr/stdout 是否包含 `failed synchronizing` / `Resource temporarily unavailable`；
2. `sync/<installation_id>/*.userdb.txt` 的 mtime 是否更新、文件大小是否 > 0；
3. 引擎退出后重试一次。

### 3.5 sync 目录名来自 installation_id（**已实测**）

- 没有 `installation.yaml` 时：`user id: unknown` → 快照写到 `sync/unknown/`。
- 引擎跑过一次 `initialize()` 后自动生成 `installation.yaml`：

```yaml
installation_id: "35cdfd89-5363-46f2-9808-08ac516be575"
rime_version: 1.13.1
```

→ 快照目录变成 `sync/35cdfd89-.../`。**上传时不要硬编码目录名**：扫描 `sync/*/*.userdb.txt`，并按 `installation.yaml` 的 `installation_id` 映射到远端的 `sync/<device_id>/`；若远端已有该 id 的目录，则先下载再 sync（librime 会把 `sync/` 下**每个子目录**当成一台设备合并，这正是 PLAN 硬约束 2 的由来）。

### 3.6 停/启 fcitx5 的正确命令与风险

`fcitx5-remote --help`（**已实测**）关键项：

```text
-c inactivate input method        -o activate input method
-r reload fcitx config            -e Request fcitx to exit
-a print fcitx's dbus address     --check Check if Fcitx is already running, if not, return 1.
```

推荐序列：

```bash
# 停（优雅，走 DBus，会保存状态）
fcitx5-remote -e
# 等它真的退出：--check 成功(=已退出，返回非0) 或进程消失
for i in $(seq 1 50); do fcitx5-remote --check >/dev/null 2>&1 || break; sleep 0.1; done
# 起
fcitx5 -d
```

风险与替代：
- `pkill -f fcitx5`：会命中所有含 "fcitx5" 的命令行（包括本脚本、面板插件），且由 systemd user unit 拉起时会被**自动重启**，出现"刚停又起来"的竞态。仅在无 DBus 的兜底路径使用，并且只针对确切 PID。
- `systemctl --user restart fcitx5`：仅当存在 user unit 才有效。本机 `systemctl --user list-unit-files | grep -i fcitx` 无输出（**已实测**），说明 Debian 默认不装该 unit，不能作为主路径。
- 本机 headless 下 `fcitx5-remote -e` 返回 0，但**没有运行中的 fcitx5**（`--check` 也返回 0），→ 该退出码不可信，必须用 `--check`/进程存在性二次确认（**已实测**）。
- apply 期间输入法不可用（PLAN §3 已声明"会打断输入"）；GUI 必须禁用按钮并显示倒计时，异常退出时用 `Drop`/guard 保证 `fcitx5 -d` 一定被调用。

### 3.7 未能实测的部分（明确标注）

| 项 | 状态 | 原因 | 验证方法 |
|---|---|---|---|
| 真实 fcitx5 会话持锁 | 未验证（机制已实测） | headless 无 X/Wayland 会话；dbus-run-session + Xvfb 下 fcitx5 能起但 rime addon 未加载（日志无 rime 行），无 userdb 句柄 | 桌面环境按 §3.3 末尾三条命令 |
| `ibus-rime` 持锁 | 未验证 | 未安装 | 同上，把路径换成 `~/.config/ibus/rime` |
| Windows 上 `/sync` 实测 | 未验证 | 无 Windows 机器 | 见 §1 的检查清单 + 检查 `%APPDATA%\Rime\sync\<id>` |

## 4. Tauri v2 打包事实

### 4.1 Windows / WebView2（官方文档，未实测）

来源：<https://v2.tauri.app/distribute/windows-installer/>

- 产物：`.msi`（WiX，**只能在 Windows 上打**）或 `-setup.exe`（NSIS，可交叉编译）。
- WebView2 安装模式对比（文档原文表格）：

| 模式 | 需要联网 | 额外体积 | 备注 |
|---|---|---|---|
| `downloadBootstrapper`（默认） | 是 | 0MB | 安装包最小；Win7 + msi 不推荐 |
| `embedBootstrapper` | 是 | ~1.8MB | 对 Win7 的 msi 更友好 |
| `offlineInstaller` | 否 | ~127MB | 离线环境推荐 |
| `fixedVersion` | 否 | ~180MB | 内嵌固定版本运行时 |
| `skip` | 否 | 0MB | 不推荐（不装运行时） |

- 文档明确：Win10 1803+ / Win11 的 WebView2 运行时随系统提供。
- 配置写法（文档原文）：

```json
{ "bundle": { "windows": { "webviewInstallMode": { "type": "downloadBootstrapper" } } } }
```
（`fixedRuntime` 需同时给 `path`。）

**本项目选择**：默认 `downloadBootstrapper`（0MB、Win10/11 绝大多数已预装），在内网/离线分发场景切 `offlineInstaller`（+~127MB）。`fixedVersion` 不选：体积 180MB 且需自行跟进安全更新。

### 4.2 Linux：deb 与 AppImage（官方文档，未实测）

来源：<https://v2.tauri.app/distribute/debian/>、<https://v2.tauri.app/distribute/appimage/>

- deb：默认依赖 `libwebkit2gtk-4.1-0`、`libgtk-3-0`，用系统托盘再加 `libappindicator3-1`；追加依赖走 `bundle > linux > deb > depends`，附加文件走 `deb > files`。
- AppImage：需要 `bundleMediaFramework` 才带 GStreamer（只在音视频场景需要，本应用不需要，保持关闭以减小体积）。
- 构建基线：**Ubuntu 22.04 / Debian 12**（仓库里有 `libwebkit2gtk-4.1-dev`）；在新系统上构建会抬高 glibc 下限，老系统会报 `GLIBC_2.xx not found`。
- 交叉注意：AppImage 打包必须装 `patchelf`；CI 官方示例还装 `xdg-utils`。

### 4.3 CI 前置（官方文档 + 版本实测）

<https://v2.tauri.app/distribute/pipelines/github/> 原文安装命令：

```bash
sudo apt-get update
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf xdg-utils
```

版本可用性（本机查 GitHub API **已实测**，2026-10）：

```text
tauri-apps/tauri-action   action-v1.0.0
actions/checkout          v7.0.1
actions/setup-node        v7.1.0
pnpm/action-setup         v6.1.0
swatinem/rust-cache       v2.9.2
actions/upload-artifact   v7.0.2
```

→ `.github/workflows/build.yml` 用这些主版本号；Rust 用 `dtolnay/rust-toolchain@stable`；Windows runner 无需额外系统依赖（WebView2 运行时在 `windows-latest` 镜像内，**推断**：官方示例对 Windows 未加任何安装步骤）。

## 5. 降级链（实现按此顺序探测，命中即停）

### 5.1 Windows 触发链

| 级别 | 手段 | 判定成功 | 失败/风险 |
|---|---|---|---|
| 首选 | `WeaselDeployer.exe /sync`（然后 `/deploy`） | 退出码 0 且 `%APPDATA%\Rime\sync\<installation_id>\*.userdb.txt` mtime 更新 | 未安装 Weasel / 另一个 deployer 在跑（返回 1）/ 参数写错会弹 GUI |
| 备选 | `WeaselDeployer.exe /deploy` | 退出码 0 且 `build/` 内 `*.bin` mtime 更新 | **不含 userdb 合并**，只是部署；用户词库仍靠 transport |
| 兜底 | 仅 transport + UI 明示"请手动右键托盘 → 同步用户资料" | 用户操作后远端 `sync/<id>/` 出现新快照 | 需人工；PLAN §7 已允许"降级为仅 transport 并告警" |

**不要尝试**：自己往 `\\.\pipe\WeaselNamedPipe` 写 `START_MAINTENANCE` 再改文件——维护模式只做 `Finalize()`（RimeWithWeasel.cpp L479-483），不会 sync，而且管道协议无版本协商。

### 5.2 Linux 触发链

| 级别 | 手段 | 判定成功 | 失败/风险 |
|---|---|---|---|
| 首选 | 停 fcitx5 → `rime_dict_manager --sync` → `rime_deployer --build <user> <shared>` → 启 fcitx5 | stderr 无 `failed synchronizing` 且快照 mtime 更新；`--build` 退出码 0 | 停/启失败会导致输入法不可用；必须 guard 保证重启 |
| 备选 | 不停 fcitx5，只 `--build`（配置类改动 lib rime 会热加载？**未验证**）+ transport 传快照 | `build/` 更新 | userdb 合并需重启后由用户触发；**不要**在持锁时反复重试 sync |
| 兜底 | 仅 transport，提示用户"退出输入法后点应用" | 远端一致 | 需人工 |

## 6. 对实现的硬性要求（由证据直接推出）

1. `rime_dict_manager` 必须 `current_dir(rime_dir)` 执行，命令里不加目录参数。
2. `--sync` 必须**解析输出**，不能只看退出码（§3.4）。
3. 快照目录必须从 `installation.yaml` 读 `installation_id`，且只扫 `sync/*/*.userdb.txt`（§3.5）。
4. Windows 端调用 deployer 时加 `CREATE_NO_WINDOW`，并把 stdout/stderr 收进日志——参数错误会弹 GUI（§1），静默失败极难排查。
5. apply 必须有 finally：无论 sync/build 是否失败都要 `fcitx5 -d`。
6. 任何情况下都不要在 `shared/` 下写 `*.userdb.txt`（PLAN 硬约束 2；librime 把 `sync/` 每个子目录当设备）。

## 7. 本文件的假设与未验证清单

- 无 Windows 环境：§1/§2 全部为源码结论 + 官方行为推断，未执行。
- 未在真实 fcitx5 桌面会话复现锁冲突（§3.7）。
- 未实测 Tauri 构建产物体积；§4 的体积数字来自官方文档表格（WebView2）与经验估算（deb/AppImage），实现后以 CI artifact 为准。
- 未实测各 WebDAV 服务商（坚果云/Nextcloud/群晖）的实际 URL 与认证行为，只在 USER-GUIDE 给出"常见形态 + 自检命令"。
