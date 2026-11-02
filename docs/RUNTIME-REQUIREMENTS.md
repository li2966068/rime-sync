# 运行库依赖说明（Windows / Linux）

> 结论先说：**rime-sync 不需要 .NET**。Windows 端只依赖系统自带的组件与 WebView2 运行时。

## 1. Windows 实际依赖

| 组件 | 是否需要 | 说明 |
|---|---|---|
| WebView2 运行时 | **需要** | 图形界面用它渲染；Win11 与较新的 Win10 通常自带。<br>安装包会检测：缺了会提示或（离线包）自动安装；不会后台静默下载 |
| .NET Framework / .NET Runtime | **不需要** | 程序是 Rust 编译的原生代码；安装器是 NSIS；输入法本体是 C++ |
| VC++ 运行库（VCRUNTIME140 / MSVCP140） | **不需要** | Tauri 产物默认**静态链接**运行库 |
| Windows 10 1803+（Build ≥ 17134） | 需要 | 更低版本安装器会直接提示并退出 |

### 证据（0.6.10 的 `rime-sync-app.exe` 导入表）

```
DLL Name: kernel32.dll      DLL Name: user32.dll      DLL Name: ole32.dll
DLL Name: combase.dll       DLL Name: shlwapi.dll     DLL Name: dwmapi.dll
DLL Name: bcrypt.dll        DLL Name: crypt32.dll     DLL Name: ws2_32.dll
DLL Name: imm32.dll         DLL Name: gdi32.dll       DLL Name: comctl32.dll
...
```

全是 Windows 系统 DLL：**没有** `VCRUNTIME140.dll`/`MSVCP140.dll`（静态链接），
**没有** `mscoree.dll`（即 .NET CLR），也没有其它第三方运行库。

## 2. 常见误判

| 现象 | 真正原因 | 处理 |
|---|---|---|
| 安装卡在 “WebView2Setup” 很久 | 旧版本（≤0.6.4）会静默联网下载 WebView2，代理受限就卡住 | 用 0.6.5+ 的 **offline-setup**（内嵌 WebView2），或先手动装 WebView2 |
| 提示缺 “.NET Framework 4.x” | 不是本程序弹的；多为系统里其它组件或旧安装器 | 与本项目无关；把弹窗标题/截图发来可确认 |
| 装完打不开、无窗口 | 多半是 WebView2 缺失或被杀软拦截 | 装 WebView2 后再试；查 `%TEMP%\rime-sync-setup.log` |
| 提示缺 `VCRUNTIME140.dll` | 极少见（静态链接）；若真出现说明用的是非官方构建 | 用官方发布包；或临时装 VC++ 2015-2022 x64 运行库 |
| 输入法里有方案但 v 模式/标点直出没生效 | 配置模板没写进 `%APPDATA%\Rime`（0.6.10 前全新安装会跳过） | 升级到 0.6.10+，或手动拷 `resources\schemas\*.custom.yaml` |

## 3. Linux 依赖

| 组件 | 说明 |
|---|---|
| fcitx5（5.0 / 5.1 对应不同发行版） | 输入法框架；`.run` 全量包内置依赖闭包 |
| Rime 数据与 librime | 随输入法包一起安装，不依赖系统 librime |
| 同步程序 | AppImage 免安装（需要系统有 FUSE 或解压运行）；deb 由安装器处理 |

## 4. 与 .NET 的关系（为什么不做 .NET 包）

程序里没有任何 .NET 程序集，也不加载 CLR；把 .NET 运行时打进安装包只会让体积增加约 55–75 MB，
对运行没有任何帮助。若将来引入依赖 .NET 的组件，再单独评估。

## 5. 需要帮助时提供这些信息

1. 安装包文件名与版本（例如 `rime-sync_0.6.12_x64-setup.exe`）；
2. `%TEMP%\rime-sync-setup.log` 里最后 30 行；
3. 弹窗截图（含标题栏）；
4. `winver` 的系统版本号。
