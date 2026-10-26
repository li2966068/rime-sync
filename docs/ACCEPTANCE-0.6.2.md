# v0.6.2 发布与验收清单

发布日期：2026-10-24　发布流水线：40096058327（v0.6.1）、40101520493（v0.6.2）

## 一、这一版解决了什么

| 问题 | 版本 | 现在的结果 |
|---|---|---|
| Windows 没有五笔 | 0.6.2 | 随包 16 个方案（五笔 86/拼音/繁体、仓颉五、笔画、地球拼音、注音、朙月拼音全系），反查可用（`znihao` → 你好） |
| 自编译 Weasel 比官方少方案 | 0.6.2 | 方案数据与官方对齐并额外补五笔；Windows 与 Linux 数据一致 |
| 同步把另一台机器的方案冲掉 | 0.6.2 | `schema_list` 取集合并集，本机顺序优先 + 追加远端独有 |
| 改设备名称后远端目录对不上 | 0.6.1 | 远端目录名统一用 `installation_id`；改名即迁移，失败回滚 |
| 去重可能删掉本机目录 | 0.6.1 | 本机判定改用 `installation_id`，误选被拒并计入失败 |
| Linux 安装被系统坏状态拖死 | 0.6.1 | 只补缺失包、不升级系统库；多架构冲突只警告并给修复命令 |
| fcitx5「立即同步」点击无效 | 0.6.1 | 随包提供 `rime-sync-cli` |
| Windows 入口静默消失 | 0.6.1 | 打 tag 时自编译失败直接红；`verify-artifacts` 断言产物内容 |
| Actions 制品配额被撑爆 | 0.6.1 | 发布改为草稿 Release 直传，不再用 `upload-artifact` |

## 二、发布产物（16 个资产）

| 类型 | 文件 | 大小 |
|---|---|---|
| Linux 一体安装器 | `rime-sync-installer_0.6.2.{jammy,noble,bookworm,trixie}.run` | 136 / 147 / 172 / 183 MiB |
| 输入法 deb | `rime-inputmethod_0.6.2.{jammy,noble,bookworm,trixie}_amd64.deb` | 5 MiB |
| 同步程序 | `rime-sync_0.6.2_amd64.deb` / `.AppImage`(+sig) | 7 / 78 MiB |
| Windows | `rime-sync_0.6.2_x64-setup.exe`(+sig) / `x64-portable.zip` | 16 / 2 MiB |
| 元数据 | `latest.json`、`WEASEL-SOURCE.json` | |

## 三、CI 断言（自动，已通过）

- [x] `rime.so` 内含 `/usr/bin/rime-sync-status`（Linux 配置页入口）
- [x] `zh_CN` 的 `fcitx5-rime.mo` 存在（中文界面）
- [x] `rime-sync-status`、`uninstall.sh` 权限 755
- [x] `setup.exe` 内嵌 `weasel-0.17.4.0-installer.exe`，其中 `WeaselServer.exe` 含 UTF-16LE「Rime 同步」×5
- [x] `WEASEL-SOURCE.json`：`mode=require`、`source=self-built`
- [x] Windows 构建日志：`[data] 方案数据：35 个 yaml + 1 个 txt，来自 9 个 recipe`，`schema_list` 16 条含 `wubi86`
- [x] `latest.json` 的 `notes` 为 CHANGELOG 0.6.2 中文段落

## 四、真机验收清单

### Windows

- [ ] 覆盖安装 `rime-sync_0.6.2_x64-setup.exe`，`%APPDATA%\Rime` 未被清空
- [ ] 小狼毫托盘右键第 4 项为「输入法自建同步」，点击进入同步设置向导
- [ ] 开始菜单「小狼毫 → 输入法设定」底部有「Rime 同步…」按钮
- [ ] 卸载程序默认询问是否连小狼毫一起卸载；选「否」后 Weasel 仍在
- [ ] 托盘 → 重新部署，方案列表可见：朙月拼音、**五笔86**、五笔拼音、仓颉五、笔画、地球拼音、注音
- [ ] 打字验证：`wubi86` 输入 `a` 出候选；`znihao` 反查得到「你好」
- [ ] 同步程序「设置 → 输入方案」可勾选/排序并保存后自动部署

### Linux（Debian 13 / Ubuntu 22.04 等）

- [ ] `sudo ./rime-sync-installer_0.6.2.<suite>.run` 安装成功，`dpkg -V rime-inputmethod` 无输出
- [ ] `fcitx5-configtool → 附加组件 → Rime` 配置页可见：用户数据目录、打开用户目录、**打开同步面板 / 立即同步 / 查看同步状态**
- [ ] 点击「立即同步」真的执行了一次 transport（`~/.config/com.rimesync.app/history.jsonl` 有新记录）
- [ ] 点击「查看同步状态」弹出桌面通知，内容为最近一次结果
- [ ] 卸载：`/usr/lib/rime-sync/uninstall.sh --purge-data` 会先列路径再删；不带参数则保留用户数据
- [ ] 同步程序「设置 → 输入方案」中可见随包 16 个方案

### 两端共通

- [ ] 同步页「预览同步」列出的动作与随后实际执行一致
- [ ] 两台机器方案不同（一台启用五笔、一台不启用）时，同步后双方都保留五笔
- [ ] 冲突页可用「用远端覆盖本地」，副本可删除
- [ ] 备份区可设保留份数、可删除单份、可一键清理

## 五、已知遗留（不影响使用）

- Windows 侧 ARM 组件未构建（`install.nsi` 用 `!ifdef` 自动跳过；有 ARM 工具链时会自动打开）
- 自编译 Weasel 的安装目录内未单独投放每个 recipe 的 LICENSE 文件；许可信息在仓库与 `docs/THIRD-PARTY.md`
- 并集合并只发生在「两边都改」的 patch 冲突路径；只有远端改时仍是整文件取远端
- 真实 WebDAV 多机演练仍建议手工做一次（CI 的 e2e 用 rclone 模拟）

## 六、出问题时怎么排查

```bash
# Linux
dpkg -V rime-inputmethod                    # 文件完整性
tail -50 /var/log/rime-sync/install.log     # 安装日志
rime-sync-cli status                        # 设备/远端状态
tail -5 ~/.config/com.rimesync.app/history.jsonl

# Windows
type %APPDATA%\com.rimesync.app\history.jsonl
type %TEMP%\rime-sync-uninstall.log
```

配置重置：删除 `~/.config/com.rimesync.app/index.json`（或 Windows 同名文件）后重新同步，
相当于重建基线；`config.json` 保留。
