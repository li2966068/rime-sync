# Linux 安装包：输入法 + 同步工具（trixie，全量 / 非全量）

目标：在 Debian 13（trixie）上用一个文件装好「自编译 Rime 输入法 + rime-sync」。
0.6.6 起 **Linux 只发布 trixie**，且不再单独发「输入法本体」deb——两个 .run 都是
「输入法本体 + 同步工具」，区别只在**依赖层**：

| 资产 | 变体 | 依赖 | 安装行为 |
| --- | --- | --- | --- |
| `rime-sync-installer_<ver>.trixie.run` | 全量（full） | 内置依赖闭包（vendor/*.deb，约 170 MB） | 断网可装：先解包缺失依赖，再装 IME，再装 sync |
| `rime-sync-installer_<ver>.trixie.thin.run` | 非全量（thin） | 不打包，`apt-get` 从系统仓库装 | 先 `apt-get update` + fcitx5 运行库，再装 IME，再装 sync |

两个包用**同一套安装脚本**（`install.sh`），安装顺序都写死为
「先 IME，后 sync」，可重复执行（旧版直接覆盖升级，不需要先卸载）。

## 1. 产物与目录

~~~
build/deb/debian-13/rime-inputmethod_<appver>~trixie_amd64.deb     # 只作为 .run 的输入
packaging/linux/vendor/debian-13/{*.deb,VENDOR-INFO,SHA256SUMS,DEPS.txt}
packaging/linux/out/offline/debian-13/{rime-inputmethod_*.deb,rime-sync_*.deb,vendor/}
packaging/linux/out/offline/debian-13-thin/{rime-inputmethod_*.deb,rime-sync_*.deb}
packaging/linux/out/offline/install.sh
packaging/linux/out/rime-sync-installer_<appver>.trixie.run
packaging/linux/out/rime-sync-installer_<appver>.trixie.thin.run
~~~

| distro key | base image | suite |
| --- | --- | --- |
| debian-13 | debian:13 | trixie |

发行版仍是参数（`DISTRO_KEY` / `SUITE` / `suite_for`）：以后要加版本，
在 `build-linux.sh`、`build-offline-installer.sh`、`verify-offline.sh` 各补一条即可。

## 2. 构建

~~~bash
# 1) trixie 的输入法（容器内编译，约 5 分钟）
scripts/ime/build-linux.sh                 # = IMAGE=debian:13

# 2) 全量包需要的依赖闭包（约 170 MB）
APT_MIRROR=http://mirrors.aliyun.com scripts/ime/fetch-vendor-deps.sh debian-13

# 3) 两个安装器一起出（--full / --thin / --skip-deps 可只出其一）
SYNC_DEB=packaging/out/rime-sync_<ver>_amd64.deb packaging/linux/build-offline-installer.sh --both debian-13
~~~

`--thin`（别名 `--skip-deps`）不需要 `packaging/linux/vendor/`，因此断网、没抓闭包时也能出包；
`--full` 反之：没有 vendor 闭包直接报错（全量包不该假装能离线装）。

容器内完成：librime 1.17.0 + fcitx5-rime 5.0.16 编译、安装 rime 方案数据、Xvfb 下加载
addon、rime-smoke 断言「你好」候选，全部作为 build-info.txt / rime-smoke.log 留存。

## 3. 安装（目标机）

~~~bash
# 全量包（推荐给离线/内网机器）
sudo bash rime-sync-installer_<ver>.trixie.run

# 非全量包（机器能访问发行版仓库时；体积小很多）
sudo bash rime-sync-installer_<ver>.trixie.thin.run

# 目录形式（调试用）
sudo packaging/linux/out/offline/install.sh                      # 全量
sudo packaging/linux/out/offline/install.sh --payload packaging/linux/out/offline/debian-13-thin
sudo packaging/linux/out/offline/install.sh --uninstall
~~~

行为：

1. 检查 root（否则提示 sudo）。
2. 读 /etc/os-release 选安装集；本包只支持 Debian 13（trixie），其他发行版会明确提示。
3. 移除与自带 librime/数据冲突的发行版包（fcitx5-rime / librime1 / librime1t64 / librime-data）。
4. 依赖层（两种包在这里分叉）：
   * **全量**：记录 dpkg 快照，校验 SHA256SUMS 后只解包「状态非 ii」的依赖；
     已配置（ii）的包不升级（多架构系统上同名包任一架构版本不同时连缺失架构也不补）；
     只 `dpkg --configure <pkg:arch>...` 自己解包的包，失败时打印 dpkg 原文、多架构
     不一致清单、回滚命令；需要升级系统库时显式加 `--upgrade-vendor`。
   * **非全量**：`apt-get update` 后装 `fcitx5 fcitx5-modules`，再由
     `apt-get install ./rime-inputmethod_*.deb` / `./rime-sync_*.deb` 解析各包的 Depends。
     **断网或仓库不可用时立即停止**（退出码 3），打印手动步骤并指向全量包：
     ~~~
     ---- 非全量包（thin）需要系统仓库或联网 ----
     离线机器请改用全量包：rime-sync-installer_<ver>.trixie.run（内置依赖闭包，断网可装）
     有网络（或已挂本地仓库）时的手动步骤：
       sudo apt-get update
       sudo apt-get install -y --no-install-recommends fcitx5 fcitx5-modules
       sudo apt-get install -y --no-install-recommends rime-inputmethod_*.deb
       sudo apt-get install -y --no-install-recommends rime-sync_*.deb
       sudo apt --fix-broken install
     ~~~
5. 严格顺序：先 IME，再 sync；任一步失败立即停止（全量用 `dpkg -i`，非全量用 `apt-get install`）。
   旧版已装时直接覆盖升级，不需要先卸载。
6. 不启动应用、不写 autostart；结束提示「安装完成，输入法设置或应用菜单里打开同步设置」。
7. 日志：/var/log/rime-sync/install.log，安装顺序时间戳：/var/log/rime-sync/install-steps.log。
   步骤名在两种包下分别是 `dpkg -i <deb>` / `apt install <deb>`，顺序断言只看先后。

卸载（--uninstall）先卸 rime-sync、再卸 rime-inputmethod，保留 ~/.local/share/fcitx5/rime 与 ~/.config/rime-sync。

## 4. 验收

~~~bash
# 两种包都验：full 断网装成功；thin 断网必须明确失败、联网必须装成功
APT_MIRROR=http://mirrors.aliyun.com packaging/linux/verify-offline.sh
packaging/linux/verify-offline.sh --variant full
packaging/linux/verify-offline.sh --variant thin --skip-online
# 安装器行为矩阵（每个用例一个全新容器，--network none 真离线）
packaging/linux/test/install-matrix.sh        # normal / broken-unrelated / multiarch /
                                              # prebroken-multiarch / upgrade / single-arch
~~~

断言（每种变体各跑一遍）：

- dpkg-query：rime-inputmethod / rime-sync 均为 install ok installed；
- 安装日志出现 `安装包类型: 全量` / `安装包类型: 非全量` 的横幅；
- /usr/share/rime-data 包含 default.yaml / essay.txt / luna_pinyin.schema.yaml；
- rime_deployer --build 退出码 0，生成 3 个 build/*.bin；
- rime-smoke 输出 deploy: success 与候选「你好」；
- Xvfb + fcitx5 日志出现 Loaded addon rime，无 deploy failure，并生成 build/*.bin；
- /var/log/rime-sync/install-steps.log 证明先 IME 后 sync；
- thin 断网安装：退出码非 0，日志含「非全量包（thin）需要系统仓库或联网」并指向全量包名。

证据保存在 build/evidence/debian-13/{full-offline-install.log,thin-offline-install.log,
thin-online-install.log,sizes.txt}，体积汇总在 build/evidence/SIZES.txt。

## 5. 设计说明与偏离

- **一个包 = IME + 同步工具**：不再发「输入法本体」deb；IME deb 只作为 .run 的载荷存在
  （verify-artifacts 会从 thin .run 里解出来做入口断言）。
- **方案数据直接进 IME deb**：不再依赖发行版 librime-data，也不需要额外的数据包。
- **default.yaml 只保留随包方案**：librime deployer 把每个缺失 schema 计为失败，
  带着未随包的 schema 会让整次 deploy 失败。
- **依赖闭包包含 fcitx5（守护进程）**：只排除 librime1/librime1t64/librime-data/fcitx5-rime；
  没有守护进程 addon 无处加载。非全量包因此单独 `apt-get install fcitx5 fcitx5-modules`。
- **重打包 rime-sync deb 的维护脚本**：旧 postinst 不认识 rime-inputmethod（虚包 Provides），
  会排队用 apt 装发行版 fcitx5-rime，反过来把我们的 IME 卸掉。新版先检查 rime-inputmethod；
  只替换维护脚本，不改应用内容与版本号。
- **不修改版本号**：IME 用 tauri.conf.json 的版本，只加 ~<suite> 后缀区分发行版。

## 6. 已知风险

- 目标机若比构建基线更旧（apt 当日候选），全量包里的库可能比系统新：安装器默认只补缺失、
  不升级已有库，必要时手动 `--upgrade-vendor`。
- 非全量包依赖目标机仓库可用；仓库里 fcitx5 版本与构建基线差距过大时 IME 可能起不来
  （IME 的 Depends 已声明 `libfcitx5core7 (>= 5.0.6)` 等下限，apt 会拒绝装到更老的系统上）。
- Debian 13 用 librime1t64：IME deb 的 Conflicts 同时列出 librime1 与 librime1t64
  （不存在的包名无副作用）；目标机若已装 t64 包，安装器会先 purge。

## 7. 体积（实测，2026-10-21；0.6.6 结构变化后 full 与旧 trixie 包同量级）

| 变体 | IME deb | 依赖 .deb 数 | 依赖闭包 | 目录集 | .run |
| --- | --- | --- | --- | --- | --- |
| 全量 (trixie) | 3,254,204 B (3.1 MiB) | 265 | 178,747,733 B (171 MiB) | 184,518,892 B (176 MiB) | 184,530,479 B (176 MiB) |
| 非全量 (trixie) | 3,254,204 B (3.1 MiB) | 0 | — | ≈ 5.6 MiB | 5,785,701 B (5.5 MiB) |

thin 只有两个 deb（IME 3.1 MiB + sync 2.4 MiB）+ 两个安装脚本，压缩后 5.5 MiB；
断言区间 5–60 MiB，且必须小于全量包的一半。

构建耗时（单机 4 核，docker 缓存镜像层后）：容器内约 296 s（librime 192 s、fcitx5-rime 6 s）；
full + thin 组装约 30–60 s（闭包拷贝占大头）。

### 验收结果（0.6.5 基线，docker run --network none，全部通过）

| 发行版 | 离线安装 | 两包 installed | Loaded addon rime | deploy success | 候选「你好」 | 安装顺序 |
| --- | --- | --- | --- | --- | --- | --- |
| debian-13 | PASS | PASS | PASS | PASS | PASS | IME 18:08:09 → sync 18:08:10 |

0.6.6 结构变更后的实测（2026-10-27，docker，同一次 `verify-offline.sh`）：

| 用例 | 结果 | 证据 |
| --- | --- | --- |
| full：`--network none` 安装 | PASS（两包 installed / Loaded addon rime / deploy success / 候选「你好」/ IME→sync 顺序） | `build/evidence/debian-13/full-offline-install.log` |
| thin：`--network none` 安装 | 按时失败（exit 3），日志含「非全量包（thin）需要系统仓库或联网」并指向全量包 | `build/evidence/debian-13/thin-offline-install.log` |
| thin：联网安装 | PASS（同上全部断言；步骤 0/2 apt 依赖 → 1/2 IME → 2/2 sync） | `build/evidence/debian-13/thin-online-install.log` |

> 上面的 full 用例用的是仓库里已快照的 vendor 闭包（对应 IME 0.4.0~trixie）；
> CI 里闭包是按当次 IME deb 现算的（`fetch-vendor-deps.sh debian-13`），不会出现
> 「闭包缺新依赖」的情况。用 0.5.2 的 IME deb 配旧闭包会报
> `libdouble-conversion3 / libpcre2-16-0 / libqt5core5t64` 缺失，这是快照问题。
