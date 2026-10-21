# vendor/ · Windows 内嵌组件

本目录的二进制**不入库**（见 `../.gitignore`），由 `../fetch-weasel.ps1` 在构建前下载。
Tauri 的 `bundle.resources` 会把本目录打包进 NSIS 安装包的 `$INSTDIR\resources\`。

## weasel-0.17.4.0-installer.exe（小狼毫官方安装包）

| 项 | 值 |
|---|---|
| 组件 | 小狼毫（Weasel）输入法，Rime 的 Windows 前端 |
| 版本 | 0.17.4.0 |
| 大小 | 12,431,118 字节 |
| SHA-256 | `cf509534a8f5f8af9c98ed7cbb8f135439f145a8cbe7e50ede42bb5b5ab45c29` |
| 发布页 | <https://github.com/rime/weasel/releases/tag/0.17.4> |
| 下载地址 | <https://github.com/rime/weasel/releases/download/0.17.4/weasel-0.17.4.0-installer.exe> |
| 许可证 | GNU General Public License v3.0（全文见 `GPL-3.0.txt`） |
| 源码 | <https://github.com/rime/weasel/tree/0.17.4> |

二进制为官方发布产物，**未做任何修改**；SHA-256 可与官方发布页比对。

## 下载

```powershell
pwsh -File packaging/windows/fetch-weasel.ps1            # 已存在且校验通过则跳过
pwsh -File packaging/windows/fetch-weasel.ps1 -Force      # 强制重新下载
pwsh -File packaging/windows/fetch-weasel.ps1 -Mirror https://gh-proxy.com   # 走镜像
```

脚本会校验大小与 SHA-256，不匹配时删除临时文件并以非 0 退出。

## CI 要求

Windows 构建 job 必须**在 `pnpm tauri build --bundles nsis` 之前**执行
`fetch-weasel.ps1`；否则 `$INSTDIR\resources\weasel-0.17.4.0-installer.exe` 不存在，
安装器会在日志里提示「未找到内嵌的小狼毫安装包」并跳过输入法安装
（rime-sync 本体仍能装好，但「装完即有小狼毫」不成立）。

## 本目录其它文件

| 文件 | 说明 |
|---|---|
| `GPL-3.0.txt` | Weasel 的 GPLv3 许可证全文（FSF 官方文本），随安装包分发 |
| `WEASEL-NOTICE.txt` | 组件来源、版本、哈希、GPLv3 合规与卸载说明 |
