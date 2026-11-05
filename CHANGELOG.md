# 更新日志

本项目遵循语义化版本；日期为发布日（UTC+8）。

## v0.6.16 — 2026-11-06

1. **「修复输入法」自举**：有些机器上小狼毫的「重新部署」静默失效（返回成功却不产出任何编译结果），安装器钩子在静默安装时也可能不执行。
   现在程序自己会在启动时检查用户目录里的方案产物，缺失或不可用时**自动从安装包自带的预编译产物铺好**，不依赖安装器、不依赖部署器。
2. **命令行一条命令修复**：`rime-sync-cli fix-ime [--force] [--json]`，输出铺入/保留/覆盖的数量与最终状态，可脚本化。
3. **设置页新增「修复输入法」**：一键铺入，带「强制覆盖」选项，并显示 `copied / kept / source` 结果；用户自己的配置永远不被覆盖。
4. 铺入过程写日志到 `<Rime 用户目录>/.rimesync-prebuilt.log`，出问题可直接查。

说明：预编译产物覆盖朙月拼音与五笔86（含依赖方案）；`RIMESYNC_PREBUILT=0` 可关闭自举，`=force` 强制覆盖产物文件。
## v0.6.15 — 2026-11-05

1. **新增「预编译产物」兜底**：有些 Windows 机器上小狼毫的「重新部署」会静默失效（返回成功却不产出任何编译结果），导致 v 模式、标点直出永远不生效。
   现在安装包自带一份预编译好的方案产物，本机部署失败或产物缺失时会自动铺到用户目录，**不依赖本机部署能力**。
   当前覆盖朙月拼音与五笔86（含其依赖方案），约 28 MB。
2. **安装器自检增加来源标记**：日志和完成提示会写明 `build-source=deploy|prebuilt|none`，一眼能看出这次是“真正部署成功”还是“用了随包产物”。
3. **覆盖策略安全**：只铺缺失的编译产物，用户自己的配置（`*.custom.yaml`）绝不覆盖；仅当检测到“已有产物不可用”时才重铺 `build/` 下的产物文件。
4. 提供开关：`RIMESYNC_PREBUILT=0` 关闭兜底、`RIMESYNC_PREBUILT=force` 强制覆盖。

说明：随包产物是用与 Linux 版相同的 librime 1.17.0 生成的；官方 Weasel 0.17.4（librime 1.13.1）理论上兼容，但未在真机验证。
## v0.6.14 — 2026-11-04

1. **修掉 0.6.13 的严重回归**：上一版在判定“小狼毫是否可用”时会被残留信息骗过，结果是旧输入法被卸掉、新的又没装回来，机器变成没有输入法。
   现在只认一个硬条件——注册表指向的目录里**真的有 `WeaselServer.exe`**；TSF 残留只写日志、不再参与判断。
2. **顺序改成“先判断、再动手”**：判定在任何破坏性动作之前；卸载旧版不再连带删除小狼毫；确认需要重装才动手。
3. **重装失败自动回滚**：先把旧目录改名 `Rime.bak-<时间戳>`，重装后复查不通过就改名回去，并报告 `rollback=ok/failed`，**不会再留下“没有输入法”的状态**。
4. **安装结束硬性自检**：小狼毫可执行文件、注册表安装目录、默认布局三者任一缺失，安装直接判失败（退出码 5）并在完成页提示；
   只有显式加 `/SKIPIME` 时才降级为警告（逃生开关本身不会被判失败）。

说明：升级会先卸载旧版再全新安装（配置保留），这是既定策略；这一版保证“卸了必定装得回来”。
## v0.6.13 — 2026-11-03

1. **真正修好「装了新版但没有输入法功能」**：0.6.12 声称修了配置目录，实际漏了关键一行（`SetShellVarContext current`），
   配置仍然被写进 `C:\ProgramData\Rime`。现在改为先解析真实用户目录 `%APPDATA%\Rime` 再写入，不再回落 ProgramData。
2. **不再放过「假就绪」的小狼毫**：以前只要目录存在就跳过安装，结果机器上可能是“有文件、没注册、服务没跑”的残废状态。
   现在按「注册表 InstallDir/WeaselRoot 有值 + 目录里真有 WeaselServer.exe」判定；不满足就结束旧进程、把残留目录改名
   `Rime.bak-<时间戳>`（保留证据不删）、用内嵌安装器静默重装并复查；仍不满足则安装失败（退出码 5 + 日志 `WEASEL-INSTALL failed`），不再静默放过。
3. **安装结束自检并自动部署**：日志新增 `SELF-CHECK` 一行（小狼毫是否就绪、配置写到哪个目录、新写入/沿用数量、部署退出码、编译产物是否更新），
   并主动跑一次 `WeaselDeployer /deploy`；非静默安装会弹一个简短结果框，出问题不用再猜。

说明：安装失败时旧版已被卸载（先卸后装是既定策略）；应急可加 `/SKIPIME` 跳过输入法安装。
## v0.6.12 — 2026-11-02

