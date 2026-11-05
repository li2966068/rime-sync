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
  * `prebuilt`：什么也不做，也不写日志（每次启动都写会淹没日志）；
  * `none` / `deploy`：`install_prebuilt`，结果写应用日志 `[bootstrap] …`；
* 任何失败（读配置、定位目录、payload 缺失、复制失败）都只记日志：
  **不弹窗、不退出、不影响同步功能**；
* 开关与安装器一致：`RIMESYNC_PREBUILT=0` 关闭自举；`RIMESYNC_PREBUILT=force`
  覆盖 `build/`。

## 7. 手动入口

### 7.1 设置页

「设置 → 修复输入法」：

* **修复输入法**：缺才铺（第一次自举失败后重试用）；
* **强制覆盖**：等价安装器的 `force`（只覆盖 `build/`，`config/*.yaml` 不动）。

两者都调用 Tauri 命令 `cmd_fix_ime`，展示 `copied / kept / overwritten / payload_source`
与日志路径。

### 7.2 CLI

```bash
# 自动定位 payload（安装目录 / deb / AppImage）
rime-sync-cli fix-ime

# 覆盖已存在的 build/（config 永不覆盖）
rime-sync-cli fix-ime --force

# 脚本消费：输出 PrebuiltReport 的 JSON
rime-sync-cli fix-ime --json

# 指定 payload（调试 / 离线手工铺）
rime-sync-cli fix-ime --payload /path/to/prebuilt --rime-dir /path/to/Rime
```

`--rime-dir` 与 `--json` 是全局参数（写在子命令前后都可以）。`fix-ime`
不访问 WebDAV、不解析密码（不触发钥匙串探测）。铺完 `build_ok=false` 时退出码为 1
（JSON 仍会先打印完整报告）。

## 8. 日志

* 应用日志：`[bootstrap] …` / `[fix-ime] …`（设置页「日志」可见）；
* 每次自举的明细追加到 `<Rime 用户目录>/.rimesync-prebuilt.log`：
  开始行（payload / rime / force / probe_before）、覆盖与失败明细、结束行（计数 / probe_after）。

该文件不在同步范围内（只同步 `*.custom.yaml`、词库快照与词典源）。

## 9. 测试

* `crates/core/src/prebuilt.rs` 单测（8 个）：缺文件铺、已有文件跳过、旧产物无标记触发
  build 覆盖、config 永不覆盖、force 只影响 build、payload 缺失的可读错误、
  probe 三态、exe 相对定位；
* `crates/cli/tests/fix_ime.rs` 集成测试（2 个）：假 payload + 临时目录跑
  `fix-ime --json`，校验 JSON 计数与真实落盘文件；
* 门禁：`cargo test --workspace --all-targets`、`cargo fmt --all -- --check`、
  `cargo clippy --workspace --all-targets -- -D warnings`、
  `pnpm exec tsc --noEmit`、`pnpm build`。

## 10. 已知限制 / 未验证

* 真机（Windows 227）上的启动自举还没有跑过：本地只验证了编译、单测与 CLI 集成测试；
* AppImage 的 payload 路径按 Tauri 资源布局与 `$APPDIR` 双路探测，未在真实
  AppImage 里验证；
* `/usr/share/rime-sync/prebuilt` 目前由 `packaging/linux/postinst` 引用，但 deb 的
  文件映射里还没有这一份（程序会优先命中 Tauri 资源目录）；两者取其一即可，重复打包
  会白占约 29 MB；
* 自举只铺产物，不触发重新部署；已运行的 Weasel / fcitx5 需要重启输入法或重新选方案
  后才会加载新的 `build/`。
