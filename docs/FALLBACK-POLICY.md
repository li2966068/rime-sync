# 回退策略（FALLBACK-POLICY）

> 一页规则：**任何"失败后自动降级"的行为，必须同时做到 写日志 + 对外可见 + 可用参数关闭。**
> 三者缺一，就是在静默丢功能，按事故处理。

## 1. 为什么有这份文档

v0.6.0 的 Windows 版发布显示成功，但「Rime 同步…」入口在安装后不存在。

真实链路：

1. Windows job 的自编译 Weasel 步骤（`scripts/ime/build-weasel.ps1`）失败；
2. 该步骤写了 `continue-on-error: true`，失败被吞成"步骤变红但 job 继续"；
3. 下一步 `fetch-weasel.ps1` 自动回退到**上游官方 Weasel 安装包**，只打了一行
   `::warning::`；
4. NSIS 安装包检查只断言"体积 ≥ 15 MiB"，上游包同样满足；
5. `publish` 照常发布，用户在发布页看不到任何提示。

结论：**回退本身不一定错，静默才是错的。** 发布出去的东西和用户以为的东西不一致，就是事故。

## 2. 规则（硬性）

任何 fallback（自动回退、降级、跳过、用旧产物顶替、用上游包顶替）必须同时满足：

| # | 要求 | 判定标准 |
|---|---|---|
| R1 | **写日志** | 回退原因、来源、时间写进构建日志或运行时日志文件；日志里能区分"自编译产物"和"回退产物" |
| R2 | **对外可见** | 构建期回退：job summary 用 `> [!CAUTION]` 红字标注 + `::error` 注解 + **发布说明顶部**写明；运行时回退：UI 里可见（红条/提示），不能只写日志 |
| R3 | **可用参数关闭** | 有显式参数/环境变量要求"不许回退，失败就失败"；发布（tag）流程默认就是"不许回退" |
| R4 | **可被断言抓住** | 产物有机器可校验的标记/断言（`verify-artifacts.sh`），不是只靠人看日志 |

补充约定：

- **默认值方向**：越靠近"发布给用户"的路径，默认越严格（`require`）；本地/手动调试可以放松，
  但必须显式指定。
- **禁止**把 `continue-on-error: true` 写死在关键构建步骤上。允许的唯一形式是按模式求值：
  `continue-on-error: ${{ steps.weasel_mode.outputs.mode != 'require' }}`（ci.yml 有守卫检查）。
- **禁止**只写 `::warning::` 就算"已告知"。warning 在 Actions 界面上就是个黄点，会被折叠。
- **回退产物必须能被识别**：发布流程要能回答"这一版到底带没带 Weasel 入口"。当前实现是
  `WEASEL-SOURCE.json`（`mode` / `source` / `selfbuild_outcome` / `librime` / `written_at`）。
  0.6.9 起 `librime` 字段记录输入法引擎来源：`self-built 1.17.0 (merged rimesync-extra)` 或
  `prebuilt 1.13.1 (no plugins)` —— 后者意味着 Windows 上没有 v 模式 / 标点 Shift 备选 / a-h 选词。

## 3. 现有回退登记表

| 位置 | 回退行为 | 日志 | 可见性 | 关闭方式 | 断言 |
|---|---|---|---|---|---|
| `release.yml` · windows | 自编译 Weasel 失败 → 上游官方安装包 | 步骤日志 + `WEASEL-SOURCE.json` | job summary `> [!CAUTION]` + 发布说明顶部红字 | `weasel_mode=require`（tag 推送强制） | `verify-artifacts.sh` 解出 `WeaselServer.exe` 搜 UTF-16LE「Rime 同步」 |
| `release.yml` · windows | `weasel_mode=fetch`（显式用上游包） | 同上 | 同上 | 选 `require` / `auto` | 同上（`build`/`fetch` 模式下降级为红字告警，`require` 直接失败） |
| `scripts/ime/build-weasel.ps1` · `-LibrimeSource prebuilt` | 显式改用上游预编译 librime（1.13.1，**不含** rimesync-extra） | 红字 banner + job summary 的「librime」行 + `WEASEL-SOURCE.json` 的 `librime` 字段 | 日志红字 `librime 用的是上游预编译包`；summary / 发布说明记为 `prebuilt 1.13.1 (no plugins)` | 去掉该参数（默认 `self`），或直接跑 `build-librime-win.ps1` | `ci.yml` 禁止 release.yml 出现 `-LibrimeTag`（发布版必须自建）；`verify-artifacts.sh` 从 setup 解出 `rime.dll` 搜 `rimesync_vnum`/`rimesync_punct` |
| `packaging/linux/postinst` + worker | 后台依赖安装最终失败（无网/包缺失） | `/var/log/rime-sync/vendor-install.log` | `~/.config/com.rimesync.app/install-warning.json` → 应用诊断页红条「离线依赖未安装完成，点击查看日志/重试」 | 手工 `apt-get install fcitx5 fcitx5-rime`，或点「重试」（pkexec 重跑 wrapper） | 告警文件存在即红条；依赖装好后后端自动清除 |
| `packaging/linux/install-vendor-deps.sh` | 有网 apt 失败 → 用自带离线 .deb 闭包 | 同一日志 | 同上一行（最终失败才写告警） | 预装 fcitx5-rime（postinst 直接跳过） | 同上 |