1. **Linux 升级与 Windows 对齐**：安装新版时先卸载旧版（输入法本体与同步程序），再全新安装；
   卸载**不删除**用户配置与数据（`~/rime`、`~/.config/fcitx5/rime`、`~/.local/share/fcitx5/rime`、`~/.config/com.rimesync.app`）。
   安装日志新增 `UNINSTALL-OLD` 阶段，卸载失败只告警、不中断安装。
2. **配置模板两平台统一规则**：缺失的才补，已存在的一律沿用（不覆盖、不改名、不备份）；
   Linux 上不再依赖 `--with-schema-tweaks` 或「首次安装」判断，日志逐文件记 `result=new|reuse|skip`。
3. **修复 Linux 日志被清空**：旧版卸载脚本会删掉 `/var/log/rime-sync`，导致安装器写进已删除的文件；现在不再删除，并加了自愈逻辑。
4. **新增运行库说明文档**：明确 Windows 只需要 WebView2，**不需要 .NET 与 VC++ 运行库**（附二进制导入表证据），
   并列出常见误判（卡在 WebView2、误报缺 .NET、缺 VCRUNTIME140 等）与处理方式。

说明：Linux 端这次改动用容器实测过「装旧版 → 装新版」：旧包被 purge、新版全新安装、用户文件 sha256 未变、缺失模板补齐、已有模板保留。
## v0.6.11 — 2026-11-02

1. **升级方式改为「先卸载旧版，再全新安装」**：安装器启动后会静默卸载旧的同步程序与（我们装的）小狼毫，再完整安装新版，
   不再出现“残留旧文件导致新功能不生效”的情况。卸载失败不会中断安装，但会在日志和安装完成提示里明确标出。
2. **卸载不删配置，新版直接复用**：卸载旧版只移除程序本身，`%APPDATA%\Rime` 里的配置文件原样保留；
   新版安装时**缺失的才补**（`default.custom.yaml`、`weasel.custom.yaml`、各方案 `*.custom.yaml`），
   已存在的一律沿用、不覆盖、不改名——你自己的改造和同步下来的设置都不会被冲掉。
   想强制刷新某个配置：删掉对应文件再装一次即可。
3. **安装日志更完整**：新增 `UNINSTALL-OLD` 阶段；结束处汇总「新写入 N 个、沿用 M 个」，卡住时能直接定位。

说明：卸载前会先结束正在运行的同步程序与小狼毫服务进程；如果你自己另装过小狼毫（不是本安装包装的），它不会被卸载，只会在日志与提示里提示手动处理。
## v0.6.10 — 2026-11-02

1. **修「全新安装后输入增强不生效」**：以前安装器发现 `%APPDATA%\Rime` 还不存在（全新装完还没跑过输入法）就整体跳过写入推荐配置，
   导致 v 模式、中文标点直出、a–h 选词全都没有——现在会先建目录再写。
2. **配置模板逐个判断**：`<方案>.custom.yaml` 只在该方案的 `<方案>.schema.yaml` 确实存在时才写入，
   老版本小狼毫（数据里没有五笔）也能吃到拼音系的增强，不会再被一条“必须有五笔”的判断整批跳过。
3. **写入结果可查**：复制后会校验并写日志（`seed <file> ok` / `FAILED copy` / `skip <file>: no …schema.yaml`），
   位置在 `%TEMP%\rime-sync-setup.log`，出问题时不用猜。

说明：已手动拷过 `resources\schemas\*.custom.yaml` 的用户不受影响（存在即跳过，不会覆盖）。
## v0.6.9 — 2026-10-27

1. **Windows 版补上输入增强**：以前 Windows 用的是官方预编译 librime，装不进我们自己的输入法插件，所以 0.6.8 的 v 模式、标点 Shift 备选、a–h 选词在 Windows 上不生效。
   现在 Windows 也**自建 librime**（与 Linux 同为 1.17.0）并合并插件，两端行为一致。
2. **构建期与发布期都加硬断言**：编译后检查 `rime.dll` 里确实含 `rimesync_vnum` / `rimesync_punct` / `rimesync_vselect`，
   发布前再从最终安装包里解出来复查一次；缺了直接失败，不会再出现“发出去才发现功能没生效”。
3. 构建时间相应变长（Windows 首次约 +25–35 分钟，命中缓存后约 +2–4 分钟），已加缓存。

说明：Windows 包内 librime 与 Weasel 的版本组合已随发布记录在 `WEASEL-SOURCE.json`；如需临时回退预编译包（不含输入增强），需显式加参数，发布流水线禁止这种回退。
## v0.6.8 — 2026-10-27

1. **输入法新增「v 模式」**：中文状态下输入 `v` + 数字，直接得到中文金额写法。
   - `v1234` → 壹仟贰佰叁拾肆 / 一千二百三十四；`v1234.56` → 壹仟贰佰叁拾肆元伍角陆分；`v0.05` → 零元零伍分；支持负号；
   - 候选用 **a–h** 选第 1–8 个（数字键会被输入法当成编码，用空格/回车也可以）；
   - 随包 16 个方案全部启用，安装时只写缺失的配置文件，不覆盖你自己的设置。
