# 方案丙 · 待派发简报（阶段四、五 + 审计剩余项）

派发顺序依赖：阶段一~三的子代理回来后验收通过，再派下面这些。

## 派发状态（2026-11-15 更新）

| 简报 | 内容 | 状态 |
|---|---|---|
| p1-spec | processors 单一真相源 + managed_custom 补 punct | ✅ 已完成并验收 |
| p1-recompile | 统一重编译通道 | ✅ 已完成并验收（我另修了它引入的 GUI 配置路径缺陷） |
| p1-template | 随包模板顺序对齐 + MANIFEST 重生成 | ✅ 已完成并验收 |
| p1-safety | 路径解析收敛 + 数据安全 B1/B2/B3 | ⚠️ 未交闭环报告；A（路径解析）与 B1（读不出来≠不存在）已落地，B2/B3 由我与 engine-safety 接手完成 |
| d-outer | D1 GUI 静默改配置 / D2 prune 静默跳过 / D4 密码 chmod / D6 引擎判据 | ✅ 已完成并验收（含 punct 降级为 partial） |
| d-ci | E1 Windows --all-targets / E2 桌面端 84 单测进 CI | ✅ 已完成并验收（CI 13 个 job） |
| d-tests | C1 ensure→patch_auto_in 集成覆盖 / C2 死代码 / C3 错误消息 / C4 root 跳过 | ✅ 已完成并验收 |
| engine-safety | transport 覆盖本地文件前留备份（B2）+ 退避状态如实报告（B1） | ✅ 已完成并验收（含「读不出来≠不存在」的三态判定） |
| packaging-xdg | postinst 校验 sha256（B2'）+ 两端认 XDG（B3'） | ✅ 已完成并验收（我另把同一套校验补进了离线安装器 install.sh） |
| tauri-env | 桌面端认 RIME_SYNC_* 环境变量（B4） | ✅ 已完成并验收 |
| **B3（我亲自做）** | restore 留不下还原点就中止 | ✅ 已完成并验收 |
| **B2''（我亲自做）** | 离线安装器 install.sh 也校验 sha256 | ✅ 已完成并验收（原为 packaging-xdg 边界外的遗漏） |
| F | 阶段四 界面合一 | ⏳ 等前三阶段落地 |
| G | 阶段五 Linux 两 deb 合并 | ⏸ 暂缓（见下） |

### 审计编号对照（历史记录里编号有出入，以内容为准）

17:03 那条记忆里的编号：⑦=退避落盘失败被吞、⑨=GUI 静默改配置、⑩=postinst 不校验 sha256、
⑪=引擎判据两端不一致+check-ime 不查 punct、⑫=shell 不认 XDG_CONFIG_HOME、⑭=restore 前快照失败被吞。

**以内容为准**，编号只是标签。

## B 批次简报（等 p1-safety / d-outer 完工后派）

### B1 退避状态落盘失败被吞（审计「⑦」）
`crates/core/src/engine.rs:342` `let retry = RetryState::load(&retry_path).unwrap_or_default();`
—— retry.json 损坏时**静默重置为 0**，用户看不到任何提示，退避形同虚设。
`crates/core/src/engine.rs:807/813` `let _ = self.retry.save(&self.retry_path);`
—— 保存失败被吞，但 `report` 里**仍然带着 `next_retry_at`** ⇒ 报告说「下次重试时间到了」，
实际根本没记下来。

**修法**：load 失败要**如实报告**（区分「文件不存在」= 首次，与「文件损坏」= 出错）；
save 失败要**清掉 report 里的 next_retry_at**，不能报一个不存在的下次重试时间。

### B2 postinst 铺随包产物不校验 sha256（审计「⑩」）
`packaging/linux/postinst:368-384` `prebuilt_copy_dir` 纯拷贝，不看
`/usr/share/rime-sync/prebuilt/MANIFEST.json`。Rust 侧 `crates/core/src/prebuilt.rs:1278`
`verify_manifest` 逐文件比字节数与 sha256，不符进 `manifest_failed` → 结论降级 partial。

**修法**：postinst 铺产物前校验一次 MANIFEST.json，对不上就跳过并写日志。