新增 fallback 时**必须**在表里加一行，否则 review 直接打回。

## 4. 发布流程的模式

`RIMESYNC_WEASEL_MODE`（`release.yml` 的 `windows` job）：

| 触发 | 默认模式 | 行为 |
|---|---|---|
| 推送 `v*` tag | `require` | 自编译失败 = job 失败，**不回退**，不发布 |
| 手动 `workflow_dispatch` + `weasel_mode=auto` | 仓库变量 `RIMESYNC_WEASEL_MODE`，缺省 `build` | 允许回退，红字标注 |
| `weasel_mode=require` | `require` | 同 tag |
| `weasel_mode=build` | `build` | 自编译失败才回退 |
| `weasel_mode=fetch` | `fetch` | 直接用上游包（离线/应急），仍然红字标注 |

`verify-artifacts` job 按模式决定严格程度：

- `require`：`WeaselServer.exe` 里找不到 UTF-16LE `Rime 同步` 字节 → **失败**（publish 不会跑）；
- `build` / `fetch`：允许，但 summary 红字 + `::warning`；
- 手工救急可以给脚本加 `--allow-upstream-weasel`（等价 `--weasel-mode build`），
  但发布说明里的红字仍然会出现。

## 5. 产物断言（verify-artifacts）

`.github/scripts/verify-artifacts.sh`：

| 产物 | 断言 |
|---|---|
| `rime-inputmethod_*.deb` | `rime.so` 内含 `/usr/bin/rime-sync-status` 字节（fcitx5-rime「查看同步状态」入口）；`usr/share/locale/zh_CN/LC_MESSAGES/fcitx5-rime.mo` 存在 |
| `rime-inputmethod_*.deb` / `rime-sync_*.deb` | 安装后布局里 `/usr/bin/rime-sync-status` 存在且 0755；`/usr/lib/rime-sync/uninstall.sh` 存在且 0755（实际由 rime-sync deb 提供，`--strict-ime-bundle` 可要求 IME deb 自带） |
| `rime-sync_*.deb` | `/usr/bin/rime-sync-status` 0755、`/usr/bin/rime-sync-app` 存在 |
| `*-setup.exe` | 解出内嵌 `weasel-*-installer.exe` → 解出 `WeaselServer.exe` → 含 UTF-16LE「Rime 同步」 |

本地跑：

```bash
bash .github/scripts/verify-artifacts.sh --selftest          # 合成产物自检（不需要真构建）
bash .github/scripts/verify-artifacts.sh --dir dist           # 校验真实产物（严格）
bash .github/scripts/verify-artifacts.sh --dir dist --weasel-mode build   # 允许上游回退
```

`ci.yml` 每次 push/PR 跑 `--selftest`（约 20 秒），`release.yml` 的
`verify-artifacts` job 在 `publish` 之前跑真实产物断言。

## 6. Review 清单

提 PR 时自查：

- [ ] 新增的"失败就降级"分支有没有写进本文第 3 节的表里？
- [ ] 回退路径有没有 `::error` / `> [!CAUTION]` / UI 提示？（只写 `::warning` 不算）
- [ ] 有没有一个参数/环境变量能关掉回退？
- [ ] 有没有一条断言能在回退发生时让 CI 红（或至少红字可见）？
- [ ] 默认值是不是"严格"的那一侧？