2. **中文标点直接上屏**：以前按 `\` 出「、」还要再敲空格，现在 `\ < > [ ] { } | % $ * ~ ! ? ; : , .` 这些键在中文状态下**直接上屏**；
   想用同一个键的其它符号时按住 **Shift**（例如 `Shift+,` 出《，`Shift+\` 出备选）。`/` 保持原样，`/fh` 这类符号输入照常可用。
3. **同步支持代理**：设置页「连接」里新增代理小节，支持 **HTTP / HTTPS / SOCKS5**（可带用户名密码，密码存在系统密钥后端/加密文件里），
   代理只作用于同步程序本身，不再需要靠环境变量；命令行也支持 `--proxy` / `--no-proxy`。

已知限制：v 模式候选用字母键选择（数字键会被继续并入输入，这是 Rime 上游行为）；
自动更新检查走 http(s) 代理，SOCKS5 下更新检查仍直连；离线安装器里的 apt/curl 不受同步代理设置影响。
## v0.6.7 — 2026-10-27

1. **新增「开机自启动」（Linux + Windows）**：设置页一键开关，开机后只驻留托盘、不弹主窗口，后台按设定间隔同步。
   - Linux 写 `~/.config/autostart/rime-sync.desktop`（XDG 规范）；**AppImage 也能用**——自启条目会指向 AppImage 文件本身（AppImage 移动后需要重新开启一次，界面会提示）；
   - Windows 写当前用户的 `Run` 注册表项（`HKCU\...\CurrentVersion\Run`），不需要管理员权限；
   - 卡片里会显示当前机制和指向的可执行文件路径，开关失败时显示具体原因。
2. 移除已不再使用的 `tauri-plugin-autostart` 依赖，自启条目由程序自己维护（可以稳定带上 `--autostart` 参数）。

已知限制：部分桌面环境（少数 Wayland 会话）可能忽略 XDG 自启项，可在系统「启动应用程序」里手动添加同一命令；
窗口隐藏后若发现定时同步被浏览器节流，后续会把调度挪到后台线程。
## v0.6.6 — 2026-10-26

1. **Linux 只提供 Debian 13（trixie）**：不再为 22.04 / 24.04 / Debian 12 出包，发布矩阵收敛到一套，下载页更清爽。
2. **两个平台都改成「一个包 = 输入法本体 + 同步工具」**：不再把输入法单独发一个 deb；安装时**先装/更新输入法本体，再装/更新同步工具**。
   每套都分两档：
   - Linux：`rime-sync-installer_<版本>.trixie.run`（**全量**，依赖闭包内置，断网可装，约 176 MiB）
     与 `rime-sync-installer_<版本>.trixie.thin.run`（**非全量**，约 5.5 MiB，依赖由 apt 联网安装；断网时会直接给出改用全量包的提示）；
   - Windows：`rime-sync_<版本>_x64-setup.exe`（**非全量**，约 15 MiB）与 `rime-sync_<版本>_x64-offline-setup.exe`（**全量**，内嵌 WebView2，约 222 MiB）。
3. **发布资产断言重写**：新增资产清单校验（12 项），确保「两个 .run、全量≥100 MiB、thin 明显更小、不再有单独的输入法 deb、Windows 两个 exe 及签名齐全」；
   草稿 Release 里的内部 deb 会在转正前删除，不会出现在下载页。

已知限制：非全量包（thin）需要能访问系统仓库；无外网请用全量包。Windows 非全量包在缺 WebView2 时会提示安装（不再静默下载）。
## v0.6.5 — 2026-10-26

1. **Windows 安装不再卡在 WebView2**：以前安装包会静默联网下载 WebView2 运行时，代理受限或断网时看起来就是“卡死”。
   现在安装器先检测系统里的 WebView2：已装直接跳过；没装时弹框说明并给出下载地址（或选择退出），**任何情况下都不再后台静默下载**。
2. **新增离线安装包**：`rime-sync_0.6.5_x64-offline-setup.exe`（内嵌 WebView2 运行时，约 150 MB），
   内网、无外网或代理受限的电脑用它一次装完，不需要手动折腾运行时。
3. **安装更可控**：新增 `/SKIPWEBVIEW`（缺 WebView2 也继续装）、`/SKIPIME`（跳过内嵌小狼毫安装）；
   安装过程写 `%TEMP%\rime-sync-setup.log`（分阶段时间戳），卡住可直接看日志定位；
   系统低于 Windows 10 1803 会明确提示并退出，不再无意义等待。
4. 顺手修掉离线包构建里的钩子路径问题（`!addincludedir`），保证离线变体一定内嵌运行时。

说明：静默安装（`/S`）且系统缺 WebView2 且未加 `/SKIPWEBVIEW` 时，安装器会直接中止并写入日志，这是有意为之——
无人值守时弹模态框更糟，装上一个跑不起来的程序更糟。
## v0.6.4 — 2026-10-25

1. **设备目录不再混乱**：设备页区分「本机 / 在线 / 历史目录」（旧版本遗留的 jdk、gs63 之类会标成历史目录，可直接注销）；
   本机新增「**重置远端身份**」（二次确认后删掉本机的远端目录，下次同步重建）。改设备名遇到目标目录已存在时，
   不再直接失败，而是三选一：**取消 / 合并（补齐快照后改名）/ 删除历史目录后迁移**；设置页还会提示“显示名称与本机目录名不同”。
2. **每台设备一个配置目录**：用户配置从共享区挪到 `sync/<设备ID>/config/files/`（带各自的 `manifest.json` 基线），
   各设备只同步自己的配置，互不覆盖；`shared/files/` 退化为**首次同步的默认模板**与词库源（`*.dict.yaml`、`custom_phrase.txt`）共享区。
   升级后旧布局 `profiles/<设备ID>/` 会自动迁移到新位置（远端与本机各一次），失败时保留旧目录并只在日志里告警，不阻断同步。
3. **词库仍然全设备共享**：学习词库继续走 `sync/<设备ID>/*.userdb.txt`，同步时合并。
4. **Windows 菜单改名**：小狼毫托盘菜单里的同步入口改为「**输入法自建同步 (&Y)**」，与「输入法设定 / 用户词典管理 / 用户资料同步」同级。
5. **修掉桌面会话里“点了没反应”的卡死**：钥匙串（keyring）不可用时会一直阻塞，导致 CLI 与 fcitx5 的「立即同步」按钮挂起；现在 keyring 调用有 2 秒上限并自动回退到 0600 文件后端，且只有真正需要密码的命令才访问钥匙串（detect/备份/历史完全不碰）。

已知限制：配置目录迁移只发生在可写策略下（只读/预览策略不会写远端，第一轮切到双向后才迁移）；
「预览同步」不会模拟迁移动作，因此存在旧布局时会多报几条“将上传”的条目；每轮可写同步会多一次 PROPFIND 探测旧目录。
## v0.6.3 — 2026-10-24

1. **配置按 key 共享（默认方案各设备独立）**：新增「设备本地键」概念，默认 `schema_list` 只保存在本机、
   上传时会从远端文件里剥离，拉取时也不会覆盖你本地的方案列表；`menu/page_size` 这类键仍然全设备统一。
   设置页可以自己增删这些键（留空即回到“全部共享”的老行为）。想恢复成“各设备方案取并集”，把 `schema_list` 移出即可。
2. **配置键显示改成「中文（English）」**：冲突页、同步结果与方案管理说明里的 `patch.menu/page_size`、
   `schema_list`、`style/font_point` 等不再只显示英文，例如「每页候选数（Candidates per page）」「输入方案列表（Schema list）」；
   鼠标悬停仍能看到原始键名，方便排查。
3. **设置页新增「设备本地键」卡片**：列出当前只存本机的键，可添加/移除，保存后立即生效；空列表会提示“所有 patch 键都会参与同步”。

已知限制：为了剥离/回填设备本地键，写回与上传的 `*.custom.yaml` 会经 YAML 重新序列化，文件里的注释与原始排版可能丢失；
与旧版本客户端同时使用时会来回覆盖远端文件，直到两端都升级。
## v0.6.2 — 2026-10-24

1. **修复「Windows 版没有五笔」**：自编译的小狼毫以前只随包朙月拼音，比官方安装包还少（缺仓颉五、笔画、地球拼音、注音）。
   现在随包 **16 个方案**：朙月拼音（含简化/繁体/流畅/全拼）、**五笔 86 / 五笔拼音 / 五笔繁体**、仓颉五（含速成）、笔画、地球拼音（含 12345）、注音（含台湾/速成）。
   Linux 的输入法包同步补齐，双端一致；安装时若系统数据里确实没有五笔，推荐配置会自动跳过而不是写坏。
2. **同步不再把输入方案同步没**：`default.custom.yaml` 的 `schema_list` 改为**集合并集**合并——
   一台机器启用了五笔、另一台没有，同步后两边都会保留五笔，顺序按本机优先、远端独有追加；面板上记为「已合并」而不是冲突。
3. **新增「输入方案」管理页**：列出随包与自定义方案，勾选启用/停用、上下调整候选顺序，点保存即写入 `default.custom.yaml`
   并自动重新部署；写入前自动备份，其它配置与注释原样保留；部署失败会明确提示可重试。
4. **Windows 打包修复**：小狼毫构建改为遍历全部方案数据、`install.nsi` 的可选载荷（ARM/OpenCC/语法模型）用条件宏处理，
   自编译失败不再静默回退（发布时直接失败）。
## v0.6.1 — 2026-10-24

1. **修好「改设备名称后远端不同步」**：远端目录名统一用 Rime 的 `installation_id`，`device_id` 只作人类可读名称；
   改名称会同时改写 `installation.yaml`、重命名本地与远端同步目录（失败自动回滚）。旧目录不存在时不再报
   「sync/<旧 ID> 不存在」，而是降级为只改本地或直接跳过。设备页的「本机」标记、发布身份、撤销设备都按同一标识判断。
2. **去重不再可能删掉自己的目录**：设备去重与删除闸门改用 `installation_id` 判定本机；同一内容至少保留一份；
   误选本机目录会被拒绝并计入失败列表。
3. **Linux 离线安装器不再被系统的坏状态拖死**：只解包真正缺失的包、不升级系统已有库；系统里存在半配置包或
   i386/amd64 版本不一致时，只警告并给出修复命令，不再中断安装；失败时打印「改了哪些包、怎么回退」。
   旧版→新版直接覆盖升级已验证：用户目录哈希不变、无残留文件。
4. **Windows 打包不再静默丢掉 Weasel 入口**：打 tag 发布时自编译失败即失败（`require` 模式），不再偷偷回退上游安装包；
   新增 `verify-artifacts` 断言产物内容（`rime.so` 是否含同步入口、`rime-sync-status`/`uninstall.sh` 是否可执行、
   `WeaselServer.exe` 里是否有「Rime 同步」）；回退时会在发布说明与更新提示里红字标注。
5. **离线依赖安装失败可见**：安装后若依赖没装成功，诊断页会显示红条并提供「查看日志 / 重试」。
6. **随包提供 `rime-sync-cli`**：fcitx5 配置页的「立即同步」按钮之前因缺这个二进制而静默无效，现在可用。
## v0.6.0 — 2026-10-24

1. **同步入口并入输入法**（终于不用单独开同步程序了）：
   - Linux：`fcitx5-configtool → 附加组件 → Rime` 配置页新增三个按钮——**打开同步面板**（直接进设置向导）、
     **立即同步**（后台跑一次 transport）、**查看同步状态**（桌面通知显示最近一次结果）；没装同步程序时点击不报错。
   - Windows：小狼毫**托盘右键菜单**与 **Weasel 设置窗口**各加「Rime 同步…」，点击直接进同步设置向导；
     找不到同步程序时弹「未安装 Rime 同步」而不是静默失败。
2. **同步预览（dry-run）**：同步页新增「预览同步」，先列出这次将**拉取 / 推送 / 冲突 / 删除 / 跳过**的文件与原因，
   确认无误再「按此执行」；预览全程只读，不写本地、不写远端、不改索引。只读策略下会提示“只会拉取”。
3. **真实 WebDAV 端到端测试进 CI**：每次 push/PR 都会用 rclone 起 WebDAV，跑两台设备的同步、冲突、
   `apply` 幂等与红线检查（`shared/` 下不得出现词库数据库），避免以后再出现“本地全绿、线上出事”。
4. **前端重构与静态约束**（内部质量）：`main.ts` 2681 行拆成 22 个单向依赖模块；非测试代码禁用
   `unwrap/expect`，避免个别错误直接把程序 panic 掉。
## v0.5.6 — 2026-10-23

本版把 0.5.4 / 0.5.5 未发布的修复一起带上（输入法设置页中英双语、localepurge 提示、`rime_dict_manager` 工作目录、僵尸进程与停止升级、锁重试），并新增：

1. **备份清理**：保留份数可配置（默认 10，可选 5/10/20/50，范围 1–100）；备份列表每行可「删除」（二次确认）；
   备份区新增「清理旧备份」。每次「立即合并生效」建完备份后按该份数自动清理最旧的。
2. **手动修改设备 ID**：设置页可直接编辑（`[a-z0-9-]`，≤64）；保存时可选「迁移远端目录」（先复制成功再删旧目录，失败回滚）
   或「仅本地修改」；「重新生成」同样走这套流程。
3. **与远端去重**（设置 → 维护）：先预览再执行，显示可释放空间：
   - 远端重复设备目录：按 `owner.json` 的 hostname+os 分组，保留本机 ID 或最新心跳，其余可删（逐文件递归删除，不依赖服务端集合删除）；
   - 远端重复快照：同一目录内按 sha256 只留最新；跨目录相同内容默认只提示，需手动勾选；
   - 本地重复备份：按内容指纹分组，只留最新。
4. **卸载时可选删除用户数据**：Linux `uninstall.sh --purge-data`（交互默认否，路径白名单校验，支持自定义 `UserDataDirPath`）；
   Windows 卸载弹窗询问（默认否），静默卸载需 `/PURGEDATA`；删除 `%APPDATA%\Rime` 与同步程序配置目录，失败只记日志不中止。
## v0.5.5 — 2026-10-23

修复「立即合并生效」在 Linux 上必然失败的问题：

1. **rime_dict_manager 工作目录**：该工具按「当前目录」读取 `installation.yaml` 与 `<schema>.userdb`，
   旧版只在父进程工作目录里运行（并仅设 `RIME_USER_DATA_DIR`），于是打开的是不存在/错误的 LevelDB，
   报 `Error opening db ... read-only`。现在 `--sync` 与 `-e/--export` 都在 `<rime_dir>` 内执行。
2. **僵尸进程误判**：`fcitx5 -d` 启动后父进程未 `wait()`，遗留 `comm=fcitx5` 的僵尸；旧判定只看
   `/proc/<pid>/comm`，把僵尸当成「仍在运行」，apply 恒超时。现在读取 `/proc/<pid>/stat` 的 state，
   跳过 `Z/X`，并在启动后 `try_wait` 回收子进程。
3. **停止输入法升级序列**：`fcitx5-remote -e` → 2s → `SIGTERM` → 3s → `SIGKILL`，超时报错带 `pid(state)`。
4. **锁错误重试**：`rime_dict_manager --sync` 遇到 `read-only`/`Error opening db`/`lock` 时每秒重试，最多 3 次。
## v0.5.4 — 2026-10-23

解决 localepurge 清掉翻译后输入法设置页变英文的问题：

1. **配置页标签中英双语**：`fcitx5-rime` 的五个设置项标签直接写成「中文 / English」，
   即使 `/usr/share/locale` 下的 `.mo` 被清理（localepurge、localepurge 类工具），页面仍然是中文；
   有语言包时仍显示对应语言。
2. **安装提示**：IME 包安装时若检测到 `localepurge`，postinst 会提示把 `zh_CN` 加入
   `/etc/locale.nopurge` 并重装，即可恢复纯中文目录。
## v0.5.3 — 2026-10-23

修掉两台 Windows 之间「配置不同步」的假冲突，并把安装/卸载统一成一个产品：

1. **忽略 customization 头**：`*.custom.yaml` 的三向比较只比 `patch`，Weasel 每次改设置重写的
   `modified_time`/`distribution_version` 不再制造冲突；只差元数据时直接采用远端内容并更新基线。
2. **冲突可解决**：冲突页新增「用远端覆盖本地」「全部用远端覆盖」；远端覆盖先 GET 成功再原子写本地、
   更新 index 基线，最后才删冲突副本，任一步失败都不动本地文件。「采用副本」注明等价于保留本地。
3. **冲突可见**：报告区列出冲突文件（最多 5 条），冲突页显示文件、冲突键（如 `patch.menu/page_size`）、
   本地/远端/副本大小。
4. **Windows 配置立即生效**：transport 改动过 `*.custom.yaml` 后自动执行 `WeaselDeployer /deploy`。
5. **统一卸载（Windows）**：安装时若本机原本没有小狼毫，记录所有权标记；卸载默认连 Weasel 一起静默卸载，
   原本已装的用户不受影响；`/KEEPWEASEL` 可保留输入法；`%APPDATA%\Rime` 始终保留。
6. **统一卸载（Linux）**：新增 `/usr/lib/rime-sync/uninstall.sh` 与桌面菜单「卸载 Rime 同步与输入法」，
   先卸同步、再卸自编译输入法，保留用户目录与配置；`install.sh --uninstall` 复用同一脚本。
## v0.5.2 — 2026-10-23

修复「同步显示成功但词库没合并」以及 Linux 输入法设置页：

1. **快照目录名改用 installation_id**：librime/小狼毫把快照写在 `sync/<installation_id>/`，而旧版用
   `config.device_id` 去找 `sync/<device_id>/`，目录不存在就静默跳过——这就是 `pushed=0`、远端没有
   快照、Linux 端始终打不出新词的原因。现在 push/pull 都用 `installation.yaml` 的 installation_id
   （缺失时回退 device_id），并兼容旧的 device_id 目录。
2. **合并加固**：apply 会轮询确认 fcitx5 真正退出（避免 LevelDB 锁），再 `rime_dict_manager --sync`
   → `rime_deployer --build` → 启回；合并前后各导出一次 userdb 并在结果里显示条目数。
3. **新快照提醒**：transport 拉到新的 `*.userdb.txt` 时结果区提示「请点『立即合并生效』」。
4. **fcitx5 配置页**：新增真正的「用户数据目录（留空 = 默认）」设置项（`UserDataDirPath`），原按钮
   改名「打开用户目录」；补 zh_CN 翻译，配置页在中文环境下完整显示中文。
5. **rime-sync 目录探测**：优先读取 fcitx5-rime 的 `UserDataDirPath`，让输入法与同步使用同一目录。
6. **Debian 安装冲突**：IME 包新增 `Conflicts/Replaces: rime-data, rime-prelude, rime-essay, rime-luna-pinyin`，
   修复 `apt install ./rime-inputmethod.deb` 报 `trying to overwrite /usr/share/rime-data/default.yaml`。
## v0.5.1 — 2026-10-22

修复 0.5.0 安装后「找不到同步设置」的两个打包缺陷：

1. **可执行位丢失**：离线安装器重打包同步 deb 时用 `chmod 0644` 抹平了所有文件，
   `/usr/bin/rime-sync-app` 变成不可执行，菜单项点了没反应。现在重打包会恢复
   `/usr/bin/*` 与 `/usr/lib/rime-sync/*` 为 0755。
2. **fcitx5 配置页未随包**：`Rime Sync` 的 addon（`rimesync.so` / `rimesync.conf` /
   `rime-sync-open-setup`）之前只为验证单独构建，没有进 IME deb。现在四个发行版的 IME
   包都会从源码编译并安装这个 addon，输入法设置里可直接编辑 WebDAV 参数。
## v0.5.0 — 2026-10-22

输入法不再是「打个包带上游 .deb」，而是**源码随仓库、每次一起编译**：

1. **源码 vendoring**：`third_party/librime`（1.17.0）、`third_party/fcitx5-rime`（5.0.16）、`third_party/weasel`（0.17.4）、
   `third_party/rime-data`（prelude/essay/luna-pinyin）随仓库走，每个目录有 FORK.md 记录上游 pin 与修改。
2. **四发行版离线安装器**：`rime-sync-installer_<ver>~{jammy,noble,bookworm,trixie}.run`，内含自编译 IME deb +
   同步 deb + 该发行版的依赖闭包；`--network none` 下四套全部实测通过（Loaded addon rime / deploy success / 候选「你好」）。
3. **安装顺序严格为输入法 → 同步程序**，安装器失败即停并保留日志；支持 `--uninstall`（先同步后输入法，保留用户数据）。
4. **fcitx5 配置页**：输入法设置里新增「Rime Sync」页，可直接编辑 WebDAV 地址/用户名/密码/远端目录/设备名，
   并有「打开 Rime 同步」按钮（ExternalOption）。
5. **Windows 自编译小狼毫**：CI 用 `third_party/weasel` 源码构建安装包，NSIS 在 PREINSTALL 阶段先装输入法
   （写 `install-ime-order.log` 作为顺序证据），再安装同步程序；失败可回退上游安装包。
6. **方案数据随包**：rime-prelude / rime-essay / luna_pinyin 打进 `/usr/share/rime-data`，装完即可输入。
## v0.4.0 — 2026-10-21

本轮把「输入法与同步」合成一个安装包，并去掉安装后自动弹窗：

1. **Linux 一体包（离线可装）**：deb 内置 fcitx5-rime 完整依赖闭包（82 个 .deb / 28 MB，来自 ubuntu 22.04），
   断网 `dpkg -i` 后台自动补齐输入法；输入法三件套放 `Recommends`，安装不阻塞（避免 dpkg 配置阶段的依赖死锁）。
2. **Windows 一体包**：NSIS 内置小狼毫（Weasel 0.17.4）安装包，安装时静默部署，随后创建「Rime 同步」与
   「同步设置」快捷方式；不再在安装完成后自动启动任何窗口。
3. **入口调整**：同步设置不再自动弹出，改为菜单入口 `rime-sync-app --setup`（Windows 同时放入 Weasel 程序组），
   带参数启动会直接进入同步设置向导，不带参数按普通启动。
4. **同步设置向导**：本机 Rime 目录/设备名、WebDAV 连接与测试（含写权限提示）、首次同步策略（默认 preview 只拉不推）、
   可选立即做一次首次同步并展示结果。
5. **Windows 黑框修复**：所有子进程（powershell 计量探测、WeaselDeployer 等）统一以 `CREATE_NO_WINDOW` 启动，
   后台静默运行，不再闪命令行窗口。
6. **发布流程**：CI 在构建前抓取 vendor 依赖与 Weasel 安装包，deb 产物做体积断言（< 20 MiB 直接失败），
   并新增 packaging 脚本守卫 job。
## v0.3.0 — 2026-10-21

本轮聚焦「首次接入安全 + 可运维性 + 桌面常驻」：

1. **首同步保护（默认安全，粘性）**：本地无 `index.json` 且远端已有别的设备或 `shared/` 数据时判定首次接入；
   `first_sync_policy` 默认 `preview`，只要不是 `normal`，每一轮 transport 都只拉不推——不建远端目录、
   不写 `owner.json`、不碰 `shared/`、不上传任何文件，同时照常拉取设备快照与 shared 文件。
   只有用户在界面点「切换为双向同步」才写入 `normal`。
2. **同步历史**：每次 transport/apply 追加 `history.jsonl`（ts/mode/ok/pulled/pushed/merged/conflicts/duration_ms/backup_id/error），
   上限 500 条；新增 `history --limit N --json`。
3. **日志轮转**：`log.txt` 单文件 5 MB，保留 3 份；轮转失败不影响同步。
4. **配置 schema v2**：新增 `schema_version` 与 `first_sync_policy`；v1（无版本号/明文密码）自动迁移到钥匙串，迁移幂等，
   遇到更高版本保留原文件并报错，不静默丢配置。
5. **词条级冲突统计**：对 `*.userdb.txt` 冲突解析两侧行，上报 `only_local` / `only_remote` / `both_changed` 与样本，
   只统计不自动合并；统计失败降级为 null 且不阻断同步。
6. **更新签名闭环**：`tauri.conf.json` 写入 minisign 公钥、updater 指向 GitHub Releases `latest.json`；
   私钥存 `.secrets/`（已 gitignore），`docs/RELEASE-SIGNING.md` 说明 GitHub Secrets 配置与轮换。
7. **托盘常驻 + 开机自启**：托盘菜单「打开主窗口 / 立即搬运 / 立即合并生效 / 查看冲突 / 退出」；关窗默认隐藏到托盘；
   接入 autostart 插件（Linux 写 `~/.config/autostart`，Windows 写注册表）。
8. **发布瘦身**：release profile 打开 `strip` / `lto=thin` / `codegen-units=1` / `opt-level=s`。
9. **测试**：119 个测试通过（109 单元 + first_sync + 5 read_only + spec2_flow），fmt/clippy 干净；
   前端补 zh/en 文案与只读提示。
## v0.2.0 — 2026-10-20

本轮的 14 项改进：

1. **备份与回滚**：新增 `BackupManager`——apply 前自动把 `sync/**/*.userdb.txt`、`*.custom.yaml`、
   `*.dict.yaml`、词表 `*.txt`、`user.yaml` 复制到 `<app_data>/backups/<时间戳>/` 并写入
   `manifest.json`（相对路径、sha256、大小、原因、版本）；备份失败则中止 apply，宁可不合并。
   支持 `list` / `restore` / `prune`（默认保留 10 份），restore 前自动再建一份 `pre-restore` 备份。

2. **密码进系统钥匙串**：新增 `SecretStore`（`KeyringStore` + 0600 `FileStore` 兜底）与
   `password_ref` 字段，配置里不再持久化明文密码；旧配置的 `password` 字段会自动迁移进钥匙串并从
   JSON 删除；`--dav-pass` 只在内存使用。

3. **残留锁回收**：锁文件记录 `{pid, hostname, started_at, boot_id}`；pid 不存活、boot_id/启动时间不匹配
   或超过 6 小时即判定为 stale 并接管，RAII `LockGuard` 保证释放，`status` 展示锁持有者。

4. **YAML 段落级合并**：`merge_yaml_patch` 对 `*.custom.yaml` 的 `patch:` 映射做 key 级三向合并，
   不同 key 互不干扰；同 key 三方差分冲突保留本地并生成 conflict 副本；YAML 非法或结构异常时
   自动回落文件级合并，不中断同步。

5. **事件驱动同步**：新增 `watch`（notify + 3s 防抖），忽略 `build/`、`*.userdb/`、`*.bin` 与自身写入
   造成的回声；只发事件不直接调 engine，避免与手动操作抢锁。

6. **服务商预设与 AList 适配**：`Preset`（AList / 坚果云 / Nextcloud / 群晖 / 自定义）提供默认路径、
   URL 归一化（补 scheme、去尾斜杠、AList 补 5244 端口与 `/dav`）和按响应头识别 AList。

7. **WebDAV 写权限探测**：`check_write()` 依次 MKCOL → PUT 1 字节 → DELETE，失败时把 HTTP 状态码
   翻译成中文可执行建议（403 + AList → 提示勾选 Webdav Read / Webdav Manage 与存储写入/删除权限）。

8. **CLI 新命令**：`backup`、`backups`、`rollback <id>`、`watch`、`probe`，全部支持 `--json`，
   便于脚本化与排障。

9. **开源许可**：根目录补齐 MIT `LICENSE`（版权人 li2966068，2026），Cargo / Tauri 元数据统一为 MIT。

10. **CI 拆分与门禁**：删除旧的 `build.yml`，新增 `ci.yml`（push / PR / 手动）——`cargo fmt --check`、
    `cargo clippy -D warnings`、`cargo test --workspace`、`tsc --noEmit`；并把浏览器测试收进同一份门禁：
    **rclone E2E job**（本地起 WebDAV → 首轮上传 → 第二轮字节级幂等 → 断言远端无 `*.userdb`、
    `shared/` 下无 `*.userdb.txt`、无 `*.bin`）。

11. **发布流水线**：新增 `release.yml`（tag `v*` 或手动指定 tag）——Linux 出 `deb` + `AppImage`，
    Windows 出 `NSIS` + **便携版 zip**；publish job 汇总产物、生成 updater 用的 `latest.json`
    （从各 `*.sig` 提取签名与下载地址）并上传 Release。

12. **打包配置**：Windows `webviewInstallMode = embedBootstrapper`（内嵌 WebView2 引导器，内网/离线可装）；
    Linux `targets = ["deb", "appimage"]`；`createUpdaterArtifacts = true` + updater 占位 endpoints/pubkey；
    `packaging/portable-zip.ps1` 生成解压即用的便携包。

13. **fcitx5 配置页**：新增 `packaging/fcitx5-addon/`（C++ + CMake）——Qt 配置页含 WebDAV 地址 / 用户名 /
    密码 / 远端目录与「打开 Rime 同步」按钮，直接读写 `~/.config/rime-sync/config.json`（与主程序共享），
    并提供最小 fcitx5 addon 共享库。

14. **文档与安装体验**：README 重写（真实截图、AList 权限勾选表、两种模式、更新密钥替换步骤）；
    USER-GUIDE 新增 AList 章节与 403 权限排查；NSIS 钩子在开始菜单创建「Rime 同步」并尝试归入
    Weasel 程序组，卸载时清理快捷方式；同步更新 `packaging/README.md` 的产物清单与体积预期。

### 兼容性

- 配置格式向后兼容：旧 `config.json` 的明文 `password` 会在首次读取时迁移进钥匙串。
- 远端目录结构不变（`shared/` + `sync/<device_id>/`），v0.1.x 客户端可继续与 v0.2.0 互通。
- 红线不变：`*.userdb/` 目录永不同步。
