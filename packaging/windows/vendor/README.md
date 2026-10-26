# vendor/ · Windows 内嵌组件

本目录的 `*.exe` **不入库**（见 `../.gitignore`），由 CI 在构建前产生：

- 主路径：`scripts/ime/build-weasel.ps1` 从 `third_party/weasel`（rime/weasel 0.17.4）编译；
- 兜底：`packaging/windows/fetch-weasel.ps1` 下载官方安装包（校验大小与 SHA-256）；
- 离线包（0.6.5）：`packaging/windows/fetch-webview2.ps1` 下载
  `MicrosoftEdgeWebView2RuntimeInstallerX64.exe`（WebView2 Evergreen Standalone，约 130 MB；
  3 次重试 + 体积/文件头校验。滚动链接不提供固定 SHA-256，需要复现时用 `-ExpectedSha256` 自锁）。

文件名固定为 `weasel-0.17.4.0-installer.exe`：`packaging/nsis-hooks.nsh` 用它做构建期
`File` 内嵌（PREINSTALL 阶段装输入法），`src-tauri/tauri.conf.json` 不再把它打进 resources。

## 产物说明

| 项 | 自编译（主路径） | 官方安装包（兜底） |
|---|---|---|
| 来源 | `third_party/weasel` @ tag 0.17.4 | rime/weasel release `0.17.4` |
| 大小 | 约 12 MiB（随 boost/librime 版本略有浮动） | 12,431,118 字节 |
| SHA-256 | 每次构建都可能不同，见 CI 步骤摘要 | `cf509534a8f5f8af9c98ed7cbb8f135439f145a8cbe7e50ede42bb5b5ab45c29` |
| 许可证 | GPL-3.0（`GPL-3.0.txt`） | 同左 |
| 输入方案 | 默认不含（plum 为空子模块） | 含官方方案数据 |

## 离线包的 WebView2 载荷

`MicrosoftEdgeWebView2RuntimeInstallerX64.exe` 只被离线变体（`../nsis-hooks-offline.nsh`，
`RIMESYNC_EMBED_WEBVIEW`）用 NSIS `File` 内嵌进 `*-offline-setup.exe`，运行期在
WEBVIEW-INSTALL 阶段 `ExecWait '/silent /install'`。在线包不读取它，安装时也**不会**
联网下载任何东西（`webviewInstallMode = skip`）。

## 本目录其它文件

| 文件 | 说明 |
|---|---|
| `GPL-3.0.txt` | GPLv3 许可证全文，随安装包分发到 `resources\GPL-3.0.txt` |
| `WEASEL-NOTICE.txt` | 组件来源、版本、构建方式、GPLv3 合规与卸载说明 |
| `README.md` | 本文件（也作为 `resources\README.md` 分发） |

## 命令

```powershell
pwsh -File scripts/ime/build-weasel.ps1            # 自编译（主路径）
pwsh -File scripts/ime/build-weasel.ps1 -Check     # 只检查依赖
pwsh -File packaging/windows/fetch-weasel.ps1      # 离线兜底：官方安装包
pwsh -File packaging/windows/fetch-webview2.ps1    # 离线包：WebView2 Standalone（约 130 MB）
```
