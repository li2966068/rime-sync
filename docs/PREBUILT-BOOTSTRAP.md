# 输入法产物自举（0.6.16）

把「铺预编译 build 产物」从安装器搬到程序自身：安装器钩子不跑、或本机
librime / WeaselDeployer 部署不出 `build/` 时，由程序自己把随包的产物铺进
Rime 用户目录。安装器的同名逻辑仍然保留（能跑就跑），程序自举是**最后一道兜底**。

## 1. 为什么必须由程序做

0.6.15 的实测结论：

* 安装包确实带了 `resources\prebuilt`（22 个文件），但 `/S` 静默安装时我们的
  NSIS 钩子**根本没执行**（`%TEMP%\rime-sync-setup.log` 里连 `phase=DONE` 都没有）；
* 测试机 192.168.61.227 上 `WeaselDeployer /deploy` 返回 0，却不产出任何
  build 文件（官方 Weasel 也一样）；
* 于是「安装器负责铺产物」不可依赖：钩子可能不跑，部署器可能假成功。

## 2. 判据（probe）

只认 **文件在 + 插件标记在**：

| 状态 | 含义 |
|---|---|
| `none` | `<Rime 用户目录>/build/luna_pinyin.schema.yaml` 不存在 |
| `deploy` | 文件存在，但不同时包含 `rimesync_vnum` 与 `rimesync_punct`（官方部署产物） |
| `prebuilt` | 文件存在且两个标记都在 —— 我们这套产物，可用 |

判据与 Windows NSIS 钩子、Linux `postinst` / `install.sh` 完全一致。只有
`prebuilt` 会被当作可用；`none` 与 `deploy` 都会触发自举。

## 3. 模块与 API

`crates/core/src/prebuilt.rs`（GUI 与 CLI 共用）：

```rust
pub enum BuildProbe { None, Deploy, Prebuilt }
pub struct PayloadLocation { pub path: PathBuf, pub source: String }
pub struct InstallOptions { pub force: bool }
pub struct PrebuiltReport {
    pub copied: usize,        // build/ 新铺
    pub kept: usize,          // build/ 已存在跳过
    pub overwritten: usize,   // build/ 被覆盖
    pub config_copied: usize, // 用户目录根下 *.yaml 新铺
    pub config_kept: usize,   // 用户目录根下 *.yaml 已存在跳过
    pub failed: usize,        // 复制失败个数（不中断）
    pub source: String,       // payload 目录
    pub build_ok: bool,       // 铺完后的判据是否可用
    pub probe_after: String,  // none / deploy / prebuilt
    pub escalated: bool,      // 是否触发 build/ 的第二次机会
    pub log_path: Option<String>,
}

pub fn probe_build(rime_user_dir: &Path) -> BuildProbe;
pub fn resolve_payload(exe_dir: Option<&Path>) -> Result<PayloadLocation>;
pub fn install_prebuilt(payload_dir: &Path, rime_user_dir: &Path) -> Result<PrebuiltReport>;
pub fn install_prebuilt_with(
    payload_dir: &Path,
    rime_user_dir: &Path,
    options: InstallOptions,
) -> Result<PrebuiltReport>;
```

`install_prebuilt` 是「不覆盖」模式；`install_prebuilt_with(… force: true)`
等价安装器的 `RIMESYNC_PREBUILT=force`。

`crates/core/src/data_patch.rs`（0.6.18，见第 9 节）：

```rust
pub enum PatchOutcome { Patched, Unchanged, Skipped }
pub struct DataPatchReport {
    pub data_dirs: Vec<String>,
    pub schemas_patched: Vec<String>,
    pub schemas_unchanged: Vec<String>,
    pub schemas_skipped: Vec<String>,
    pub backup_dir: String,
    pub errors: Vec<String>,
}

pub fn data_dirs() -> Vec<PathBuf>;
pub fn patch_dirs(dirs: &[PathBuf], version: &str) -> DataPatchReport;
pub fn patch_file(path: &Path, version: &str) -> Result<PatchOutcome>;
pub fn backup_path(path: &Path) -> PathBuf;
pub fn weasel_root_from_reg_output(output: &str) -> Option<PathBuf>;
```

