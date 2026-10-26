; rime-sync · NSIS 安装钩子（离线变体）
; 与 packaging/nsis-hooks.nsh 的唯一差别：定义 RIMESYNC_EMBED_WEBVIEW，
; 让 PREINSTALL 阶段的 WEBVIEW-INSTALL 用**内嵌**的 WebView2 Evergreen Standalone
; 安装器（约 130 MB）静默安装，全程不联网 —— 供受限代理 / 内网环境使用。
;
; 选用方式（构建期，不改主配置）：
;   pnpm tauri build --bundles nsis --config src-tauri/tauri.offline.conf.json
; 载荷（不入库，CI 构建时下载到 packaging/windows/vendor/）：
;   MicrosoftEdgeWebView2RuntimeInstallerX64.exe  (go.microsoft.com/fwlink/?linkid=2124701)
; 载荷缺失时：
;   编译期 → makensis 告警（rime-sync: offline build ... without WebView2 payload）
;   运行期 → WEBVIEW-INSTALL 阶段弹框说明并 Abort（绝不回退成联网下载）
;
; 注意：本文件必须放在 packaging/ 下（与 nsis-hooks.nsh 同目录），
; 因为 nsis-hooks.nsh 用 ${__FILEDIR__}\windows\vendor\... 定位内嵌载荷。
!define RIMESYNC_EMBED_WEBVIEW
; 关键：Tauri 用 makensis 时主脚本在 src-tauri/target/... 下，!include 的相对路径
; 按「主脚本目录 / cwd」解析，而不是本文件目录 —— 所以先把本文件目录加进 include 路径。
!addincludedir "${__FILEDIR__}"
!include "nsis-hooks.nsh"