### B3 shell 不认 XDG_CONFIG_HOME（审计「⑫」）
`packaging/linux/postinst:94`、`uninstall.sh:173`、`install-extra-ime.sh:274`
一律用 `$home/.config/fcitx5/conf/rime.conf`。Rust 侧 `crates/core/src/rime_dir.rs:99-108`
的 `config_home()` 认 `$XDG_CONFIG_HOME`。默认值一侧也不一致：引擎默认用户目录是
`$XDG_DATA_HOME/fcitx5/rime`（`third_party/fcitx5-rime/src/rimeengine.cpp`），
Rust 候选列表只有 `~/.local/share/fcitx5/rime`。

**注**：与 p1-safety 的路径解析收敛同文件，**必须等它完工再派**。

### B4 apply_env 只在 CLI（体验一致性）
`crates/core/src/config.rs:399` 的 `apply_env()` 只被 `crates/cli/src/main.rs:531` 调用，
**Tauri 从不调** ⇒ 环境变量 `RIME_SYNC_DAV_URL` / `RIME_SYNC_DAV_USER` /
`RIME_SYNC_DAV_PASS` / `RIME_SYNC_RIME_DIR` / `RIME_SYNC_DAV_DIR` /
`RIME_SYNC_FIRST_SYNC_POLICY` 在**桌面端完全不生效**。

**注**：要改 `src-tauri/src/lib.rs`（d-outer 正在改同一文件），**必须等它完工再派**。

## 简报 D：GUI 与外围一致性（审计 ⑨⑩⑪⑫⑬⑭）

**边界文件**：`src-tauri/src/lib.rs`、`crates/core/src/prune.rs`、`crates/core/src/secret.rs`、
`crates/core/src/prebuilt.rs`、`src/ui/*.ts`、`crates/cli/src/main.rs`（仅必要处）

### D1（审计⑨，中）GUI 静默改掉用户填的配置
`src-tauri/src/lib.rs:150-159` `detect_rime`：
```rust
rime_dir::detect(config.rime_dir.as_deref().map(Path::new))
    .or_else(|_| rime_dir::detect(None))     // ← 悄悄换目录
```
`src-tauri/src/lib.rs:256-270` `cmd_get_config`：配置里的 `rime_dir` 目录不存在时，
**把 `config.rime_dir` 清空并写回磁盘**（用户手工填的路径被吃掉）。

对照 CLI（`crates/cli/src/main.rs:1750-1755`）：**直接失败报错**，什么都不做。

**修法**：GUI 与 CLI 对齐——配置里的目录不存在时**明确报错**（或至少在 UI 上明确提示
「配置目录不存在，已改用 X」），**不要静默清空用户填的值**。CLI 侧还做了
`fs::canonicalize`（main.rs:1754），GUI 不做 ⇒ 软链目录下两边显示的路径不同。

**验收**：同一份坏配置，CLI 报错、GUI 也报错（或明确提示），两边**行为一致**；
GUI **不再静默改写** config.json。

### D2（审计⑩，中）探测不到 Rime 目录时 prune 整段跳过却报成功
`crates/core/src/prune.rs:205-208`：
```rust
None => rime_dir::detect(None).ok().map(|detection| detection.path),
```
`.ok()` 把「找不到」变成 None ⇒ 用户目录分支（build/ 产物、用户 *.custom.yaml、
用户 default.custom.yaml 的 schema_list）**整段不裁剪**，而报告仍是「删除 0 个文件，
保留 6，失败 0」——用户看不出什么都没做。调用点：`crates/cli/src/main.rs:1541`、
`src-tauri/src/prebuilt_cmds.rs:1229`。`ime_state::ensure` 走显式 `Some(rime_dir)`，不受影响。

**修法**：探测失败时**如实报告**（warning 或失败），不要静默跳过。

**验收**：探测不到目录时，prune 的输出里**看得出**用户目录没被裁剪。

### D3（审计⑬，中）三个 shell 脚本不认 XDG_CONFIG_HOME
`packaging/linux/postinst:94`、`uninstall.sh:173`、`install-extra-ime.sh:274`
一律用 `$home/.config/fcitx5/conf/rime.conf`。而 Rust 侧 `crates/core/src/rime_dir.rs:99-108`
的 `config_home()` 认 `$XDG_CONFIG_HOME`。默认值一侧也不一致：引擎默认用户目录是
`$XDG_DATA_HOME/fcitx5/rime`（`third_party/fcitx5-rime/src/rimeengine.cpp` 的
`StandardPath::global().userDirectory(StandardPath::Type::PkgData)`），
而 Rust 候选列表只有 `~/.local/share/fcitx5/rime`（rime_dir.rs:71）。

