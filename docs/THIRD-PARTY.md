# 第三方组件与许可证

本仓库自身以 MIT 发布（见根目录 `LICENSE`）。分发的安装包里还包含下列第三方组件。

## 1. 小狼毫 Weasel 0.17.4.0（Windows 安装包内嵌）

| 项 | 值 |
|---|---|
| 组件 | 小狼毫输入法 / Weasel（Rime 输入法引擎的 Windows 前端） |
| 版本 | 0.17.4.0 |
| 版权 | Copyleft RIME Developers / 式恕堂 |
| 许可证 | **GNU General Public License v3.0** |
| 源码 | <https://github.com/rime/weasel/tree/0.17.4> |
| 发布页 | <https://github.com/rime/weasel/releases/tag/0.17.4> |
| 二进制 | `weasel-0.17.4.0-installer.exe`，12,431,118 字节 |
| SHA-256 | `cf509534a8f5f8af9c98ed7cbb8f135439f145a8cbe7e50ede42bb5b5ab45c29` |

### 分发方式

rime-sync 的 Windows 安装包**原样内嵌**官方发布的小狼毫安装器（未修改、未重新打包），
并在安装时以 `/S` 静默执行。因此：

- 用户拿到的 Weasel 与官方发布完全一致，可用上面的 SHA-256 对比官方发布页；
- 我们不修改 Weasel 源码，也不链接其代码；两者通过文件系统/进程边界交互。

### GPLv3 合规

- 许可证全文随安装包分发：安装后位于
  `C:\Program Files\rime-sync\resources\GPL-3.0.txt`；
  源码仓库副本在 `packaging/windows/vendor/GPL-3.0.txt`。
- 对应的完整源码可从 <https://github.com/rime/weasel/tree/0.17.4> 获取
  （官方发布页同时提供 `debug_symbols.7z`，但构建脚本以官方仓库为准）。
- 组件来源、版本与哈希另见 `packaging/windows/vendor/WEASEL-NOTICE.txt`
  （同样随安装包安装到 `resources\WEASEL-NOTICE.txt`）。

### 安装与卸载

- 只有检测不到小狼毫时才静默安装，已安装则跳过（不覆盖用户版本）。
- **卸载 rime-sync 默认保留小狼毫**及其词库、配置与程序组；
  卸载 rime-sync 不等于卸载输入法。移除 Weasel 请用「设置 → 应用」中的 Weasel 条目。

### 升级版本

改 `packaging/windows/fetch-weasel.ps1` 里的 `$WeaselVersion/$WeaselTag/$WeaselSize/$WeaselSha256`，
并同步 `packaging/nsis-hooks.nsh` 的 `RIMESYNC_WEASEL_INSTALLER` / `RIMESYNC_WEASEL_DIRVER`
（后者是 Weasel 自己的安装目录名 `weasel-<ver>`，注意 0.17.4 与 0.17.4.0 的区别）。

## 2. Linux 侧的第三方 deb

deb 安装时会从 `packaging/linux/vendor/` 安装 fcitx5 / librime 等发行版软件包，
其版本、来源与许可证见 `packaging/linux/README.Debian` 与各包自带版权文件。

## 3. 其他

- 图标：见 `packaging/assets/README.md`。
- Rust / 前端依赖的许可证由 Cargo / pnpm 各自的元数据描述，未在此逐一列举。
