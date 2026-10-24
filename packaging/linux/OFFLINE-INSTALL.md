# 离线安装：输入法 + 同步程序

目标：在 Debian 12/13 与 Ubuntu 22.04/24.04 上，不联网安装「自编译 Rime 输入法 + rime-sync」。

## 1. 产物与目录

~~~
build/deb/<distro>/rime-inputmethod_<appver>~<suite>_amd64.deb
packaging/linux/vendor/<distro>/{*.deb,VENDOR-INFO,SHA256SUMS,DEPS.txt}
packaging/linux/out/offline/<distro>/{rime-inputmethod_*.deb,rime-sync_*.deb,vendor/}
packaging/linux/out/offline/install.sh
packaging/linux/out/rime-sync-installer_<appver>~<suite>.run
~~~

| distro key | base image | suite | Debian version |
| --- | --- | --- | --- |
| ubuntu-22.04 | ubuntu:22.04 | jammy | 0.4.0~jammy |
| ubuntu-24.04 | ubuntu:24.04 | noble | 0.4.0~noble |
| debian-12 | debian:12 | bookworm | 0.4.0~bookworm |
| debian-13 | debian:13 | trixie | 0.4.0~trixie |

每个发行版单独编译：fcitx5 5.0（22.04）与 5.1（24.04/12/13）ABI 不兼容，不能复用产物。

## 2. 构建

~~~bash
# 四个发行版依次编译（每个约 4–6 分钟，首次还要下构建镜像）
APT_MIRROR=http://mirrors.aliyun.com ALL=1 scripts/ime/build-linux.sh

# 也可单独指定
IMAGE=ubuntu:24.04 scripts/ime/build-linux.sh

# 抓取每个发行版的运行期依赖闭包（约 130–179 MB/发行版）
APT_MIRROR=http://mirrors.aliyun.com scripts/ime/fetch-vendor-deps.sh

# 打包成目录集与自解压 .run
packaging/linux/build-offline-installer.sh
~~~

容器内完成：librime 1.17.0 + fcitx5-rime 5.0.16 编译、安装 rime 方案数据、Xvfb 下加载 addon、rime-smoke 断言「你好」候选，全部作为 build-info.txt / rime-smoke.log 留存。

## 3. 安装（目标机）

~~~bash
sudo bash rime-sync-installer_<ver>~jammy.run        # 推荐：自解压（从 Release 下载后没有可执行位，用 bash 调用）
# 或目录形式
sudo packaging/linux/out/offline/install.sh
sudo packaging/linux/out/offline/install.sh --payload /path/to/offline
sudo packaging/linux/out/offline/install.sh --uninstall
~~~

行为：

1. 检查 root（否则提示 sudo）。
2. 读 /etc/os-release 选对应目录；找不到时列出支持的发行版并询问是否改用联网 apt。
3. 移除与自带 librime/数据冲突的发行版包（fcitx5-rime / librime1 / librime1t64 / librime-data）。
4. 记录 dpkg 快照（/var/log/rime-sync/dpkg-before-install.txt + dpkg-l-before-install.txt），
   校验 SHA256SUMS 后只解包"状态非 ii"的依赖：
   * 已配置（ii）的包**不升级**（多架构系统上同名包任一架构版本不同时连缺失架构也不补）；
   * 只配置本次解包的包（`dpkg --configure <pkg:arch>...`），不再 `dpkg --configure -a`，
     系统里无关的半配置包不会被判成安装失败；
   * 需要升级系统库时显式加 `--upgrade-vendor`。
   失败时打印 dpkg 原文尾部、多架构版本不一致清单与修复命令（apt --fix-broken / dpkg --remove-architecture）。
5. 严格顺序：先 dpkg -i rime-inputmethod_*.deb，再 dpkg -i rime-sync_*.deb；任一步失败立即停止。
   旧版已装时直接覆盖升级，不需要先卸载。
6. 不启动应用、不写 autostart；结束提示「安装完成，输入法设置或应用菜单里打开同步设置」。
7. 日志：/var/log/rime-sync/install.log，安装顺序时间戳：/var/log/rime-sync/install-steps.log。

卸载（--uninstall）先卸 rime-sync、再卸 rime-inputmethod，保留 ~/.local/share/fcitx5/rime 与 ~/.config/rime-sync。

## 4. 验收

~~~bash
APT_MIRROR=http://mirrors.aliyun.com packaging/linux/verify-offline.sh
# 安装器行为矩阵（每个用例一个全新容器，--network none 真离线）
packaging/linux/test/install-matrix.sh        # normal / broken-unrelated / multiarch /
                                              # prebroken-multiarch / upgrade / single-arch
~~~

对每个发行版在 docker run --network none 下用 .run 安装，然后断言：