`schemas_patched.len()`（即 `report.patched()`）就是 CLI / 界面报告里的
`data_patched`。

## 4. payload 结构与铺入规则

```text
<payload>/
  build/*                      → <rime_user_dir>/build/
  config/*.yaml                → <rime_user_dir>/
  MANIFEST.json                （不铺，仅供生成脚本/排障）
```

逐文件执行：

1. `build/`：**缺才铺**。目标已存在就计入 `kept`，不动。
2. `config/*.yaml`：**只补缺**，任何模式下都不覆盖（用户配置）。
3. **第二次机会**：第一遍铺完 `probe_build` 仍不是 `prebuilt`（典型：
   用户目录里本来就有旧部署产物、没有插件标记，「缺才铺」把它们跳过去了）→ 只对
   `build/` **覆盖一次**（`escalated=true`）。`build/` 是部署器随时重写的
   派生数据，覆盖安全；`config/*.yaml` 绝不参与。
4. `force=true`：第一遍就覆盖 `build/`，因此不需要第二次机会。
5. 单个文件复制失败只计入 `failed` 并继续，不中断整批。

返回值里的 `build_ok=false` 表示「铺完了但仍不可用」——这不是 IO 错误，
调用方自行决定退出码 / 提示。payload 目录本身缺失（没有 `build/`）才是 `Err`，
错误信息会给出具体路径与 `RIMESYNC_PREBUILT_DIR` 提示。

## 5. payload 目录定位

按顺序探测，第一个含 `build/` 子目录的胜出：

| 顺序 | 路径 | 场景 |
|---|---|---|
| 0 | `$RIMESYNC_PREBUILT_DIR` | 调试 / 测试强制指定（设了但目录不合法 → 直接报错，不静默回落） |
| 1 | `<exe 目录>/resources/prebuilt` | Windows NSIS 安装版 |
| 2 | `<exe 目录>/../lib/rime-sync/resources/prebuilt` | Tauri 在 Linux 的 deb / AppImage 资源目录（`/usr/bin/rime-sync` → `/usr/lib/rime-sync/...`） |
| 3 | `$APPDIR/usr/share/rime-sync/prebuilt` | AppImage |
| 4 | `$APPDIR/usr/lib/rime-sync/resources/prebuilt` | AppImage（Tauri 资源布局） |
| 5 | `/usr/share/rime-sync/prebuilt` | Linux deb |

全部不存在时返回可读错误，并列出检查过的路径。

## 6. 启动自举的时机与降级

`src-tauri/src/lib.rs` 的 `setup` 里调用 `prebuilt_cmds::bootstrap(&handle)`：

* **后台线程**，`setup` 立即返回 —— 磁盘拷贝绝不阻塞窗口出现；
* 读配置 → 定位 Rime 用户目录 → `probe_build`；
  * `prebuilt`：不铺文件，但**每一步仍然写日志**（0.6.17 起，见第 8 节）；
  * `none` / `deploy`：`install_prebuilt`，结果写应用日志 `[bootstrap] …` 与
    `bootstrap.log`；
* 任何失败（读配置、定位目录、payload 缺失、复制失败）都只记日志：
  **不弹窗、不退出、不影响同步功能**，并且失败原因一定落在 `bootstrap.log` 里；
* 开关与安装器一致：`RIMESYNC_PREBUILT=0` 关闭自举；`RIMESYNC_PREBUILT=force`
  覆盖 `build/`。
* 之后无论 `probe` 是什么都会跑一次 data 方案补丁（第 9 节）：即使 `prebuilt`
  也会跑，因为 227 的实测是「build 已经对了但引擎不用它」；同样只记日志，不阻塞启动。

## 7. 手动入口

### 7.1 设置页

「设置 → 修复输入法」：

* **修复输入法**：缺才铺（第一次自举失败后重试用）；
* **强制覆盖**：等价安装器的 `force`（只覆盖 `build/`，`config/*.yaml` 不动）。

