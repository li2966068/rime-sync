# v0.6.4 验收清单（设备身份 · 每设备配置 · 菜单改名）

发布日期：2026-10-26　tag：`v0.6.4`（提交 `f19c1e5`）

## 这一版改了什么

| 变更 | 影响 |
|---|---|
| 远端目录命名统一用 `installation_id`，设备页区分「本机 / 在线 / 历史目录」 | 旧版遗留目录（jdk、gs63、5095…）可在设备页注销 |
| 本机新增「重置远端身份」 | 删掉 `sync/<本机ID>`，下次同步重建 |
| 改设备名遇到目标已存在 | 三选一：取消 / 合并（补齐快照） / 删除历史目录后迁移 |
| 用户配置改到 `sync/<设备ID>/config/files/` | 每台设备只同步自己的配置；`shared/files` 只作首次模板与词库源 |
| 旧布局 `profiles/<id>` 自动迁移 | 远端与本机各一次，失败保留旧目录且不阻断同步 |
| 学习词库仍然共享 | 仍走 `sync/<设备ID>/*.userdb.txt` 合并 |
| Windows 菜单项改名「输入法自建同步 (&Y)」 | 与输入法设定 / 用户词典管理 / 用户资料同步同级 |
| 钥匙串调用 2 秒超时 + 按需解析 | CLI 与 fcitx5「立即同步」按钮不再卡死 |

## 真机验收

### 设备身份（Windows 或 Linux 任一台）

- [ ] 设备页能看到三类标记：本机 / 在线 / 历史目录
- [ ] 历史目录（如 `jdk`）可直接「注销」，远端目录消失
- [ ] 本机行「重置远端身份」二次确认后，`sync/<本机ID>` 被删，下次同步重新出现
- [ ] 把设备名改成已存在的名字 → 弹三选一；选「合并」后两边快照都保留；选「删除目标后迁移」只允许历史目录
- [ ] 设置页显示「显示名称与本机目录名不同：远端目录是 <installation_id>」（当两者不同时）

### 每设备配置（两台机器）

- [ ] 远端结构：`sync/<A>/config/files/default.custom.yaml` 与 `sync/<B>/config/files/...` 各一份
- [ ] A 改 `menu/page_size` → 同步 → B 的配置**不变**
- [ ] A 改 `schema_list`（设为五笔）→ 同步 → B 仍是自己的方案
- [ ] 新设备首次同步：从 `shared/files` 拉到默认模板（日志/notes 有「从 shared/files 拉取 N 个默认配置」）
- [ ] 旧布局升级：远端如有 `profiles/<id>`，同步后自动搬到 `sync/<id>/config` 并删除旧目录
- [ ] 同步后按一次「预览同步」：不再出现反复上传同一条配置（收敛）

### 词库

- [ ] 两台机器各自打字后同步，词库条目数都增长（共享生效）
- [ ] `sync/<id>/*.userdb.txt` 仍在（未被新的 config 目录影响）

### Windows 菜单

- [ ] 托盘右键：第 1 输入法设定(S)、第 2 用户词典管理(D)、第 3 用户资料同步(N)、第 4 **输入法自建同步(Y)**、第 5 分隔线
- [ ] 点第 4 项直接进同步设置向导
- [ ] 三种语言下都没有乱码或方框

### 卡死回归

- [ ] 在桌面会话里点 fcitx5「立即同步」→ 有响应（不再无反应）
- [ ] `rime-sync-cli --config ~/.config/com.rimesync.app/config.json --json detect` 1 秒内返回

## 已知限制

- 配置目录迁移只在可写策略（normal）下发生；预览/只读策略第一轮不做，切到双向后才迁移；
- 「预览同步」不模拟迁移，旧布局下会多报几条“将上传”；
- 每轮可写同步多一次 `PROPFIND` 探测旧目录；
- 无 Secret Service 的桌面会话里，需要凭据的命令每次多 ~2 秒探测（超时后回退文件后端）。

## 排障

```bash
tail -5 ~/.config/com.rimesync.app/history.jsonl
rime-sync-cli --config ~/.config/com.rimesync.app/config.json --json status
cat ~/.config/com.rimesync.app/config.json | head -20
# 远端结构（WebDAV 上）
#   shared/files/            ← 首次模板 + 词库源
#   sync/<设备ID>/            ← 快照 *.userdb.txt
#   sync/<设备ID>/config/     ← 该设备自己的配置 + manifest.json
```
