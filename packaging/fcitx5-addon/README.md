# fcitx5 配置页（真正的 addon）

「Rime 同步设置」是 fcitx5 里的一个 **Configurable addon**：在 fcitx5 配置工具的
「附加组件」列表里能看到 **Rime 同步**，点齿轮直接打开 WebDAV 表单，改动写回
`~/.config/rime-sync/config.json`（与主程序共用同一份文件）。

## 调研结论：能做成配置页，但只能是「选项树 + External 按钮」

fcitx5 没有「addon 自带 QWidget」的接口，配置页由 fcitx5-configtool 的通用渲染器
按 Configuration 的选项元数据现场生成。证据（fcitx5 5.0.14 / kcm-fcitx5 5.0.11 源码）：

| 结论 | 证据 |
|---|---|
| addon 只要实现 `getConfig()/setConfig()` 并声明 `Configurable=True` | `/usr/include/Fcitx5/Core/fcitx/addoninstance.h` 的 `virtual const Configuration *getConfig() const` / `virtual void setConfig(const RawConfig &)`；`fcitx5/src/modules/dbus/dbusmodule.cpp` 的 `GetConfig`/`SetConfig`（`fcitx://config/addon/<name>` 分支要求 `addonInfo->isConfigurable()`） |
| 页面内容 = 选项树，按 Type 选控件 | `kcm-fcitx5/src/lib/configwidgetslib/optionwidget.cpp` `OptionWidget::addWidget()`：String→QLineEdit、Boolean→QCheckBox、Enum→QComboBox、Key→按键框、List→列表控件、External→按钮 |
| addon 不能塞自定义 Qt 控件 | 该渲染器只认上面这些 Type；没有任何「addon 提供页面」的注册点，要自定义页面只能改 fcitx5-configtool 源码或写 Plasma KCM |
| 动作按钮是可以做的 | `ExternalOption`（`fcitx-config/option.h`）`typeString()=="External"`，`dumpDescription` 写入 `External=<uri>`；`ExternalOptionWidget` 点击后 `QProcess::startDetached(program, args)`（非 `fcitx://` URI 按「程序 + 参数」启动） |
| 齿轮按钮从哪来 | `kcm-fcitx5/src/lib/configwidgetslib/addonselector.cpp`：`ConfigurableRole` 为真才显示齿轮，点击打开 `fcitx://config/addon/<name>` |

所以本仓库的实现是：**选项树（字符串/布尔）+ 一个 External 动作按钮**，
不追求自定义控件；独立 Qt 配置页 `rime-sync-config` 只作为没有 fcitx5-configtool
时的退路保留。

## 配置项 ↔ config.json

| 配置页 | 选项类型 | 写入位置 |
|---|---|---|
| WebDAV 地址 | String | `webdav.url`（缺尾斜杠自动补 `/`） |
| 用户名 | String | `webdav.username` |
| 密码（留空=不修改） | String（`skipSave`） | 仅在填写时写 `webdav.password`，见下 |
| 远端目录 | String | `webdav.remote_dir`（空 → `rime-sync`） |
| 设备名 / device_id | String | `device_id`（空 → 不写，主程序按 Rime 安装自动生成） |
| 应用同步时暂停 fcitx5 | Boolean | `stop_fcitx5_during_apply` |
| 打开 Rime 同步（WebDAV 设置向导） | External | 执行 `rime-sync-open-setup` |

写盘：`QSaveFile` 原子替换 + `chmod 0600`；先把整份 JSON 读出来再改字段，
`schema_version`、`sync_kinds`、`exclude_globs` 等未知键原样保留。

### 密码怎么处理

- fcitx 自己的配置（`~/.config/fcitx5/conf/rimesync.conf`）**永远不含密码**：
  密码选项带了 `skipSave()` 注解，fcitx 保存 addon 配置时会跳过它（实测该文件根本不会生成）。
- 用户在配置页填了密码时，才写进 `config.json`（0600）。主程序下次运行会走
  `config.rs` 的 `legacy_password` 迁移：搬进系统钥匙串（无 Secret Service 时是
  0600 的 `secrets.json`），然后把 `password` 从 config.json 删掉。
- 不想让明文落盘：密码留空，点「打开 Rime 同步（WebDAV 设置向导）」，由主程序向导写入钥匙串。

## 构建与安装

一条命令（docker，CI 可用）：