两者都调用 Tauri 命令 `cmd_fix_ime`，展示 `copied / kept / overwritten / payload_source`
与日志路径。
0.6.18 起报告里还会带 `data_patched` 与 `data`（第 9 节）；data 写入失败时
日志里会有提权提示，不弹窗、不中断。

### 7.2 CLI

Windows 安装版从 0.6.17 起随包带 `rime-sync-cli.exe`（`<安装目录>\rime-sync-cli.exe`，
与主程序同级）；便携版解压目录里也是同一份。Linux deb 在 `/usr/bin/rime-sync-cli`，
AppImage 在 `$APPDIR/usr/bin/rime-sync-cli`。

```bash
# 自动定位 payload（安装目录 / deb / AppImage）
rime-sync-cli fix-ime

# 覆盖已存在的 build/（config 永不覆盖）
rime-sync-cli fix-ime --force

# 只铺 build/，不给 data 方案打补丁
rime-sync-cli fix-ime --no-data-patch

# 脚本消费：输出 PrebuiltReport + data 补丁计数的 JSON
rime-sync-cli fix-ime --json

# 指定 payload（调试 / 离线手工铺）
rime-sync-cli fix-ime --payload /path/to/prebuilt --rime-dir /path/to/Rime
```

`--rime-dir` 与 `--json` 是全局参数（写在子命令前后都可以）。`fix-ime`
不访问 WebDAV、不解析密码（不触发钥匙串探测）。铺完 `build_ok=false` 时退出码为 1
（JSON 仍会先打印完整报告）。

## 8. 日志

有两条独立的日志，排查时先看 `bootstrap.log`——它证明「自举到底跑没跑」。

### 8.1 启动自举日志 `bootstrap.log`（0.6.17）

| 平台 | 路径 |
|---|---|
| Windows | `%APPDATA%\com.rimesync.app\bootstrap.log` |
| Linux | `~/.config/com.rimesync.app/bootstrap.log` |

（即 Tauri 的 `app_config_dir`；目录不存在会自动创建，日志文件本身写不进去时
降级成只写应用日志，不影响自举。）

每一步单独一行，先落盘再执行下一步；失败也写。格式：

```text
[2026-11-06 07:12:33] bootstrap begin version=0.6.17 exe=C:\Program Files\rime-sync\rime-sync-app.exe RIMESYNC_PREBUILT=<unset>
[2026-11-06 07:12:33] rime_dir=C:\Users\me\AppData\Roaming\Rime source=config
[2026-11-06 07:12:33] probe_before=none
[2026-11-06 07:12:33] payload hit path=C:\Program Files\rime-sync\resources\prebuilt source=exe:resources/prebuilt
[2026-11-06 07:12:33] install start force=false
[2026-11-06 07:12:34] install done copied=20 kept=0 overwritten=0 config_copied=2 config_kept=0 failed=0 probe_after=prebuilt build_ok=true escalated=false core_log=C:\Users\me\AppData\Roaming\Rime\.rimesync-prebuilt.log
[2026-11-06 07:12:34] bootstrap end result=ok
```

字段与取值：

| 行 | 说明 |
|---|---|
| `bootstrap begin` | 版本、exe 路径、进程启动时的 `RIMESYNC_PREBUILT` |
| `rime_dir=` | 命中的 Rime 用户目录与来源（`source=config` / 自动探测） |
| `probe_before=` | `none` / `deploy` / `prebuilt` |
| `payload hit` | 命中的 payload 目录与候选来源（`exe:resources/prebuilt`、`appimage:…`、`env:RIMESYNC_PREBUILT_DIR`） |
| `payload failed` | 所有候选都没命中；`error=` 里列出检查过的路径 |
| `install done` | `copied` / `kept` / `overwritten` / `config_copied` / `config_kept` / `failed` / `probe_after` / `build_ok` / `escalated` |
| `bootstrap end` | `result=ok` / `degraded`（铺了但仍不可用）/ `skip`（附 `reason=`）/ `failed`（附 `stage=`） |
| `config failed` / `rime_dir failed` / `install failed` | 异常原文，失败路径也有结束行 |

