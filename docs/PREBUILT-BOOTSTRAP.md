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

## 2. 判据（assess，0.6.23 起唯一）

判据只有一处实现：`crate::ime_state::assess`。它按目标方案逐个检查
`build/<id>.schema.yaml` 与配套 bin，**不再有第二套「双标记 / 单标记 + bin」判据**：

| SchemaState 字段 | 取值 | 含义 |
|---|---|---|
| `schema_yaml` | `ok` | 文件在；主方案（`primary`）还必须含 `rimesync_vnum` |
| | `stale-no-marker` | 文件在，但主方案没有插件标记（官方部署 / 进程内重建产物） |
| | `missing` | 文件不在 |
| `bins` | `ok` / `missing` | `*.prism.bin` / `*.table.bin` / `*.reverse.bin` 至少一个在 |

`build_ok = 全部目标方案 usable`（schema_yaml=ok 且 bins=ok，且目标集合非空），
`reason` 是 `ready` / `no-targets` / `schema-missing` / `schema-stale` /
`bins-missing` / `mixed` 之一。

目标集合（`KeepSet::resolve`）= `retained-schemas.txt` ∪ 依赖闭包，再按
「这份 payload 或用户目录里真的有该方案的 schema.yaml」过滤 —— 纯 `__include`
基类（例如 `luna_pinyin`）两边都没有 schema.yaml 时不参与判定（原
`prebuilt::schema_required` 的豁免规则，现在也只剩这一处）。

`probe_build` 保留为**薄封装**（`none` / `deploy` / `prebuilt` 三态，报告与老调用点
用），内部同样调用 `assess`；`prebuilt::schema_ready` 同理（薄封装
`ime_state::schema_ok_in`）。

## 3. 模块与 API

`crates/core/src/prebuilt.rs`（GUI 与 CLI 共用）：

```rust
pub enum BuildProbe { None, Deploy, Prebuilt }
pub struct PayloadLocation { pub path: PathBuf, pub source: String }
pub struct InstallOptions {
    pub force: bool,
    pub explicit: bool,
    // None = 现场探测；ime_state::ensure 传的是「装完输入法之后」重新解析的那份（M1）。
    pub data_dirs: Option<Vec<PathBuf>>,
}
pub struct SchemaBuild { pub id: String, pub primary: bool, pub state: String } // ok/repaired/missing
pub struct PrebuiltReport {
    pub copied: usize,        // build/ 新铺
    pub kept: usize,          // build/ 已存在跳过
    pub overwritten: usize,   // build/ 被覆盖
    pub config_copied: usize, // 用户目录根下 *.yaml 新铺
    pub config_kept: usize,   // 用户目录根下 *.yaml 已存在跳过
    pub config_skipped: Vec<String>, // data 里没有该方案 → 门禁跳过（M2）
    pub failed: usize,        // 复制失败个数（不中断）
    pub source: String,       // payload 目录
    pub build_ok: bool,       // 铺完后的判据是否可用
    pub probe_after: String,  // none / deploy / prebuilt
    pub escalated: bool,      // 是否触发 build/ 的第二次机会
    pub log_path: Option<String>,
    pub build_schemas: Vec<SchemaBuild>, // 3 个主方案的 ok/repaired/missing
    pub build_repaired: Vec<String>,     // 本次补齐的方案（含依赖）
    pub build_missing: Vec<String>,      // 补不齐的方案
    pub deploy_method: String,           // none / prebuilt / auto / inproc / deferred
    pub deploy_result: String,           // ok / partial / failed / skipped / deferred
    pub deploy_ok: bool,
    pub deploy_ms: u64,
    pub deploy_detail: String,
    pub deploy_inproc: String,           // 进程内部署的一句话结论
}

// 唯一判据与唯一状态机（0.6.23）：
pub fn assess(rime_dir: &Path, data_dirs: &[PathBuf], retained: &KeepSet) -> Assessment;
pub fn ensure(
    rime_dir: &Path,
    payload: Option<&Path>,
    data_dirs: &[PathBuf],
    opts: &Options,
) -> Outcome;

// 兼容薄封装（内部只调 assess）：
pub fn probe_build(rime_user_dir: &Path) -> BuildProbe;
pub fn resolve_payload(exe_dir: Option<&Path>) -> Result<PayloadLocation>;
pub fn install_prebuilt(payload_dir: &Path, rime_user_dir: &Path) -> Result<PrebuiltReport>;
pub fn install_prebuilt_with(
    payload_dir: &Path,
    rime_user_dir: &Path,
    options: InstallOptions,
) -> Result<PrebuiltReport>;
pub fn target_schemas(data_dirs: &[PathBuf]) -> Vec<TargetSchema>;
pub fn check_build(user: &Path, payload: Option<&Path>, data_dirs: &[PathBuf]) -> Vec<SchemaBuild>;
pub fn finish_deploy(
    report: &mut PrebuiltReport,
    rime_user_dir: &Path,
    data_dirs: &[PathBuf],
    options: FinishOptions, // { explicit, skip_touch, defer }
    inproc: Option<&librime::InprocReport>,
) -> deploy::DeployOutcome;
```

`install_prebuilt` 是「不覆盖」模式；`install_prebuilt_with(… force: true)`
等价安装器的 `RIMESYNC_PREBUILT=force`。

`crates/core/src/data_patch.rs`（0.6.18，见第 9 节）：

```rust
pub enum PatchOutcome { Patched, Unchanged, Skipped }
pub struct DataPatchReport {
    pub data_dirs: Vec<String>,                 // 实际打补丁的目录（0 或 1 个）
    pub data_dir: String,                       // 实际使用；空 = 没找到
    pub data_candidates: Vec<DataDirCandidate>, // 探测过的全部候选（含不存在）
    pub data_error: Option<String>,             // 全都不是目录时的可读原因
    pub schemas_patched: Vec<String>,
    pub schemas_unchanged: Vec<String>,
    pub schemas_skipped: Vec<String>,
    pub backup_dir: String,
    pub errors: Vec<String>,
}
pub struct DataDirCandidate { path: String, source: String, exists: bool }

pub fn resolve_data_dirs() -> DataDirResolution;
pub fn data_dirs() -> Vec<PathBuf>;          // 兼容入口：只取实际目录
pub fn resolve_windows(
    reg: &WeaselRegistryValues,
    program_files: Option<&Path>,
) -> DataDirResolution;
pub fn patch_auto(version: &str) -> DataPatchReport;  // 解析 + 打补丁（CLI/GUI 都用它）
pub fn patch_dirs(dirs: &[PathBuf], version: &str) -> DataPatchReport;
pub fn patch_file(path: &Path, version: &str) -> Result<PatchOutcome>;
pub fn backup_path(path: &Path) -> PathBuf;
pub fn weasel_registry_from_output(output: &str) -> WeaselRegistryValues;
pub fn weasel_version_dirs(root: &Path) -> Vec<PathBuf>;
pub fn weasel_version(name: &str) -> Option<Vec<u64>>;

0.6.20 的 data 目录解析顺序（逐项探测存在性，第一个命中的胜出，全部候选进报告）：
RIMESYNC_WEASEL_DATA → <WeaselRoot>\data → <WeaselRoot>\weasel-*\data（版本号最大的）→
<InstallDir>\data → <InstallDir>\weasel-*\data → %ProgramFiles%\Rime 下的前两种布局 →
%ProgramFiles%\Rime\data（旧行为兜底）→ Linux 的 /usr/share/rime-data。
227 的 data_dirs=["C:\Program Files\Rime\data"]（目录不存在）就死在只拼了 `<WeaselRoot>\data`。

`crates/core/src/ime.rs`（0.6.20，见第 10 节）：

```rust
pub struct WeaselHealth {
    pub present: bool,      // 服务端 exe + 带插件标记的 rime.dll
    pub version: String,    // weasel-<ver> 目录名里的版本
    pub root: String,
    pub server_exe: String,
    pub rime_dll: String,
    pub plugin: bool,       // rime.dll 里有没有 rimesync_vnum
    pub reason: String,     // exe-present / registry-missing / server-missing / plugin-missing …
}