```bash
scripts/ime/build-fcitx5-addon.sh                 # 编译 + 装进容器 /usr + 冒烟测试
scripts/ime/build-fcitx5-addon.sh --no-smoke      # 只编译
OUT=dist/fcitx5-addon scripts/ime/build-fcitx5-addon.sh
APT_MIRROR=mirrors.tuna.tsinghua.edu.cn scripts/ime/build-fcitx5-addon.sh   # 慢链路
BUILD_NETWORK=host ...                             # docker build 的 DNS 需要走宿主时
```

安装路径（`DESTDIR` 暂存树在 `$OUT/root`）：

| 路径 | 内容 |
|---|---|
| `/usr/lib/<triplet>/fcitx5/rimesync.so` | addon 共享库（`-DMODULE`，无 `lib` 前缀） |
| `/usr/share/fcitx5/addon/rimesync.conf` | addon 元数据（`Configurable=True`, `Library=rimesync`） |
| `/usr/bin/rime-sync-open-setup` | 「打开 Rime 同步」按钮执行的脚本 |
| `/usr/bin/rime-sync-config` + `/usr/share/applications/rime-sync-config.desktop` | 可选的独立配置页 |

本机直接编译（需要 fcitx5 开发头文件，本机没有就进容器）：

```bash
sudo apt install cmake g++ qt6-base-dev libfcitx5core-dev libfcitx5config-dev gettext
cmake -S . -B build -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build -j
sudo cmake --install build
```

只想要独立配置页（没有 fcitx5 开发头文件）：

```bash
cmake -S . -B build -DRIMESYNC_BUILD_FCITX5_ADDON=OFF ...
```

## 验证

`scripts/ime/build-fcitx5-addon.sh` 的冒烟测试在容器里做四件事，全部有断言：

1. 检查安装路径 + `ldd rimesync.so` 无 `not found`；
2. Xvfb + `dbus-run-session` 启动 fcitx5，日志里出现 `Loaded addon rimesync` 且无加载错误；
3. 用 **fcitx5-configtool 打开配置页时的同一个 DBus 接口**取值：
   `GetConfig("fcitx://config/addon/rimesync")` 返回 6 个选项，且 `webdav.url` 的值来自预置的 config.json；
4. `SetConfig` 写回后检查 `config.json`：0600、字段更新、未知键保留、URL 补尾斜杠，
   并确认密码没进 fcitx 配置。

另外在 Xvfb + dbus 里装了 `fcitx5-config-qt` 做真实 UI 验证（xdotool 点击 + `import` 截图）：

- 「附加组件」页出现 `Rime Sync / Configure WebDAV sync for rime-sync` 一行 + Configure 按钮；
- 点开后是这个表单：WebDAV 地址 / 用户名 / 密码（留空=不修改） / 远端目录 / 设备名 /
  「应用同步时暂停 fcitx5」复选框 / 「打开 Rime 同步（WebDAV 设置向导）」按钮，
  字段值来自预置的 config.json；
- 在界面上把「远端目录」改成 `ui-dir` 后点 OK，config.json 里 `webdav.remote_dir` 变成
  `ui-dir`，权限 `0600`，`schema_version`/`sync_kinds` 等键仍在；
- 点「打开 Rime 同步」按钮，进程真的被执行：用一个假的 `rime-sync-app` 记录到
  参数是 `--setup`（说明 External 按钮 → `rime-sync-open-setup` → 向导这条链是通的）。

手工验证（桌面环境）：

```bash
fcitx5-configtool        # 「附加组件」→ Rime 同步 → 齿轮
# 或者直接看 addon 暴露的配置树：
gdbus call --session --dest org.fcitx.Fcitx5 --object-path /controller \
  --method org.fcitx.Fcitx.Controller1.GetConfig 'fcitx://config/addon/rimesync'
```

## 已知限制

- 选项树画不了自定义控件：密码框在配置页里是明文 QLineEdit（fcitx5-configtool 的
  String 控件没有 echo 模式），所以密码建议走向导。
- 每次「应用」都会重写 config.json：字段值正确、未知键保留，但键顺序/缩进会变。
- UI 验证是在 Ubuntu 22.04 容器 + Xvfb 里用 Qt5 版 fcitx5-config-qt 5.0.11 做的，
  没在真实桌面上跑；Plasma 的 KCM 走 QML 的 `ExternalOption.qml`，同样调用
  `launchExternal()` 执行命令，未单独实测。
- 发行版若用 Qt6 版 configtool，渲染逻辑相同（同一份 optionwidget.cpp），但未实测。
- addon 为了读写 JSON 链接了 Qt Core，fcitx5 进程会多加载 libQt6Core（仅打开配置页时
  才加载 addon，因为 `OnDemand=True`）。