文件超过 1 MiB 时轮转成 `bootstrap.log.1`（只留一份），不会无限增长。

排查顺序（PowerShell）：

```powershell
# 1) 自举到底跑没跑（没有这个文件 = 进程没启动到这里 / 配置目录不可写）
Get-Content "$env:APPDATA\com.rimesync.app\bootstrap.log" -ErrorAction SilentlyContinue

# 2) 只看失败与结束行
Get-Content "$env:APPDATA\com.rimesync.app\bootstrap.log" -ErrorAction SilentlyContinue |
  Select-String -Pattern "failed|bootstrap end|payload"

# 3) 安装目录里到底有没有随包产物与 CLI
Get-ChildItem "$env:ProgramFiles\rime-sync\resources\prebuilt\build" -Filter *.schema.yaml
Test-Path "$env:ProgramFiles\rime-sync\rime-sync-cli.exe"

# 4) 同一条路径手工重跑（会写同样的计数，另有 CLI 自己的输出）
& "$env:ProgramFiles\rime-sync\rime-sync-cli.exe" fix-ime
```

### 8.2 铺入明细 `<Rime 用户目录>/.rimesync-prebuilt.log`

每次自举的明细追加到这里（由 `crates/core/src/prebuilt.rs` 写）：
开始行（payload / rime / force / probe_before）、覆盖与失败明细、结束行
（计数 / probe_after）。`bootstrap.log` 里的 `core_log=` 就是它的路径。

### 8.3 应用日志

设置页「日志」可见的 `[bootstrap] …` / `[fix-ime] …`：与 `bootstrap.log` 同一批事件，
但只保留最近若干条，且进程退出就没了。

`.rimesync-prebuilt.log` 与 `bootstrap.log` 都不在同步范围内（只同步
`*.custom.yaml`、词库快照与词典源）。

## 9. data 方案补丁（0.6.18）

### 9.1 为什么还要改 data

0.6.16/0.6.17 的兜底只把随包产物铺进 `%APPDATA%\Rime\build\`。227 实测：

* `build/luna_pinyin.schema.yaml` 里已经有 `rimesync_vnum`（translators）、
  `rimesync_vselect`（processors），`rime.dll` 里也确实有插件符号；
* 但按 `a` 选词后上屏文本后面多出一个 `a`、小键盘数字完全没反应、`v1234`
  也没有候选 —— **引擎没有使用我们铺的 build**，它在用 `<WeaselRoot>\data\`
  下的原版方案（或部署失败后回退）；
* 该机的 `WeaselDeployer /deploy` 恒返回 0 且不产出任何文件，部署链不可用。

所以从 0.6.18 起，`fix-ime` 与启动自举在铺完 build 之后，还会**直接改引擎实际
加载的方案文件**：

| 平台 | 目标 |
|---|---|
| Windows | `<WeaselRoot>\data\<schema>.schema.yaml`；`WeaselRoot` 取注册表 `HKLM\Software\WOW6432Node\Rime\Weasel` 的 `WeaselRoot`，没有就取 `InstallDir`；都读不到时退回 `%ProgramFiles%\Rime\data` |
| Linux | `/usr/share/rime-data/<schema>.schema.yaml`；目录不存在就跳过并记进报告 |

`RIMESYNC_WEASEL_DATA` 可以显式指定目录（多个用系统路径分隔符；空串或 `0` 关闭
补丁，测试靠它隔离）。

### 9.2 写入内容

对 data 目录下每一个 `*.schema.yaml`（只扫一层，不动子目录）：

| 位置 | 改动 |
|---|---|
| `engine/translators` | 首位插入 `rimesync_vnum`（已在别处就挪到首位，不重复） |
| `engine/processors` | 在 `recognizer` 之后插入 `rimesync_vselect`，紧接着插入 `numpad`；没有 `recognizer` 时依次退到 `key_binder` / `speller` 之前，都没有就追加到末尾 |
| `recognizer/patterns` | 增加 `vnum: '^[vV][0-9.]*$'`；已有 `vnum`（用户自定义）就不动 |
| 文件顶部 | 插入 `# patched by rime-sync <version>`（已有就不再插，不追版本号） |