**注**：本项与 p1-safety 的任务 A 有重叠（都在改这三个 shell 脚本）。
**p1-safety 回来后再派本项**，避免冲突。本项重点是 **XDG**（不是解析规则）。

### D4（审计⑭，中低）密码文件先落盘再 chmod
`crates/core/src/secret.rs:193` `fs::write(&self.path, ...)` 先落盘，
`:198` 才 `set_permissions(0o600)` ⇒ 同机其他用户有极短窗口可读到密码文件。
对照 `crates/core/src/config.rs:371-397` 的 tmp→chmod→rename **顺序是对的**。

**修法**：照 config.rs 的正确顺序改（写临时文件 → chmod → rename）。

### D5（审计⑪，中）postinst 铺随包产物不校验 sha256
`packaging/linux/postinst:368-384` `prebuilt_copy_dir` 纯拷贝，不看
`/usr/share/rime-sync/prebuilt/MANIFEST.json`。Rust 侧
`crates/core/src/prebuilt.rs:1278` `verify_manifest` 逐文件比字节数与 sha256，
不符进 `manifest_failed` → 结论降级 partial。

**修法**：postinst 铺产物前校验一次 MANIFEST.json，对不上就跳过并写日志。

### D6（审计⑫，中）引擎健康判据两端不一致 + check-ime 不查 punct
- `crates/core/src/ime.rs:47` `PLUGIN_MARKER = "rimesync_vnum"`（单标记）
- `crates/core/src/prebuilt.rs:30` `MARKERS = ["rimesync_vnum", "rimesync_punct"]`（两个）
- `packaging/linux/install.sh:749-752`、`postinst:362-365`（要两个）
- `crates/core/src/ime_state.rs:690`（`||`，任一即可）

只含 vnum 不含 punct 的引擎在 Windows 上被判为健康 → 不触发换装；而 `check-ime`
全程只查 numpad（`ime_state.rs:672-737`），不查 punct ⇒ 报 `ok` 但标点直出是关的。

**修法**：判据统一（以 `prebuilt.rs::MARKERS` 为准）；`check-ime` 增加 punct 检查。
**注意**：这条与 p1-spec 的「补 punct」相关——如果 p1-spec 已经把 punct 补进了
`managed_custom`，D6 只需对齐判据与体检。

## 简报 E：CI 护栏（审计 L3 L9 + 方案丙的安全网）

**边界文件**：`.github/workflows/ci.yml`、`scripts/ci/*`

### E1（L3）windows-typecheck 没加 --all-targets
`.github/workflows/ci.yml:114-118` 跑 `cargo check -p rime-sync-core/-cli
--target x86_64-pc-windows-gnu` **没有 `--all-targets`** ⇒ `#[cfg(windows)]` 的
**测试代码**从不过类型检查；Linux CI 上 Windows 分支直接被编译掉。

**修法**：加 `--all-targets`。

### E2（L9）桌面端 84 个单元测试 CI 一个都不跑
`src-tauri/` 是**独立 workspace**（自己的 `[workspace]` 在 `src-tauri/Cargo.toml:43`），
根 `Cargo.toml` 的 `exclude` 里有 `src-tauri` ⇒ 根 `cargo test --workspace` 只含
`crates/core` 与 `crates/cli`。`grep -rn '#\[test\]' src-tauri/src/ | wc -l` = **84**。

**障碍（必须处理）**：CI 没装 tauri 的 Linux 系统依赖（webkit2gtk 等），
且 **Ubuntu 22.04 装不了 tauri v2 要的 `webkit2gtk-4.1`**（只有 4.0）
⇒ 要跑就得换 **ubuntu-24.04** runner + 装一批依赖。

**约束**：public 仓库 runner **免费**（2026-11-15 记忆：账号 li2966068 是 GitHub Free、
无付款方式，仓库已转 public ⇒ standard runner 独立免费）。
**绝不提议加付款方式或调高 budget 上限。**

