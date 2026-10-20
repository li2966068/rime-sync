# fcitx5 配置页 / addon

给 rime-sync 提供一个**在输入法框架里就能打开**的 WebDAV 配置页：

- 字段：WebDAV 地址 / 用户名 / 密码 / 远端目录
- 按钮：**保存**、**打开 Rime 同步**（保存后启动主程序）
- 落盘：`~/.config/rime-sync/config.json` 的 `webdav` 段——与主程序共用同一份配置
  （遵循 `$XDG_CONFIG_HOME`，默认 `~/.config`）

## 为什么配置页是独立程序，而不是内嵌在 fcitx5-configtool 里

fcitx5 的通用配置渲染器只认识「选项树」（字符串/布尔/枚举），画不出「打开 Rime 同步」这种动作按钮。
因此这里拆成两件东西：

| 目标 | 产物 | 作用 |
|---|---|---|
| `rime-sync-config` | Qt Widgets 可执行文件 + `.desktop` | 真正的配置页（表单 + 按钮） |
| `rimesync` | fcitx5 addon 共享库（`MODULE`） | 注册进 fcitx5 addon 表，保留扩展点；不放 UI |

## 构建

依赖：

```bash
# Debian / Ubuntu
sudo apt install cmake g++ qt6-base-dev libfcitx5core-dev
# Qt5 环境把 qt6-base-dev 换成 qtbase5-dev 即可，CMakeLists 会自动选 Qt6/Qt5
```

```bash
cmake -S . -B build -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build -j
sudo cmake --install build
```

只想要配置页、不装 fcitx5（或没装开发头文件）时：

```bash
cmake -S . -B build -DRIMESYNC_BUILD_FCITX5_ADDON=OFF -DCMAKE_INSTALL_PREFIX=/usr
cmake --build build -j
```

产物：

| 路径 | 内容 |
|---|---|
| `<prefix>/bin/rime-sync-config` | 配置页 |
| `<prefix>/share/applications/rime-sync-config.desktop` | 应用菜单入口「Rime 同步 · 配置」 |
| `<prefix>/lib/fcitx5/rimesync.so` + `rimesync.conf` | fcitx5 addon（受 `RIMESYNC_BUILD_FCITX5_ADDON` 控制） |

## 配置文件格式

只写 `webdav` 段，其余键（`device_id`、`sync_kinds` 等）原样保留：

```json
{
  "webdav": {
    "url": "http://127.0.0.1:5244/dav/",
    "username": "alist",
    "password": "******",
    "remote_dir": "aliyun/rime-sync"
  }
}
```

写入用 `QSaveFile` 原子替换，随后把权限收紧到 `0600`（文件含密码）。
密码留空表示**不修改**已保存的密码，避免每次打开配置页都要重输。

## 让「打开 Rime 同步」找到主程序

查找顺序：`$RIME_SYNC_BIN` → `PATH` 里的 `rime-sync` → `/usr/bin/rime-sync`。
便携版/自定义安装路径请设 `RIME_SYNC_BIN`，例如：

```bash
RIME_SYNC_BIN="$HOME/Applications/rime-sync.AppImage" rime-sync-config
```

## 已知限制

- **本轮未编译验证**：开发机只装了 `fcitx5` 运行时，没有 `libfcitx5core-dev` 与 Qt 开发包；
  代码按 fcitx5 5.1 的 `AddonFactory` / `AddonInstance` API 编写，但未在 CI 构建。
  首次构建若报头文件路径差异，按发行版调整 `find_package(Fcitx5Core)` 即可。
- addon 目前不注册输入法、不拦截按键，只占位；同步逻辑仍由 rime-sync 主程序负责。
- 密码按 SPEC 要求写入 `config.json`。若主程序已启用系统钥匙串存储，两处需要保持同一后端，
  否则会出现「配置页保存了密码但主程序读不到」——这是后续要合的 TODO（addon 侧改调
  `secret-tool` / libsecret）。