pub struct ImeReport {
    pub platform: String,
    pub present: bool,
    pub installed: bool,          // 本次是否真的装上了
    pub version: String,
    pub installer: String,
    pub installer_source: String,
    pub install: String,          // installed / already-present / skipped(platform) / disabled / failed
    pub error: Option<String>,
    pub needs_elevation: bool,
    pub log: String,
}

pub fn ensure(exe_dir: Option<&Path>) -> ImeReport;                 // 平台分派
pub fn check_root(root: &Path) -> WeaselHealth;
pub fn check_values(reg: &WeaselRegistryValues) -> WeaselHealth;
pub fn resolve_installer(exe_dir: Option<&Path>) -> Option<(PathBuf, String)>;
```
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
   - 例外见 [第 14 节](#14-用户-customyaml-增量修补与小键盘常驻判定)：用户目录里
     **rime-sync 自己生成**的那份 `<方案>.custom.yaml` 会被**增量修补**（先备份成
     `<文件>.rimesync-orig`）；用户自己写过的文件一个字都不动。
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
* 读配置 → 定位 Rime 用户目录 → `ime_state::ensure(Options::bootstrap())`：
  `Mode::Bootstrap` + `install_ime / seed_templates = true`，但**部署 deferred**
  （`FinishOptions::bootstrap()` → `deploy::defer_plan`：不 taskkill、不跑部署器、
  Linux 不跑 `fcitx5-remote -r`，只刷 mtime 让引擎下次启动自己重建）；
  `Options::install()` 只留给「安装完成后首次运行 / CLI `ensure-ime --mode install`」；
  * 已可用（assess 通过）：不铺文件，结论 Noop，**每一步仍然写日志**（第 8 节）；
  * 不可用：assess → 铺 prebuilt → data 补丁 → 部署 → 再 assess，结果写应用
    日志 `[bootstrap] …` 与 `bootstrap.log`；
* 任何失败（读配置、定位目录、payload 缺失、复制失败）都只记日志：
  **不弹窗、不退出、不影响同步功能**，并且失败原因一定落在 `bootstrap.log` 里；
* 开关与安装器一致：`RIMESYNC_PREBUILT=0` 关闭自举；`RIMESYNC_PREBUILT=force`
  覆盖 `build/`。
* data 方案补丁（第 9 节）现在也在 ensure 里：build 可用才写（227 的实测是
  「build 已经对了但引擎不用它」），同样只记日志，不阻塞启动。
* data 目录在**装完输入法之后**会重新解析一次（M1）：调用方解析时 Weasel 往往
  还没装上（全新机器上为空），用旧结果做模板门禁会把 `default.custom.yaml` 整份跳过；
  重新解析后 config 模板与 payload `config/` 的 wubi86 门禁（M2）用的是同一份目录。

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

# 覆盖已存在的 build/（default/weasel 的 *.custom.yaml 永不覆盖；
# rime-sync 自己生成的 <方案>.custom.yaml 会被增量修补并先备份，用户自己改过的不动）
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

0.6.23 起 `fix-ime` 只是「ensure + 输入法本体自举 + 方案裁剪」的外壳，产物与部署
全部走 `ime_state::ensure`。另外两个子命令是给程序/CI 用的（安装器已不再调用它们）：

~~~bash
# 只读判据（不写任何文件）：设置页诊断、CI 断言都用它
rime-sync-cli --rime-dir /path/to/Rime check-ime --json --report check.json

# 唯一状态机：assess → 铺 prebuilt → patch data → 部署 → 再 assess
rime-sync-cli --rime-dir /path/to/Rime ensure-ime --json --report ensure.json
~~~

`check-ime` 的 JSON：`result`（ok / failed）、`build_ok`、`reason`、`detail`、
`targets`、`schemas[]{id,primary,schema_yaml,bins}`、`missing[]`。
`ensure-ime` 的 JSON：平铺 `PrebuiltReport`（`build_ok` / `build_schemas` /
`build_repaired` / `deploy_*`）+ `result` / `ok` / `detail` / `repaired[]` /
`actions[]{kind,changed,detail}` / `schemas[]`。

**结论语义（只有一个来源）**：

| result | 含义 | 退出码 |
|---|---|---|
| `ok` | 本次改过东西，且最终 `assess().build_ok == true` | 0 |
| `noop` | 本来就是可用状态，本次一个字节都没改 | 0 |
| `partial` | 最终可用，但本次有改动失败（`detail` 必非空） | 1 |
| `failed` | 最终不可用（`detail` 必非空） | 1 |

`--report <FILE>` 把同样的 JSON 落盘（安装器不解析 stdout，避免 NSIS 的
1024 字符变量上限）。

### 7.3 安装器（NSIS）不再碰输入法

B 方案起，安装器**只交付程序本体**：它不调用 `ensure-ime` / `check-ime`，不判定
输入法，不铺 prebuilt，不跑部署，也不自检、不设退出码。原来的 SELF-CHECK 整段删除
（连同 `RimeSyncProbeBuild` / `RimeSyncEnsureIme` / `RimeSyncCheckIme` 等函数）。

产物与部署全部由**程序首次启动**时的 `ime_state::ensure(Mode::Install)` 负责：
输入法本体 → config 模板 → prebuilt → data → 部署 → assess。安装版首次启动的
完整语义、失败交互与升级路径见
[WINDOWS-IME-BOOTSTRAP.md](WINDOWS-IME-BOOTSTRAP.md)。

CLI 的 `ensure-ime --mode install` 与程序走同一个状态机；CI 的 windows-e2e 用它
模拟「装完后首次启动」，断言 `result=ok` 且 build 里含 `rimesync_vnum`。

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

* `build/luna_pinyin_simp.schema.yaml` 里已经有 `rimesync_vnum`（translators）、
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
| `punctuator/half_shape` | 0.6.20 起深合并两条中文态直出：`"\\": "、"`、`":": "："`（**标量**，不是 `{commit: …}`，原因见下）；已有同键一律跳过（用户配置优先）。`punctuator` 段缺失就补 `import_preset: default` + `half_shape`；是标量（`punctuator: default`）就规范化为映射并保留原 preset 名 |
| 文件顶部 | 插入 `# patched by rime-sync <version>`（已有就不再插，不追版本号） |

