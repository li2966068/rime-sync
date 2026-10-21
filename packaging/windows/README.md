# Windows 打包（内嵌小狼毫 + 静默安装）

目标是「一个安装包装完即有小狼毫，且不自动弹任何窗口」。

## 1. 文件

| 文件 | 作用 |
|---|---|
| `fetch-weasel.ps1` | 下载固定版本的 Weasel 官方安装包到 `vendor/`，校验大小与 SHA-256 |
| `vendor/` | 内嵌组件（**二进制不入库**，见 `.gitignore`）+ GPLv3 全文 + 来源说明 |
| `../nsis-hooks.nsh` | NSIS 钩子：静默装 Weasel、建快捷方式、卸载清理 |
| `../../src-tauri/tauri.conf.json` | `bundle.resources` 把 `vendor/` 打进 `$INSTDIR\resources\`；`installMode = perMachine` |

体积预期：NSIS 安装包会在原有体积上增加约 12MB（内嵌的 Weasel 安装器，LZMA 压缩后
实际压缩率取决于 tauri 的 `compression` 设置；`installer.nsi` 默认 LZMA）。

## 2. 构建前置（**CI 必须做**）

```powershell
pwsh -File packaging/windows/fetch-weasel.ps1
pnpm tauri build --bundles nsis
```

`fetch-weasel.ps1` 必须在 `tauri build` **之前**执行，否则安装包里没有
`resources\weasel-0.17.4.0-installer.exe`，安装器只会在日志里提示并跳过输入法安装
（rime-sync 本体仍能装好，但「装完即有小狼毫」不成立）。

release.yml 的 Windows job 建议插一步（另一个代理负责改 CI）：

```yaml
- name: Fetch Weasel installer
  shell: pwsh
  run: ./packaging/windows/fetch-weasel.ps1
```

Linux job **不需要**执行（`vendor/` 里只有 GPL 文本与说明，deb 里多几 KB 文档）。
本机 PowerShell 语法自检：

```powershell
pwsh -NoProfile -Command "[void][scriptblock]::Create((Get-Content -Raw packaging/windows/fetch-weasel.ps1))"
```

## 3. 安装行为

1. **perMachine**：安装到 `C:\Program Files\rime-sync`，以管理员运行
   （Weasel 自己要写 Program Files 与 HKLM，非提权装不了）。
   代价：自动更新（NSIS 更新包）同样会弹 UAC，需要用户点一次「是」。
2. 安装段的 POSTINSTALL 钩子里依次执行：
   - 探测小狼毫：`<Program Files>\Rime\weasel-0.17.4\WeaselServer.exe`、
     `<Program Files>\Rime\Weasel\WeaselServer.exe`（32/64 位都查）。
     **已安装则跳过**，不重装、不降级用户现有的小狼毫。
   - 未安装则执行（`ExecWait`，等它装完再继续）：
     ```
     "$INSTDIR\resources\weasel-0.17.4.0-installer.exe" /S
     ```
     Weasel 是 NSIS 安装器，`/S` 即静默；它默认装到
     `C:\Program Files\Rime\weasel-0.17.4` 并自行注册输入法。
   - 退出码非 0、或根本启动不了，都只写安装日志（`DetailPrint`），**不阻断** rime-sync 安装。
3. **不启动主程序**：Tauri 模板的完成页有「运行 rime-sync」复选框，钩子用
   `MUI_FINISHPAGE_RUN_NOTCHECKED` 把它的默认状态改成不勾选；钩子里没有任何
   `Exec`/`ExecWait` 主程序的语句（只有上面那条 Weasel 安装）。
4. 快捷方式（perMachine → 「所有用户」开始菜单
   `C:\ProgramData\Microsoft\Windows\Start Menu\Programs`）：
   - `Rime 同步\Rime 同步.lnk` → `rime-sync-app.exe`
   - `Rime 同步\同步设置.lnk` → `rime-sync-app.exe --setup`
   - 若检测到 Weasel 程序组（`小狼毫输入法` / `小狼毫輸入法` / `Weasel`），再放一份
     `同步设置.lnk` 进去；只补这一个文件，不动 Weasel 自己的快捷方式。
   - Tauri 模板自动生成的 `Rime 同步\rime-sync.lnk` 会被删掉，保证文件夹里只有上述两项。

## 4. 卸载行为

`PREUNINSTALL` 钩子只删本安装器创建的东西：

- `Rime 同步` 程序组里的两个快捷方式（组删空后一并 `RMDir`）；
- Weasel 程序组里的 `同步设置.lnk`（**不删** Weasel 自己的快捷方式、不删程序组）；
- 旧版钩子可能留下的 `Rime 同步.lnk` 与桌面快捷方式；
- `resources\weasel-0.17.4.0-installer.exe`、`WEASEL-NOTICE.txt`、`GPL-3.0.txt`。

**默认保留小狼毫**及其词库/配置：卸载 rime-sync 不等于卸载输入法。
如需移除小狼毫，用「设置 → 应用」里的 Weasel 条目（或开始菜单里的卸载快捷方式）。

## 5. 无法在本机验证的点

- NSIS 安装包只能在 Windows 上构建/执行：本仓库的 CI（windows-latest）负责最终验证。
  本地已用 NSIS 3.11 的 `makensis` 加 stub 模板编译过钩子（见 PR 报告），确认语法、
  宏展开与中文字符串编码没问题；`perMachine` + `SetShellVarContext all` 的实际路径
  仍是 CI/真机验证项。
- Weasel 静默安装在无交互 CI 会话里是否完整注册输入法（可能需要重启 explorer）未验证。