- dpkg-query：rime-inputmethod / rime-sync 均为 install ok installed；
- /usr/share/rime-data 包含 default.yaml / essay.txt / luna_pinyin.schema.yaml；
- rime_deployer --build 退出码 0，生成 3 个 build/*.bin；
- rime-smoke 输出 deploy: success 与候选「你好」；
- Xvfb + fcitx5 日志出现 Loaded addon rime，无 deploy failure，并生成 build/*.bin；
- /var/log/rime-sync/install-steps.log 证明先 IME 后 sync。

证据保存在 build/evidence/<distro>/offline-install.log，体积汇总在 build/evidence/SIZES.txt。

## 5. 设计说明与偏离

- **方案数据直接进 IME deb**：不再依赖发行版 librime-data，也不需要额外的数据包。
- **default.yaml 只保留 luna_pinyin**：librime deployer 把每个缺失 schema 计为 failure，带着未随包的 schema 会让整次 deploy 失败。
- **依赖闭包包含 fcitx5（守护进程）**：题目的排除名单写了 fcitx5，但没有它 addon 无处加载，离线安装就不可用；只排除了 librime1/librime1t64/librime-data/fcitx5-rime。不需要可用的话可用 ROOT_PKGS="" 重新抓。
- **重打包 rime-sync deb 的维护脚本**：旧 postinst 不认识 rime-inputmethod（虚包 Provides），会排队用 apt 装发行版 fcitx5-rime，反过来把我们的 IME 卸掉。新版 postinst 先检查 rime-inputmethod；只替换维护脚本，不改应用内容与版本号。
- **不修改版本号**：IME 用 tauri.conf.json 的 0.4.0，只加 ~<suite> 后缀区分发行版。

## 6. 已知风险

- Ubuntu 24.04 / Debian 13 的 fcitx5 5.1 必须重新编译，每个发行版增加约 5–8 分钟构建时间。
- Debian 13 / Ubuntu 24.04 用 librime1t64：IME deb 的 Conflicts 同时列出 librime1 与 librime1t64（不存在的包名无副作用）；目标机若已装 t64 包，安装器会先 purge。
- rime-sync deb 仍是现有 0.3.0 产物（本任务不改版本号）；与 IME 0.4.0 版本号不一致但无冲突。
- 离线依赖闭包包含 GTK/WebKit，单发行版 .run 约 129–176 MiB；这是「带完整运行期」的代价，四个发行版合计约 611 MiB。
- 库版本取自构建容器（apt 当日候选）；目标机若比构建基线更旧，可能需要先做系统更新。
- 22.04 的 glog 为 0.5，librime 以 ENABLE_LOGGING=OFF 编译（上游支持的无日志构建），功能不受影响。

## 7. 体积（实测，2026-10-21）

| 发行版 | IME deb | 依赖 .deb 数 | 依赖闭包 | 目录离线集 | .run |
| --- | --- | --- | --- | --- | --- |
| ubuntu-22.04 (jammy) | 3,156,512 B (3.0 MiB) | 226 | 130,268,349 B (124 MiB) | 135,920,595 B (130 MiB) | 135,548,197 B (129 MiB) |
| ubuntu-24.04 (noble) | 3,272,312 B (3.1 MiB) | 249 | 142,083,085 B (136 MiB) | 147,850,208 B (141 MiB) | 147,344,106 B (141 MiB) |
| debian-12 (bookworm) | 3,171,772 B (3.0 MiB) | 254 | 167,713,539 B (160 MiB) | 173,380,578 B (166 MiB) | 173,381,663 B (166 MiB) |
| debian-13 (trixie) | 3,254,204 B (3.1 MiB) | 265 | 178,747,733 B (171 MiB) | 184,498,031 B (176 MiB) | 184,499,458 B (176 MiB) |

四个 .run 合计 640,773,424 B（约 611 MiB）。Debian 12/13 的闭包更大，主要因为
Debian 的 WebKitGTK 依赖链更宽（mesa-libgallium、procps 等），而 22.04 的
WebKit 依赖在桌面机上通常已存在。

### 构建耗时（单机 4 核，docker 缓存镜像层后）

| 发行版 | 容器内总耗时 | librime | fcitx5-rime |
| --- | --- | --- | --- |
| ubuntu-22.04 | 267 s | 161 s | 7 s |
| ubuntu-24.04 | 303 s | 195 s | 7 s |
| debian-12 | 308 s | 200 s | 8 s |
| debian-13 | 296 s | 192 s | 6 s |


### 验收结果（docker run --network none，全部通过）

| 发行版 | 离线安装 | 两包 installed | Loaded addon rime | deploy success | 候选「你好」 | 安装顺序 |
| --- | --- | --- | --- | --- | --- | --- |
| ubuntu-22.04 | PASS | PASS | PASS | PASS | PASS | IME 18:13:16 → sync 18:13:17 |
| ubuntu-24.04 | PASS | PASS | PASS | PASS | PASS | IME 18:18:16 → sync 18:18:17 |
| debian-12 | PASS | PASS | PASS | PASS | PASS | IME 18:02:54 → sync 18:02:55 |
| debian-13 | PASS | PASS | PASS | PASS | PASS | IME 18:08:09 → sync 18:08:10 |

对应日志：

- build/evidence/<distro>/offline-install.log（安装 + 全部断言）
- build/evidence/ubuntu-22.04/uninstall-test.log（--uninstall 顺序与保留用户数据）
- build/evidence/SIZES.txt（体积汇总）