**验收**：CI 里 `src-tauri` 的 84 个测试真的跑了；报告里给出 job 名与通过数。

## 简报 F：阶段四 界面合一（方案丙第四阶段）

**边界文件**：`src-tauri/src/lib.rs`、`src/app.ts`、`src/main.ts`、`packaging/nsis-hooks*.nsh`、
`packaging/linux/*`（入口部分）

**已核实的现状**：
- 主程序已有启动参数机制：`src-tauri/src/lib.rs:942` `setup_requested(args)`（认 `--setup`）、
  `:948` `autostart_requested(args)`（认 `--autostart` / `--minimized`）
- 前端已有页面切换：`src/app.ts:47` `export function switchPanel(name: string)`
  （`data-panel` 的五个页面：connection / sync / conflicts / devices / settings）
- 输入法侧**不含任何同步逻辑**：`third_party/librime/plugins/rimesync-extra/` 只有
  numpad / punct / vnum / vselect 四个处理器
- 入口现在就是「拉起主程序」（`--setup`），Weasel 菜单 / fcitx5 配置工具里的按钮

**要做**：输入法内的同步入口 → **唤起主程序并指定页面**（如 `--panel sync`），
而不是拉起独立进程。主程序收到参数后 `switchPanel` 到对应页。

**约束**：入口侧**只做调用**，绝不把 WebDAV 写进 C++。

**✅ 已落地**：

- `src-tauri/src/lib.rs`：新增 `--panel <页面>` 解析（白名单 `PANEL_NAMES`）、
  `Inner.panel_request` 状态、`cmd_take_panel_request` 命令、`request_panel()` 统一入口；
  接入 `tauri-plugin-single-instance`，`handle_second_instance()` 处理第二次启动的
  `--panel` / `--setup`（切页 + 把窗口带到前台）。
- `src/panels.ts` + `src/panels.test.ts`：页面名白名单的纯逻辑单测（node --test，
  零测试框架依赖），CI 的 frontend job 会跑。
- `src/api.ts`：`takePanelRequest`。
- `src/app.ts`：`switchToRequestedPanel()`（只认白名单）、`init()` 取启动参数切页、
  监听 `panel://request` / `setup://request` / `tray://action`（后者修掉了托盘
  「查看冲突」点了没反应的缺陷）。
- 入口侧：`packaging/fcitx5-addon/scripts/rime-sync-open-setup.sh` 与
  `packaging/nsis-hooks.nsh` 的「同步设置」都改成 `--panel sync --setup`。

**实测结论**：第二个实例退出码 0、已有实例的 stderr 记录 `--panel conflicts requested`
且继续存活 —— 转交链路通。

## 简报 G：阶段五 Linux 两 deb 合并（✅ 已落地，v0.6.33）

做法：`packaging/linux/merge-debs.sh` 把输入法 deb 与同步 deb 合成单个 `rime-sync` 包
（`Provides: fcitx5-rime`，`Conflicts/Replaces` 覆盖旧包名与发行版
`fcitx5-rime`/`librime1*`/`rime-data*`），输入法判定用的 shim 随包下发。

**下面这段是合并之前的判断，留作背景**：

> 可行性已核实（见 `docs/CONVERGENCE.md`），但有个硬约束：
> `rime-inputmethod` 与发行版 `fcitx5-rime`/`rime-data` 的 `Conflicts`/`Replaces` 关系
> （2026-10-22 记忆：Debian 13 上发行版 `rime-data-*` 与自带 `rime-data` 冲突，
> **尚未在装过发行版 rime-data 的机器上验过**）——把同步程序塞进这个包会让卸载语义绑死，
> **用户单独卸载同步程序时可能连带卸掉输入法**。
>
> ⇒ 阶段五先不做，等阶段一~四落地、发行链稳定后再单独评估。Windows 侧不受影响。

**发布后验收**：合并后的 deb 在 Debian 13 上可装可卸，输入法载荷完整（`rime.so` /
`rime.conf` / `usr/share/rime-data`），离线 `.run` 内 269 个 vendor deb 的 sha256
全部校验通过，公开资产里 0 个 `.deb`（搜狗词库从未进包）。