`numpad` 是 0.6.18 新增的插件组件（见 `third_party/librime/PLUGINS.md` 第 4 节）：
把 `KP_0..KP_9` / `KP_Decimal` 归一化成 `0..9` / `.` 后交回引擎，**不改任何
schema 的 alphabet**，对普通拼音输入同样生效。

### 9.3 写盘纪律

1. 读文件 -> `serde_yaml` 解析（不合法就整份跳过，一个字节都不写）；
2. 只做行级插入（保留原有缩进、注释、键顺序与换行风格 CRLF/LF）；
3. 渲染后与原文比较：**相同就是 Unchanged**——这就是幂等的定义；
4. 与原文不同时，再解析一次确认合法，并核对目标状态（translators 首位、
   processors 相邻、vnum 存在），任何一步不满足就取消写入；
5. 备份 `<file>.rimesync-orig`（**只做一次**，已存在不覆盖），然后写回；
6. 输出始终是 UTF-8 无 BOM（输入带 BOM 时去掉）。

结构不适用的文件（没有 `engine`、列表是 flow 风格 `processors: [a, b]`、
`patterns: {}` 等）计为 `schemas_skipped`，**不会**凭空写一个坏文件。

### 9.4 手工 smoke（容器里跑过）

`scripts/ime/rime-vmode-smoke.cc` 在真实部署过的 data 方案上按真键：

```sh
g++ -std=c++17 -I<librime>/src -I<librime>/include \
    scripts/ime/rime-vmode-smoke.cc -L<build>/lib -lrime -o /tmp/vmode-smoke
LD_LIBRARY_PATH=<build>/lib /tmp/vmode-smoke <shared_data_dir> <user_data_dir>
```

它断言：`v 1 2 3 4 + a` 与 `v KP_1..KP_4 + a` 都上屏
`壹仟贰佰叁拾肆元整`、上屏文本里没有多余的 `a`、输入上下文清空；
`v KP_1 KP_Decimal KP_5 + space` 上屏 `壹元伍角整`；字母输入不受影响。

### 9.5 幂等与回滚

重复执行（每次启动都会跑）：

* 三处补丁都已在目标位置就不重复插入，也不挪位置；
* 文件已经是目标状态时 `data_patched=0`、`schemas_unchanged` 里能看到它，
  且一个字节都不写；
* 备份永远是**第一次**补丁前的原版。

回滚某个方案：

```powershell
# Windows（需要管理员权限）
Copy-Item "$env:ProgramFiles\Rime\weasel-*\data\luna_pinyin.schema.yaml.rimesync-orig" \
          "$env:ProgramFiles\Rime\weasel-*\data\luna_pinyin.schema.yaml" -Force
```

```bash
# Linux（data 目录属 root，需要 sudo）
sudo cp /usr/share/rime-data/luna_pinyin.schema.yaml.rimesync-orig \
        /usr/share/rime-data/luna_pinyin.schema.yaml
```

### 9.6 权限（Program Files / /usr/share）

写失败时**不 panic、不影响同步与启动**：

* CLI 在 stderr 打印 `warning: 访问 … 失败：…（需要管理员权限…）`；
* JSON 报告里 `data.errors` 带同样原因（含「管理员」字样）；
* 设置页「修复输入法」把失败原因写进日志；启动自举只写 `bootstrap.log`，
  不把每次启动的权限失败推成用户可见的日志（Linux 的 `/usr/share/rime-data`
  属 root，属正常现象）。

解决办法：以管理员身份（Windows）/ `sudo`（Linux）跑一次

```powershell
& "$env:ProgramFiles\rime-sync\rime-sync-cli.exe" fix-ime
```

或先以管理员身份启动 rime-sync 再点「修复输入法」。

### 9.7 报告字段

`rime-sync-cli fix-ime` 的文本输出：

```text
data_patched=1 data_unchanged=0 data_skipped=0 data_failed=0 backup_dir=/usr/share/rime-data
data_dir=/usr/share/rime-data
data_file=/usr/share/rime-data/luna_pinyin.schema.yaml
```

