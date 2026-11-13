# Linux 安装包：输入法 + 同步工具（trixie，离线安装包 / 在线安装包）

目标：在 Debian 13（trixie）上用一个文件装好「自编译 Rime 输入法 + rime-sync」。
0.6.6 起 **Linux 只发布 trixie**，且不再单独发「输入法本体」deb——两个 .run 都是
「输入法本体 + 同步工具」，区别只在**依赖层**：

| 资产 | 变体 | 依赖 | 安装行为 |
| --- | --- | --- | --- |
| `rime-sync-installer_<ver>.trixie.offline.run` | 离线安装包（offline） | 内置依赖闭包（vendor/*.deb，约 170 MB） | 断网可装：先解包缺失依赖，再装 IME，再装 sync |
| `rime-sync-installer_<ver>.trixie.online.run` | 在线安装包（online） | 不打包，`apt-get` 从系统仓库装 | 先 `apt-get update` + fcitx5 运行库，再装 IME，再装 sync |

> 两个包都自带完整输入法，区别只在依赖库：离线包把依赖打包在内（断网可装），在线包安装时联网下载依赖。

两个包用**同一套安装脚本**（`install.sh`），安装顺序都写死为
「先卸载旧版 → 装 IME → 装 sync」，可重复执行。升级语义（0.6.12 起）：
**先卸载旧版，再全新安装**——旧版 rime-inputmethod / rime-sync 被 `purge`，
用户配置与数据（~/rime、~/.config/fcitx5/rime、~/.local/share/fcitx5/rime、
~/.config/com.rimesync.app）一律保留；新版安装时配置模板只补缺失，已存在的一律沿用
（不覆盖、不改名、不备份）。详见第 3 节。

## 1. 产物与目录

~~~
build/deb/debian-13/rime-inputmethod_<appver>~trixie_amd64.deb     # 只作为 .run 的输入
packaging/linux/vendor/debian-13/{*.deb,VENDOR-INFO,SHA256SUMS,DEPS.txt}
packaging/linux/out/offline/debian-13/{rime-inputmethod_*.deb,rime-sync_*.deb,vendor/}
packaging/linux/out/offline/debian-13-online/{rime-inputmethod_*.deb,rime-sync_*.deb}
packaging/linux/out/offline/install.sh
packaging/linux/out/rime-sync-installer_<appver>.trixie.offline.run
packaging/linux/out/rime-sync-installer_<appver>.trixie.online.run
~~~

| distro key | base image | suite |
| --- | --- | --- |
| debian-13 | debian:13 | trixie |

两个 `.run` 里的 `rime-sync_<appver>_amd64.deb` 自带命令行工具
`/usr/bin/rime-sync-cli`（fcitx5 配置页的「立即同步」按钮也调它）。它由打包时 Tauri 的
sidecar（`bundle.externalBin`）写入，**deb 里只有这一份**：0.6.23 起
`bundle.linux.deb.files` 里同名的映射已删除，否则两个来源会往同一个路径写字。
`packaging/linux/prepare-sync-deb.sh` 重新打包维护脚本时会断言该路径恰好一份。

发行版仍是参数（`DISTRO_KEY` / `SUITE` / `suite_for`）：以后要加版本，
在 `build-linux.sh`、`build-offline-installer.sh`、`verify-offline.sh` 各补一条即可。

## 2. 构建

~~~bash
# 1) trixie 的输入法（容器内编译，约 5 分钟）
scripts/ime/build-linux.sh                 # = IMAGE=debian:13

# 2) 离线安装包需要的依赖闭包（约 170 MB）
APT_MIRROR=http://mirrors.aliyun.com scripts/ime/fetch-vendor-deps.sh debian-13

# 3) 两个安装器一起出（--offline / --online 可只出其一；--full / --thin / --skip-deps 是兼容旧写法，仍可用）
SYNC_DEB=packaging/out/rime-sync_<ver>_amd64.deb packaging/linux/build-offline-installer.sh --both debian-13
~~~

`--online`（兼容旧写法：`--thin` / `--skip-deps`）不需要 `packaging/linux/vendor/`，因此断网、没抓闭包时也能出包；
`--offline`（兼容旧写法：`--full`）反之：没有 vendor 闭包直接报错（离线安装包不该假装能离线装）。

容器内完成：librime 1.17.0 + fcitx5-rime 5.0.16 编译、安装 rime 方案数据、Xvfb 下加载
addon、rime-smoke 断言「你好」候选，全部作为 build-info.txt / rime-smoke.log 留存。

## 3. 安装（目标机）

~~~bash
# 离线安装包（推荐给离线/内网机器）
sudo bash rime-sync-installer_<ver>.trixie.offline.run

# 在线安装包（机器能访问发行版仓库时；体积小很多）
sudo bash rime-sync-installer_<ver>.trixie.online.run

# 目录形式（调试用）
sudo packaging/linux/out/offline/install.sh                      # 离线安装包
sudo packaging/linux/out/offline/install.sh --payload packaging/linux/out/offline/debian-13-online
sudo packaging/linux/out/offline/install.sh --uninstall
~~~

行为：

1. 检查 root（否则提示 sudo）。
2. 读 /etc/os-release 选安装集；本包只支持 Debian 13（trixie），其他发行版会明确提示。
3. **UNINSTALL-OLD：先卸载旧版**（0.6.12 起，与 Windows 安装器一致）。
   `dpkg-query` 检测到 rime-inputmethod / rime-sync 已装（含 rc 残留）时，用
   `apt-get purge -y` 卸掉它们（失败退回 `dpkg --purge --force-depends`），再全新安装；
   purge 失败只警告，不打断安装。两个包都**不拥有**用户数据目录——安装器会先检查
   包的文件清单里没有任何用户目录路径，purge 后再逐条确认这些目录仍在：
   ~/rime、~/.config/fcitx5/rime、~/.local/share/fcitx5/rime、~/.config/com.rimesync.app
   日志里的阶段名为 `UNINSTALL-OLD`。fcitx5 若仍在运行，会提示重新登录桌面或
   在图形会话内执行 `systemctl --user restart fcitx5`。
4. 移除与自带 librime/数据冲突的发行版包（fcitx5-rime / librime1 / librime1t64 / librime-data）。
5. 依赖层（两种包在这里分叉）：
   * **离线安装包**：记录 dpkg 快照，校验 SHA256SUMS 后只解包「状态非 ii」的依赖；
     已配置（ii）的包不升级（多架构系统上同名包任一架构版本不同时连缺失架构也不补）；
     只 `dpkg --configure <pkg:arch>...` 自己解包的包，失败时打印 dpkg 原文、多架构
     不一致清单、回滚命令；需要升级系统库时显式加 `--upgrade-vendor`。
   * **在线安装包**：`apt-get update` 后装 `fcitx5 fcitx5-modules`，再由
     `apt-get install ./rime-inputmethod_*.deb` / `./rime-sync_*.deb` 解析各包的 Depends。
     **断网或仓库不可用时立即停止**（退出码 3），打印手动步骤并指向离线安装包：
     ~~~
     ---- 在线安装包（online）需要系统仓库或联网 ----
     离线机器请改用离线安装包：rime-sync-installer_<ver>.trixie.offline.run（内置依赖闭包，断网可装）
     有网络（或已挂本地仓库）时的手动步骤：
       sudo apt-get update
       sudo apt-get install -y --no-install-recommends fcitx5 fcitx5-modules
       sudo apt-get install -y --no-install-recommends rime-inputmethod_*.deb
       sudo apt-get install -y --no-install-recommends rime-sync_*.deb
       sudo apt --fix-broken install
     ~~~
6. 严格顺序：先 IME，再 sync；任一步失败立即停止（离线安装包用 `dpkg -i`，在线安装包用 `apt-get install`）。
   因为旧版刚被卸载，这里一定走全新安装（dpkg.log 里是 install，不是 upgrade）。
7. 配置模板**缺失才补**：`/usr/share/rime-sync/defaults/default.custom.yaml` 与
   `defaults/schemas/*.custom.yaml` 只在用户目录缺同名文件时写入；已存在的一律沿用
   （不覆盖、不改名、不备份）。日志逐文件记 `seed file=<name> result=new|reuse|skip`。
   用户词库、词表、自定义词典全程不动。
8. 不启动应用、不写 autostart；结束提示「安装完成，输入法设置或应用菜单里打开同步设置」。
9. 日志：/var/log/rime-sync/install.log，安装顺序时间戳：/var/log/rime-sync/install-steps.log。
   步骤名在两种包下分别是 `dpkg -i <deb>` / `apt install <deb>`；升级时还应有
   `UNINSTALL-OLD rime-inputmethod rime-sync`，且排在安装步骤之前。

卸载（--uninstall）先卸 rime-sync、再卸 rime-inputmethod，默认保留全部用户数据
（~/rime、~/.config/fcitx5/rime、~/.local/share/fcitx5/rime、~/.config/com.rimesync.app、
~/.config/rime-sync）；只有显式 `--purge-data` 才删除，且删除前会列出全部绝对路径。

## 4. 验收

~~~bash
# 两种包都验：离线安装包断网装成功；在线安装包断网必须明确失败、联网必须装成功
APT_MIRROR=http://mirrors.aliyun.com packaging/linux/verify-offline.sh
packaging/linux/verify-offline.sh --variant offline        # 新写法；旧写法 --variant full 仍可用
packaging/linux/verify-offline.sh --variant online --skip-online   # 新写法；旧写法 --variant thin 仍可用
# 安装器行为矩阵（每个用例一个全新容器，--network none 真离线）
packaging/linux/test/install-matrix.sh        # normal / broken-unrelated / multiarch /
                                              # prebroken-multiarch / upgrade-fresh / single-arch
packaging/linux/test/install-matrix.sh upgrade-fresh   # 先装旧版，再装新版
~~~

断言（每种变体各跑一遍）：

- dpkg-query：rime-inputmethod / rime-sync 均为 install ok installed；
- 安装日志出现 `安装包类型:` 横幅，标明离线安装包 / 在线安装包；
- /usr/share/rime-data 包含 default.yaml / essay.txt / luna_pinyin.schema.yaml；
- rime_deployer --build 退出码 0，生成 3 个 build/*.bin；
- rime-smoke 输出 deploy: success 与候选「你好」；
- Xvfb + fcitx5 日志出现 Loaded addon rime，无 deploy failure，并生成 build/*.bin；
- /var/log/rime-sync/install-steps.log 证明先 IME 后 sync；
- 升级用例（upgrade-fresh）：旧版被 purge（dpkg.log 是 remove/install，不是 upgrade）、
  用户文件 sha256 未变、缺失模板补回（result=new）、已有模板沿用（result=reuse）、
  日志有 UNINSTALL-OLD 阶段；
- 在线安装包断网安装：退出码非 0，日志含「需要系统仓库或联网」的提示并指向离线安装包文件名。

证据保存在 build/evidence/debian-13/ 下（具体文件名以 verify-offline.sh 实际输出为准），体积汇总在 build/evidence/SIZES.txt。

## 5. 设计说明与偏离

- **一个包 = IME + 同步工具**：不再发「输入法本体」deb；IME deb 只作为 .run 的载荷存在
  （verify-artifacts 会从在线安装包 .run 里解出来做入口断言）。
- **方案数据直接进 IME deb**：不再依赖发行版 librime-data，也不需要额外的数据包。
- **default.yaml 只保留随包方案**：librime deployer 把每个缺失 schema 计为失败，
  带着未随包的 schema 会让整次 deploy 失败。
- **依赖闭包包含 fcitx5（守护进程）**：只排除 librime1/librime1t64/librime-data/fcitx5-rime；
  没有守护进程 addon 无处加载。在线安装包因此单独 `apt-get install fcitx5 fcitx5-modules`。
- **重打包 rime-sync deb 的维护脚本**：旧 postinst 不认识 rime-inputmethod（虚包 Provides），
  会排队用 apt 装发行版 fcitx5-rime，反过来把我们的 IME 卸掉。新版先检查 rime-inputmethod；
  只替换维护脚本，不改应用内容与版本号。
- **不修改版本号**：IME 用 tauri.conf.json 的版本，只加 ~<suite> 后缀区分发行版。
- **升级 = 卸载旧版 + 全新安装**（0.6.12 起）：每个新版本都先 purge 旧版
  rime-inputmethod / rime-sync，再全新安装；用户配置与数据不在包的文件清单里，
  所以 purge 不会删它们。新版只补缺失的配置模板（`seed file=<name> result=new|reuse`），
  想强制刷新某个模板：删掉用户目录里对应文件再装一次。

## 6. 已知风险

- 目标机若比构建基线更旧（apt 当日候选），离线安装包里的库可能比系统新：安装器默认只补缺失、
  不升级已有库，必要时手动 `--upgrade-vendor`。
- 在线安装包依赖目标机仓库可用；仓库里 fcitx5 版本与构建基线差距过大时 IME 可能起不来
  （IME 的 Depends 已声明 `libfcitx5core7 (>= 5.0.6)` 等下限，apt 会拒绝装到更老的系统上）。
- Debian 13 用 librime1t64：IME deb 的 Conflicts 同时列出 librime1 与 librime1t64
  （不存在的包名无副作用）；目标机若已装 t64 包，安装器会先 purge。

## 7. 体积（近似值；历史实测 2026-10-21，0.6.6 结构变化后量级不变）

| 变体 | IME deb | 依赖 .deb 数 | 依赖闭包 | 目录集 | .run |
| --- | --- | --- | --- | --- | --- |
| 离线安装包 (trixie) | 约 3.1 MiB | 约 265 | 约 170 MiB | 约 200 MiB | 约 200 MiB |
| 在线安装包 (trixie) | 约 3.1 MiB | 0 | — | 约 35 MiB | 约 35 MiB |

> 两个包都自带完整输入法，区别只在依赖库：离线包把依赖打包在内（断网可装），在线包安装时联网下载依赖。

表中体积均为**近似值**（量级参考，精确字节数随依赖闭包与 IME deb 变化）；断言阈值：
离线安装包 ≥ 100 MiB，在线安装包 5–60 MiB 且必须小于离线安装包的一半。

在线安装包只有两个 deb（IME 约 3.1 MiB + sync 约 2.4 MiB）+ 两个安装脚本，压缩后约 35 MiB（近似值）。

构建耗时（单机 4 核，docker 缓存镜像层后）：容器内约 296 s（librime 192 s、fcitx5-rime 6 s）；
离线安装包 + 在线安装包组装约 30–60 s（闭包拷贝占大头）。

### 验收结果（0.6.5 基线，docker run --network none，全部通过）

| 发行版 | 离线安装 | 两包 installed | Loaded addon rime | deploy success | 候选「你好」 | 安装顺序 |
| --- | --- | --- | --- | --- | --- | --- |
| debian-13 | PASS | PASS | PASS | PASS | PASS | IME 18:08:09 → sync 18:08:10 |

0.6.6 结构变更后的实测（2026-10-27，docker，同一次 `verify-offline.sh`）：

| 用例 | 结果 | 证据 |
| --- | --- | --- |
| 离线安装包：`--network none` 安装 | PASS（两包 installed / Loaded addon rime / deploy success / 候选「你好」/ IME→sync 顺序） | build/evidence/debian-13/ 下对应日志 |
| 在线安装包：`--network none` 安装 | 按时失败（exit 3），日志含「需要系统仓库或联网」的提示并指向离线安装包 | build/evidence/debian-13/ 下对应日志 |
| 在线安装包：联网安装 | PASS（同上全部断言；步骤 0/2 apt 依赖 → 1/2 IME → 2/2 sync） | build/evidence/debian-13/ 下对应日志 |

> 上面的离线安装包用例用的是仓库里已快照的 vendor 闭包（对应 IME 0.4.0~trixie）；
> CI 里闭包是按当次 IME deb 现算的（`fetch-vendor-deps.sh debian-13`），不会出现
> 「闭包缺新依赖」的情况。用 0.5.2 的 IME deb 配旧闭包会报
> `libdouble-conversion3 / libpcre2-16-0 / libqt5core5t64` 缺失，这是快照问题。

---

## 8. 开机自启动（0.6.7）

安装完成后在设置页「开机自启动」一键开启，写的是**用户级**条目，不需要 root：

- 文件：`~/.config/autostart/rime-sync.desktop`，权限 0644，内容含
  `Type=Application`、`Name=Rime 同步`、`Exec="<路径>" --autostart`、`Icon=rime-sync`、`Terminal=false`。
- 路径：deb 安装为 `/usr/bin/rime-sync-app`；AppImage 运行时取 `$APPIMAGE` 的真实路径
  （`current_exe()` 指向的临时挂载点重启即失效，不能写进条目）。
- `--autostart` 启动时只驻留托盘、不显示主窗口；同步仍按设置页的搬运间隔执行。
- 关闭开关会删除该文件（同时清理旧版本可能留下的 `rime-sync-app.desktop`）。
- `rime-sync-uninstall.desktop` 不清理用户目录里的自启条目，需要时删掉上面的文件即可。
- **AppImage 移动/改名后必须重新开启一次**，否则条目指向不存在的旧路径；设置页会给出提示。

---

## 9. 预编译 build 产物兜底（0.6.15）

### 9.1 什么时候才用得上

有些机器上 rime 部署永远不产出 `build/`：Windows 上实测
`WeaselDeployer /deploy` 返回 0 但什么都不写；Linux 上同样会被用户目录权限、
librime 版本、插件缺失搞成「部署静默失败」。这种机器上用户目录里没有
`build/luna_pinyin_simp.schema.yaml`，输入法装好了也打不出字。

0.6.15 起 `rime-inputmethod` 直接带一份**别处已经部署好的** `build/`：

| 位置 | 内容 |
|---|---|
| `/usr/share/rime-sync/prebuilt/build/` | 保留方案（`luna_pinyin_simp` / `wubi_pinyin` / `wubi86`）的 `*.schema.yaml` + `*.bin`、依赖方案（`luna_pinyin`、`stroke`、`pinyin_simp`）的产物、`default.yaml` |
| `/usr/share/rime-sync/prebuilt/config/` | 生成时用的 `default.custom.yaml`（`schema_list` = 这 3 套）+ 3 个 `<schema>.custom.yaml` |
| `/usr/share/rime-sync/prebuilt/MANIFEST.json` | 清单（大小 / sha256 / 体积 / rime 版本） |

判据（与 Windows 安装器一致）：`~/.local/share/fcitx5/rime/build/luna_pinyin_simp.schema.yaml`
存在，且文件里同时有 `rimesync_vnum` 与 `rimesync_punct`。

### 9.2 谁在什么时候铺

* `rime-sync` 的 `postinst`（`dpkg --configure` 阶段，`install.sh` 装两个 deb 时也会走到）；
* 离线 `.run` 安装器的 `install.sh`（两个 deb 装完之后再兜一次底）。

两边逻辑相同：

1. 判据已满足 → 沿用用户目录里的产物（不覆盖、不备份、不改名）；
2. 否则把 `prebuilt/build/*` 拷到 `~/.local/share/fcitx5/rime/build/`，
   `prebuilt/config/*.yaml` 拷到 `~/.local/share/fcitx5/rime/` —— **逐个文件，存在即跳过**；
3. 复查判据；仍不满足时**只对 build/ 覆盖重铺一遍**——用户目录里可能本来就有旧的
   `build/luna_pinyin_simp.schema.yaml`（没有插件标记），「存在即跳过」会把它留下，
   兜底就白做了。`build/` 是部署器随时重写的派生数据，覆盖它安全；
   `config/*.yaml` 是用户配置，**绝不参与这一遍**。日志里记 `escalated=1`；
4. 复查仍失败只告警、不影响安装退出码；
5. 结果写 `/var/log/rime-sync/prebuilt.log`（`ok source=prebuilt copied= kept= escalated=`），
   屏幕上也有一行 `预编译产物：…`。

文件属主会 chown 给真实登录用户（`RIMESYNC_TARGET_USER` / `SUDO_USER` / `PKEXEC_UID` /
`logname` / 唯一普通用户，与 `install-warning.json` 用的是同一套解析）。

### 9.3 关闭 / 强制覆盖

| 目的 | 做法 |
|---|---|
| 关闭兜底 | `sudo RIMESYNC_PREBUILT=0 bash install.sh …` 或 `sudo RIMESYNC_PREBUILT=0 dpkg -i rime-inputmethod_*.deb rime-sync_*.deb` |
| 强制覆盖已有产物 | `sudo RIMESYNC_PREBUILT=force bash install.sh …` |
| 手工铺一次（不重装） | 用桌面账号：`mkdir -p ~/.local/share/fcitx5/rime/build && cp -a /usr/share/rime-sync/prebuilt/build/. ~/.local/share/fcitx5/rime/build/`，需要配置再 `cp -n /usr/share/rime-sync/prebuilt/config/*.yaml ~/.local/share/fcitx5/rime/`，然后 `fcitx5-remote -r` |
| 不要这份产物 | `rm -rf ~/.local/share/fcitx5/rime/build/luna_pinyin.* ~/.local/share/fcitx5/rime/build/wubi86.*` 后重新部署 |

### 9.4 构建期校验

`scripts/ime/package-ime-deb.sh` 会把 `packaging/rime/prebuilt` 打进 IME deb，并在打包时断言：

* `prebuilt/build/<schema>.schema.yaml` 存在且含 `rimesync_vnum` / `rimesync_punct`；
* `config/default.custom.yaml` 里列的方案都在 `/usr/share/rime-data` 里（否则部署必失败）；
* 缺产物直接 `exit 1`（确实要构建不带产物的包才设 `ALLOW_NO_PREBUILT=1`）。

产物由 `scripts/ime/gen-prebuilt-build.py` 在构建期用 `rime_deployer` 真实部署生成，
体积 29.1 MB（预算 60 MB；保留清单是发布契约，超预算会直接报错，不会偷偷少带方案）。
校验：`scripts/ime/gen-prebuilt-build.py --check`（清单 / 标记 / 绝对路径 / 体积 / 打包接线，
不需要 docker；在 CI 里当门禁用）。

PC 上（Windows 安装包）用的是同一份产物，两边的判据与复制规则刻意保持一致，
改一边必须同时改另一边。