`numpad` 是 0.6.18 新增的插件组件（见 `third_party/librime/PLUGINS.md` 第 4 节）：
把 `KP_0..KP_9` / `KP_Decimal` 归一化成 `0..9` / `.` 后交回引擎，**不改任何
schema 的 alphabet**，对普通拼音输入同样生效。

注意这份列表写在 **data 目录**（共享方案）里，而用户目录那份 `<方案>.custom.yaml`
也会 `patch:` 同一个 `engine/processors` 列表 —— librime 对列表是**整体替换**，
所以用户目录里那份会**整段顶掉**这里的列表（含 `numpad`）。详见
[第 14 节](#14-用户-customyaml-增量修补与小键盘常驻判定)。

0.6.19 起 `KP_Decimal` 有一个唯一的例外：**没有组字时直接上屏 ASCII 句点
`.`**（`engine_->CommitText`，不再交回标点处理器），所以中文态也是 `.` 而不是
`。`；正在组字时仍按 `.` 交回引擎（v 模式 `v12.34` 依赖它）。主键盘 `.` 不受
影响，中文态照旧是 `。`。英文/ASCII 态由 `ascii_composer` 先把按键交给客户端，
结果与主键盘 `.` 完全一致。

### 9.3 写盘纪律

1. 读文件 -> `serde_yaml` 解析（不合法就整份跳过，一个字节都不写）；
2. 只做行级插入（保留原有缩进、注释、键顺序与换行风格 CRLF/LF）；
3. 渲染后与原文比较：**相同就是 Unchanged**——这就是幂等的定义；
4. 与原文不同时，再解析一次确认合法，并核对目标状态（translators 首位、
   processors 相邻、vnum 存在、本次注入的 punctuator 条目 commit 正确），任何一步
   不满足就取消写入；
5. 备份 `<file>.rimesync-orig`（**只做一次**，已存在不覆盖），然后写回；
6. 输出始终是 UTF-8 无 BOM（输入带 BOM 时去掉）。

结构不适用的文件（没有 `engine`、列表是 flow 风格 `processors: [a, b]`、
`patterns: {}` 等）计为 `schemas_skipped`，**不会**凭空写一个坏文件；punctuator
深合并也只在其它补丁能安全落地的文件上做（flow 风格 schema 不塞新段）。

punctuator 深合并的边界：

* **为什么写成标量**：`rime-prelude/symbols.yaml` 里 `'\' : [ 、, '\', ＼ ]` 是列表；
  schema 里用 `{commit: "、"}` 映射覆盖预设列表会让 librime 合并配置失败，
  整个方案部署不出来（容器 smoke 实测：`build/luna_pinyin_simp.schema.yaml` 根本不生成，
  引擎对所有按键都没反应）。标量是 `ConfigValue`，直接替换预设值，
  `TranslateUniquePunct` 立刻上屏、不弹候选列表——功能与 `{commit}` 相同；
* `punctuator` 段缺失 → 文件末尾补 `import_preset: default` + `half_shape`；
* `punctuator: <preset>`（标量）→ 换成映射，`import_preset` 保留原 preset 名；
* 已是映射但没有 `half_shape` → 在 `punctuator` 块末尾补 `half_shape`；
* `half_shape` 里已有 `"\\"` 或 `":"` → **跳过、一个字节都不改**，
  原因写进报告的 `punctuator_skipped`；
* 半角 `/` 键与 `/fh` 等 `symbols` 预设不受影响（只新增两个键）。

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

0.6.19 追加的断言：中文态无组字时 `KP_Decimal` 上屏单字节 `2E`（不是 `。`）；
ASCII 态与主键盘 `.` 一样交给客户端、IME 不提交中文标点；`v12.34 + a` 上屏
`壹拾贰元叁角肆分` 且小数点本身不上屏；`v12. + a` 不产生金额且组字不被清空；
普通拼音组字中 `KP_Decimal` 与主键盘 `.` 的结果完全一致；主键盘 `.` 中文态
仍然上屏 `。`。

0.6.20 追加的断言（punctuator 深合并，同一个 smoke）：中文态 `\\` 上屏恰好「、」
且没有候选列表、没有残留组字；Shift+; 上屏恰好「：」；`,` `;` `'` `[` `]`
分别仍是「，」「；」「‘」「「」「」」；`/fh` 仍出符号表并上屏第一个「©」；
英文态 `\\` 与 `;` 仍交给客户端（`process_key` 返回 false、无 IME 提交）。

> 注意：注入 `half_shape` 的必须是**标量**（`"\\": "、"`）。用
> `{commit: "、"}` 映射覆盖 symbols 预设里的列表会让整个方案部署失败
> （`build/luna_pinyin.schema.yaml` 不生成、引擎对所有键无响应）——这是
> 0.6.20 容器 smoke 实测出来的坑，`data_patch` 已按标量写入并有单测兜底。

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
punctuator_patched=2 punctuator_skipped=0
data_dir=/usr/share/rime-data
data_candidates=[/usr/share/rime-data]
data_file=/usr/share/rime-data/luna_pinyin.schema.yaml
weasel_present=true weasel_installed=false weasel_version=0.17.4 weasel_install=already-present
weasel_installer=C:\Program Files\rime-sync\resources\weasel\weasel-0.17.4.0-installer.exe source=exe:resources/weasel
```

一个候选都不存在时，还会多一行可读原因（列出所有探测过的路径与来源）：

```text
data_dir=-
data_candidates=[C:\Program Files\Rime\data, ...]
data_error=未找到可用的 Rime data 目录：已探测 2 个候选，全部不存在：...
```

`--json` 时在 PrebuiltReport 上平铺 `data_patched` 与 `weasel_*` 字段，
另加一个 `data` 对象：

```json
{
  "data_patched": 1,
  "weasel_present": false,
  "weasel_installed": true,
  "weasel_version": "0.17.4",
  "weasel_installer": "C:\\Program Files\\rime-sync\\resources\\weasel\\weasel-0.17.4.0-installer.exe",
  "weasel_install": "installed",
  "weasel_error": null,
  "data": {
    "data_dirs": ["C:\\Program Files\\Rime\\weasel-0.17.4\\data"],
    "data_dir": "C:\\Program Files\\Rime\\weasel-0.17.4\\data",
    "data_candidates": [
      { "path": "C:\\Program Files\\Rime\\data", "source": "registry:WeaselRoot", "exists": false },
      { "path": "C:\\Program Files\\Rime\\weasel-0.17.4\\data", "source": "glob:WeaselRoot\\weasel-*", "exists": true }
    ],
    "data_error": null,
    "punctuator_patched": 2,
    "punctuator_skipped": [],
    "schemas_patched": ["C:\\Program Files\\Rime\\weasel-0.17.4\\data\\luna_pinyin.schema.yaml"],
    "schemas_unchanged": [],
    "schemas_skipped": [],
    "backup_dir": "C:\\Program Files\\Rime\\weasel-0.17.4\\data",
    "errors": []
  }
}
```

`--no-data-patch` 只铺 build/，完全不碰 data（报告里 `data_patched=0`）；
输入法本体检查 / 装回不受它影响。

## 10. 输入法本体自举（0.6.20）

### 10.1 起因（227 实测）

0.6.19 用 setup.exe /S 静默安装时，Tauri 的 NSIS 钩子没有执行：旧 Weasel 被 Tauri
卸载器卸掉，钩子里的「未就绪 → 内嵌安装器装回」也没跑，结果机器上没有任何输入法。
结论：输入法本体不能只靠安装器钩子兜底，程序自己必须能判定 + 装回。

### 10.2 判定（与 NSIS 钩子同一判据）

Windows：

1. 注册表 HKLM\Software\WOW6432Node\Rime\Weasel（32 位视图）与
   HKLM\Software\Rime\Weasel（64 位视图）的 WeaselRoot / InstallDir 有值；
2. 该目录里真有 WeaselServer.exe——直接在根目录，或在 weasel-*\ 版本目录里
   （取版本号最大的一个）；
3. WeaselServer.exe 同目录的 rime.dll 里能搜到 rimesync_vnum。

任一条不满足 = weasel_present=false，原因是 registry-missing / server-missing /
rime-dll-missing / plugin-missing / rime-dll-unreadable。
Linux 完全跳过（由包管理器负责），报告 weasel_install=skipped(platform)。

### 10.3 装回

* 安装器：<exe 目录>/resources/weasel/weasel-0.17.4.0-installer.exe
  （Tauri resources；开发态回落到 packaging/windows/vendor/）；
* 参数固定 /S /NOREBOOT（静默、绝不允许安装器重启机器），等待上限 300 秒
  （与 NSIS 钩子的 RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS 一致）；
* 装完立即复查判定；不通过就是 failed，带上安装器退出码与可读原因；
* 需要管理员权限（spawn 740 / 退出码 5 / UAC 被拒 1223）→ weasel_needs_elevation=true，
  界面提示「以管理员身份重启 rime-sync 后再点修复输入法」；
* 开关：RIMESYNC_INSTALL_IME=0 只检查不安装。

### 10.4 触发点

* 启动自举（prebuilt_cmds::bootstrap）：后台线程里先检查 / 装回，再铺 build/；
  失败只写 bootstrap.log 与应用日志，不阻塞窗口；
* 设置页「修复输入法」与 rime-sync-cli fix-ime：同步执行，结果进报告。

### 10.5 报告字段

```text
weasel_present=true weasel_installed=false weasel_version=0.17.4 weasel_install=already-present
weasel_installer=C:\Program Files\rime-sync\resources\weasel\weasel-0.17.4.0-installer.exe source=exe:resources/weasel
```

| 字段 | 含义 |
|---|---|
| weasel_present | 检查（安装后复查）是否就绪 |
| weasel_installed | 本次是否真的用随包安装器装上了 |
| weasel_version | 命中的 Weasel 版本（从 weasel-<ver> 目录名取） |
| weasel_installer | 随包安装器路径；空 = 没找到 |
| weasel_install | installed / already-present / skipped(platform) / disabled / failed |
| weasel_error | 可读失败原因（含提权提示） |

### 10.6 打包接线

* src-tauri/tauri.windows.conf.json 把
  packaging/windows/vendor/weasel-0.17.4.0-installer.exe 映射到
  resources/weasel/weasel-0.17.4.0-installer.exe（Windows 平台配置：Linux 的
  deb/AppImage 不该背这 12 MB）；
* 与 NSIS 钩子内嵌的是同一个 vendor 文件（fetch-weasel.ps1 / 自编译产出），
  所以两处不会各带一份不同版本；
* packaging/windows/check-nsis-hooks.sh 断言：映射存在、文件名与
  crates/core/src/ime.rs 的 INSTALLER_FILE 一致、卸载时清掉 resources\weasel；
  工作区里有 vendor 文件时还校验 ≥8 MiB + MZ 头。

## 11. 测试


* `crates/core/src/prebuilt.rs` 单测（8 个）：缺文件铺、已有文件跳过、旧产物无标记触发
  build 覆盖、config 永不覆盖、force 只影响 build、payload 缺失的可读错误、
  probe 三态、exe 相对定位；
  （这里的「config 永不覆盖」说的是 **prebuilt.rs 的铺入路径**；用户 custom.yaml 的
  增量修补是**另一条路径**（`managed_custom.rs`，见[第 14 节](#14-用户-customyaml-增量修补与小键盘常驻判定)）。）
* `crates/core/src/data_patch.rs` 单测（12 个）：注册表输出解析（WeaselRoot +
  InstallDir 都保留、只有 InstallDir、非 REG_SZ 忽略）、`weasel-*` 版本解析与排序、
  data 目录解析 5 条路径（WeaselRoot 命中 / WeaselRoot 没有 data 走版本目录（227 形状）/
  InstallDir 通配多个 weasel-* 取最高版本 / 默认 %ProgramFiles% 布局 / 全部不存在时的
  错误文案与候选清单）、`RIMESYNC_WEASEL_DATA` 解析、列表项名解析；
* `crates/core/src/ime.rs` 单测（11 个）：`<root>\weasel-9.9.9\WeaselServer.exe` 伪造目录
  的就绪判定、根目录直接布局、缺 exe / 缺 rime.dll / rime.dll 没有 `rimesync_vnum`、
  注册表值为空、WeaselRoot 失效后回落 InstallDir、安装器参数 `/S /NOREBOOT`、
  安装器缺失的可读错误、安装成功后复查、需要管理员权限、已就绪不跑安装器、
  `RIMESYNC_INSTALL_IME=0` 关闭；
* `crates/core/tests/data_patch_real.rs` 集成测试（18 个）：三处补丁 + 顶部标记、
  幂等（第二次逐字节相同）、备份只做一次且不被后续编辑覆盖、CRLF 与「无结尾换行」
  保持、UTF-8 无 BOM、坏 YAML 不动文件、无 engine / flow 风格跳过、用户自定义
  vnum 不被改写、目录级计数与 `backup_dir`、权限错误可读、缺 recognizer 的合成方案；
  0.6.20 起另有 6 个 punctuator 用例：段缺失 / 标量 / 已是映射三种形态都能注入、
  已有 `"\\"` 或 `":"` 时不覆盖且文件不变、重复执行逐字节相同、CRLF + 无 BOM 保持；
* `crates/core/tests/managed_custom_repair.rs` 集成测试（16 个）：旧模板被修好且
  `numpad` 紧跟在 `rimesync_vselect` 后面、已修好的文件不再写、用户自己写的文件跳过、
  用户关掉 rime-sync 定制后跳过、文件里没有任何 rimesync 名字就跳过、幂等且备份只写一次、
  CRLF 保持、BOM 保持、`numpad` 位置不对时挪到 `rimesync_vselect` 后面、没有
  `rimesync_vselect` 就跳过、嵌套的 processors 列表也支持、没有 processors 列表就跳过、
  坏输入只报错不 panic、两种 rime-sync 标记都认得出来、批量扫描与逐方案报告、
  批量修时缺失文件跳过；
* `crates/core/tests/ime_custom_repair_flow.rs` 集成测试（5 个）：ensure 修好 227 形状的
  旧 custom.yaml、用户自己写的文件不动、用户关掉定制的文件不动、对 custom 修补幂等、
  修补算作「本次改了磁盘」（否则会误判成什么都没做、跳过部署）；
* `crates/core/tests/numpad_assess.rs` 集成测试（12 个）：旧 custom 修好后仍报
  artifact-stale、第二轮 ensure 仍然报缺口、产物里有 numpad 就是 ok、关掉定制既不改也不报、
  用户自己的文件既不改也不报、判定只读不改文件、没有 processors 列表不算来源侧缺口、
  没有产物不算缺口、只判保留主方案、产物缺失但方案存在 = no-artifact、状态可序列化、
  每个保留方案都有逐项状态；
* `crates/cli/tests/numpad_report.rs` 集成测试（7 个）：check-ime 报缺口且只读、
  check-ime 与 ensure-ime 结论一致、产物里有 numpad 时 check-ime 是 ok、文本模式打印
  人能看懂的 numpad 行、ensure-ime 的 JSON 带 numpad 段、空目录上 check-ime 不 panic、
  check-ime 不动用户目录里任何文件；
* `crates/cli/tests/fix_ime.rs` 集成测试（4 个）：假 payload + 临时目录跑
  `fix-ime --json`，校验 JSON 计数、真实落盘文件与 `--no-data-patch`；
* `scripts/ime/rime-vmode-smoke.cc`（容器 smoke，手工跑）：在真实部署过的 data
  方案上敲 `v1234 + a` 与 `KP_1..KP_4 + a`，断言上屏无多余 `a`、两者结果一致；
  0.6.19 起还覆盖 `KP_Decimal` 的六种情形（中文态无组字 / ASCII 态 / v12.34 /
  v12. 边界 / 拼音组字中 / 主键盘 `.` 防回归）；0.6.20 起同一个 smoke 还断言
  中文态 `\\` → 恰好「、」（且不弹候选列表）、`:` → 恰好「：」、`,` `;` `'` `[` `]`
  行为不变、`/fh` 仍可用，以及英文态 `\\` / `;` 仍交给客户端；
* `third_party/librime/plugins/rimesync-extra/test/engine_smoke_test.cc`（对真实
  librime 手工编译，见文件头）：`TestNumpad` 直接驱动 `numpad` 处理器，断言
  无组字时 `KP_Decimal` 立即上屏一个 `0x2E`、有组字时与主键盘 `.` 结果一致；
* 门禁：`cargo test --workspace --all-targets`、`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets -- -D warnings`、
  `pnpm exec tsc --noEmit`、`pnpm build`、
  `python3 scripts/ime/gen-prebuilt-build.py --check`、
  `bash packaging/windows/check-nsis-hooks.sh --strict`。

## 12. 已知限制 / 未验证

* 真机（Windows 227）上的启动自举还没有跑过：本地只验证了编译、单测与 CLI 集成测试；
  0.6.17 起 `%APPDATA%\com.rimesync.app\bootstrap.log` 会逐行记录每一步，
  真机复测时先看它就能区分「没跑」与「跑了但失败」；
* AppImage 的 payload 路径按 Tauri 资源布局与 `$APPDIR` 双路探测，未在真实
  AppImage 里验证；
* `/usr/share/rime-sync/prebuilt` 目前由 `packaging/linux/postinst` 引用，但 deb 的
  文件映射里还没有这一份（程序会优先命中 Tauri 资源目录）；两者取其一即可，重复打包
  会白占约 29 MB；
* 自举会装输入法本体、播模板并铺产物，但**部署是 deferred**（不 taskkill、不跑
  部署器、Linux 不跑 `fcitx5-remote -r`）：已运行的 Weasel / fcitx5 需要重启输入法、
  点「重新部署」或重新选方案后才会加载新的 `build/`；显式部署只发生在
  `ensure-ime --mode install|fix` 与设置页「修复输入法」。
* Linux 的 custom 降级（共享 data 目录不可写 → 用户级 `<schema>.custom.yaml`）在启动
  自举里 **不跑 `fcitx5-remote -r`**：只记 `deploy=deferred` 并在 GUI 日志提示
  「已更新输入法配置，请点『重新部署』或重启输入法」，避免打断正在打字的用户；
  只有用户点「修复输入法」或手动跑 `rime-sync-cli fix-ime` 才会真正触发重载。
* data 方案补丁在**真实 227 上还没有跑过**：本地验证的是单测、CLI 集成测试、
  容器里「真实 patched data + 真实 librime 部署 + 真按键」的 smoke。**没验证
  Program Files 的写权限路径**（非管理员进程写 `%ProgramFiles%\Rime\weasel-*\data`
  应失败并给出提权提示）；真机复测时看 `bootstrap.log` 里的 `data patch` 行与
  `data patch error=` 行；
* Linux 的 `/usr/share/rime-data` 属 root：GUI 以普通用户启动时补丁必然失败
  （只写 `bootstrap.log`，不打扰用户），要在安装期生效需要以 root 跑一次
  `rime-sync-cli fix-ime`；随包 deb 的 `postinst` 目前还没有调用它；
* 当前没有「一键提权重跑」按钮，提权需要用户自己以管理员身份执行一次 CLI。

## 13. 部署链（0.6.23）：为什么不再依赖 WeaselDeployer

### 13.1 227 实测证据

2026-11-06 在测试机 192.168.61.227（Windows）上逐项核对：

| 证据 | 结果 |
|---|---|
| `%APPDATA%\Rime\build` 的 18 个文件时间戳 | 全部停在**铺 prebuilt 的那一刻**（11-06 06:42），之后没有任何新产物 |
| `build\luna_pinyin_simp.schema.yaml` | **不存在**；而 `<WeaselRoot>\data\luna_pinyin_simp.schema.yaml` 存在 |
| `WeaselDeployer.exe /deploy` | **恒返回 0**，不写任何 build 文件（用官方 Weasel 安装包同样） |
| 原因 | 它只是通过 IPC 请求 WeaselServer 部署；非交互会话下请求静默失败，退出码仍是 0 |
| 后果 | 引擎启动时按时间戳判断 build 是否最新；我们铺的 prebuilt 永远比 data 新，于是「本机部署」这条链**永远不生效** |

结论：**不能再把 WeaselDeployer 当作部署的主要手段**。安装期与自举都不调用它。

### 13.2 现在怎么做

| 场景 | 手段 | deploy_method |
|---|---|---|
| 用户显式「重新部署 / 修复输入法」（GUI / `rime-sync-cli fix-ime` / 安装自检） | **进程内**直接调 librime C API（`RimeSetup` → `RimeInitialize` → `RimeStartMaintenance` → `RimeJoinMaintenanceThread` → `RimeFinalize`） | `inproc` |
| 进程内不可用（找不到库/符号、初始化失败） | 铺随包 prebuilt + 让引擎下次启动自己部署 | `prebuilt` / `auto` |
| 已经可用、什么都不用做 | —— | `none` |

进程内部署的实现（`crates/core/src/librime.rs`）：

* 库**一律动态加载**（Windows `LoadLibraryW`、Linux `dlopen`），不引入任何编译期
  链接依赖——CI 上没有 librime 也能构建；
* traits：`shared_data_dir=<WeaselRoot>\data`、`user_data_dir=%APPDATA%\Rime`、
  `app_name=rime.sync.deploy`。`RimeTraits` 用 `#[repr(C)]` 镜像结构体表示，
  `data_size = size_of::<RimeTraitsC>()`（64 位 96 / 32 位 48），**不再手写 96**——
  字段顺序由单测对着 `third_party/librime/src/rime_api.h` 校验；
* 符号先找 C 名字（`RimeSetup`），再找 Itanium ABI 的 C++ 名字
  （`_Z9RimeSetupP13rime_traits_t`，自编译 librime 没加 `extern "C"` 时只剩这个）；
* 找不到库/符号、初始化失败 → `deploy_method=inproc_failed` + 可读原因，**不 panic**；
* 失败判据（0.6.23 B1）只有两条，任一成立即 `inproc_failed`：
  1. `RimeStartMaintenance` 返回 `False`（`Bool` 就是 int，头文件
     `rime_api_deprecated.h:40`；`RimeJoinMaintenanceThread` 返回 void，不再当 int 读）；
  2. 调用前后 `build/` 的文件数与 mtime（含目录 mtime）完全没变。
  `installation.yaml` 只作为旁证写进明细，**不再**当判据、也不短路。

CLI：

```bash
# 进程内部署（Windows 走 <WeaselRoot>\rime.dll，Linux 走 librime.so.1）
rime-sync-cli --rime-dir "$APPDATA\Rime" deploy-ime
rime-sync-cli --rime-dir "$APPDATA\Rime" deploy-ime --json
# 输出：deploy_method=inproc deploy_ok=true deploy_ms=6030
```

### 13.3 让「自动部署」的条件成立（纯 mtime 触发）

引擎的真实判据（`third_party/librime/src/rime/lever/deployment_tasks.cc:45-78`）：

```text
last_modified = max(data 目录自身 mtime,
                    data 目录下直接子文件的 *.yaml mtime（排除 user.yaml）)
needs_deploy  = last_modified > user.yaml 的 var/last_build_time
```

也就是「data 比上一次成功部署的时间新」。`build/` 的 mtime 不参与判定，
`var/last_build_time` 只由引擎自己在上一次部署结束时写入。铺完 prebuilt / 打完
data 补丁之后，`prebuilt::finish_deploy` 会把条件做成成立：

1. 把 data 下相关方案的 `*.schema.yaml` 的 mtime 刷成现在（`File::set_modified`）；
   `deploy::touch_time` 再取「build 最新 mtime + 1s」做下限，保证「build 不比 data 新」
   这个不变量（时钟回拨 / 预置产物 / FAT 那种粗粒度时间戳都不会把条件弄反）；
2. 需要时 `taskkill /IM WeaselServer.exe /F` —— 由**系统在用户下次输入时自动拉起**，
   拉起时引擎看到条件成立就会自己部署。**绝不**从会话 0 `Start-Process` 拉起：那会
   得到会话 0 的僵尸实例（我们踩过这个坑）。

**不再写 `user.yaml` 的 `var/last_build_time`（0.6.23 c）。** `user.yaml` 属于会跨设备
同步的文件，把它归零会让其它设备下次启动也多做一次部署；它同时还是输入法运行时正在写
的文件（见 13.3.1）。触发条件因此只剩 mtime 这一条。

刷 mtime 时的兜底（0.6.23 B2）：共享 data 目录写不进去（Windows 上 `Program Files` 需要
管理员）时，会再试 `<用户目录>\<方案>.schema.yaml`，两者都失败才算失败，日志列出尝试过的
路径。显式修复时若本次**没有任何方案被改动**（内容哈希与上次相同），直接跳过刷 mtime，
免得每点一次「修复输入法」都让引擎全量重建（0.6.23 C8）。

进程内部署已经成功时，第 2 步跳过（`deploy_method=inproc`，不再碰服务）。
写不进去只记失败，不影响 prebuilt 兜底。

Linux：写方案补丁后的重载仍由 data_patch 负责（`fcitx5-remote -r` → `rime_deployer --build`），
Bootstrap 仍是 `deploy=deferred`；显式 `deploy-ime` 走 13.3.2 的优先级链。

#### 13.3.1 部署前必须先停服务（0.6.23 d）

进程内 librime 部署会写 `user.yaml` / `installation.yaml`，而常驻的 WeaselServer 也在
写同两个文件——0.6.22 的顺序是「先部署、后 taskkill」，于是两边抢文件。现在的顺序：

| 步骤 | 动作 | 失败怎么办 |
|---|---|---|
| 1 | `taskkill /IM WeaselServer.exe /F`，然后用 `tasklist /FI "IMAGENAME eq WeaselServer.exe" /NH` 每 250 ms 确认一次，最多 3 秒 | 退出码 128（进程本来就不在）= 成功；其它非 0 = 失败 |
| 2 | 进程内 librime 部署（`RimeSetup` → `RimeInitialize` → `RimeStartMaintenance` → `RimeJoinMaintenanceThread` → `RimeFinalize`） | 失败照旧记 `deploy_inproc`，退回 prebuilt |
| 3 | 结束，**不拉起服务** | 由系统在用户下次输入时自动拉起 |

服务在 3 秒内仍存活（或 `taskkill` 失败）→ **跳过进程内部署**，降级为 prebuilt：
日志写 `deploy_method=prebuilt reason=server-alive`、`deploy_inproc=skipped reason=server-alive`。
此时 data 的 mtime 仍会被刷新，等用户下次重启输入法时由引擎自己部署。

`RIMESYNC_DEPLOY_NO_RESTART=1` 保持原义：完全不碰 WeaselServer（排障用）；此时没有
「已确认退出」这一步，进程内部署照跑——这是显式打开调试开关的代价。

收尾（引擎 apply 之后的重载）仍走单次 `taskkill`（`deploy::kill_server_once`，不轮询
确认）：重载是 best-effort，不值得让一次同步多等 3 秒。需要「确认服务已退出再写
user.yaml」的只有进程内部署，用的是 `deploy::stop_before_deploy`。

taskkill 非 0/128（进程拒绝终止、权限不足……）在**重载**路径上只记 `warning=`，结果仍是
`ok`，不会让整次 apply 判失败（0.6.23 B3）：重载没做成不影响已经铺好的产物。整次 apply
只有在「build 不可用且随包产物也补不齐」时才失败。

显式修复的顺序（0.6.23 B4）：**铺随包产物 → 写 data/用户目录补丁 → 再进程内部署 →
逐方案校验**。进程内部署会把 `build/<id>.schema.yaml` 重写成 librime 自己的产物（不带
插件标记）；校验到主方案缺标记、或该方案缺任何 `.bin` 时，把该方案的 `schema.yaml`
与全部 `.bin` **一起**从随包产物重铺（不允许「schema 来自 inproc、bin 来自 prebuilt」
的混合态），日志写 `build_source=prebuilt` 与 `reverted=[…]`。

覆盖范围：设置页「修复输入法」（`cmd_fix_ime`）、`rime-sync-cli fix-ime`、
`rime-sync-cli deploy-ime`、WeaselTrigger 的「重新部署」都走同一套
`deploy::stop_before_deploy` / `deploy::inproc_after_stop`。启动自举不跑进程内
librime（不写 `user.yaml`），因此没有「部署前停服务」这一步，仍在收尾时按需结束服务。

#### 13.3.2 Linux 显式部署：外部部署器优先（0.6.23）

`rime-sync-cli deploy-ime` 在 Linux 上不再无条件 dlopen librime（fcitx5-rime 可能正
持着 userdb / 文件锁）。顺序见 `crates/core/src/linux_deploy.rs`，每步都有 120 秒超时：

| 顺序 | 条件 | 动作 | `deploy_method` |
|---|---|---|---|
| 1 | 找到 `rime_deployer`，且 fcitx5 **没在跑** | `rime_deployer --build <用户目录> <共享目录>` | `external_ok` / `external_failed` |
| 2 | 找到部署器但 fcitx5 **在跑**（0.6.23 B5） | 先 `fcitx5-remote -r` **重载**（不杀进程）；重载成功才继续跑 `rime_deployer --build` | `external_ok` / `external_failed` |
| 3 | 有部署器、fcitx5 在跑、但**重载失败** | 不执行 build，直接跳过 | `skipped`（`reason=ime-running`，detail 写明「未执行 build」） |
| 4 | 没有部署器，且 `fcitx5-remote` 存在 + fcitx5 在跑 | 不碰用户的输入法，直接跳过 | `skipped`（`reason=ime-running`） |
| 5 | 两者都不满足 | 进程内 dlopen librime | `inproc` / `inproc_failed` |

`skipped` 时 CLI 退出码非 0，detail 里给出「先退出输入法（`fcitx5-remote -e`）后重试，
或在托盘菜单点「重新部署」」。两个调试开关：`RIMESYNC_DEPLOY_FORCE_INPROC=1` 强制走
进程内；`RIMESYNC_DEPLOY_UNSAFE_EXTERNAL=1` 允许在 fcitx5 运行时**直接** build（跳过重载，
会有抢锁风险，只用于排障）。

外部部署器的输出重定向到临时文件（不是 piped 后不读）——否则写满 64 KiB 管道缓冲区就会
「部署器卡住」被误判成超时；超时后 `kill` + `wait` 回收，不留僵尸。进程内部署超时按失败
处理，但会等后台线程自然结束（CLI 退出前一定 join），日志写明「超时后仍可能在后台完成」。

### 13.4 日志字段

```text
deploy_method=inproc|prebuilt|auto|none   deploy_result=ok|partial|failed|skipped
deploy_ok=true   deploy_ms=6030   deploy_detail=reason=inproc-deployed server_before=stopped taskkill=exit0 tasklist=gone poll=0 touched=3 auto_failed=0
deploy_inproc=method=inproc ok=true ms=6030 lib=C:\Program Files\Rime\rime.dll ...
# 停不下来时（0.6.23 d）：
deploy_method=prebuilt reason=server-alive ...   deploy_inproc=skipped reason=server-alive (taskkill=exit0 but WeaselServer.exe alive after 3000ms)
```

安装自检（SELF-CHECK）同样输出 `deploy_method=` / `deploy_result=` / `deploy_ms=`，
并多一行 `phase=DEPLOY-IME`（进程内部署的退出码与耗时）与 `phase=SERVER-RESTART`
（taskkill 的退出码）。

### 13.5 build 按方案校验（0.6.23 A）

判据不再只看 `build\luna_pinyin_simp.schema.yaml` 一个文件：

* 目标方案 = `packaging/rime/retained-schemas.txt`（3 个主方案）+ 依赖闭包
  （`prune::keep_set`：`__include` / `dependencies` / `dictionary`）；
* 主方案：`build\<id>.schema.yaml` 必须存在且含 `rimesync_vnum`；
  依赖方案：只要求存在 `build\<id>.schema.yaml`，不要求标记；
* 两者都要有至少一个 `<id>.prism.bin` / `<id>.table.bin` / `<id>.reverse.bin`；
* 纯 `__include` 基类（例如 `luna_pinyin`：部署器只合并不落盘 `<id>.schema.yaml`）
  在用户目录与 payload 里都没有 schema.yaml 时视为「本就不需要」，不参与判定；
* 缺什么补什么：整树只补缺失文件，主方案的 schema.yaml 存在但缺标记时覆盖一次
  （build/ 是派生数据）；**已有可用产物一个字节都不动**；
* 报告：`build_schemas=luna_pinyin_simp:ok,wubi_pinyin:repaired,wubi86:missing`、
  `build_repaired=`、`build_missing=`；补不齐只告警（CLI 退出码仍按判据文件判定）。

安装自检侧（NSIS）用 `RIMESYNC_BUILD_SCHEMA_1..3` 三个 define 做同一件事，
`check-nsis-hooks.sh --strict` 会断言这三个值与 `retained-schemas.txt` 逐项一致。

### 13.6 验收命令

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo check --manifest-path src-tauri/Cargo.toml
pnpm exec tsc --noEmit && pnpm build
python3 scripts/ime/gen-prebuilt-build.py --check
bash packaging/windows/check-nsis-hooks.sh --strict
```

### 13.7 CI 的 Windows 类型检查（0.6.23 a）

`ci.yml` 的 `windows-typecheck` job 与 `release.yml` windows job 的
`typecheck · cargo check (core + cli, fail fast)` 步骤只做类型检查，不链接、不打包：

```bash
# CI（ubuntu runner，交叉检查 Windows 专属代码）
cargo check -p rime-sync-core --target x86_64-pc-windows-gnu
cargo check -p rime-sync-cli  --target x86_64-pc-windows-gnu
# release.yml 的 windows job（原生 Windows，不需要交叉工具链）
cargo check -p rime-sync-core -p rime-sync-cli
```

为什么不是 `x86_64-pc-windows-msvc`：`core → reqwest → rustls → ring` 的 build.rs 用
cc-rs 编译 C，msvc 目标需要 MSVC 的 `lib.exe`，ubuntu runner 上不存在；本机实测报错
`error occurred in cc-rs: failed to find tool "lib.exe"`（ring 0.17.14）。`--no-default-features`
绕不过去（reqwest 不是可选依赖），只 check「纯 Rust 部分」又会漏掉 Windows 专属代码。
gnu 目标的 `target_os` 同样是 `windows`，而 `cargo check` 不链接，所以对
「`cfg(windows)` 代码能否编过」等价；job 里装 `gcc-mingw-w64-x86-64` 只为编 ring 的 C。

本地验证的边界：Linux 开发机上 `cargo check`（宿主）通过；msvc 目标在 ring 处失败
（如上）；gnu 目标需要 mingw 的 gcc，开发机上没有，因此这条命令只在 CI 上真正跑过。
若哪天 ring 改了构建方式导致 gnu 也编不动，把 job 改成 `runs-on: windows-latest` +
原生 `cargo check` 即可（release.yml 里已经是这样）。

## 14. 用户 custom.yaml 增量修补与小键盘常驻判定

（本节描述 0.6.26 引入的行为。）

这一节说明缺陷三（小键盘数字键失效）的完整机制与修法。

### 14.1 现象与根因

现象：装好 rime-sync 后，**小键盘**的数字键（KP_0..KP_9、KP_Decimal）打不出数字，
主键盘数字正常。

根因是 librime 的 `patch:` 语义**不是合并，而是整段替换**：
`PatchLiteral::Resolve()` 对 list 类型的值直接 `*target = value`。
而 `AutoPatchConfigPlugin::ReviewCompileOutput()` 会自动追加一条
`PatchReference(<schema>.custom, "patch", true)` —— 也就是**用户目录下的
`<schema>.custom.yaml` 一定会参与这次 patch**。于是链条是这样断的：

1. rime-sync 往 data 目录的 `engine/processors` 里插了 `numpad`（见[第 9 节](#9-data-方案补丁0618)）；
2. 但用户目录里那份 `<schema>.custom.yaml` 里**也**写了一份 `engine/processors`
   （rime-sync 自己生成的模板，见 `packaging/rime/schemas/*.custom.yaml`），
   它是整段替换，把上一步插进去的 `numpad` 一起顶掉了；
3. 编译产物里就没有 `numpad` 这个 processor，小键盘数字键失效。

而「用户目录里那份 custom.yaml 是旧的」是常态：安装策略是「已存在的一律沿用
（不覆盖）」，所以 **v0.6.8 ~ v0.6.17 期间生成的老 custom.yaml 会一直留在用户目录里**，
永远等不到新模板。v0.6.18 才把 `numpad` 加进模板，但老文件不会被替换。

模板历史（`packaging/rime/schemas/*.custom.yaml`）：

| 版本 | `rimesync_vselect` | `numpad` |
|---|---|---|
| v0.6.7 | 无 | 无 |
| v0.6.8 ~ v0.6.17 | 有 | **无** |
| v0.6.18 起 | 有 | 有 |

所以：**从 v0.6.8 ~ v0.6.17 装过、并且一直沿用旧文件的机器，小键盘是坏的**，
而且重装、升级、重跑 `ensure` 都不会自己好。

### 14.2 修法：只增量修补 rime-sync 自己生成的那一份

新增 `crates/core/src/managed_custom.rs`（配套 `crates/core/src/yaml_text.rs`
做纯文本 YAML 编辑）。规则（`repair_file`）：

| 情况 | 结果 |
|---|---|
| 文件没有 rime-sync 标记 | `Skipped("not-managed")` —— 用户自己写的，一个字节都不动 |
| 有标记，但定制已关闭（`rimesync_vnum` / `rimesync_punct` / `rimesync_vselect` 都不在） | `Skipped("rimesync-disabled")` |
| 有标记、定制开着，但没有 `engine/processors` 列表 | `Skipped("no-processors-list")` |
| 有 `rimesync_vselect` 但没有 `numpad` | **在 `rimesync_vselect` 后面紧接着插入 `numpad`** |
| 已有 `numpad`（且位置正确） | `Unchanged` —— **完全不写盘** |

配套纪律：

* **先备份再改**：备份成 `<文件>.rimesync-orig`，**只备份一次**（已存在就不覆盖，
  保住最初那份）；
* **纯文本编辑**（`yaml_text.rs`）：CRLF/LF、结尾换行、BOM、注释、缩进都原样保留，
  不重新序列化整个 YAML；
* **写盘前复查**：改完重新用 serde_yaml 解析，并确认 `rimesync_vselect` 与
  `numpad` 仍然相邻；任何一项不成立就**放弃写入**（不写半成品）；
* **幂等**：跑第二遍是 `Unchanged`，也不产生第二次备份。

「rime-sync 自己的文件」的判据是**文件里有 rime-sync 的标记**
（文件里有任一行以 `# 由 rime-sync 生成` 或 `# patched by rime-sync` 开头）**且定制还开着**
（文件里还有 `rimesync_vnum` / `rimesync_punct` / `rimesync_vselect` 之一）。
用户照 [INPUT-ENHANCEMENTS 第 4 节](INPUT-ENHANCEMENTS.md#4-怎么关闭) 关掉定制
（删掉文件、或删掉 `rimesync_*` 那几行）之后，rime-sync 就不再碰它。

`default.custom.yaml` / `weasel.custom.yaml` **永远不参与**：它们没有 rime-sync 标记。

### 14.3 常驻判定：每轮都重新判，不靠「这轮有没有改过文件」

`ime_state::assess_numpad(rime_dir, data_dirs)`（只读）给每个**保留主方案**
一个状态：

| 状态 | 含义 | 算缺口吗 |
|---|---|---|
| `ok` | 产物里有 numpad | 否 |
| `source-stale` | 来源侧就坏：用户 custom.yaml 自己定义的 `engine/processors` 里没有 numpad | 是（可修） |
| `artifact-stale` | 来源侧没问题，但 `build/<id>.schema.yaml` 里没有 numpad（还没重编译） | 是 |
| `no-artifact` | 压根没有编译产物（还没编译过） | 否（产物可用性由 `assess` 判） |
| `skipped` | 用户自己改过的文件，或用户关掉了定制 | 否 |

**为什么必须常驻判**：修完用户 custom.yaml 之后，引擎可能一直没重编译。
如果判定只挂在「本轮改没改过文件」上，第二轮就会报 ok —— 又回到缺陷三当初
那种「报告说 ok、小键盘其实用不了」的静默失败。所以 `assess_numpad` **每轮都重新跑**，
只看**当前磁盘状态**。只判保留主方案（依赖方案的产物本来就没有 numpad）。

### 14.4 报告与出口

* CLI（`check-ime` / `ensure-ime` / `fix-ime`）的 JSON 报告都多一个 `numpad` 段：
  `{ ok, missing: ["<id>=<state>"], detail: ["<id>=<state>"], hint }`；
  文本模式多一行 `numpad=ok` 或 `numpad=问题 <ids>（<hint>）`。
* `check-ime` 的结论口径：`build_ok=false` → `failed`；有小键盘缺口 → `partial`；
  其余 → `ok`。**`partial` 与 `failed` 一样非零退出** —— 「小键盘不能用」不算成功。
* GUI：「修复输入法」的报告带同一段 `numpad`；启动自举的状态里多一个原因码
  `numpad-pending`（`imeBanner.reasonNumpadPending`）。
* `ensure` 的动作日志里有一条 `custom-repair`（改了哪些文件、备份在哪），
  修补算「本次改了磁盘」→ 会继续走部署。

### 14.5 重编译的时机：Windows 与 Linux 不一样

判定挂在产物上，所以**产物里出现 numpad 才算修好**，而产物要等引擎重编译。
librime 决定要不要重编译看的是 `ConfigNeedsUpdate()`：拿
`__build_info.timestamps[resource_id]` 和 `resolver->ResolvePath(resource_id)`
的 mtime 比 —— resolver 的根是**用户数据目录**，所以被比的就是**用户目录里那份
`<id>.custom.yaml`**。我们改的正是这个文件，因此改完就满足了「需要重编译」。

* **Windows**：`ensure` 会走进程内部署（`deploy::decide` → `DeployMethod::Inproc`），
  部署即重编译，所以**修完这一轮产物里就有 numpad**。若仍报 `artifact-stale`，
  再点一次「修复输入法」即可。
* **Linux**：`ensure` 不在进程内部署（`deploy::decide` 直接返回
  `platform:linux-keeps-existing-deploy`），重编译交给 librime 自己在下次引擎
  启动 / 维护时做（`DetectModifications` 比较用户 + 共享目录里所有 `.yaml` 的
  最大 mtime 与 `var/last_build_time`）。所以 **Linux 上第一轮多半还是 partial，
  重启输入法后的下一轮才干净**。这是平台固有的差异，不是 bug。