`--json` 时在 PrebuiltReport 上平铺 `data_patched`，另加一个 `data` 对象：

```json
{
  "data_patched": 1,
  "data": {
    "data_dirs": ["/usr/share/rime-data"],
    "schemas_patched": ["/usr/share/rime-data/luna_pinyin.schema.yaml"],
    "schemas_unchanged": [],
    "schemas_skipped": [],
    "backup_dir": "/usr/share/rime-data",
    "errors": []
  }
}
```

`--no-data-patch` 只铺 build/，完全不碰 data（报告里 `data_patched=0`）。

## 10. 测试

* `crates/core/src/prebuilt.rs` 单测（8 个）：缺文件铺、已有文件跳过、旧产物无标记触发
  build 覆盖、config 永不覆盖、force 只影响 build、payload 缺失的可读错误、
  probe 三态、exe 相对定位；
* `crates/core/src/data_patch.rs` 单测（6 个）：注册表输出解析（WeaselRoot 优先、
  InstallDir 兜底、非 REG_SZ 忽略）、`RIMESYNC_WEASEL_DATA` 解析、列表项名解析、
  平台默认目录（`/usr/share/rime-data`）即使不存在也要出现在报告里；
* `crates/core/tests/data_patch_real.rs` 集成测试（12 个）：三处补丁 + 顶部标记、
  幂等（第二次逐字节相同）、备份只做一次且不被后续编辑覆盖、CRLF 与「无结尾换行」
  保持、UTF-8 无 BOM、坏 YAML 不动文件、无 engine / flow 风格跳过、用户自定义
  vnum 不被改写、目录级计数与 `backup_dir`、权限错误可读、缺 recognizer 的合成方案；
* `crates/cli/tests/fix_ime.rs` 集成测试（4 个）：假 payload + 临时目录跑
  `fix-ime --json`，校验 JSON 计数、真实落盘文件与 `--no-data-patch`；
* `scripts/ime/rime-vmode-smoke.cc`（容器 smoke，手工跑）：在真实部署过的 data
  方案上敲 `v1234 + a` 与 `KP_1..KP_4 + a`，断言上屏无多余 `a`、两者结果一致；
* 门禁：`cargo test --workspace --all-targets`、`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets -- -D warnings`、
  `pnpm exec tsc --noEmit`、`pnpm build`。

## 11. 已知限制 / 未验证

* 真机（Windows 227）上的启动自举还没有跑过：本地只验证了编译、单测与 CLI 集成测试；
  0.6.17 起 `%APPDATA%\com.rimesync.app\bootstrap.log` 会逐行记录每一步，
  真机复测时先看它就能区分「没跑」与「跑了但失败」；
* AppImage 的 payload 路径按 Tauri 资源布局与 `$APPDIR` 双路探测，未在真实
  AppImage 里验证；
* `/usr/share/rime-sync/prebuilt` 目前由 `packaging/linux/postinst` 引用，但 deb 的
  文件映射里还没有这一份（程序会优先命中 Tauri 资源目录）；两者取其一即可，重复打包
  会白占约 29 MB；
* 自举只铺产物，不触发重新部署；已运行的 Weasel / fcitx5 需要重启输入法或重新选方案
  后才会加载新的 `build/`。
* data 方案补丁在**真实 227 上还没有跑过**：本地验证的是单测、CLI 集成测试、
  容器里「真实 patched data + 真实 librime 部署 + 真按键」的 smoke。**没验证
  Program Files 的写权限路径**（非管理员进程写 `%ProgramFiles%\Rime\weasel-*\data`
  应失败并给出提权提示）；真机复测时看 `bootstrap.log` 里的 `data patch` 行与
  `data patch error=` 行；
* Linux 的 `/usr/share/rime-data` 属 root：GUI 以普通用户启动时补丁必然失败
  （只写 `bootstrap.log`，不打扰用户），要在安装期生效需要以 root 跑一次
  `rime-sync-cli fix-ime`；随包 deb 的 `postinst` 目前还没有调用它；
* 当前没有「一键提权重跑」按钮，提权需要用户自己以管理员身份执行一次 CLI。
