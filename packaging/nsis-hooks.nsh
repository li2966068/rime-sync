; rime-sync · NSIS 安装钩子
; 由 src-tauri/tauri.conf.json 的 bundle.windows.nsis.installerHooks 引用。
; 离线变体：packaging/nsis-hooks-offline.nsh 先 !define RIMESYNC_EMBED_WEBVIEW
; 再 !include 本文件（由 src-tauri/tauri.offline.conf.json 选用）。
; Tauri 在 installer.nsi / uninstaller.nsi 里以 !ifmacrodef 方式插入这些宏，
; 因此本文件只定义宏与私有函数，不覆盖 Tauri 自己的安装逻辑。
;
; 安装顺序（验收点）：输入法 → 同步程序
;   Tauri 模板的 Install 段顺序是：
;       SetOutPath $INSTDIR
;       !insertmacro NSIS_HOOK_PREINSTALL      <-- 本文件的 PREINSTALL
;       File "${MAINBINARYSRCPATH}"             <-- 这里才开始拷我们的程序
;       File ... resources ...                 <-- resources 也是这之后才落地
;       !insertmacro NSIS_HOOK_POSTINSTALL
;   所以：
;     1) PREINSTALL 阶段不能读 $INSTDIR\resources（文件还没解压出来）。
;     2) 要真正做到“先装输入法”，安装包必须在构建期用 NSIS 自己的 File 指令内嵌，
;        运行时先解到 $PLUGINSDIR 再 ExecWait —— 见下面的 RIMESYNC_WEASEL_EXE。
;     3) 内嵌源路径用 ${__FILEDIR__}（= 本文件所在目录 packaging/）。Tauri 会把
;        installerHooks 路径 canonicalize 成绝对路径再 !include，因此这里得到的是
;        <repo>\packaging\windows\vendor\weasel-<ver>-installer.exe。
;
; 验收证据：除了 DetailPrint，安装过程还会把带时间戳的日志追加到两个文件
;   $INSTDIR\install-ime-order.log   （0.6.4 及以前的验收文件，保留）
;   $TEMP\rime-sync-setup.log        （0.6.5 起统一安装日志，UTF-16LE）
; 阶段名（phase=）：OS-CHECK / WEBVIEW-CHECK / WEBVIEW-INSTALL / IME-CHECK /
;                   IME-INSTALL / IME-REPAIR / POSTINSTALL / SELF-CHECK / DONE；
;                   失败路径同样写日志。
;
; 0.6.14：修 0.6.13 的致命回归 —— 就绪判定被 TSF/注册表残留骗过。
;   真机 A：Weasel 已被卸掉（HKLM\Software\WOW6432Node\Rime\Weasel 不存在、
;   C:\Program Files\Rime 是空目录、没有 WeaselServer 进程），但 TSF 残留还在；
;   旧判定把残留当证据 → 判成就绪 → action=skip → 没有输入法。
;   1) 就绪判定 RimeSyncWeaselReady 只剩唯一硬条件：注册表 InstallDir/WeaselRoot
;      （32/64 视图都读）指向的目录里**真的有 WeaselServer.exe**。TSF 注册与进程状态
;      只写日志（phase=TSF-EVIDENCE），绝不参与判定。
;   2) 顺序改成「先判定 → 再决定卸/重装」：PREINSTALL 先判一次（IME-CHECK stage=preflight），
;      卸旧 rime-sync 之后再判一次（决定权在 RimeSyncInstallWeaselPre）；已就绪只跳过安装
;      （action=skip reason=exe-present），未就绪才 taskkill → 旧目录改名 Rime.bak-<ts>
;      （仅当目录非空且 exe 在）→ 内嵌安装器 → 复查。
;   3) 复查仍不就绪 → 回滚：Rime.bak-<ts> 改回 Rime，日志
;      WEASEL-INSTALL failed ... rollback=ok|failed，SetErrorLevel 5 + Abort。
;   4) 卸旧 rime-sync 不再连带卸 Weasel（UNINSTALL-OLD 只卸同步程序）：先卸 Weasel
;      再判就绪，正是 0.6.13 那个「卸掉后没装回来」的根因。
;   5) POSTINSTALL 自检加硬性检查（默认布局的 WeaselServer.exe +
;      HKLM\Software\WOW6432Node\Rime\Weasel\InstallDir）：任一不满足 →
;      phase=SELF-CHECK weasel-ready=no + 非静默弹框 + SetErrorLevel 5。
; 0.6.13：修 Weasel「假就绪」与「配置写错目录」两个坑（判据与顺序已在 0.6.14 重做）。
;   1) 就绪判定换成组合条件 RimeSyncWeaselReady（见函数头）：注册表
;      HKLM\Software\Rime\Weasel 的 InstallDir/WeaselRoot 必须存在，且它指向的目录里
;      有 WeaselServer.exe；只凭「目录里有文件」不再算就绪。
;   2) 未就绪 → taskkill → 残留目录改名 Rime.bak-<ts>（不删，留证据）→ 内嵌安装器 /S
;      （nsExec，300 秒超时）→ 复查；仍不就绪 = 硬失败（WEASEL-INSTALL failed + 非零退出）。
;   3) 配置播种在 SetShellVarContext current 下解析真实用户目录（%APPDATA%\Rime），
;      不再写进 C:\ProgramData\Rime。
;   4) POSTINSTALL 末尾自检 phase=SELF-CHECK：就绪与否 / 配置目录与数量 /
;      调一次 WeaselDeployer.exe /deploy / build\luna_pinyin_simp.schema.yaml 是否生成或更新。
; （静默安装看不到 DetailPrint，这两个文件是唯一可复核的证据；前者卸载时删除）
;
; WebView2（0.6.5）：tauri.conf.json 的 webviewInstallMode 已改为 skip。
; 安装器**不再**用 embedBootstrapper 静默联网下载（受限代理下会卡死在 WebView2Setup）。
; 运行期在 PREINSTALL 第一步由 RimeSyncEnsureWebView2 处理：
;   已装                         → 记日志继续
;   未装 + /SKIPWEBVIEW          → 记日志继续（应用可能起不来，用户自己装）
;   未装 + 离线包（EMBED_WEBVIEW）→ 内嵌 Standalone 安装器静默安装（/silent /install）
;   未装 + 其它（在线包）         → 弹框说明下载地址：继续（我已装好）/ 退出（Abort）
;   探测：HKLM/HKCU × 64/32 位视图读 EdgeUpdate Clients\{GUID}\pv，另用程序文件兜底。
;
; 安装器命令行参数：
;   /SKIPWEBVIEW  缺 WebView2 也不阻断（不联网、不弹框、只写日志）
;   /SKIPIME      跳过内嵌小狼毫安装（已有输入法时强制跳过）
;
; Windows 版本门槛：${AtLeastWin10} 且 CurrentBuildNumber >= 17134（Windows 10 1803），
; 低于门槛弹框说明并 Abort。
;
; 所有权（卸载时才知道该不该连小狼毫一起卸）：
;   安装前先探测小狼毫是否已存在（RimeSyncFindWeaselRoot）。
;   - 已存在   → 不是我们装的，**不写标记**，卸载时保留。
;   - 不存在   → 静默安装内嵌的小狼毫；安装器退出码 0 后写所有权标记：
;                  HKLM\Software\rime-sync（64 位视图）
;                    BundledWeasel   = 1
;                    WeaselRoot      = 探测到的 Weasel 程序目录
;                    WeaselUninstall = Weasel 的 UninstallString（带引号的卸载器）
;                    BundledAt       = 安装时间戳
;                卸载时据此决定是否连小狼毫一起卸；升级安装（小狼毫已存在）不会清掉旧标记。
;
; 卸载（PREUNINSTALL）：
;   1. 读 HKLM\Software\rime-sync 的 BundledWeasel；不是 1 → 保留小狼毫。
;   2. 命令行 /KEEPWEASEL 或环境变量 RIMESYNC_KEEP_WEASEL=1 → 保留，并清掉标记
;      （所有权交还用户，避免以后再有人替用户卸输入法）。
;   3. 否则先跑标记里的 UninstallString（追加 /S），失败再跑 <WeaselRoot>\uninstall.exe /S；
;      两条都失败只记日志，**不 Abort**，rime-sync 继续卸载。
;   4. 无论 Weasel 卸载成功与否，都删掉标记。
;   5. 卸载日志：$TEMP\rime-sync-uninstall.log（不随 $INSTDIR 删除，便于事后复核）。
;
; 用户数据（默认保留，必须显式确认才删）：
;   - 交互卸载（非静默）：MessageBox MB_YESNO|MB_ICONEXCLAMATION 询问
;     「是否同时删除本地用户数据？」，默认「否」（直接关掉窗口也算否）。
;   - 静默卸载（/S）：只有命令行带 /PURGEDATA 才删，否则一律保留。
;   - 删除范围严格限定两条：%APPDATA%\Rime（Rime 用户词库与方案自定义）与
;     %APPDATA%\com.rimesync.app（rime-sync 的配置与备份）。先 RMDir /r，再检查是否
;     真的消失；失败只写卸载日志并继续卸载，绝不 Abort；不碰这两条路径的父目录。
;   - 询问与每条删除结果都写进 $TEMP\rime-sync-uninstall.log
;     （phase=PREUNINSTALL purge-userdata=...）。Weasel 0.17.4 的卸载器同样不删用户数据
;     （见 third_party/weasel/output/install.nsi 的 Section "Uninstall"）。
;
; 前提（见 tauri.conf.json 的 bundle.windows.nsis）：
;   installMode = perMachine  → 安装器以管理员运行，$SMPROGRAMS / $INSTDIR 都是
;   「所有用户」上下文（Tauri 的 SetContext 宏已 SetShellVarContext all）。
;
; 安装结束不启动 rime-sync：Tauri 模板在完成页有「运行」复选框，
; 这里把它的默认状态改成不勾选（MUI_FINISHPAGE_RUN_NOTCHECKED），
; 用户想用只能自己从开始菜单点开。
;
; 注意：本文件的 Function 会在 !include 处立即编译，而 ${MAINBINARYNAME} /
; ${PRODUCTNAME} 要等 Tauri 模板在 include 之后才定义，所以这两个符号只在
; 宏体里使用，由宏在插入时展开后经 $R6 / $R7 传给函数。

; /KEEPWEASEL、/SKIPWEBVIEW、/SKIPIME 都用 ${GetOptions} 解析（Tauri 模板也
; include 了 FileFunc.nsh，该头文件自带 FILEFUNC_INCLUDED 保护，重复 include 安全）。
!include "FileFunc.nsh"
; 系统版本判定：${AtLeastWin10}（Win10/Win11 都为真）+ CurrentBuildNumber 补 1803 门槛。
; WinVer.nsh 自带 ___WINVER__NSH___ 保护，Tauri 模板若已 include 也不冲突。
!include "WinVer.nsh"
; WinVer.nsh 的 LogicLib 扩展需要 LogicLib.nsh（自带 LOGICLIB_INCLUDED 保护）。
!include "LogicLib.nsh"

!define RIMESYNC_NAME "Rime 同步"
!define RIMESYNC_SETUP_NAME "同步设置"
!define RIMESYNC_WEASEL_INSTALLER "weasel-0.17.4.0-installer.exe"
!define RIMESYNC_WEASEL_DIRVER "0.17.4"
!define RIMESYNC_ORDER_LOG "$INSTDIR\install-ime-order.log"
!define RIMESYNC_UNINSTALL_LOG "$TEMP\rime-sync-uninstall.log"

; 0.6.5 统一安装日志（UTF-16LE）：$INSTDIR\install-ime-order.log 仍保留，两者都写；
; %TEMP% 下这一份在安装结束后仍可复核，失败路径（Abort 前）也写。
!define RIMESYNC_SETUP_LOG "$TEMP\rime-sync-setup.log"

; WebView2 Evergreen 运行时：EdgeUpdate 客户端 GUID（没有这个键 = 没装）
!define RIMESYNC_WEBVIEW_GUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
; 在线引导安装器地址：只打印给用户手动装，安装器自己绝不联网下载（受限代理会卡死）
!define RIMESYNC_WEBVIEW_URL "https://go.microsoft.com/fwlink/?linkid=2124703"
!define RIMESYNC_WEBVIEW_INSTALLER "MicrosoftEdgeWebView2RuntimeInstallerX64.exe"
; 仅离线包（nsis-hooks-offline.nsh 定义 RIMESYNC_EMBED_WEBVIEW）才内嵌这个 ~130MB 载荷
!define RIMESYNC_WEBVIEW_EXE "${__FILEDIR__}\windows\vendor\${RIMESYNC_WEBVIEW_INSTALLER}"
; Windows 10 1803 的构建号：WebView2 运行时官方最低要求，低于它直接拦下
!define RIMESYNC_MIN_WIN10_BUILD "17134"

; 缺 WebView2 / 安装失败时的说明正文（中英按运行时 $LANGUAGE 选）；
; $R0（中文附加原因）/ $R1（英文附加原因）由调用方在 Call 前填好。
!define RIMESYNC_WEBVIEW_MSG_ZH "缺少 Microsoft Edge WebView2 运行时，rime-sync 无法显示界面。$\r$\n$\r$\n请先手动安装 WebView2（在线安装器约 1.5 MB；内网/受限代理请用离线安装包，约 130 MB）：$\r$\n${RIMESYNC_WEBVIEW_URL}$\r$\n$\r$\n装好后重新运行本安装包。$\r$\n$\r$\n"
!define RIMESYNC_WEBVIEW_MSG_EN "Microsoft Edge WebView2 Runtime is missing, so rime-sync cannot show its window.$\r$\n$\r$\nInstall WebView2 manually first (about 1.5 MB online bootstrapper; behind a restricted proxy use the offline installer, about 130 MB):$\r$\n${RIMESYNC_WEBVIEW_URL}$\r$\n$\r$\nThen run this installer again.$\r$\n$\r$\n"

; 所有权标记（卸载时读同一个键）；64 位视图，与 perMachine 安装上下文一致。
!define RIMESYNC_REG_KEY "Software\rime-sync"

; 卸载时用户数据的处理决定（1 = 删除，0 = 保留）与来源（prompt / /PURGEDATA / none）。
; 用全局 Var 而不是寄存器：Section Uninstall 里要跨两次函数调用保存它。
!ifndef RIMESYNC_USERDATA_VARS
  !define RIMESYNC_USERDATA_VARS
  Var /GLOBAL RimeSyncPurgeUserData
  Var /GLOBAL RimeSyncPurgeSource
!endif


; ---------------------------------------------------------------------------
; 0.6.11「升级即全新安装」：每个新版本都先把旧版（同步程序 + 自建输入法）静默卸载，
; 再全新安装；POSTINSTALL 按新版本无条件重写推荐配置（覆盖前备份 <file>.bak-<版本>）。
; 卸载器最多等 120 秒（nsExec /TIMEOUT），超时就结束进程继续装，绝不 Abort。
; ---------------------------------------------------------------------------
!define RIMESYNC_UNINSTALL_TIMEOUT_MS "120000"

; 0.6.13：内嵌小狼毫安装器的等待上限（装一次最多 5 分钟），自检里部署的等待上限 20 秒。
; nsExec 的 /TIMEOUT 必须在编译期就是常量，所以两次等待各用一个 define、各用一条
; 调用（RimeSyncRunAndWait 120s / RimeSyncRunWeaselInstaller 300s）。
!define RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS "300000"
!define RIMESYNC_DEPLOY_TIMEOUT_MS "20000"

; 0.6.13：Weasel 的 TSF 文本服务 CLSID（third_party/weasel/WeaselTSF/Globals.cpp 的
; c_clsidTextService）。只给自检日志里「TSF 有没有注册」这条证据用，不参与就绪判定。
!define RIMESYNC_TSF_CLSID "{A3F4CDED-B1E9-41EE-9CA6-7B4D0DE6CB0A}"

; 升级流程要跨函数保存的状态。寄存器（$0-$9 / $R0-$R9）会被 Call 链里的
; RimeSyncStamp / RimeSyncFindWeaselRoot / RimeSyncLog 冲掉，只能用全局 Var 保存。
!ifndef RIMESYNC_UPGRADE_VARS
  !define RIMESYNC_UPGRADE_VARS
  Var /GLOBAL RimeSyncOldUninstKey      ; 命中的 Uninstall 子键名（或 (instdir)）
  Var /GLOBAL RimeSyncOldUninstRaw      ; 该子键的 UninstallString（未归一化）
  Var /GLOBAL RimeSyncWeaselKept        ; 1 = 旧小狼毫仍在，本次跳过输入法安装
  Var /GLOBAL RimeSyncSeedVersion       ; 备份后缀用的版本号
  Var /GLOBAL RimeSyncSeedWritten       ; 本次写入的配置文件数
  Var /GLOBAL RimeSyncSeedBackups       ; 本次生成的备份数（0.6.12 起恒为 0，保留字段兼容旧日志）
  Var /GLOBAL RimeSyncSeedReused        ; 本次沿用的已有配置文件数
  Var /GLOBAL RimeSyncSeedWeaselRoot    ; 本次探测到的小狼毫根目录
  Var /GLOBAL RimeSyncSeedHandle        ; FindFirst/FindNext 句柄
  Var /GLOBAL RimeSyncSeedName          ; FindFirst/FindNext 文件名
!endif

; 0.6.13：就绪判定与安装自检的跨函数状态（RimeSyncStamp / RimeSyncLog 会冲掉寄存器，
; 这些值必须活在全局变量里）。同样只声明一次。
!ifndef RIMESYNC_READY_VARS
  !define RIMESYNC_READY_VARS
  Var /GLOBAL RimeSyncWeaselReady       ; 1 = 就绪（注册表值 + WeaselServer.exe 都在）
  Var /GLOBAL RimeSyncWeaselRootDir     ; 就绪时的程序目录（含 WeaselServer.exe）
  Var /GLOBAL RimeSyncWeaselRegDir      ; 注册表里读到的原始目录（没命中 exe 时用于改名）
  Var /GLOBAL RimeSyncWeaselReadySrc    ; 判定依据：命中的注册值名 / none
  Var /GLOBAL RimeSyncWeaselReadyReason ; 未就绪原因（ASCII，写日志用）
  Var /GLOBAL RimeSyncWeaselTsfState    ; TSF 注册证据：yes / no
  Var /GLOBAL RimeSyncWeaselTsfDll      ; CLSID\InprocServer32（有就读出来写日志）
  Var /GLOBAL RimeSyncWeaselProcState   ; WeaselServer.exe：running / stopped / unknown
  Var /GLOBAL RimeSyncWeaselBackupDir   ; 残留目录改名后的路径（没改名则空）
  Var /GLOBAL RimeSyncWeaselBackupOrig  ; 改名前的原目录（回滚要改回这里）
  Var /GLOBAL RimeSyncWeaselRollback    ; 回滚结果：ok / failed / none
  Var /GLOBAL RimeSyncWeaselRegInstallDir ; 自检：注册表 InstallDir 原值（32/64 任一视图）
  Var /GLOBAL RimeSyncWeaselDefaultExe  ; 自检：默认布局里有 WeaselServer.exe（yes/no）
  Var /GLOBAL RimeSyncWeaselSelfCheckFail ; 1 = 自检硬性检查不通过
  Var /GLOBAL RimeSyncRimeUserDir       ; 真实用户配置目录（%APPDATA%\Rime）
  Var /GLOBAL RimeSyncSelfCheckWarn     ; 自检告警条数
  Var /GLOBAL RimeSyncDeployExit        ; WeaselDeployer /deploy 的退出码
  Var /GLOBAL RimeSyncDeployRan         ; 1 = 本次确实调用过部署
  Var /GLOBAL RimeSyncBuildState        ; generated / updated / stale / missing / unknown
  Var /GLOBAL RimeSyncFileExists        ; RimeSyncFileTimeGet：1 = 文件存在
  Var /GLOBAL RimeSyncFileHi            ; …FILETIME 高 32 位（十进制字符串）
  Var /GLOBAL RimeSyncFileLo            ; …FILETIME 低 32 位
  Var /GLOBAL RimeSyncBuildExistsBefore ; 部署前 build 文件是否存在
  Var /GLOBAL RimeSyncBuildHiBefore     ; 部署前 FILETIME 高 32 位
  Var /GLOBAL RimeSyncBuildLoBefore     ; 部署前 FILETIME 低 32 位
  ; 0.6.15：预编译 build 产物兜底（某些机器上 WeaselDeployer /deploy 返回 0 但什么都不写）
  Var /GLOBAL RimeSyncBuildSource        ; deploy / prebuilt / none
  Var /GLOBAL RimeSyncBuildProbe         ; yes = build\luna_pinyin_simp.schema.yaml 存在且含插件标记
  Var /GLOBAL RimeSyncPrebuiltMode       ; auto / force / off（RIMESYNC_PREBUILT 环境变量）
  Var /GLOBAL RimeSyncPrebuiltState      ; ok / failed / payload-missing / skipped / none
  Var /GLOBAL RimeSyncPrebuiltCopied     ; 本次新铺的文件数
  Var /GLOBAL RimeSyncPrebuiltKept       ; 已存在而沿用的文件数
  Var /GLOBAL RimeSyncPrebuiltEffMode    ; 当前这一遍用的模式（auto / force）
  Var /GLOBAL RimeSyncPrebuiltRecopy     ; 1 = 第一遍铺完仍不可用，只对 build/ 覆盖重铺过
  Var /GLOBAL RimeSyncContains           ; RimeSyncFileContains 的结果：yes / no
  ; 0.6.21：非保留方案清理（实现只有 rime-sync-cli prune-schemas 一份）
  Var /GLOBAL RimeSyncPruneState         ; ok / failed / no-cli / skipped
!endif

; 构建期由 scripts/ime/build-weasel.ps1 放到 packaging/windows/vendor/。
; 不存在时不直接报错：允许“离线兜底/根本没编译”的构建继续，只给编译告警，
; 运行时再退回旧的 resources 布局（旧布局在 PREINSTALL 阶段通常拿不到文件）。
!define RIMESYNC_WEASEL_EXE "${__FILEDIR__}\windows\vendor\${RIMESYNC_WEASEL_INSTALLER}"

!if /FileExists "${RIMESYNC_WEASEL_EXE}"
  !define RIMESYNC_WEASEL_EMBEDDED
!else
  !warning "rime-sync: vendored weasel installer not found, IME will not be installed (run scripts/ime/build-weasel.ps1)"
!endif

; 离线包的 WebView2 载荷（packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe）。
; 只在定义了 RIMESYNC_EMBED_WEBVIEW 的离线构建里检查；缺失时编译告警，
; 运行时在 WEBVIEW-INSTALL 阶段显式失败（绝不回退成联网下载）。
!ifdef RIMESYNC_EMBED_WEBVIEW
  !if /FileExists "${RIMESYNC_WEBVIEW_EXE}"
    !define RIMESYNC_WEBVIEW_PAYLOAD
  !else
    !warning "rime-sync: offline build (RIMESYNC_EMBED_WEBVIEW) without WebView2 payload; run packaging/windows/fetch-webview2.ps1"
  !endif
!endif

; 完成页「运行 rime-sync」复选框默认不勾选（不自动启动主程序）
!define MUI_FINISHPAGE_RUN_NOTCHECKED

; ---------------------------------------------------------------------------
; 时间戳：$R2 = "YYYY-MM-DD HH:MM:SS"（本地时间）
; 占用 $1..$9（都是函数内 scratch），$0 / $R0..$R9 不受影响。
; ---------------------------------------------------------------------------
; 安装器与卸载器的 $R2/$R1 都是独立的寄存器，函数体用宏共用，${UN} 前缀区分两套名字
; （卸载段里只能 Call un.*；宏内标签也必须带前缀，否则重复定义）。
!macro RIMESYNC_FN_STAMP UN
Function ${UN}RimeSyncStamp
  System::Call '*(&i2,&i2,&i2,&i2,&i2,&i2,&i2,&i2) p .r1'
  System::Call 'kernel32::GetLocalTime(p r1)'
  System::Call '*$1(&i2.r2,&i2.r3,&i2.r4,&i2.r5,&i2.r6,&i2.r7,&i2.r8,&i2.r9)'
  System::Free $1
  IntFmt $2 "%.4u" $2
  IntFmt $3 "%.2u" $3
  IntFmt $5 "%.2u" $5
  IntFmt $6 "%.2u" $6
  IntFmt $7 "%.2u" $7
  IntFmt $8 "%.2u" $8
  StrCpy $R2 "$2-$3-$5 $6:$7:$8"
FunctionEnd
!macroend
!insertmacro RIMESYNC_FN_STAMP ""
!insertmacro RIMESYNC_FN_STAMP "un."

; ---------------------------------------------------------------------------
; 顺序日志（安装）：入参 $R3 = 单行文本（保持 ASCII，避免代码页问题）
; 追加到 $INSTDIR\install-ime-order.log
; ---------------------------------------------------------------------------
Function RimeSyncLog
  ; $R3 = 一行文本（调用方已写入 $R2 时间戳与 phase=）
  ; 同时写两份：旧验收文件（0.6.4 及以前的路径）+ 0.6.5 统一 setup 日志（UTF-16LE）
  ClearErrors
  FileOpen $R4 "${RIMESYNC_ORDER_LOG}" a
  IfErrors rimesync_log_setup
    FileWrite $R4 "$R3$\r$\n"
    FileClose $R4
  rimesync_log_setup:
  ClearErrors
  FileOpen $R4 "${RIMESYNC_SETUP_LOG}" a
  IfErrors rimesync_log_done
    ; UTF-16LE：NSIS 只在文件为空时写 BOM，追加不会每行重复
    FileWriteUTF16LE $R4 "$R3$\r$\n"
    FileClose $R4
  rimesync_log_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 卸载日志：入参 $R3 = 单行文本
; 追加到 $TEMP\rime-sync-uninstall.log。与安装日志不同，它不在 $INSTDIR 里，
; 卸载器把程序目录删掉之后仍可复核（静默卸载唯一的证据）。
; ---------------------------------------------------------------------------
!macro RIMESYNC_FN_UNINSTALL_LOG UN
Function ${UN}RimeSyncUninstallLog
  ClearErrors
  FileOpen $R4 "${RIMESYNC_UNINSTALL_LOG}" a
  IfErrors rimesync_ulog_${UN}done
  FileWrite $R4 "$R3$\r$\n"
  FileClose $R4
  rimesync_ulog_${UN}done:
FunctionEnd
!macroend
!insertmacro RIMESYNC_FN_UNINSTALL_LOG "un."

; ---------------------------------------------------------------------------
; 推荐 Rime 配置播种（0.6.12：缺失才补，已存在沿用；0.6.13：写进真实用户目录）
;   目标目录：%APPDATA%\Rime\ —— 由 SetShellVarContext current 解析后存进
;   $RimeSyncRimeUserDir，本段所有写入（default.custom.yaml / weasel.custom.yaml /
;   schemas\<schema>.custom.yaml）只认这一个绝对路径，不再直接用 shell 变量 $APPDATA。
;   为什么：perMachine 安装器里 Tauri 模板已经 SetShellVarContext all，此时 $APPDATA
;   指向 C:\ProgramData；0.6.12 的实测机器上模板全落进了 C:\ProgramData\Rime，
;   用户目录 %APPDATA%\Rime 一个都没有。
;   规则：目录不存在就创建；目标已存在就沿用（不覆盖、不改名、不备份），缺失才写新版。
;   用户数据一律不动：本段只写上面三类 *.custom.yaml，写入清单里没有
;   *.userdb*、*.txt 词表、自定义词典，也没有任何按通配删除的动作。
;   日志：每个文件一行 seed file=<name> result=<new|reuse|failed>；
;         末尾一行 configs=/reused= 汇总，DONE 阶段把这两个数字报给用户。
; ---------------------------------------------------------------------------

; 单个文件：存在则备份后覆盖
; 入参：$0 = 模板源，$1 = 目标目录，$2 = 目标文件名（含扩展名）
; 出参：$3 = new | backup | failed，$5 = 备份路径（没备份则为空）
; 累加：$RimeSyncSeedWritten / $RimeSyncSeedBackups（调用方负责清零）
Function RimeSyncWriteConfigFile
  ; 0.6.12 起：用户目录里已有的配置**一律复用**，不覆盖、不改名。
  ; 只有缺失的文件才写入新版内容——卸载旧版时也不会删掉这些配置，所以升级后
  ; 用户的改造（含同步下来的设置）原样保留。
  StrCpy $3 "new"
  StrCpy $5 ""
  CreateDirectory "$1"
  IfFileExists "$1\*.*" 0 rimesync_wcfg_failed

  IfFileExists "$1\$2" rimesync_wcfg_reuse

  ClearErrors
  CopyFiles /SILENT "$0" "$1\$2"
  IfFileExists "$1\$2" 0 rimesync_wcfg_failed
  IntOp $RimeSyncSeedWritten $RimeSyncSeedWritten + 1
  Return

  rimesync_wcfg_reuse:
    IntOp $RimeSyncSeedReused $RimeSyncSeedReused + 1
    StrCpy $3 "reuse"
    Return

  rimesync_wcfg_failed:
  StrCpy $3 "failed"
FunctionEnd

; 记一行日志 + 进度面板提示
; 入参：$6 = 文件名，$3 = new|backup|failed，$5 = 备份路径（可空）
Function RimeSyncLogSeedResult
  StrCpy $R3 "[rime-sync] $R2 seed file=$6 result=$3"
  Call RimeSyncLog
  StrCmp $3 "reuse" 0 rimesync_seedlog_not_reuse
    DetailPrint "[rime-sync] $6 已存在，保留现有配置（沿用你的设置）"
    Return
  rimesync_seedlog_not_reuse:
  StrCmp $3 "failed" 0 rimesync_seedlog_ok
    DetailPrint "[rime-sync] 写入 $6 失败（目标 $1）"
    Return
  rimesync_seedlog_ok:
  DetailPrint "[rime-sync] 写入 $6（新版）"
FunctionEnd

Function RimeSyncSeedRimeConfig
  StrCpy $RimeSyncSeedWritten 0
  StrCpy $RimeSyncSeedBackups 0
  StrCpy $RimeSyncSeedReused 0

  ; 0.6.13：先切成 current 上下文把真实用户目录解析出来，再立刻切回 all。
  ; 只解析一次、后面全用绝对路径，避免任何一条写入路径再依赖 shell 变量上下文。
  SetShellVarContext current
  StrCpy $RimeSyncRimeUserDir "$APPDATA"
  ReadEnvStr $R0 "APPDATA"
  StrCmp $R0 "" rimesync_seed_context_ready
    StrCpy $RimeSyncRimeUserDir "$R0"
  rimesync_seed_context_ready:
  SetShellVarContext all
  StrCmp $RimeSyncRimeUserDir "" rimesync_seed_no_userdir
  StrCpy $RimeSyncRimeUserDir "$RimeSyncRimeUserDir\Rime"

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=POSTINSTALL 补写缺失的推荐配置（已存在沿用，目标 $RimeSyncRimeUserDir）"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL seed begin version=$RimeSyncSeedVersion policy=reuse-existing dir=$RimeSyncRimeUserDir context=current"
  Call RimeSyncLog

  ; 目录不存在就创建
  CreateDirectory "$RimeSyncRimeUserDir"
  IfFileExists "$RimeSyncRimeUserDir\*.*" rimesync_seed_dir_ready
    DetailPrint "[rime-sync] 无法创建 $RimeSyncRimeUserDir，跳过推荐配置写入"
    StrCpy $R3 "[rime-sync] $R2 seed skipped: cannot create Rime user dir dir=$RimeSyncRimeUserDir"
    Call RimeSyncLog
    Return
  rimesync_seed_dir_ready:

  ; 模板引用的方案必须真在 Weasel 的 data 里，否则 librime 把缺失方案算 failure、
  ; 整次部署失败。门禁只决定「写不写」，备份/覆盖规则不变。
  Call RimeSyncFindWeaselRoot
  StrCpy $RimeSyncSeedWeaselRoot $R8

  ; 1) default.custom.yaml：列了 16 个随包方案（含五笔），要求 data 里有 wubi86
  IfFileExists "$INSTDIR\resources\default.custom.yaml" rimesync_seed_default_check
    StrCpy $R3 "[rime-sync] $R2 seed skip default.custom.yaml: no template"
    Call RimeSyncLog
    Goto rimesync_seed_weasel
  rimesync_seed_default_check:
  StrCmp $RimeSyncSeedWeaselRoot "" rimesync_seed_default_gate
  IfFileExists "$RimeSyncSeedWeaselRoot\data\wubi86.schema.yaml" rimesync_seed_default_write
  rimesync_seed_default_gate:
    DetailPrint "[rime-sync] 小狼毫 data 没有 wubi86（官方安装包不含五笔），跳过推荐 default.custom.yaml，避免部署失败"
    StrCpy $R3 "[rime-sync] $R2 seed skip default.custom.yaml: no wubi86 in data"
    Call RimeSyncLog
    Goto rimesync_seed_weasel
  rimesync_seed_default_write:
    StrCpy $0 "$INSTDIR\resources\default.custom.yaml"
    StrCpy $1 "$RimeSyncRimeUserDir"
    StrCpy $2 "default.custom.yaml"
    Call RimeSyncWriteConfigFile
    StrCpy $6 "default.custom.yaml"
    Call RimeSyncLogSeedResult

  ; 2) weasel.custom.yaml：只影响外观（候选横向），不依赖 data，无条件写
  rimesync_seed_weasel:
  IfFileExists "$INSTDIR\resources\weasel.custom.yaml" rimesync_seed_weasel_check
    StrCpy $R3 "[rime-sync] $R2 seed skip weasel.custom.yaml: no template"
    Call RimeSyncLog
    Goto rimesync_seed_schemas
  rimesync_seed_weasel_check:
    StrCpy $0 "$INSTDIR\resources\weasel.custom.yaml"
    StrCpy $1 "$RimeSyncRimeUserDir"
    StrCpy $2 "weasel.custom.yaml"
    Call RimeSyncWriteConfigFile
    StrCpy $6 "weasel.custom.yaml"
    Call RimeSyncLogSeedResult

  ; 3) <schema>.custom.yaml：逐个模板判断 Weasel data 里有没有对应 <schema>.schema.yaml
  rimesync_seed_schemas:
  IfFileExists "$INSTDIR\resources\schemas\*.custom.yaml" rimesync_seed_schema_start
    StrCpy $R3 "[rime-sync] $R2 seed skip schemas: no templates"
    Call RimeSyncLog
    Goto rimesync_seed_summary
  rimesync_seed_schema_start:
  StrCmp $RimeSyncSeedWeaselRoot "" rimesync_seed_schema_no_weasel
  FindFirst $RimeSyncSeedHandle $RimeSyncSeedName "$INSTDIR\resources\schemas\*.custom.yaml"
  rimesync_seed_schema_loop:
  StrCmp $RimeSyncSeedName "" rimesync_seed_schema_end
    StrCpy $7 $RimeSyncSeedName -12                    ; 去掉 .custom.yaml → <schema>
    IfFileExists "$RimeSyncSeedWeaselRoot\data\$7.schema.yaml" rimesync_seed_schema_write rimesync_seed_schema_missing
    rimesync_seed_schema_write:
      StrCpy $0 "$INSTDIR\resources\schemas\$RimeSyncSeedName"
      StrCpy $1 "$RimeSyncRimeUserDir"
      StrCpy $2 "$RimeSyncSeedName"
      Call RimeSyncWriteConfigFile
      StrCpy $6 "$RimeSyncSeedName"
      Call RimeSyncLogSeedResult
      Goto rimesync_seed_schema_next
    rimesync_seed_schema_missing:
      StrCpy $R3 "[rime-sync] $R2 seed skip $RimeSyncSeedName: no $7.schema.yaml in data"
      Call RimeSyncLog
    rimesync_seed_schema_next:
    FindNext $RimeSyncSeedHandle $RimeSyncSeedName
    Goto rimesync_seed_schema_loop
  rimesync_seed_schema_end:
  FindClose $RimeSyncSeedHandle
  Goto rimesync_seed_summary
  rimesync_seed_schema_no_weasel:
    DetailPrint "[rime-sync] 没探测到小狼毫，跳过 schema 增强配置（无法确认方案存在）"
    StrCpy $R3 "[rime-sync] $R2 seed skip schemas: no weasel root"
    Call RimeSyncLog

  rimesync_seed_summary:
  DetailPrint "[rime-sync] 配置：新写入 $RimeSyncSeedWritten 个，沿用已有 $RimeSyncSeedReused 个（目标 $RimeSyncRimeUserDir）"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL seed done dir=$RimeSyncRimeUserDir configs=$RimeSyncSeedWritten reused=$RimeSyncSeedReused version=$RimeSyncSeedVersion"
  Call RimeSyncLog
  Return

  ; 解析不出用户目录：只记日志跳过（宁可少写，也不写进 ProgramData 或猜一个路径）
  rimesync_seed_no_userdir:
  DetailPrint "[rime-sync] 警告：解析不到当前用户的 %APPDATA%，跳过推荐配置写入"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL seed skipped reason=no-appdata"
  Call RimeSyncLog
FunctionEnd

; ---------------------------------------------------------------------------
; 探测：$R5 的候选目录命中则写入 $R8（未命中不动 $R8）
; Weasel 的 InstallDir 注册值指向父目录（<...>\Rime），真正的程序目录是它下面的
; weasel-<ver>；WeaselRoot 则直接是程序目录。两种布局都试，自定义安装目录才不会漏。
; ---------------------------------------------------------------------------
!macro RIMESYNC_FN_WEASEL_ROOT_FROM_DIR UN
Function ${UN}RimeSyncWeaselRootFromDir
  StrCmp $R5 "" rimesync_wrfd_${UN}done
  IfFileExists "$R5\WeaselServer.exe" rimesync_wrfd_${UN}direct rimesync_wrfd_${UN}try_versioned
  rimesync_wrfd_${UN}try_versioned:
  IfFileExists "$R5\weasel-${RIMESYNC_WEASEL_DIRVER}\WeaselServer.exe" rimesync_wrfd_${UN}versioned
  ; 版本号目录名不一定是随包的那一个（用户自己装的 0.18.x 之类）：枚举一遍
  ; weasel-* 子目录，命中就用。$0/$1 在这里当 scratch。
  FindFirst $0 $1 "$R5\weasel-*"
  rimesync_wrfd_${UN}scan:
    StrCmp $1 "" rimesync_wrfd_${UN}scan_end
    IfFileExists "$R5\$1\WeaselServer.exe" rimesync_wrfd_${UN}scan_hit
    FindNext $0 $1
    Goto rimesync_wrfd_${UN}scan
  rimesync_wrfd_${UN}scan_hit:
    StrCpy $R8 "$R5\$1"
  rimesync_wrfd_${UN}scan_end:
  FindClose $0
  Goto rimesync_wrfd_${UN}done
  rimesync_wrfd_${UN}direct:
    StrCpy $R8 "$R5"
    Goto rimesync_wrfd_${UN}done
  rimesync_wrfd_${UN}versioned:
    StrCpy $R8 "$R5\weasel-${RIMESYNC_WEASEL_DIRVER}"
  rimesync_wrfd_${UN}done:
FunctionEnd
!macroend
!insertmacro RIMESYNC_FN_WEASEL_ROOT_FROM_DIR ""
!insertmacro RIMESYNC_FN_WEASEL_ROOT_FROM_DIR "un."

; ---------------------------------------------------------------------------
; 探测：$R8 = 小狼毫安装根目录（含 WeaselServer.exe），未安装则空字符串
; 说明：Weasel 0.17 的安装根目录是 <Program Files>\Rime\weasel-0.17.4，
;       老版本是 <Program Files>\Rime\Weasel；自定义目录无法枚举，由注册表兜底。
; ---------------------------------------------------------------------------
!macro RIMESYNC_FN_FIND_WEASEL_ROOT UN
Function ${UN}RimeSyncFindWeaselRoot
  StrCpy $R8 ""
  ; 1) 注册表：Weasel 写 HKLM\Software\Rime\Weasel 的 WeaselRoot / InstallDir。
  ;    自定义安装目录只能靠这一条；64/32 位视图都读（Weasel 的 NSIS 是 32 位，
  ;    但 WeaselSetup 是原生 x64，两个视图都可能命中）。读完恢复 64 位视图
  ;    （本工程只出 x64/arm64 安装包，与 Tauri SetContext 的默认一致）。
  SetRegView 64
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"
  Call ${UN}RimeSyncWeaselRootFromDir
  StrCmp $R8 "" 0 rimesync_root_${UN}done
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"
  Call ${UN}RimeSyncWeaselRootFromDir
  StrCmp $R8 "" 0 rimesync_root_${UN}done
  SetRegView 32
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"
  Call ${UN}RimeSyncWeaselRootFromDir
  StrCmp $R8 "" 0 rimesync_root_${UN}done
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"
  Call ${UN}RimeSyncWeaselRootFromDir
  SetRegView 64
  StrCmp $R8 "" 0 rimesync_root_${UN}done
  ; 2) 默认安装目录
  StrCmp $PROGRAMFILES64 "" rimesync_root_${UN}pf32
    IfFileExists "$PROGRAMFILES64\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}\WeaselServer.exe" rimesync_root_${UN}set64ver
    IfFileExists "$PROGRAMFILES64\Rime\Weasel\WeaselServer.exe" rimesync_root_${UN}set64
  rimesync_root_${UN}pf32:
    IfFileExists "$PROGRAMFILES\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}\WeaselServer.exe" rimesync_root_${UN}set32ver
    IfFileExists "$PROGRAMFILES\Rime\Weasel\WeaselServer.exe" rimesync_root_${UN}set32
  Goto rimesync_root_${UN}done
  rimesync_root_${UN}set64ver:
    StrCpy $R8 "$PROGRAMFILES64\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}"
    Goto rimesync_root_${UN}done
  rimesync_root_${UN}set64:
    StrCpy $R8 "$PROGRAMFILES64\Rime\Weasel"
    Goto rimesync_root_${UN}done
  rimesync_root_${UN}set32ver:
    StrCpy $R8 "$PROGRAMFILES\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}"
    Goto rimesync_root_${UN}done
  rimesync_root_${UN}set32:
    StrCpy $R8 "$PROGRAMFILES\Rime\Weasel"
  rimesync_root_${UN}done:
FunctionEnd
!macroend
!insertmacro RIMESYNC_FN_FIND_WEASEL_ROOT ""
!insertmacro RIMESYNC_FN_FIND_WEASEL_ROOT "un."

; ---------------------------------------------------------------------------
; 0.6.14 就绪判定（安装前 / 安装后 / 自检三处共用）
;   「就绪」只有一个硬条件，下面两条同时成立才算，缺一即未就绪：
;     1) HKLM\Software\Rime\Weasel 的 InstallDir 或 WeaselRoot 存在
;        （Weasel 的安装器是 32 位 NSIS，值通常落在 Wow6432Node；32/64 位视图都读）；
;     2) 该值指向的目录里真的有 WeaselServer.exe（Weasel 0.17 的 InstallDir 写的是
;        父目录 <Program Files>\Rime，真正的程序目录是它下面的 weasel-<ver>，两种都认）。
;   TSF 文本服务注册、WeaselServer.exe 是否在跑：**只写日志**（RimeSyncWeaselLogEvidence
;   写 phase=TSF-EVIDENCE），绝不参与判定。0.6.13 的真机上 Weasel 已被卸掉，TSF 与注册表
;   残留还在，判定被残留骗成「已就绪」→ 跳过安装 → 机器上没有输入法。0.6.14 只认
;   「注册表指向的目录里真有 WeaselServer.exe」这一条：目录空 / exe 缺 / 注册表缺 = 未就绪。
;   出参：$RimeSyncWeaselReady / RootDir / ReadySrc / ReadyReason / RegDir，
;         证据 $RimeSyncWeaselTsfState / TsfDll / ProcState（只读，不参与判定）。
; ---------------------------------------------------------------------------
; 单个候选目录：$R5 = 注册值里的目录，$1 = 依据描述；命中就写就绪状态（保留第一处命中）
Function RimeSyncWeaselReadyFromDir
  StrCmp $R5 "" rimesync_wrfd_skip
  StrCmp $RimeSyncWeaselReady "1" rimesync_wrfd_skip
  ; $1 是依据标签（要写进日志）。RootFromDir 扫 weasel-* 时会用 $0/$1 作 scratch，
  ; 所以这里先压栈，别让依据被冲掉。
  Push $0
  Push $1
  StrCpy $R8 ""
  Call RimeSyncWeaselRootFromDir
  Pop $1
  Pop $0
  StrCmp $R8 "" rimesync_wrfd_no_exe
  StrCpy $RimeSyncWeaselReady "1"
  StrCpy $RimeSyncWeaselRootDir "$R8"
  StrCpy $RimeSyncWeaselReadySrc "$1"
  StrCpy $RimeSyncWeaselReadyReason "ok"
  Return
  rimesync_wrfd_no_exe:
    ; 注册表有值但目录里没有 WeaselServer.exe：记下原始目录，修复时整目录改名留证
    StrCpy $RimeSyncWeaselRegDir "$R5"
    StrCpy $RimeSyncWeaselReadyReason "no-WeaselServer.exe"
    ; 目录不存在 / 空目录分开记原因：日志上一眼看出是「空目录残留」还是「缺 exe」
    IfFileExists "$R5\*.*" rimesync_wrfd_reason_done
    IfFileExists "$R5" 0 rimesync_wrfd_reason_done
    StrCpy $RimeSyncWeaselReadyReason "empty-dir"
  rimesync_wrfd_reason_done:
    Return
  rimesync_wrfd_skip:
FunctionEnd

Function RimeSyncWeaselReady
  Push $1
  Push $R5

  StrCpy $RimeSyncWeaselReady "0"
  StrCpy $RimeSyncWeaselRootDir ""
  StrCpy $RimeSyncWeaselRegDir ""
  StrCpy $RimeSyncWeaselReadySrc "none"
  StrCpy $RimeSyncWeaselReadyReason "registry-missing"
  StrCpy $RimeSyncWeaselTsfState "no"
  StrCpy $RimeSyncWeaselTsfDll ""
  StrCpy $RimeSyncWeaselProcState "unknown"

  SetRegView 64
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"
  StrCpy $1 "HKLM\Software\Rime\Weasel\InstallDir(view=64)"
  Call RimeSyncWeaselReadyFromDir
  StrCmp $RimeSyncWeaselReady "1" rimesync_ready_done
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"
  StrCpy $1 "HKLM\Software\Rime\Weasel\WeaselRoot(view=64)"
  Call RimeSyncWeaselReadyFromDir
  StrCmp $RimeSyncWeaselReady "1" rimesync_ready_done

  SetRegView 32
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"
  StrCpy $1 "HKLM\Software\Rime\Weasel\InstallDir(view=32)"
  Call RimeSyncWeaselReadyFromDir
  StrCmp $RimeSyncWeaselReady "1" rimesync_ready_done
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"
  StrCpy $1 "HKLM\Software\Rime\Weasel\WeaselRoot(view=32)"
  Call RimeSyncWeaselReadyFromDir

  rimesync_ready_done:
  SetRegView 64
  ; 证据（判不出来不算未就绪，避免误判）
  Call RimeSyncWeaselTsfProbe
  Call RimeSyncWeaselServerRunning

  Pop $R5
  Pop $1
FunctionEnd

; TSF 文本服务注册（只作证据）：读 CTF\TIP 下 Weasel 的 CLSID 与 CLSID\InprocServer32。
; 注：WeaselTSF 源码里有个历史拼写 "Microsft"，系统真正写入的是 Microsoft；
; 这里只查 Microsoft，查不到就记 tsf=no，绝不影响就绪判定。
Function RimeSyncWeaselTsfProbe
  Push $0
  Push $1
  SetRegView 64
  StrCpy $RimeSyncWeaselTsfState "no"
  StrCpy $RimeSyncWeaselTsfDll ""
  EnumRegKey $0 HKLM "SOFTWARE\Microsoft\CTF\TIP\${RIMESYNC_TSF_CLSID}" 0
  ReadRegStr $1 HKLM "SOFTWARE\Classes\CLSID\${RIMESYNC_TSF_CLSID}\InprocServer32" ""
  StrCpy $RimeSyncWeaselTsfDll "$1"
  StrCmp $0 "" rimesync_tsf_dll_only
    StrCpy $RimeSyncWeaselTsfState "yes"
    Goto rimesync_tsf_done
  rimesync_tsf_dll_only:
    StrCmp $1 "" rimesync_tsf_done
    StrCpy $RimeSyncWeaselTsfState "yes"
  rimesync_tsf_done:
  SetRegView 64
  Pop $1
  Pop $0
FunctionEnd

; 进程证据（只作证据）：tasklist 找不到进程时输出的是本地化提示，所以只比输出开头
; 是不是 CSV 行首的 "WeaselServer.ex"；拿不到就记 unknown。
Function RimeSyncWeaselServerRunning
  Push $0
  Push $1
  StrCpy $RimeSyncWeaselProcState "unknown"
  nsExec::ExecToStack 'tasklist /FI "IMAGENAME eq WeaselServer.exe" /FO CSV /NH'
  Pop $0
  Pop $1
  StrCmp $0 "0" rimesync_proc_check
    Goto rimesync_proc_done
  rimesync_proc_check:
  StrCpy $0 $1 15
  StrCmp $0 '"WeaselServer.e' rimesync_proc_running
  StrCmp $1 "" rimesync_proc_done
  StrCpy $RimeSyncWeaselProcState "stopped"
  Goto rimesync_proc_done
  rimesync_proc_running:
    StrCpy $RimeSyncWeaselProcState "running"
  rimesync_proc_done:
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14：把 TSF / 进程证据写进日志（只写日志，绝不参与判定）。
;   入参：$R2 = 时间戳（调用方已 Call RimeSyncStamp）；$R3 临时占用后恢复。
; ---------------------------------------------------------------------------
Function RimeSyncWeaselLogEvidence
  Push $R3
  StrCpy $R3 "[rime-sync] $R2 phase=TSF-EVIDENCE tsf=$RimeSyncWeaselTsfState tsfdll=$RimeSyncWeaselTsfDll proc=$RimeSyncWeaselProcState note=log-only-not-used-for-readiness"
  Call RimeSyncLog
  Pop $R3
FunctionEnd

; ---------------------------------------------------------------------------
; 探测：$R9 = 已存在的 Weasel 程序组路径，没有则空字符串
; Weasel 安装器按界面语言建组：简体「小狼毫输入法」/ 繁体「小狼毫輸入法」/ 英文「Weasel」
; ---------------------------------------------------------------------------
Function RimeSyncFindWeaselGroup
  StrCpy $R9 ""
  IfFileExists "$SMPROGRAMS\小狼毫输入法\*.*" rimesync_group_hans
  IfFileExists "$SMPROGRAMS\小狼毫輸入法\*.*" rimesync_group_hant
  IfFileExists "$SMPROGRAMS\Weasel\*.*" rimesync_group_en
  Goto rimesync_group_done
  rimesync_group_hans:
    StrCpy $R9 "$SMPROGRAMS\小狼毫输入法"
    Goto rimesync_group_done
  rimesync_group_hant:
    StrCpy $R9 "$SMPROGRAMS\小狼毫輸入法"
    Goto rimesync_group_done
  rimesync_group_en:
    StrCpy $R9 "$SMPROGRAMS\Weasel"
  rimesync_group_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 读出 Weasel 自己的卸载命令：$R5 = 带引号的卸载器路径，找不到则空字符串。
; Weasel 的安装器是 32 位 NSIS，键写在 Wow6432Node（32 位视图）；个别构建/自定义
; 安装可能落到 64 位视图，两个都读。只碰 $R1 / $R5，不动 $R8。
; 值形如 "C:\Program Files\Rime\weasel-0.17.4\uninstall.exe"（已带引号）；
; 万一没带引号，这里补上，后面才能安全地拼 " /S"。
; ---------------------------------------------------------------------------
!macro RIMESYNC_FN_FIND_WEASEL_UNINST UN
Function ${UN}RimeSyncFindWeaselUninstallString
  StrCpy $R5 ""
  SetRegView 32
  ReadRegStr $R5 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Weasel" "UninstallString"
  StrCmp $R5 "" 0 rimesync_uninst_${UN}normalize
  SetRegView 64
  ReadRegStr $R5 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Weasel" "UninstallString"
  StrCmp $R5 "" rimesync_uninst_${UN}done
  rimesync_uninst_${UN}normalize:
    StrCpy $R1 $R5 1
    StrCmp $R1 '"' rimesync_uninst_${UN}done
    StrCpy $R5 '"$R5"'
  rimesync_uninst_${UN}done:
  SetRegView 64
FunctionEnd
!macroend
!insertmacro RIMESYNC_FN_FIND_WEASEL_UNINST ""
!insertmacro RIMESYNC_FN_FIND_WEASEL_UNINST "un."

; ---------------------------------------------------------------------------
; 写所有权标记（只在“本次安装前没有小狼毫、且内嵌安装器正常退出”的分支调用）
; 入参：$R8 = 小狼毫根目录（可为空），$R2 = 时间戳
; 写入失败极少见，只记日志，不阻断安装（卸载时会退回“保留小狼毫”）。
; ---------------------------------------------------------------------------
Function RimeSyncMarkBundledWeasel
  Call RimeSyncFindWeaselUninstallString
  ClearErrors
  SetRegView 64
  WriteRegStr HKLM "${RIMESYNC_REG_KEY}" "BundledWeasel" "1"
  WriteRegStr HKLM "${RIMESYNC_REG_KEY}" "WeaselRoot" "$R8"
  WriteRegStr HKLM "${RIMESYNC_REG_KEY}" "WeaselUninstall" "$R5"
  WriteRegStr HKLM "${RIMESYNC_REG_KEY}" "BundledAt" "$R2"
  IfErrors rimesync_mark_failed
    DetailPrint "[rime-sync] 已记录小狼毫所有权：卸载 rime-sync 时会连小狼毫一起卸载（用户数据保留）"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL ownership marker written"
    Call RimeSyncLog
    Return
  rimesync_mark_failed:
    DetailPrint "[rime-sync] 警告：写小狼毫所有权标记失败，卸载 rime-sync 时将保留小狼毫"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL ownership marker write failed"
    Call RimeSyncLog
FunctionEnd

; ---------------------------------------------------------------------------
; 系统版本门槛：Windows 10 1803（10.0.17134）及以上
;   ${AtLeastWin10} 只看主版本号（Win10 / Win11 都为真），构建号再拦 1803 以下。
;   低于门槛：写日志 → 弹框说明 → Abort（不写任何文件、不装输入法）。
; ---------------------------------------------------------------------------
Function RimeSyncCheckWindows
  Push $0
  Push $1
  Push $R2
  Push $R3

  Call RimeSyncStamp
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  ; 只取前 5 位：个别系统/预览版写成 "17134.1"；Win10 1803+ 的构建号都是 5 位。
  StrCpy $1 $0 5

  ${If} ${AtLeastWin10}
    ${If} $1 == ""
      ; 读不到构建号但系统确实是 Windows 10+：放行（宁可装上，也不要误拦）
      Goto rimesync_win_ok
    ${EndIf}
    ${If} $1 >= ${RIMESYNC_MIN_WIN10_BUILD}
      Goto rimesync_win_ok
    ${EndIf}
  ${EndIf}

  DetailPrint "[rime-sync] 系统版本过低（build=$0），要求 Windows 10 1803（10.0.17134）或更高"
  StrCpy $R3 "[rime-sync] $R2 phase=OS-CHECK pass=no build=$0 min=${RIMESYNC_MIN_WIN10_BUILD} abort"
  Call RimeSyncLog
  IntCmp $LANGUAGE 2052 rimesync_win_zh rimesync_win_en rimesync_win_en
  rimesync_win_zh:
    MessageBox MB_OK|MB_ICONSTOP "本安装包需要 Windows 10 1803（10.0.17134）或更高版本。$\r$\n$\r$\n当前系统构建号：$0$\r$\n$\r$\n请先升级 Windows，再重新运行本安装包。安装已中止，未写入任何文件。"
    Goto rimesync_win_abort
  rimesync_win_en:
    MessageBox MB_OK|MB_ICONSTOP "This installer requires Windows 10 version 1803 (10.0.17134) or later.$\r$\n$\r$\nCurrent build number: $0$\r$\n$\r$\nUpdate Windows first, then run this installer again. Setup was aborted; no files were written."
  rimesync_win_abort:
  SetErrorLevel 3
  Abort

  rimesync_win_ok:
  StrCpy $R3 "[rime-sync] $R2 phase=OS-CHECK pass=yes build=$0"
  Call RimeSyncLog
  Pop $R3
  Pop $R2
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; WebView2 探测（0.6.5）
;   $R1 = 注册表 pv 原始值 → 有效则 $R7=1
;   pv 为空 / "0" / "0.0.0.0" 都算没装（卸载残键常见）
; ---------------------------------------------------------------------------
Function RimeSyncWebView2PvValid
  StrCmp $R1 "" rimesync_wvpv_done
  StrCmp $R1 "0" rimesync_wvpv_done
  StrCmp $R1 "0.0.0.0" rimesync_wvpv_done
  StrCpy $R7 "1"
  rimesync_wvpv_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 探测 WebView2 运行时：$R7=1 已装，$R5=版本号（或 "file-only"）
;   1) HKLM / HKCU 的 EdgeUpdate Clients\{GUID}\pv，64 位与 32 位视图都读
;      （32 位视图下 HKLM\Software 自动映射到 WOW6432Node，x64 运行时通常写那里）
;   2) 注册表没写（精简/绿色安装）时，退回程序文件：EdgeWebView\Application\*\msedgewebview2.exe
;   退出时把注册表视图恢复成 64 位（本工程只出 x64 安装包，与 Tauri 默认一致）
; ---------------------------------------------------------------------------
Function RimeSyncFindWebView2
  Push $R1
  StrCpy $R5 ""
  StrCpy $R7 "0"

  SetRegView 64
  ReadRegStr $R1 HKLM "Software\Microsoft\EdgeUpdate\Clients\${RIMESYNC_WEBVIEW_GUID}" "pv"
  Call RimeSyncWebView2PvValid
  StrCmp $R7 "1" rimesync_wv_hit
  ReadRegStr $R1 HKCU "Software\Microsoft\EdgeUpdate\Clients\${RIMESYNC_WEBVIEW_GUID}" "pv"
  Call RimeSyncWebView2PvValid
  StrCmp $R7 "1" rimesync_wv_hit

  SetRegView 32
  ReadRegStr $R1 HKLM "Software\Microsoft\EdgeUpdate\Clients\${RIMESYNC_WEBVIEW_GUID}" "pv"
  Call RimeSyncWebView2PvValid
  StrCmp $R7 "1" rimesync_wv_hit
  ReadRegStr $R1 HKCU "Software\Microsoft\EdgeUpdate\Clients\${RIMESYNC_WEBVIEW_GUID}" "pv"
  Call RimeSyncWebView2PvValid
  StrCmp $R7 "1" rimesync_wv_hit
  SetRegView 64

  ; 注册表兜底：程序文件在就算已装
  StrCmp $PROGRAMFILES64 "" rimesync_wv_file_pf32
    IfFileExists "$PROGRAMFILES64\Microsoft\EdgeWebView\Application\*\msedgewebview2.exe" rimesync_wv_file
  rimesync_wv_file_pf32:
    IfFileExists "$PROGRAMFILES\Microsoft\EdgeWebView\Application\*\msedgewebview2.exe" rimesync_wv_file
  Goto rimesync_wv_done

  rimesync_wv_file:
    StrCpy $R7 "1"
    StrCpy $R5 "file-only"
    Goto rimesync_wv_done

  rimesync_wv_hit:
    StrCpy $R5 $R1

  rimesync_wv_done:
  SetRegView 64
  Pop $R1
FunctionEnd

; ---------------------------------------------------------------------------
; 中止（缺 WebView2 / 内嵌安装失败）：绝不回退成联网下载
;   入参：$R0 = 中文附加原因，$R1 = 英文附加原因（正文常量见 RIMESYNC_WEBVIEW_MSG_*）
;   调用方负责先写日志（说明为什么失败），这里只负责弹框与 Abort。
;   只有离线包（内嵌安装器失败 / 载荷缺失）会用到它，所以按 !ifdef 编译，
;   否则在线包里会触发「not referenced」告警（makensis -WX 下等于失败）。
; ---------------------------------------------------------------------------
!ifdef RIMESYNC_EMBED_WEBVIEW
Function RimeSyncAbortNoWebView2
  IntCmp $LANGUAGE 2052 rimesync_abort_zh rimesync_abort_en rimesync_abort_en
  rimesync_abort_zh:
    MessageBox MB_OK|MB_ICONSTOP "${RIMESYNC_WEBVIEW_MSG_ZH}$R0"
    Goto rimesync_abort_end
  rimesync_abort_en:
    MessageBox MB_OK|MB_ICONSTOP "${RIMESYNC_WEBVIEW_MSG_EN}$R1"
  rimesync_abort_end:
  SetErrorLevel 4
  Abort
FunctionEnd
!endif

; ---------------------------------------------------------------------------
; WebView2：已装就过；没装按「离线包 / /SKIPWEBVIEW / 弹框询问」三条路走
;   - 这里绝不下载任何东西：在线包只给地址，离线包只用内嵌的 Standalone 安装器。
;   - 每条路径（含失败）都写 phase=WEBVIEW-CHECK 或 phase=WEBVIEW-INSTALL 日志。
; ---------------------------------------------------------------------------
Function RimeSyncEnsureWebView2
  Push $0
  Push $1
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R5
  Push $R7
  Push $R8

  Call RimeSyncFindWebView2
  StrCmp $R7 "1" rimesync_wvc_present

  ; ---- 未安装 ----
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=no"
  Call RimeSyncLog
  DetailPrint "[rime-sync] 未检测到 Microsoft Edge WebView2 运行时"

  ; /SKIPWEBVIEW：明确跳过（不联网、不弹框、不阻断）
  ${GetOptions} $CMDLINE "/SKIPWEBVIEW" $R0
  IfErrors rimesync_wvc_offline
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=no skip=1 switch=/SKIPWEBVIEW note=app-may-not-start"
    Call RimeSyncLog
    DetailPrint "[rime-sync] 收到 /SKIPWEBVIEW：跳过 WebView2，继续安装（应用可能无法启动）"
    Goto rimesync_wvc_done

  rimesync_wvc_offline:
  !ifdef RIMESYNC_EMBED_WEBVIEW
    !ifdef RIMESYNC_WEBVIEW_PAYLOAD
      StrCmp $PLUGINSDIR "" 0 rimesync_wvc_have_plugins
        InitPluginsDir
      rimesync_wvc_have_plugins:
      SetOutPath "$PLUGINSDIR\rime-sync-webview"
      File "/oname=${RIMESYNC_WEBVIEW_INSTALLER}" "${RIMESYNC_WEBVIEW_EXE}"
      StrCpy $0 "$PLUGINSDIR\rime-sync-webview\${RIMESYNC_WEBVIEW_INSTALLER}"
      SetOutPath "$INSTDIR"

      Call RimeSyncStamp
      StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-INSTALL begin embedded=1 args=/silent /install"
      Call RimeSyncLog
      DetailPrint "[rime-sync] 正在静默安装内嵌的 WebView2 运行时（不联网）"
      ClearErrors
      ExecWait '"$0" /silent /install' $1
      IfErrors rimesync_wvc_launch_failed
      StrCpy $R8 $1
      Call RimeSyncStamp
      StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-INSTALL exit=$R8"
      Call RimeSyncLog
      Goto rimesync_wvc_recheck

      rimesync_wvc_launch_failed:
        StrCpy $R8 "exec-error"
        Call RimeSyncStamp
        StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-INSTALL exit=exec-error"
        Call RimeSyncLog

      rimesync_wvc_recheck:
      ; 退出码非 0 也可能已经装上（有的版本会返回「需要重启」）：一律复查一次
      Call RimeSyncFindWebView2
      StrCmp $R7 "1" rimesync_wvc_install_ok
      ; 内嵌安装失败 → 弹框说明并 Abort（不回退联网）
      DetailPrint "[rime-sync] 警告：内嵌 WebView2 安装失败（exit=$R8），安装中止"
      StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-INSTALL installed=no exit=$R8 abort"
      Call RimeSyncLog
      StrCpy $R0 "内嵌安装器退出码：$R8。"
      StrCpy $R1 "The bundled installer exited with code $R8."
      Call RimeSyncAbortNoWebView2

      rimesync_wvc_install_ok:
        Call RimeSyncStamp
        StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-INSTALL installed=yes pv=$R5"
        Call RimeSyncLog
        DetailPrint "[rime-sync] WebView2 运行时安装完成（$R5）"
        Delete "$PLUGINSDIR\rime-sync-webview\${RIMESYNC_WEBVIEW_INSTALLER}"
        RMDir "$PLUGINSDIR\rime-sync-webview"
        SetOutPath "$INSTDIR"
        Goto rimesync_wvc_done
    !else
      ; 离线构建缺陷：编译期已 !warning；运行时显式失败，避免静默装出跑不起来的程序
      DetailPrint "[rime-sync] 警告：离线包缺少内嵌 WebView2 安装器，安装中止"
      StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-INSTALL installed=no payload=missing abort"
      Call RimeSyncLog
      StrCpy $R0 "本离线安装包里没有内置 WebView2 安装器（构建缺陷），请改用在线安装包。"
      StrCpy $R1 "This offline package has no bundled WebView2 installer (build error); use the online installer instead."
      Call RimeSyncAbortNoWebView2
    !endif
  !else
    ; 在线包：只给下载地址，绝不自动下载（0.6.4 的 embedBootstrapper 就是这么卡死的）
    IfSilent rimesync_wvc_silent_abort
    IntCmp $LANGUAGE 2052 rimesync_wvc_prompt_zh rimesync_wvc_prompt_en rimesync_wvc_prompt_en
    rimesync_wvc_prompt_zh:
      MessageBox MB_YESNO|MB_ICONEXCLAMATION "未检测到 Microsoft Edge WebView2 运行时。$\r$\n$\r$\nrime-sync 需要它才能显示界面。本安装包不会联网下载，请先手动安装（在线安装器约 1.5 MB；内网/受限代理请下载离线安装包，约 130 MB）：$\r$\n${RIMESYNC_WEBVIEW_URL}$\r$\n$\r$\n「是」= 我已装好，继续安装$\r$\n「否」= 退出安装（不写入任何文件）" IDYES rimesync_wvc_prompt_ok IDNO rimesync_wvc_prompt_quit
      Goto rimesync_wvc_prompt_quit
    rimesync_wvc_prompt_en:
      MessageBox MB_YESNO|MB_ICONEXCLAMATION "Microsoft Edge WebView2 Runtime is not installed.$\r$\n$\r$\nrime-sync needs it to show its window. This installer never downloads anything; install WebView2 yourself first (about 1.5 MB online bootstrapper; behind a restricted proxy use the offline installer, about 130 MB):$\r$\n${RIMESYNC_WEBVIEW_URL}$\r$\n$\r$\nYes = I have installed it, continue$\r$\nNo  = Abort setup (no files written)" IDYES rimesync_wvc_prompt_ok IDNO rimesync_wvc_prompt_quit
      Goto rimesync_wvc_prompt_quit

    rimesync_wvc_prompt_ok:
      ; 复查一次：用户可能刚装完（新装的可能还没写注册表，只记日志不拦）
      Call RimeSyncFindWebView2
      Call RimeSyncStamp
      StrCmp $R7 "1" rimesync_wvc_prompt_ok_found
        StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=no user=continue note=still-missing"
        Call RimeSyncLog
        DetailPrint "[rime-sync] 用户确认已装 WebView2，但复查仍未探测到；继续安装"
        Goto rimesync_wvc_done
      rimesync_wvc_prompt_ok_found:
        StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=yes pv=$R5 user=continue"
        Call RimeSyncLog
        DetailPrint "[rime-sync] 复查到 WebView2（$R5），继续安装"
        Goto rimesync_wvc_done

    rimesync_wvc_prompt_quit:
      Call RimeSyncStamp
      StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=no user=quit abort"
      Call RimeSyncLog
      SetErrorLevel 4
      Abort

    rimesync_wvc_silent_abort:
      Call RimeSyncStamp
      StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=no silent=1 abort note=use-/SKIPWEBVIEW"
      Call RimeSyncLog
      DetailPrint "[rime-sync] 静默安装且缺 WebView2：中止（如需继续请加 /SKIPWEBVIEW）"
      SetErrorLevel 4
      Abort
  !endif

  ; ---- 已安装 ----
  rimesync_wvc_present:
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=WEBVIEW-CHECK installed=yes pv=$R5"
  Call RimeSyncLog
  DetailPrint "[rime-sync] 已检测到 WebView2 运行时（$R5），跳过安装"

  rimesync_wvc_done:
  SetRegView 64
  Pop $R8
  Pop $R7
  Pop $R5
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 升级即全新安装（0.6.11）
;   顺序：停进程 → 卸旧同步程序 → 卸旧自建输入法 → 清所有权标记 → PREINSTALL 继续
;   任何一步失败都只写日志、绝不 Abort：装上一个能用的新版，比半路退出好。
;   阶段名统一记 UNINSTALL-OLD（含被卸载对象、退出码、耗时毫秒）。
; ---------------------------------------------------------------------------

; 枚举 Uninstall 注册项找我们自己的条目（SLOT = 32 / 64 位视图）。
; 命中时写 $RimeSyncOldUninstKey（子键名）与 $RimeSyncOldUninstRaw（UninstallString）；
; 归一化放到宏外面做，免得冲掉循环里的 $0-$3。
!macro RIMESYNC_SCAN_OUR_UNINST SLOT
  SetRegView ${SLOT}
  StrCpy $0 0
  rimesync_scan_${SLOT}_loop:
    EnumRegKey $1 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall" $0
    StrCmp $1 "" rimesync_scan_${SLOT}_end
    IntOp $0 $0 + 1
    ReadRegStr $2 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$1" "DisplayName"
    StrCpy $3 $2 9
    StrCmp $3 "rime-sync" rimesync_scan_${SLOT}_hit
    StrCmp $2 "${PRODUCTNAME}" rimesync_scan_${SLOT}_hit
    ReadRegStr $3 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$1" "Publisher"
    StrCmp $3 "rime-sync" rimesync_scan_${SLOT}_hit
    Goto rimesync_scan_${SLOT}_loop
  rimesync_scan_${SLOT}_hit:
    StrCpy $RimeSyncOldUninstKey "$1"
    ReadRegStr $RimeSyncOldUninstRaw HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$1" "UninstallString"
  rimesync_scan_${SLOT}_end:
  SetRegView 64
!macroend

; ---------------------------------------------------------------------------
; 归一化注册表 UninstallString → $R9 = 现有卸载器完整路径（不带引号），否则空字符串。
; 只认「整个字符串就是一个 exe 路径」的两种写法：
;   "C:\Program Files\Rime\weasel-0.17.4\uninstall.exe"   （NSIS 卸载器，带引号）
;   C:\Rime\weasel\uninstall.exe                          （万一没带引号，整体当路径）
; 带附加参数（"x.exe" /S）的写法不猜，直接判无效，交给调用方兜底。
; 路径不存在同样判无效（旧版被手动删掉的情况）。
; 注：不用 StrFunc.nsh 的 ${StrStr}——它要现生成函数，与 Tauri 模板的 include 顺序纠缠不清。
; ---------------------------------------------------------------------------
Function RimeSyncNormalizeUninstallPath
  StrCmp $R9 "" rimesync_nup_done
  StrCpy $1 $R9 1
  StrCmp $1 '"' 0 rimesync_nup_check
    StrCpy $2 $R9 "" 1                       ; 去掉起始引号
    StrLen $3 $2
    IntCmp $3 0 rimesync_nup_clear
    IntOp $3 $3 - 1
    StrCpy $4 $2 1 $3                        ; 最后一个字符
    StrCmp $4 '"' 0 rimesync_nup_clear       ; 末尾不是引号 → 带参数，不认
    StrCpy $2 $2 -1                          ; 去掉末尾引号
    StrCpy $R9 $2
  rimesync_nup_check:
  IfFileExists "$R9" rimesync_nup_done
  rimesync_nup_clear:
    StrCpy $R9 ""
  rimesync_nup_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 执行命令行并等待进程退出，最多 RIMESYNC_UNINSTALL_TIMEOUT_MS 毫秒。
; 用 nsExec::ExecToStack /TIMEOUT 代替 ExecWait：ExecWait 没有超时，卸载器一旦卡住
; 安装器就永远停在那儿；nsExec 到点结束进程并返回 "timeout"。
; 入参：$R9 = 完整命令行（可执行文件已带引号，参数在后）
; 出参：$0 = 退出码字符串（"0" / "timeout" / "error" / 其他），$1 = 耗时毫秒
; 说明：$2-$4 被当 scratch；失败只返回码，绝不 Abort。
; ---------------------------------------------------------------------------
Function RimeSyncRunAndWait
  DetailPrint "[rime-sync] 执行：$R9（最多等待 ${RIMESYNC_UNINSTALL_TIMEOUT_MS} ms）"
  System::Call 'kernel32::GetTickCount() i .r3'
  nsExec::ExecToStack /TIMEOUT=${RIMESYNC_UNINSTALL_TIMEOUT_MS} '$R9'
  Pop $0
  Pop $2                                     ; 卸载器输出：丢弃（只留退出码，免得日志被刷满）
  System::Call 'kernel32::GetTickCount() i .r4'
  IntOp $1 $4 - $3
  IntCmp $1 0 rimesync_runwait_done rimesync_runwait_wrap rimesync_runwait_done
  rimesync_runwait_wrap:
    StrCpy $1 0                              ; GetTickCount 49 天回绕 → 按 0 记
  rimesync_runwait_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 先停掉可能占着旧文件的进程（静默 /F）。
; 不杀的话旧卸载器删不掉 exe/dll，会白等到超时；taskkill 找不到进程返回非 0，忽略。
; ---------------------------------------------------------------------------
Function RimeSyncKillRunningProcesses
  StrCpy $R9 'taskkill /IM rime-sync-app.exe /F'
  Call RimeSyncRunAndWait
  StrCpy $R9 'taskkill /IM WeaselServer.exe /F'
  Call RimeSyncRunAndWait
FunctionEnd


; ---------------------------------------------------------------------------
; 0.6.13：文件名安全的时间戳 $R2 = "YYYYMMDD-HHMMSS"（Rime.bak-<ts> 用）
; 占用 $1..$9 作 scratch，与 RimeSyncStamp 一样不碰 $0 / $R0..$R9。
; ---------------------------------------------------------------------------
Function RimeSyncStampCompact
  System::Call '*(&i2,&i2,&i2,&i2,&i2,&i2,&i2,&i2) p .r1'
  System::Call 'kernel32::GetLocalTime(p r1)'
  System::Call '*$1(&i2.r2,&i2.r3,&i2.r4,&i2.r5,&i2.r6,&i2.r7,&i2.r8,&i2.r9)'
  System::Free $1
  IntFmt $2 "%.4u" $2
  IntFmt $3 "%.2u" $3
  IntFmt $5 "%.2u" $5
  IntFmt $6 "%.2u" $6
  IntFmt $7 "%.2u" $7
  IntFmt $8 "%.2u" $8
  StrCpy $R2 "$2$3$5-$6$7$8"
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14：不就绪时把「非空且真有 WeaselServer.exe」的残留目录改名留证（绝不删除，
;   复查失败时按原名改回，见 RimeSyncRollbackWeasel）。
;   候选：注册表里读到的目录 → <Program Files>\Rime（64 位）→ <Program Files>\Rime
;   新名字：<原目录>.bak-YYYYMMDD-HHMMSS（时间戳保证不撞已有备份）
;   改名前必须过 RimeSyncWeaselRootFromDir（目录里真有 exe）：空目录 / 缺 exe 的目录
;   没有可恢复的程序，改名只会把证据藏起来 —— 直接跳过并记日志。
;   出参：$RimeSyncWeaselBackupDir = 改名后的路径（没改名则空）
;         $RimeSyncWeaselBackupOrig = 改名前的原目录
;   失败只记日志：文件被占用时也继续跑安装器，最终由复查/回滚决定成败。
; ---------------------------------------------------------------------------
Function RimeSyncBackupLeftoverDir
  Push $0
  Push $1
  Push $R5
  Push $R8
  Push $9
  StrCpy $9 "$0"
  StrCmp $9 "" rimesync_bk_done
  StrCmp $RimeSyncWeaselBackupDir "" 0 rimesync_bk_done
  IfFileExists "$9\*.*" 0 rimesync_bk_skip
  ; 目录里必须真有 WeaselServer.exe（直接布局与 weasel-<ver> 子目录都认）
  StrCpy $R5 "$9"
  StrCpy $R8 ""
  Call RimeSyncWeaselRootFromDir
  StrCmp $R8 "" rimesync_bk_skip
  StrCpy $1 "$9.bak-$R2"
  ClearErrors
  Rename "$9" "$1"
  IfErrors rimesync_bk_failed
  IfFileExists "$1\*.*" rimesync_bk_ok
  IfFileExists "$1" 0 rimesync_bk_failed
  rimesync_bk_ok:
    StrCpy $RimeSyncWeaselBackupDir "$1"
    StrCpy $RimeSyncWeaselBackupOrig "$9"
    DetailPrint "[rime-sync] 已把残留的小狼毫目录改名留证：$9 → $1"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-REPAIR rename ok from=$9 to=$1 exe=$R8"
    Call RimeSyncLog
    Goto rimesync_bk_done
  rimesync_bk_failed:
    DetailPrint "[rime-sync] 警告：无法把残留目录改名（$9 → $1），继续尝试安装"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-REPAIR rename failed from=$9 to=$1"
    Call RimeSyncLog
    Goto rimesync_bk_done
  rimesync_bk_skip:
    StrCpy $R3 "[rime-sync] $R2 phase=IME-REPAIR rename skipped dir=$9 reason=empty-or-no-WeaselServer.exe"
    Call RimeSyncLog
  rimesync_bk_done:
  Pop $9
  Pop $R8
  Pop $R5
  Pop $1
  Pop $0
FunctionEnd

Function RimeSyncBackupLeftoverWeasel
  Push $0
  Call RimeSyncStampCompact
  StrCpy $RimeSyncWeaselBackupDir ""
  StrCpy $RimeSyncWeaselBackupOrig ""
  StrCpy $0 "$RimeSyncWeaselRegDir"
  Call RimeSyncBackupLeftoverDir
  StrCpy $0 "$PROGRAMFILES64\Rime"
  Call RimeSyncBackupLeftoverDir
  StrCpy $0 "$PROGRAMFILES\Rime"
  Call RimeSyncBackupLeftoverDir
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14：复查仍不就绪 → 把改过名的目录改回原名（回滚），别把机器留在「没有输入法」。
;   入参：$RimeSyncWeaselBackupDir / $RimeSyncWeaselBackupOrig（改名函数写的），
;         $R2 = 紧凑时间戳（RimeSyncStampCompact 的产物，用来拼 .incomplete-<ts>）
;   出参：$RimeSyncWeaselRollback = ok / failed / none
;         ok     = 目录已改回原名，且里面能找到 WeaselServer.exe
;                  （只代表文件回来了：注册表可能仍缺失，下次安装会再修）
;         failed = 改名失败（文件被占用等），需要人工处理
;         none   = 本次没改过名，没有可回滚的东西
;   绝不 Abort：回滚失败也继续走硬失败路径，把原因写进日志。
; ---------------------------------------------------------------------------
Function RimeSyncRollbackWeasel
  Push $0
  Push $1
  Push $R5
  Push $R8
  StrCpy $RimeSyncWeaselRollback "none"
  StrCmp $RimeSyncWeaselBackupDir "" rimesync_rb_done
  ; $R2 可能已被正常时间戳覆盖（含空格与冒号，不能进文件名）→ 重新取紧凑时间戳
  Call RimeSyncStampCompact
  IfFileExists "$RimeSyncWeaselBackupDir\*.*" rimesync_rb_have
  IfFileExists "$RimeSyncWeaselBackupDir" 0 rimesync_rb_done
  rimesync_rb_have:
  ; 安装器可能已经建了同名目录：先挪到 <原目录>.incomplete-<ts> 留证，别覆盖新证据
  IfFileExists "$RimeSyncWeaselBackupOrig\*.*" rimesync_rb_move
  IfFileExists "$RimeSyncWeaselBackupOrig" rimesync_rb_move
  Goto rimesync_rb_rename
  rimesync_rb_move:
    Rename "$RimeSyncWeaselBackupOrig" "$RimeSyncWeaselBackupOrig.incomplete-$R2"
  rimesync_rb_rename:
  ClearErrors
  Rename "$RimeSyncWeaselBackupDir" "$RimeSyncWeaselBackupOrig"
  IfErrors rimesync_rb_failed
  ; 恢复成功的判据：原名目录里又能找到 WeaselServer.exe
  StrCpy $R5 "$RimeSyncWeaselBackupOrig"
  StrCpy $R8 ""
  Call RimeSyncWeaselRootFromDir
  StrCmp $R8 "" rimesync_rb_failed
  StrCpy $RimeSyncWeaselRollback "ok"
  StrCpy $RimeSyncWeaselRootDir "$R8"
  Goto rimesync_rb_log
  rimesync_rb_failed:
    StrCpy $RimeSyncWeaselRollback "failed"
  rimesync_rb_log:
  DetailPrint "[rime-sync] 回滚：$RimeSyncWeaselBackupDir → $RimeSyncWeaselBackupOrig（$RimeSyncWeaselRollback）"
  StrCpy $R3 "[rime-sync] $R2 phase=WEASEL-ROLLBACK result=$RimeSyncWeaselRollback from=$RimeSyncWeaselBackupDir to=$RimeSyncWeaselBackupOrig exe=$R8"
  Call RimeSyncLog
  rimesync_rb_done:
  Pop $R8
  Pop $R5
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.13：跑内嵌小狼毫安装器并等待退出，最多 ${RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS}（300 秒）。
; 与卸载用的 RimeSyncRunAndWait 分开：两次等待上限不同，而 nsExec 的 /TIMEOUT 必须在
; 编译期就是常量。入参 $R9 = 完整命令行；出参 $0 = 退出码（"0"/"timeout"/"error"/其它），
; $1 = 耗时毫秒（与 RimeSyncRunAndWait 一致）。
; ---------------------------------------------------------------------------
Function RimeSyncRunWeaselInstaller
  DetailPrint "[rime-sync] 执行：$R9（最多等待 ${RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS} ms）"
  System::Call 'kernel32::GetTickCount() i .r3'
  nsExec::ExecToStack /TIMEOUT=${RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS} '$R9'
  Pop $0
  Pop $2
  System::Call 'kernel32::GetTickCount() i .r4'
  IntOp $1 $4 - $3
  IntCmp $1 0 rimesync_wiw_done rimesync_wiw_wrap rimesync_wiw_done
  rimesync_wiw_wrap:
    StrCpy $1 0                              ; GetTickCount 49 天回绕 → 按 0 记
  rimesync_wiw_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.13：读文件时间（自检判断 build\luna_pinyin_simp.schema.yaml 是「更新」还是「没动」）
; 入参：$R9 = 文件路径
; 出参：$RimeSyncFileExists = 1/0；$RimeSyncFileHi / Lo = FILETIME 高/低 32 位（十进制）
;       读不到就留空，调用方按「判不出来」处理（只告警，绝不据此中止安装）。
; 说明：只用 System 插件调 kernel32，不引入额外插件；失败路径一律安静返回。
; ---------------------------------------------------------------------------
Function RimeSyncFileTimeGet
  Push $1
  Push $2
  Push $3
  Push $4

  StrCpy $RimeSyncFileExists "0"
  StrCpy $RimeSyncFileHi ""
  StrCpy $RimeSyncFileLo ""
  IfFileExists "$R9" 0 rimesync_ft_done
  StrCpy $RimeSyncFileExists "1"

  ClearErrors
  System::Call 'kernel32::CreateFileW(w "$R9", i 0x80000000, i 3, p 0, i 3, i 0x80, p 0) p .r1'
  IfErrors rimesync_ft_done
  StrCmp $1 "0" rimesync_ft_done
  StrCmp $1 "-1" rimesync_ft_done
  StrCmp $1 "4294967295" rimesync_ft_done
  System::Call '*(&i4, &i4) p .r2'
  System::Call 'kernel32::GetFileTime(p r1, p 0, p 0, p r2)'
  System::Call '*$r2(&i4.r3, &i4.r4)'
  System::Free $r2
  System::Call 'kernel32::CloseHandle(p r1)'
  StrCpy $RimeSyncFileHi "$3"
  StrCpy $RimeSyncFileLo "$4"

  rimesync_ft_done:
  Pop $4
  Pop $3
  Pop $2
  Pop $1
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14：自检硬性检查 B 的扫描器 —— 默认布局 <base>\Rime\weasel-*\WeaselServer.exe。
;   入参：$R5 = 基目录（$PROGRAMFILES64 / $PROGRAMFILES）
;   出参：$RimeSyncWeaselDefaultExe = yes / no；$9 = 命中的 exe 路径（写日志用）
;   只读枚举，找不到就安静返回（判的是「有没有」，不抛错、不 Abort）。
; ---------------------------------------------------------------------------
Function RimeSyncScanDefaultLayout
  Push $0
  Push $1
  Push $2
  StrCmp $R5 "" rimesync_sdl_done
  StrCmp $RimeSyncWeaselDefaultExe "yes" rimesync_sdl_done
  StrCpy $2 "$R5\Rime"
  ClearErrors
  FindFirst $0 $1 "$2\weasel-*"
  IfErrors rimesync_sdl_done
  rimesync_sdl_loop:
    StrCmp $1 "" rimesync_sdl_close
    IfFileExists "$2\$1\WeaselServer.exe" rimesync_sdl_hit
    FindNext $0 $1
    Goto rimesync_sdl_loop
  rimesync_sdl_hit:
    StrCpy $RimeSyncWeaselDefaultExe "yes"
    StrCpy $9 "$2\$1\WeaselServer.exe"
  rimesync_sdl_close:
  FindClose $0
  rimesync_sdl_done:
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.15：预编译 build 产物兜底
;
; 为什么：测试机 192.168.61.227 上 WeaselDeployer.exe /deploy 返回 0 但什么都不写
; （%APPDATA%\Rime\{user.yaml,installation.yaml,build} 一直停在旧日期），手动起
; WeaselServer 也一样。这种机器上「本机部署」永远不生效，只能把别处部署好的 build/
; 产物随安装包带过来，安装时直接铺进用户目录。
;
; 判据（与 Windows 安装器 / Linux postinst 完全一致）：
;   <用户目录>\Rime\build\luna_pinyin_simp.schema.yaml 存在，且文件里同时出现
;   rimesync_vnum 与 rimesync_punct（这是随包 *.custom.yaml 才有的插件标记；
;   官方部署出来的没有，所以「有文件」不等于「是我们这套」）。
;
; 开关（安装包与静默安装都认，不弹额外界面）：
;   RIMESYNC_PREBUILT=0      关闭兜底
;   RIMESYNC_PREBUILT=force  已存在的文件也覆盖（用户要强制刷新时用）
; ---------------------------------------------------------------------------

; 文件里是否含某个子串（大小写敏感，够用）
; 入参：$R9 = 文件路径，$R8 = 子串
; 出参：$RimeSyncContains = yes / no
; 用 shlwapi!StrStrW 逐行找，不额外 !include（Tauri 模板不一定带 StrFunc.nsh）
Function RimeSyncFileContains
  Push $0
  Push $1
  Push $2

  StrCpy $RimeSyncContains "no"
  IfFileExists "$R9" 0 rimesync_fc_done
  ClearErrors
  FileOpen $0 "$R9" r
  IfErrors rimesync_fc_done
  rimesync_fc_loop:
    FileRead $0 $1
    IfErrors rimesync_fc_close
    System::Call 'shlwapi::StrStrW(w "$1", w "$R8") p .r2'
    StrCmp $2 "0" rimesync_fc_loop
      StrCpy $RimeSyncContains "yes"
  rimesync_fc_close:
  FileClose $0

  rimesync_fc_done:
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; 判「用户目录里的 build 产物可用」
; 入参：$9 = 用户 Rime 目录（%APPDATA%\Rime）
; 出参：$RimeSyncBuildProbe = yes / no
Function RimeSyncProbeBuild
  Push $0

  StrCpy $RimeSyncBuildProbe "no"
  StrCmp $9 "" rimesync_pb_done
  StrCpy $R9 "$9\build\luna_pinyin_simp.schema.yaml"
  StrCpy $R8 "rimesync_vnum"
  Call RimeSyncFileContains
  StrCmp $RimeSyncContains "yes" 0 rimesync_pb_done
  StrCpy $R8 "rimesync_punct"
  Call RimeSyncFileContains
  StrCmp $RimeSyncContains "yes" 0 rimesync_pb_done
  StrCpy $RimeSyncBuildProbe "yes"

  rimesync_pb_done:
  Pop $0
FunctionEnd

; 读 RIMESYNC_PREBUILT 环境变量
; 出参：$RimeSyncPrebuiltMode = auto（缺省） / force / off
Function RimeSyncPrebuiltMode
  Push $0

  StrCpy $RimeSyncPrebuiltMode "auto"
  ReadEnvStr $0 "RIMESYNC_PREBUILT"
  StrCmp $0 "" rimesync_pm_done
  StrCmp $0 "0" 0 rimesync_pm_force
    StrCpy $RimeSyncPrebuiltMode "off"
    Goto rimesync_pm_done
  rimesync_pm_force:
  StrCmp $0 "force" 0 rimesync_pm_done
    StrCpy $RimeSyncPrebuiltMode "force"

  rimesync_pm_done:
  Pop $0
FunctionEnd

; 逐个文件复制一个目录：存在即跳过（$RimeSyncPrebuiltEffMode=force 才覆盖）
; 入参：$4 = 源目录，$6 = 目标目录
; 累加：$RimeSyncPrebuiltCopied / $RimeSyncPrebuiltKept；有文件复制失败 → $RimeSyncPrebuiltState=failed
Function RimeSyncCopyPrebuiltDir
  Push $0
  Push $1
  Push $2

  StrCmp $4 "" rimesync_cpd_done
  StrCmp $6 "" rimesync_cpd_done
  CreateDirectory "$6"
  ClearErrors
  FindFirst $1 $2 "$4\*.*"
  IfErrors rimesync_cpd_done

  rimesync_cpd_loop:
    StrCmp $2 "" rimesync_cpd_close
    StrCmp $2 "." rimesync_cpd_next
    StrCmp $2 ".." rimesync_cpd_next
    IfFileExists "$6\$2" 0 rimesync_cpd_copy
      StrCmp $RimeSyncPrebuiltEffMode "force" rimesync_cpd_copy
      IntOp $RimeSyncPrebuiltKept $RimeSyncPrebuiltKept + 1
      Goto rimesync_cpd_next
    rimesync_cpd_copy:
      ClearErrors
      CopyFiles /SILENT "$4\$2" "$6\$2"
      IfFileExists "$6\$2" 0 rimesync_cpd_failed
      IntOp $RimeSyncPrebuiltCopied $RimeSyncPrebuiltCopied + 1
      Goto rimesync_cpd_next
    rimesync_cpd_failed:
      StrCpy $RimeSyncPrebuiltState "failed"
      StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT copy failed file=$2"
      Call RimeSyncLog
    rimesync_cpd_next:
    FindNext $1 $2
    Goto rimesync_cpd_loop

  rimesync_cpd_close:
  FindClose $1

  rimesync_cpd_done:
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; 兜底主流程：部署 → 判定 → （不满足时）铺预编译产物 → 复查
; 出参：$RimeSyncBuildSource = deploy / prebuilt / none
;       $RimeSyncPrebuiltState = ok / failed / payload-missing / skipped / none
; 失败只记日志 + 告警，绝不 Abort（程序已经装好，用户要的是能用的输入法和日志）
Function RimeSyncEnsurePrebuiltBuild
  Push $0
  Push $1
  Push $4
  Push $5
  Push $6

  StrCpy $RimeSyncBuildSource "none"
  StrCpy $RimeSyncPrebuiltState "none"
  StrCpy $RimeSyncPrebuiltCopied "0"
  StrCpy $RimeSyncPrebuiltKept "0"
  StrCpy $RimeSyncPrebuiltRecopy "0"
  StrCpy $RimeSyncPrebuiltEffMode "auto"

  ; 用户配置目录：播种阶段解析出来的优先，否则按进程环境兜底
  StrCpy $5 "$RimeSyncRimeUserDir"
  StrCmp $5 "" 0 rimesync_epb_have_dir
    ReadEnvStr $5 "APPDATA"
    StrCmp $5 "" rimesync_epb_no_userdir
    StrCpy $5 "$5\Rime"
  rimesync_epb_have_dir:

  ; 1) 部署真的产出了我们这套可用文件 → 什么都不用做
  StrCpy $9 "$5"
  Call RimeSyncProbeBuild
  StrCmp $RimeSyncBuildProbe "yes" 0 rimesync_epb_deploy_miss
    StrCpy $RimeSyncBuildSource "deploy"
    StrCpy $RimeSyncPrebuiltState "skipped"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT ok source=deploy reason=deploy-produced-usable-build"
    Call RimeSyncLog
    Goto rimesync_epb_end
  rimesync_epb_deploy_miss:

  ; 2) 开关：RIMESYNC_PREBUILT=0 关闭
  Call RimeSyncPrebuiltMode
  StrCmp $RimeSyncPrebuiltMode "off" 0 rimesync_epb_seed
    StrCpy $RimeSyncPrebuiltState "skipped"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT skipped reason=disabled-by-env RIMESYNC_PREBUILT=0"
    Call RimeSyncLog
    Goto rimesync_epb_end
  rimesync_epb_seed:

  ; 3) 载荷在不在（$INSTDIR\resources\prebuilt，由 tauri resources 铺进来）
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT begin mode=$RimeSyncPrebuiltMode src=$INSTDIR\resources\prebuilt"
  Call RimeSyncLog
  ClearErrors
  FindFirst $0 $1 "$INSTDIR\resources\prebuilt\build\*.*"
  IfErrors rimesync_epb_payload_missing
  FindClose $0
  Goto rimesync_epb_copy

  rimesync_epb_payload_missing:
    StrCpy $RimeSyncPrebuiltState "payload-missing"
    IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT failed reason=payload-missing dir=$INSTDIR\resources\prebuilt"
    Call RimeSyncLog
    DetailPrint "[rime-sync] 警告：安装包里没有预编译 build 产物（$INSTDIR\resources\prebuilt），部署又不产出时输入法可能打不出字"
    Goto rimesync_epb_end

  rimesync_epb_copy:
  ; 4) 第一遍：先铺 build（*.yaml / *.bin）到 <用户目录>\Rime\build，
  ;    再铺 config 里的 *.yaml 到 <用户目录>\Rime；都是「存在即跳过」。
  StrCpy $RimeSyncPrebuiltRecopy "0"
  StrCpy $RimeSyncPrebuiltEffMode "$RimeSyncPrebuiltMode"
  StrCpy $4 "$INSTDIR\resources\prebuilt\build"
  StrCpy $6 "$5\build"
  Call RimeSyncCopyPrebuiltDir
  StrCpy $4 "$INSTDIR\resources\prebuilt\config"
  StrCpy $6 "$5"
  Call RimeSyncCopyPrebuiltDir

  StrCpy $9 "$5"
  Call RimeSyncProbeBuild
  StrCmp $RimeSyncBuildProbe "yes" rimesync_epb_ok
  ; force 已经把 build/ 全覆盖过，再铺一遍没有意义
  StrCmp $RimeSyncPrebuiltMode "force" rimesync_epb_failed

  ; 4b) 第二遍（只在第一遍没解决问题时）：用户目录里本来就有旧的 build 产物
  ;     （例如 10 月那次部署留下的、没有 rimesync_ 插件标记），「存在即跳过」
  ;     把它们跳过去了，现在还是不可用。build/ 是部署器随时会重写的派生数据，
  ;     不是用户数据，所以只对 build/ 升级成覆盖模式再铺一遍；config/*.yaml
  ;     是用户配置，绝不参与这一遍。
  StrCpy $RimeSyncPrebuiltRecopy "1"
  StrCpy $RimeSyncPrebuiltEffMode "force"
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT retry reason=probe-after-skip-only scope=build-only"
  Call RimeSyncLog
  StrCpy $4 "$INSTDIR\resources\prebuilt\build"
  StrCpy $6 "$5\build"
  Call RimeSyncCopyPrebuiltDir
  StrCpy $9 "$5"
  Call RimeSyncProbeBuild
  StrCmp $RimeSyncBuildProbe "yes" rimesync_epb_ok
  Goto rimesync_epb_failed

  rimesync_epb_ok:
    StrCpy $RimeSyncBuildSource "prebuilt"
    StrCpy $RimeSyncPrebuiltState "ok"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT ok source=prebuilt copied=$RimeSyncPrebuiltCopied kept=$RimeSyncPrebuiltKept mode=$RimeSyncPrebuiltMode escalated=$RimeSyncPrebuiltRecopy dir=$5\build"
    Call RimeSyncLog
    DetailPrint "[rime-sync] 部署没有产出 build 产物，已铺入随包的预编译产物（新铺 $RimeSyncPrebuiltCopied 个，沿用 $RimeSyncPrebuiltKept 个）"
    Goto rimesync_epb_end

  rimesync_epb_failed:
    StrCpy $RimeSyncPrebuiltState "failed"
    StrCpy $RimeSyncBuildSource "none"
    IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT failed reason=probe-after-copy copied=$RimeSyncPrebuiltCopied kept=$RimeSyncPrebuiltKept escalated=$RimeSyncPrebuiltRecopy file=$5\build\luna_pinyin_simp.schema.yaml"
    Call RimeSyncLog
    DetailPrint "[rime-sync] 错误：预编译产物铺完仍没有可用的 build\luna_pinyin_simp.schema.yaml（只告警，不中止安装）"
    IfSilent rimesync_epb_end
      MessageBox MB_OK|MB_ICONEXCLAMATION "rime-sync 安装完成，但输入法数据没能铺好：本机部署没有生成 build 产物，随包的预编译产物复查也没通过。$\r$\n$\r$\n请把日志 $TEMP\rime-sync-setup.log 里的 phase=PREBUILT 行发给维护者。$\r$\n用户数据（%APPDATA%\Rime）不受影响。"
    Goto rimesync_epb_end

  rimesync_epb_no_userdir:
    StrCpy $RimeSyncPrebuiltState "skipped"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREBUILT skipped reason=no-userdir（解析不到 %APPDATA%）"
    Call RimeSyncLog

  rimesync_epb_end:
  Pop $6
  Pop $5
  Pop $4
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14 安装结束自检（POSTINSTALL 末尾）
;   1) 再判一次就绪（RimeSyncWeaselReady），TSF/进程证据另起一行只写日志
;   2) 硬性检查 A：HKLM\Software\Rime\Weasel\InstallDir（32/64 视图任一）必须有值
;   3) 硬性检查 B：<Program Files>\Rime\weasel-*\WeaselServer.exe 必须存在
;      → 1)~3) 任一不满足：日志 phase=SELF-CHECK weasel-ready=no、非静默弹 STOP 框、
;        SetErrorLevel 5（安装标记为失败）；这里仍然不 Abort（程序已经装好）
;   4) 报配置目标目录与写入数量（new / reuse）
;   5) 调一次 WeaselDeployer.exe /deploy（最多等 ${RIMESYNC_DEPLOY_TIMEOUT_MS}），
;      再检查 <用户目录>\Rime\build\luna_pinyin_simp.schema.yaml 是否生成/更新
;   日志关键字（验收用）：phase=SELF-CHECK / weasel-ready= / install-dir= / default-exe=
;   / deploy= / build=
; ---------------------------------------------------------------------------
Function RimeSyncSelfCheck
  Push $0
  Push $1
  Push $3
  Push $4
  Push $R0
  Push $R1

  StrCpy $RimeSyncSelfCheckWarn "0"
  StrCpy $RimeSyncWeaselSelfCheckFail "0"
  StrCpy $RimeSyncDeployRan "0"
  StrCpy $RimeSyncDeployExit "skipped"
  StrCpy $RimeSyncBuildState "unknown"
  StrCpy $RimeSyncBuildExistsBefore "0"
  StrCpy $RimeSyncBuildHiBefore ""
  StrCpy $RimeSyncBuildLoBefore ""
  StrCpy $RimeSyncBuildSource "none"
  StrCpy $RimeSyncBuildProbe "no"
  StrCpy $RimeSyncPrebuiltMode "auto"
  StrCpy $RimeSyncPrebuiltState "none"
  StrCpy $RimeSyncPrebuiltCopied "0"
  StrCpy $RimeSyncPrebuiltKept "0"

  Call RimeSyncWeaselReady
  Call RimeSyncWeaselLogEvidence

  ; 硬性检查 A：注册表 InstallDir 必须有值（32/64 视图任一；Weasel 是 32 位安装器）
  StrCpy $RimeSyncWeaselRegInstallDir ""
  SetRegView 64
  ReadRegStr $0 HKLM "Software\Rime\Weasel" "InstallDir"
  StrCmp $0 "" 0 rimesync_sc_reg_done
  SetRegView 32
  ReadRegStr $0 HKLM "Software\Rime\Weasel" "InstallDir"
  rimesync_sc_reg_done:
  SetRegView 64
  StrCpy $RimeSyncWeaselRegInstallDir "$0"

  ; 硬性检查 B：默认布局 $PROGRAMFILES64\Rime\weasel-*\WeaselServer.exe
  StrCpy $RimeSyncWeaselDefaultExe "no"
  StrCpy $9 ""
  StrCpy $R5 "$PROGRAMFILES64"
  Call RimeSyncScanDefaultLayout
  StrCpy $R5 "$PROGRAMFILES"
  Call RimeSyncScanDefaultLayout

  ; 就绪 + InstallDir + 默认布局 exe：任一不满足 → 自检硬失败（下面统一记日志/弹框/退出码 5）
  StrCmp $RimeSyncWeaselReady "1" rimesync_sc_hard_a
    StrCpy $RimeSyncWeaselSelfCheckFail "1"
  rimesync_sc_hard_a:
  StrCmp $RimeSyncWeaselRegInstallDir "" 0 rimesync_sc_hard_b
    StrCpy $RimeSyncWeaselSelfCheckFail "1"
  rimesync_sc_hard_b:
  StrCmp $RimeSyncWeaselDefaultExe "yes" rimesync_sc_hard_c
    StrCpy $RimeSyncWeaselSelfCheckFail "1"
  rimesync_sc_hard_c:

  ; 配置目录：播种阶段解析出来的真实用户目录优先，否则按进程环境兜底
  StrCpy $0 "$RimeSyncRimeUserDir"
  StrCmp $0 "" 0 rimesync_sc_have_dir
    ReadEnvStr $0 "APPDATA"
    StrCpy $0 "$0\Rime"
  rimesync_sc_have_dir:

  ; 部署前先记一次 build 文件时间（判断「更新」而不只是「存在」）
  StrCpy $R9 "$0\build\luna_pinyin_simp.schema.yaml"
  Call RimeSyncFileTimeGet
  StrCpy $RimeSyncBuildExistsBefore "$RimeSyncFileExists"
  StrCpy $RimeSyncBuildHiBefore "$RimeSyncFileHi"
  StrCpy $RimeSyncBuildLoBefore "$RimeSyncFileLo"

  StrCmp $RimeSyncWeaselRootDir "" rimesync_sc_log
  IfFileExists "$RimeSyncWeaselRootDir\WeaselDeployer.exe" rimesync_sc_deploy
    StrCpy $RimeSyncDeployExit "no-deployer"
    IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1
    StrCpy $R3 "[rime-sync] $R2 phase=SELF-CHECK warning=no-weasel-deployer root=$RimeSyncWeaselRootDir"
    Call RimeSyncLog
    ; 没有部署器就部署不了，正是预编译产物该上场的时候（跳过 build 状态判定，直接兜底）
    Goto rimesync_sc_build_done

  rimesync_sc_deploy:
  StrCpy $R9 '"$RimeSyncWeaselRootDir\WeaselDeployer.exe" /deploy'
  DetailPrint "[rime-sync] 自检：调用一次部署（$R9，最多等待 ${RIMESYNC_DEPLOY_TIMEOUT_MS} ms）"
  nsExec::ExecToStack /TIMEOUT=${RIMESYNC_DEPLOY_TIMEOUT_MS} '$R9'
  Pop $RimeSyncDeployExit
  Pop $1
  StrCpy $RimeSyncDeployRan "1"
  StrCpy $R3 "[rime-sync] $R2 phase=SELF-CHECK deploy exit=$RimeSyncDeployExit"
  Call RimeSyncLog

  ; 部署后再读一次：不存在 → missing；部署前不存在 → generated；
  ; 高 32 位变化或低 32 位前进 → updated；没动 → stale
  StrCpy $R9 "$0\build\luna_pinyin_simp.schema.yaml"
  Call RimeSyncFileTimeGet
  StrCmp $RimeSyncFileExists "1" rimesync_sc_build_present
    StrCpy $RimeSyncBuildState "missing"
    Goto rimesync_sc_build_done
  rimesync_sc_build_present:
  StrCmp $RimeSyncBuildExistsBefore "1" rimesync_sc_build_compare
    StrCpy $RimeSyncBuildState "generated"
    Goto rimesync_sc_build_done
  rimesync_sc_build_compare:
  StrCmp $RimeSyncFileHi "" rimesync_sc_build_stale
  StrCmp $RimeSyncBuildHiBefore "" rimesync_sc_build_stale
  StrCmp $RimeSyncFileHi $RimeSyncBuildHiBefore rimesync_sc_build_same_hi rimesync_sc_build_updated
  rimesync_sc_build_same_hi:
  IntOp $1 $RimeSyncFileLo - $RimeSyncBuildLoBefore
  IntCmp $1 0 rimesync_sc_build_stale rimesync_sc_build_updated rimesync_sc_build_updated
  rimesync_sc_build_stale:
    StrCpy $RimeSyncBuildState "stale"
    Goto rimesync_sc_build_done
  rimesync_sc_build_updated:
    StrCpy $RimeSyncBuildState "updated"
  rimesync_sc_build_done:

  ; 0.6.15：部署没有产出（或产出的不是我们这套）时，铺随包的预编译 build 产物。
  ; 该函数自己写 phase=PREBUILT 日志；失败只加告警，绝不 Abort。
  Call RimeSyncEnsurePrebuiltBuild

  ; 兜底成功（build-source=prebuilt）就不再报 build-not-updated：产物确实可用。
  StrCmp $RimeSyncBuildSource "prebuilt" rimesync_sc_build_ok
  StrCmp $RimeSyncBuildState "updated" rimesync_sc_build_ok
  StrCmp $RimeSyncBuildState "generated" rimesync_sc_build_ok
    IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1
    DetailPrint "[rime-sync] 警告：部署后 build\luna_pinyin_simp.schema.yaml 没有生成/更新（$RimeSyncBuildState），只告警不中止"
    StrCpy $R3 "[rime-sync] $R2 phase=SELF-CHECK warning=build-not-updated state=$RimeSyncBuildState file=$0\build\luna_pinyin_simp.schema.yaml"
    Call RimeSyncLog
  rimesync_sc_build_ok:

  rimesync_sc_log:
  ; 0.6.14：自检不再「只告警」。就绪 / InstallDir / 默认布局 exe 任一不满足 =
  ; 安装标记为失败（weasel-ready=no + 非静默弹 STOP 框 + SetErrorLevel 5）。
  ; 这里不 Abort：程序已经装好，用户要的是日志与提示，而不是半路退出。
  StrCmp $RimeSyncWeaselSelfCheckFail "0" rimesync_sc_hard_ok
  ; /SKIPIME 是「明确要求不动输入法」的逃生开关：只记告警，不把安装判失败
  StrCmp $RimeSyncWeaselKept "1" rimesync_sc_hard_skip
    IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1
    DetailPrint "[rime-sync] 错误：自检未通过（ready=$RimeSyncWeaselReady install-dir=$RimeSyncWeaselRegInstallDir default-exe=$RimeSyncWeaselDefaultExe），安装标记为失败"
    StrCpy $R3 "[rime-sync] $R2 phase=SELF-CHECK weasel-ready=no ready=$RimeSyncWeaselReady reason=$RimeSyncWeaselReadyReason root=$RimeSyncWeaselRootDir install-dir=$RimeSyncWeaselRegInstallDir default-exe=$RimeSyncWeaselDefaultExe expected=$PROGRAMFILES64\Rime\weasel-*\WeaselServer.exe"
    Call RimeSyncLog
    SetErrorLevel 5
    IfSilent rimesync_sc_hard_failed
      MessageBox MB_OK|MB_ICONSTOP "安装已完成，但输入法自检没有通过：$\r$\n$\r$\n就绪=$RimeSyncWeaselReady（$RimeSyncWeaselReadyReason）$\r$\n注册表 InstallDir=$RimeSyncWeaselRegInstallDir$\r$\n默认布局 weasel-*\WeaselServer.exe=$RimeSyncWeaselDefaultExe$\r$\n$\r$\n安装已标记为失败（退出码 5）。请把日志 $TEMP\rime-sync-setup.log 里的 phase=SELF-CHECK 行发给维护者。$\r$\n用户数据（%APPDATA%\Rime）不受影响。"
  rimesync_sc_hard_failed:
    Goto rimesync_sc_hard_ok
  rimesync_sc_hard_skip:
    IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1
    StrCpy $R3 "[rime-sync] $R2 phase=SELF-CHECK warning=weasel-not-ready-skipped reason=$RimeSyncWeaselReadyReason skip=1 switch=/SKIPIME"
    Call RimeSyncLog
  rimesync_sc_hard_ok:

  StrCpy $R3 "[rime-sync] $R2 phase=SELF-CHECK weasel-ready=$RimeSyncWeaselReady root=$RimeSyncWeaselRootDir source=$RimeSyncWeaselReadySrc proc=$RimeSyncWeaselProcState install-dir=$RimeSyncWeaselRegInstallDir default-exe=$RimeSyncWeaselDefaultExe configdir=$0 configs=$RimeSyncSeedWritten reused=$RimeSyncSeedReused deploy=$RimeSyncDeployExit ran=$RimeSyncDeployRan build=$RimeSyncBuildState build-source=$RimeSyncBuildSource prebuilt=$RimeSyncPrebuiltState copied=$RimeSyncPrebuiltCopied kept=$RimeSyncPrebuiltKept warnings=$RimeSyncSelfCheckWarn"
  Call RimeSyncLog
  DetailPrint "[rime-sync] 自检：就绪=$RimeSyncWeaselReady（$RimeSyncWeaselRootDir）；注册表 InstallDir=$RimeSyncWeaselRegInstallDir；默认布局 exe=$RimeSyncWeaselDefaultExe；配置 $0：新写入 $RimeSyncSeedWritten / 沿用 $RimeSyncSeedReused；部署=$RimeSyncDeployExit；build=$RimeSyncBuildState；build 来源=$RimeSyncBuildSource（预编译 $RimeSyncPrebuiltState：新铺 $RimeSyncPrebuiltCopied / 沿用 $RimeSyncPrebuiltKept）；告警=$RimeSyncSelfCheckWarn"

  ; 非静默时给用户一份简报（静默安装只写日志）；自检硬失败时前面已弹 STOP 框，不再弹
  IfSilent rimesync_sc_done
  StrCmp $RimeSyncWeaselSelfCheckFail "1" rimesync_sc_done
  StrCpy $3 "输入法（小狼毫）：就绪（$RimeSyncWeaselRootDir）"
  StrCmp $RimeSyncWeaselReady "1" rimesync_sc_msg_ready
    StrCpy $3 "输入法（小狼毫）：未就绪（原因 $RimeSyncWeaselReadyReason），请查看日志"
  rimesync_sc_msg_ready:
  StrCpy $4 "部署：exit=$RimeSyncDeployExit；build\luna_pinyin_simp.schema.yaml：$RimeSyncBuildState"
  StrCmp $RimeSyncSelfCheckWarn "0" rimesync_sc_msg_ok
    StrCpy $4 "$4（告警 $RimeSyncSelfCheckWarn 条）"
  rimesync_sc_msg_ok:
  MessageBox MB_OK|MB_ICONINFORMATION "rime-sync 安装完成，自检结果：$\r$\n$\r$\n$3$\r$\n配置目录：$0$\r$\n　　新写入 $RimeSyncSeedWritten 个，沿用已有 $RimeSyncSeedReused 个$\r$\n$4$\r$\n$\r$\n日志：$TEMP\rime-sync-setup.log"

  rimesync_sc_done:
  Pop $R1
  Pop $R0
  Pop $4
  Pop $3
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.13：输入法不可用 → 硬失败。0.6.12 及以前这里只写一行日志就继续，结果装出一个
; 「有文件、没注册表、没服务」的假就绪输入法，用户看不到任何提示。
; 入参：$R0 = 中文附加原因，$R1 = 英文附加原因（调用方已先写 WEASEL-INSTALL failed 日志）。
; 静默安装不弹框（只写日志 + 非零退出码），交互安装弹一次说明。
; ---------------------------------------------------------------------------
Function RimeSyncAbortWeasel
  IfSilent rimesync_abortw_silent
  IntCmp $LANGUAGE 2052 rimesync_abortw_zh rimesync_abortw_en rimesync_abortw_en
  rimesync_abortw_zh:
    MessageBox MB_OK|MB_ICONSTOP "小狼毫输入法安装后仍然不可用，安装已中止（0.6.14 起这是硬失败，旧目录回滚结果见 $R0 与日志 rollback=）。$\r$\n$\r$\n$R0$\r$\n$\r$\n处理办法：先在「控制面板 → 程序和功能」里卸载残留的「小狼毫输入法」，再重新运行本安装包。日志：$TEMP\rime-sync-setup.log（看 phase=IME-CHECK 与 WEASEL-INSTALL failed 行）。$\r$\n$\r$\n用户数据（%APPDATA%\Rime）不受影响。"
    Goto rimesync_abortw_silent
  rimesync_abortw_en:
    MessageBox MB_OK|MB_ICONSTOP "Weasel is still not usable after installation; setup was aborted (hard failure since 0.6.14; see the rollback= field in the log).$\r$\n$\r$\n$R1$\r$\n$\r$\nFix: uninstall the leftover Weasel from Control Panel -> Programs, then run this installer again. Log: $TEMP\rime-sync-setup.log (look for phase=IME-CHECK and WEASEL-INSTALL failed).$\r$\n$\r$\nUser data (%APPDATA%\Rime) is untouched."
  rimesync_abortw_silent:
  SetErrorLevel 5
  Abort
FunctionEnd

; ---------------------------------------------------------------------------
; 卸载旧版同步程序：$INSTDIR\uninstall.exe 优先，其次 Uninstall 注册项（两个视图）。
; 命令形如 "C:\Program Files\rime-sync\uninstall.exe" /S _?=C:\Program Files\rime-sync
; （_?= 的目录在 NSIS 卸载器里必须裸写、不引号；见 NSIS 手册的静默卸载示例）。
; 只写日志，不 Abort；日志行写在卸载之后 —— 旧卸载器会删掉 $INSTDIR\install-ime-order.log。
; ---------------------------------------------------------------------------
Function RimeSyncUninstallOldRimeSync
  StrCpy $R9 ""
  StrCpy $RimeSyncOldUninstKey ""
  StrCpy $RimeSyncOldUninstRaw ""

  IfFileExists "$INSTDIR\uninstall.exe" rimesync_oldrs_instdir
  ; 32 位视图在前：Tauri 的 NSIS 是 x86 进程，键通常落在 Wow6432Node
  !insertmacro RIMESYNC_SCAN_OUR_UNINST 32
  StrCmp $RimeSyncOldUninstKey "" 0 rimesync_oldrs_reg
  !insertmacro RIMESYNC_SCAN_OUR_UNINST 64
  StrCmp $RimeSyncOldUninstKey "" rimesync_oldrs_missing

  rimesync_oldrs_reg:
    StrCpy $R9 $RimeSyncOldUninstRaw
    Call RimeSyncNormalizeUninstallPath
    StrCmp $R9 "" rimesync_oldrs_missing
    Goto rimesync_oldrs_build
  rimesync_oldrs_instdir:
    StrCpy $R9 "$INSTDIR\uninstall.exe"
    StrCpy $RimeSyncOldUninstKey "(instdir)"

  rimesync_oldrs_build:
  ${GetParent} "$R9" $0
  StrCpy $5 '"$R9" /S _?=$0'
  StrCpy $R9 $5
  Call RimeSyncRunAndWait
  StrCpy $R0 $0                              ; 退出码（Stamp 会冲掉 $1-$9，先挪到 $R0/$R1）
  StrCpy $R1 $1                              ; 耗时
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=UNINSTALL-OLD 旧版同步程序卸载器退出码 $R0（耗时 $R1 ms）"
  StrCpy $R3 "[rime-sync] $R2 phase=UNINSTALL-OLD target=rime-sync exit=$R0 ms=$R1 source=$RimeSyncOldUninstKey"
  Call RimeSyncLog
  Return

  rimesync_oldrs_missing:
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=UNINSTALL-OLD 没找到旧版 rime-sync 卸载器（首次安装或已手动卸载）"
  StrCpy $R3 "[rime-sync] $R2 phase=UNINSTALL-OLD target=rime-sync result=none"
  Call RimeSyncLog
FunctionEnd


; ---------------------------------------------------------------------------
; PREINSTALL：把旧版同步程序清干净，后面就按全新安装走，不再有「检测到已存在就跳过」的分支。
; 0.6.14 起**不在这里卸小狼毫**：先卸 Weasel 再判就绪，残留（TSF/注册表）能骗过判据，
; 结果是「判成就绪 → 跳过安装 → Weasel 没装回来」。Weasel 的替换统一交给
; RimeSyncInstallWeaselPre（判定 → 改名留证 → 重装 → 复查 → 失败回滚）。
; ---------------------------------------------------------------------------
Function RimeSyncUninstallOldVersion
  StrCpy $RimeSyncWeaselKept "0"
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=UNINSTALL-OLD begin policy=clean-install"
  Call RimeSyncLog

  Call RimeSyncKillRunningProcesses
  Call RimeSyncUninstallOldRimeSync
  ; 0.6.14：这里不再调用 Weasel 的卸载器。Weasel 只在「判成未就绪」时由
  ; RimeSyncInstallWeaselPre 改名 + 重装 + 回滚，绝不先卸再判。

  ; 标记只描述「还归 rime-sync 管的小狼毫」：旧卸载器通常已经清掉，这里兜底清一次；
  ; 新版装完由 RimeSyncMarkBundledWeasel 重新写。
  SetRegView 64
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "BundledWeasel"
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "WeaselRoot"
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "WeaselUninstall"
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "BundledAt"
  DeleteRegKey /ifempty HKLM "${RIMESYNC_REG_KEY}"

  ; 旧卸载器可能把 $INSTDIR 一起删了，而后面还要把日志写回 $INSTDIR
  CreateDirectory "$INSTDIR"
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=UNINSTALL-OLD done（下一步：全新安装）"
  Call RimeSyncLog
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14：PREINSTALL 第一步（只读）—— 在任何破坏性动作之前先判一次就绪并留证据。
;   日志行 phase=IME-CHECK stage=preflight：装之前到底就绪没就绪，事后可复核。
;   TSF / 进程证据单独一行 phase=TSF-EVIDENCE，只写日志、不参与判定。
;   真正的决定在 RimeSyncInstallWeaselPre：旧卸载器（0.6.13 及以前）可能顺手删掉
;   Weasel，所以卸完旧程序必须再判一次。
; ---------------------------------------------------------------------------
Function RimeSyncWeaselPreflight
  Call RimeSyncWeaselReady
  Call RimeSyncStamp
  Call RimeSyncWeaselLogEvidence
  StrCpy $R3 "[rime-sync] $R2 phase=IME-CHECK stage=preflight ready=$RimeSyncWeaselReady reason=$RimeSyncWeaselReadyReason root=$RimeSyncWeaselRootDir source=$RimeSyncWeaselReadySrc action=pending-decision"
  Call RimeSyncLog
FunctionEnd

; ---------------------------------------------------------------------------
; 0.6.14：安装/修复小狼毫（在 Tauri 拷主程序与 resources 之前）
;   顺序：先判定 → 再决定卸/重装（判定绝不看 TSF/进程残留）：
;     已就绪（注册表指向的目录里真有 WeaselServer.exe）→ 跳过安装，
;        日志 phase=IME-CHECK ready=yes ... action=skip reason=exe-present；
;     未就绪 → taskkill → 残留目录改名 Rime.bak-<ts>（仅当目录非空且 exe 在）→
;        内嵌安装器 /S /NOREBOOT（nsExec，300 秒超时）→ 复查 →
;        仍不就绪 = 回滚（Rime.bak-<ts> 改回 Rime）+ 硬失败
;        （日志 WEASEL-INSTALL failed ... rollback=ok|failed + SetErrorLevel 5 + Abort）。
; 0.6.13 的回归：判定曾被 TSF/注册表残留骗成「已就绪」→ 跳过安装 → 机器上没有输入法。
; ---------------------------------------------------------------------------
Function RimeSyncInstallWeaselPre
  ; ---- 0) 先判定（唯一硬条件：注册表值指向的目录里真有 WeaselServer.exe）----
  Call RimeSyncWeaselReady
  Call RimeSyncStamp
  Call RimeSyncWeaselLogEvidence

  ; /SKIPIME：显式跳过（已装但探测不到 / 用户不想动输入法时的唯一出口）
  ${GetOptions} $CMDLINE "/SKIPIME" $R1
  IfErrors rimesync_pre_no_skipime
    DetailPrint "[rime-sync] 收到 /SKIPIME：跳过内嵌小狼毫安装（当前就绪=$RimeSyncWeaselReady）"
    StrCpy $RimeSyncWeaselKept "1"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-CHECK skip=1 switch=/SKIPIME ready=$RimeSyncWeaselReady root=$RimeSyncWeaselRootDir"
    Call RimeSyncLog
    Return
  rimesync_pre_no_skipime:

  StrCmp $RimeSyncWeaselReady "1" rimesync_pre_already
  Goto rimesync_pre_repair

  ; ---- 已就绪：只跳过安装，绝不动现有 Weasel ----
  rimesync_pre_already:
  DetailPrint "[rime-sync] 小狼毫已就绪（$RimeSyncWeaselRootDir；依据 $RimeSyncWeaselReadySrc；进程 $RimeSyncWeaselProcState），跳过安装"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-CHECK ready=yes root=$RimeSyncWeaselRootDir source=$RimeSyncWeaselReadySrc proc=$RimeSyncWeaselProcState action=skip reason=exe-present"
  Call RimeSyncLog
  Return

  ; ---- 未就绪：改名留证 → 装内嵌安装器 → 复查 → 失败回滚 ----
  rimesync_pre_repair:
  DetailPrint "[rime-sync] 小狼毫未就绪（$RimeSyncWeaselReadyReason），开始修复安装"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-CHECK ready=no reason=$RimeSyncWeaselReadyReason root=$RimeSyncWeaselRootDir regdir=$RimeSyncWeaselRegDir action=repair"
  Call RimeSyncLog

  ; 1) 停掉占用文件的进程（找不到进程时 taskkill 返回非 0，忽略）
  StrCpy $R9 'taskkill /IM WeaselServer.exe /F'
  Call RimeSyncRunAndWait

  ; 2) 残留目录改名留证（只改「非空且 exe 在」的目录；时间戳进 $R2，回滚要用）
  Call RimeSyncBackupLeftoverWeasel

  ; 3) 解出内嵌安装器
  StrCpy $R0 ""
  StrCpy $R4 ""
  !ifdef RIMESYNC_WEASEL_EMBEDDED
    ; $PLUGINSDIR 在安装器退出时由 NSIS 删除；这里只在还没初始化时初始化
    StrCmp $PLUGINSDIR "" 0 rimesync_pre_have_plugins
      InitPluginsDir
    rimesync_pre_have_plugins:
    SetOutPath "$PLUGINSDIR\rime-sync-weasel"
    File "/oname=${RIMESYNC_WEASEL_INSTALLER}" "${RIMESYNC_WEASEL_EXE}"
    StrCpy $R0 "$PLUGINSDIR\rime-sync-weasel\${RIMESYNC_WEASEL_INSTALLER}"
    SetOutPath "$INSTDIR"
  !else
    ; 兜底：0.6.20 起安装包把安装器放在 resources\weasel\（Tauri resources），
    ; 旧布局 resources\<安装器> 也认。两者都只在 POSTINSTALL 之后才存在，
    ; 但这里仍检查一次，便于“自定义模板先解压 resources”的场景。
    IfFileExists "$INSTDIR\resources\weasel\${RIMESYNC_WEASEL_INSTALLER}" 0 rimesync_pre_legacy
      StrCpy $R0 "$INSTDIR\resources\weasel\${RIMESYNC_WEASEL_INSTALLER}"
      Goto rimesync_pre_checked
    rimesync_pre_legacy:
    IfFileExists "$INSTDIR\\resources\${RIMESYNC_WEASEL_INSTALLER}" 0 rimesync_pre_checked
      StrCpy $R0 "$INSTDIR\\resources\${RIMESYNC_WEASEL_INSTALLER}"
    rimesync_pre_checked:
  !endif

  StrCmp $R0 "" 0 rimesync_pre_run
  ; 没有载荷就无法修复：先把改过名的旧目录回滚，再按硬失败退出
  StrCpy $R4 "payload-missing"
  StrCpy $R7 "payload-missing"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL payload=missing installer=${RIMESYNC_WEASEL_INSTALLER}"
  Call RimeSyncLog
  Goto rimesync_pre_fail

  rimesync_pre_run:
  ; /NOREBOOT：Weasel 的 .onInit 命中旧 UninstallString 时会 SetRebootFlag，
  ; 静默模式下一旦触发就可能直接重启机器；安装器不能这么干。
  StrCpy $R9 '"$R0" /S /NOREBOOT'
  Call RimeSyncRunWeaselInstaller
  StrCpy $R4 $0                              ; 退出码（RimeSyncStamp 会冲掉 $1-$9，先挪到 $R4/$R5）
  StrCpy $R5 $1                              ; 耗时
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=PREINSTALL 小狼毫安装器退出码 $R4（耗时 $R5 ms）"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL weasel installer exit=$R4 ms=$R5 backup=$RimeSyncWeaselBackupDir"
  Call RimeSyncLog

  ; 4) 装完复查：就绪判定必须过（注册表 + WeaselServer.exe），否则回滚 + 硬失败
  Call RimeSyncWeaselReady
  StrCmp $RimeSyncWeaselReady "1" rimesync_pre_ready
  StrCpy $R7 "$RimeSyncWeaselReadyReason"
  Goto rimesync_pre_fail

  ; 5) 失败：先把改过名的目录改回原名（回滚），再硬失败
  rimesync_pre_fail:
  StrCmp $R7 "" 0 rimesync_pre_fail_reason
    StrCpy $R7 "$RimeSyncWeaselReadyReason"
  rimesync_pre_fail_reason:
  Call RimeSyncRollbackWeasel
  Call RimeSyncStamp
  DetailPrint "[rime-sync] 错误：小狼毫安装后仍不就绪（$R7），回滚=$RimeSyncWeaselRollback，安装中止"
  StrCpy $R3 "[rime-sync] $R2 WEASEL-INSTALL failed reason=$R7 installer-exit=$R4 rollback=$RimeSyncWeaselRollback backup=$RimeSyncWeaselBackupDir orig=$RimeSyncWeaselBackupOrig root=$RimeSyncWeaselRootDir regdir=$RimeSyncWeaselRegDir proc=$RimeSyncWeaselProcState"
  Call RimeSyncLog
  StrCpy $R0 "内嵌安装器退出码：$R4；就绪判定：未就绪（$R7）；程序目录：$RimeSyncWeaselRootDir；注册表 InstallDir：$RimeSyncWeaselRegDir；旧目录备份：$RimeSyncWeaselBackupDir（回滚：$RimeSyncWeaselRollback）"
  StrCpy $R1 "Bundled installer exit code: $R4; readiness: not ready ($R7); program dir: $RimeSyncWeaselRootDir; registry InstallDir: $RimeSyncWeaselRegDir; backup: $RimeSyncWeaselBackupDir (rollback: $RimeSyncWeaselRollback)"
  Call RimeSyncAbortWeasel

  rimesync_pre_ready:
  Call RimeSyncStamp
  DetailPrint "[rime-sync] 小狼毫已就绪（$RimeSyncWeaselRootDir；依据 $RimeSyncWeaselReadySrc）"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL ready=yes root=$RimeSyncWeaselRootDir source=$RimeSyncWeaselReadySrc proc=$RimeSyncWeaselProcState backup=$RimeSyncWeaselBackupDir"
  Call RimeSyncLog

  ; 所有权标记（卸载时据此决定是否连小狼毫一起卸）
  StrCpy $R8 $RimeSyncWeaselRootDir
  Call RimeSyncMarkBundledWeasel

  !ifdef RIMESYNC_WEASEL_EMBEDDED
    Delete "$PLUGINSDIR\rime-sync-weasel\${RIMESYNC_WEASEL_INSTALLER}"
    RMDir "$PLUGINSDIR\rime-sync-weasel"
  !endif
  ; 关键：Tauri 模板紧接着会 File 主程序，必须把输出目录恢复成 $INSTDIR
  SetOutPath "$INSTDIR"
FunctionEnd
; ---------------------------------------------------------------------------
; 开始菜单「Rime 同步」：只放「Rime 同步」与「同步设置」（后者带 --setup）
; 入参：$R6 = 主程序完整路径；$R7 = Tauri 模板自己建的 rime-sync.lnk
; ---------------------------------------------------------------------------
Function RimeSyncCreateOwnShortcuts
  ; 模板会先建好 ${PRODUCTNAME}.lnk；删掉它，保持用户要求的两个入口
  Delete "$R7"

  CreateDirectory "$SMPROGRAMS\${RIMESYNC_NAME}"
  CreateShortcut "$SMPROGRAMS\${RIMESYNC_NAME}\${RIMESYNC_NAME}.lnk" \
    "$R6" "" \
    "$R6" 0 SW_SHOWNORMAL "" "Rime 词库与配置的 WebDAV 双向同步"
  CreateShortcut "$SMPROGRAMS\${RIMESYNC_NAME}\${RIMESYNC_SETUP_NAME}.lnk" \
    "$R6" "--setup" \
    "$R6" 0 SW_SHOWNORMAL "" "打开 Rime 同步设置（--setup，不启动主窗口）"

  DetailPrint "[rime-sync] 已创建开始菜单快捷方式：$SMPROGRAMS\${RIMESYNC_NAME}"
FunctionEnd

; ---------------------------------------------------------------------------
; 把「同步设置」快捷方式也放进 Weasel 程序组（仅当检测到小狼毫时）
; 入参：$R6 = 主程序完整路径
; ---------------------------------------------------------------------------
Function RimeSyncCreateWeaselShortcut
  Call RimeSyncFindWeaselGroup
  StrCmp $R9 "" rimesync_ws_probe_root
    Goto rimesync_ws_write

  rimesync_ws_probe_root:
  ; 程序组被删掉但安装目录还在 → 用简体组名补建
  Call RimeSyncFindWeaselRoot
  StrCmp $R8 "" rimesync_ws_done
  StrCpy $R9 "$SMPROGRAMS\小狼毫输入法"

  rimesync_ws_write:
  CreateDirectory "$R9"
  CreateShortcut "$R9\${RIMESYNC_SETUP_NAME}.lnk" \
    "$R6" "--setup" \
    "$R6" 0 SW_SHOWNORMAL "" "打开 Rime 同步设置（--setup，不启动主窗口）"
  DetailPrint "[rime-sync] 已在 Weasel 程序组创建「同步设置」快捷方式：$R9"

  rimesync_ws_done:
FunctionEnd

; ---------------------------------------------------------------------------
; 卸载小狼毫（仅当安装时记录了所有权标记）
; 调用链：
;   1. HKLM\Software\rime-sync 读 BundledWeasel / WeaselUninstall / WeaselRoot
;   2. 没有标记 → 保留（不是我们装的）
;   3. /KEEPWEASEL 或 RIMESYNC_KEEP_WEASEL 非空非 0 → 保留，并清掉标记
;   4. 先跑标记里的 UninstallString + /S；非 0/error/timeout 时再跑
;      <WeaselRoot>\uninstall.exe /S（与第一次同一条命令则不重复跑）
;   5. 无论成功失败都删标记，然后继续 rime-sync 自己的卸载（不 Abort）
; 用户数据：%APPDATA%\Rime 从未被本函数或 Weasel 卸载器删除。
; 寄存器：进入时压栈、退出时恢复（本宏是 Tauri 卸载段的第一条语句，
;         后面还有 CheckIfAppIsRunning 等宏依赖 $R0/$R1）。
; ---------------------------------------------------------------------------
Function un.RimeSyncUninstallBundledWeasel
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  Push $8
  Push $9
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  Push $R5
  Push $R6
  Push $R7
  Push $R8
  Push $R9

  Call un.RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=PREUNINSTALL 开始卸载（本地用户数据默认保留）"
  StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL begin"
  Call un.RimeSyncUninstallLog

  SetRegView 64
  ReadRegStr $R6 HKLM "${RIMESYNC_REG_KEY}" "BundledWeasel"
  ReadRegStr $R9 HKLM "${RIMESYNC_REG_KEY}" "WeaselUninstall"
  ReadRegStr $R7 HKLM "${RIMESYNC_REG_KEY}" "WeaselRoot"
  StrCmp $R6 "1" rimesync_un_owned
    DetailPrint "[rime-sync] 没有小狼毫所有权标记：保留当前小狼毫（不是本安装器装的）"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL no ownership marker, keep weasel"
    Call un.RimeSyncUninstallLog
    Goto rimesync_un_done

  rimesync_un_owned:
  ; 保留选项：/KEEPWEASEL 或环境变量 RIMESYNC_KEEP_WEASEL（非空且非 0）
  ; 先看命令行开关，再退到环境变量（GetOptions 会用到 $0..$5，故 env 在其后读）
  ${GetOptions} $CMDLINE "/KEEPWEASEL" $R0
  IfErrors rimesync_un_keep_env
    StrCpy $R1 "1"
    Goto rimesync_un_keep_decide
  rimesync_un_keep_env:
    ReadEnvStr $R1 "RIMESYNC_KEEP_WEASEL"
  rimesync_un_keep_decide:
  StrCmp $R1 "" rimesync_un_locate
  StrCmp $R1 "0" rimesync_un_locate
    DetailPrint "[rime-sync] 收到保留选项（/KEEPWEASEL）：保留小狼毫，并把所有权交还用户"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL keepweasel requested, keep weasel"
    Call un.RimeSyncUninstallLog
    Goto rimesync_un_clear_marker

  rimesync_un_locate:
  ; $R8 = 安装时记录的根目录；为空则现场探测（用户改过安装目录/换过版本）
  StrCpy $R8 $R7
  StrCmp $R8 "" 0 rimesync_un_locate_cmd
    Call un.RimeSyncFindWeaselRoot
  rimesync_un_locate_cmd:
  ; $R9 = 安装时记录的 UninstallString；为空则现场读注册表
  ; （不能放在 $R5：RimeSyncFindWeaselRoot 会拿 $R5 当 scratch）
  StrCmp $R9 "" 0 rimesync_un_attempt1
    Call un.RimeSyncFindWeaselUninstallString
    StrCpy $R9 $R5
  rimesync_un_attempt1:
  StrCmp $R9 "" rimesync_un_attempt2
    DetailPrint "[rime-sync] 静默卸载小狼毫：$R9 /S"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstall attempt=uninstallstring"
    Call un.RimeSyncUninstallLog
    nsExec::ExecToLog '$R9 /S'
    Pop $0
    StrCmp $0 "0" rimesync_un_ok
      DetailPrint "[rime-sync] 警告：小狼毫卸载器退出码 $0（UninstallString），改试安装目录下的 uninstall.exe"
      StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstall failed exit=$0 via=uninstallstring"
      Call un.RimeSyncUninstallLog

  rimesync_un_attempt2:
  StrCmp $R8 "" rimesync_un_missing
  IfFileExists "$R8\uninstall.exe" 0 rimesync_un_missing
  StrCmp $R9 '"$R8\uninstall.exe"' rimesync_un_missing
    DetailPrint "[rime-sync] 静默卸载小狼毫：$R8\uninstall.exe /S"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstall attempt=root-uninstaller"
    Call un.RimeSyncUninstallLog
    nsExec::ExecToLog '"$R8\uninstall.exe" /S'
    Pop $0
    StrCmp $0 "0" rimesync_un_ok
      DetailPrint "[rime-sync] 警告：小狼毫卸载失败（$R8\uninstall.exe 退出码 $0），继续卸载 rime-sync"
      StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstall failed exit=$0 via=root"
      Call un.RimeSyncUninstallLog
      Goto rimesync_un_clear_marker

  rimesync_un_missing:
    DetailPrint "[rime-sync] 警告：找不到小狼毫卸载器（可能已被手动卸载），跳过"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstaller not found"
    Call un.RimeSyncUninstallLog
    Goto rimesync_un_clear_marker

  rimesync_un_ok:
    DetailPrint "[rime-sync] 小狼毫已静默卸载；本地用户数据按用户选择处理（见 purge-userdata 日志）"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstalled"
    Call un.RimeSyncUninstallLog

  rimesync_un_clear_marker:
  ; 标记只表示「rime-sync 装的、且仍归 rime-sync 管」；卸载流程结束时一律清掉
  SetRegView 64
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "BundledWeasel"
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "WeaselRoot"
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "WeaselUninstall"
  DeleteRegValue HKLM "${RIMESYNC_REG_KEY}" "BundledAt"
  DeleteRegKey /ifempty HKLM "${RIMESYNC_REG_KEY}"
  StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL ownership marker cleared"
  Call un.RimeSyncUninstallLog

  rimesync_un_done:
  Pop $R9
  Pop $R8
  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
  Pop $9
  Pop $8
  Pop $7
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 卸载：询问是否删除本地用户数据（默认保留）
; 交互模式弹 MessageBox（默认「否」，关掉窗口也算否）；静默模式只在命令行带
; /PURGEDATA 时删。这里只记决定，真正的删除交给 un.RimeSyncPurgeUserData，
; 放在 Section Uninstall 末尾（等小狼毫卸载器跑完，免得它把文件又写回去）。
; ---------------------------------------------------------------------------
Function un.RimeSyncAskPurgeUserData
  Push $0
  Push $R0
  Push $R1
  Push $R2
  Push $R3

  ; 目标目录取进程环境里的 %APPDATA%（当前用户）。不直接用 $APPDATA：
  ; perMachine 卸载器执行过 SetShellVarContext all，shell 变量可能被映射到
  ; 「所有用户」上下文，那样会指到 ProgramData。
  ReadEnvStr $R0 "APPDATA"
  StrCmp $R0 "" 0 rimesync_pua_have_appdata
    StrCpy $R0 "$APPDATA"
  rimesync_pua_have_appdata:

  StrCpy $RimeSyncPurgeUserData "0"
  StrCpy $RimeSyncPurgeSource "none"

  IfSilent rimesync_pua_silent
    MessageBox MB_YESNO|MB_ICONEXCLAMATION "是否同时删除本地用户数据？$\r$\n$\r$\n将被永久删除（不可恢复）：$\r$\n$R0\Rime（Rime 用户词库与方案自定义）$\r$\n$R0\com.rimesync.app（rime-sync 的配置与备份）$\r$\n$\r$\n选择「否」会保留这些数据（默认，推荐）。" IDYES rimesync_pua_accept IDNO rimesync_pua_decline
    Goto rimesync_pua_decline

  rimesync_pua_silent:
    ${GetOptions} $CMDLINE "/PURGEDATA" $R1
    IfErrors rimesync_pua_decline
    StrCpy $RimeSyncPurgeUserData "1"
    StrCpy $RimeSyncPurgeSource "/PURGEDATA"
    Goto rimesync_pua_log

  rimesync_pua_accept:
    StrCpy $RimeSyncPurgeUserData "1"
    StrCpy $RimeSyncPurgeSource "prompt"
    Goto rimesync_pua_log

  rimesync_pua_decline:
    StrCpy $RimeSyncPurgeUserData "0"
    StrCpy $RimeSyncPurgeSource "none"

  rimesync_pua_log:
  Call un.RimeSyncStamp
  StrCmp $RimeSyncPurgeUserData "1" rimesync_pua_log_yes
    DetailPrint "[rime-sync] 保留本地用户数据（未选择删除）"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata=no source=$RimeSyncPurgeSource"
    Call un.RimeSyncUninstallLog
    Goto rimesync_pua_done
  rimesync_pua_log_yes:
    DetailPrint "[rime-sync] 已确认删除本地用户数据（source=$RimeSyncPurgeSource）"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata=yes source=$RimeSyncPurgeSource"
    Call un.RimeSyncUninstallLog
  rimesync_pua_done:

  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 删除一条用户数据目录：入参 $0 = 绝对路径，$R2 = 时间戳
; 先 RMDir /r 再检查是否真的消失；失败只写卸载日志，绝不 Abort（不中止卸载）。
; 只删这一条明确给出的路径，不碰父目录。
; ---------------------------------------------------------------------------
Function un.RimeSyncPurgeDir
  Push $0
  Push $R3

  StrCmp $0 "" rimesync_pudir_done
  IfFileExists "$0\*.*" rimesync_pudir_exists
  IfFileExists "$0" rimesync_pudir_exists
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata skip-not-found $0"
    Call un.RimeSyncUninstallLog
    DetailPrint "[rime-sync] 本地用户数据目录不存在，跳过：$0"
    Goto rimesync_pudir_done

  rimesync_pudir_exists:
  ClearErrors
  RMDir /r "$0"
  IfFileExists "$0\*.*" rimesync_pudir_failed
  IfFileExists "$0" rimesync_pudir_failed
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata removed $0"
    Call un.RimeSyncUninstallLog
    DetailPrint "[rime-sync] 已删除本地用户数据：$0"
    Goto rimesync_pudir_done

  rimesync_pudir_failed:
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata failed $0"
    Call un.RimeSyncUninstallLog
    DetailPrint "[rime-sync] 警告：删除本地用户数据失败（继续卸载）：$0"

  rimesync_pudir_done:
  Pop $R3
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 卸载：按既定决定删除本地用户数据（严格两条路径）
;   1. %APPDATA%\Rime            Rime 用户词库与方案自定义
;   2. %APPDATA%\com.rimesync.app rime-sync 的配置与备份
; 读不到 %APPDATA% 时什么都不删（宁可少删，也不猜一个路径去 RMDir /r）。
; ---------------------------------------------------------------------------
Function un.RimeSyncPurgeUserData
  Push $0
  Push $1
  Push $R2
  Push $R3

  StrCmp $RimeSyncPurgeUserData "1" 0 rimesync_pud_done
  Call un.RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata begin source=$RimeSyncPurgeSource"
  Call un.RimeSyncUninstallLog
  DetailPrint "[rime-sync] 开始删除本地用户数据（Rime 用户目录 + rime-sync 配置/备份）"

  ReadEnvStr $1 "APPDATA"
  StrCmp $1 "" 0 rimesync_pud_have_appdata
    StrCpy $1 "$APPDATA"
  rimesync_pud_have_appdata:
  StrCmp $1 "" rimesync_pud_no_appdata

  StrCpy $0 "$1\Rime"
  Call un.RimeSyncPurgeDir
  StrCpy $0 "$1\com.rimesync.app"
  Call un.RimeSyncPurgeDir
  Goto rimesync_pud_done_log

  rimesync_pud_no_appdata:
    DetailPrint "[rime-sync] 警告：读不到 %APPDATA%，跳过用户数据删除（未删除任何文件）"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata skipped no-appdata"
    Call un.RimeSyncUninstallLog

  rimesync_pud_done_log:
  Call un.RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL purge-userdata done"
  Call un.RimeSyncUninstallLog

  rimesync_pud_done:
  Pop $R3
  Pop $R2
  Pop $1
  Pop $0
FunctionEnd

; ---------------------------------------------------------------------------
; 宏：Tauri 插入点
; ---------------------------------------------------------------------------
!macro NSIS_HOOK_PREINSTALL
  ; 0.6.14 顺序：系统版本 → 输入法就绪判定（只读）→ WebView2 → 卸旧同步程序 →
  ;   输入法（再判一次；未就绪才改名重装 + 失败回滚）→ 交还 Tauri 拷主程序。
  ; **判定必须在任何破坏性动作之前**：0.6.13 先卸 Weasel 再判，残留（TSF/注册表）
  ; 能骗过判据，于是「判成就绪 → 跳过安装」，机器上最终没有输入法。
  ; 全部在 Tauri File 主程序之前；系统版本是只读检查，放在最前面（先卸载再 Abort
  ; 会把用户晾在「旧版没了、新版没装」的半路）。
  Call RimeSyncCheckWindows
  Call RimeSyncWeaselPreflight
  Call RimeSyncEnsureWebView2
  Call RimeSyncUninstallOldVersion
  CreateDirectory "$INSTDIR\\resources"
  ; 旧卸载器（0.6.13 及以前）可能在 PREUNINSTALL 里连带删掉 Weasel，所以这里必须
  ; 再判一次：已就绪只跳过安装，未就绪就装回来（决定权在 RimeSyncInstallWeaselPre）。
  Call RimeSyncInstallWeaselPre
!macroend

; ---------------------------------------------------------------------------
; 0.6.21：把非保留方案从 data / 用户目录 / build 里清掉（幂等、可回滚）
;   * 随包只保留 3 个方案（朙月拼音·简化字 / 五笔拼音 / 五笔86），清单的单一来源是
;     packaging/rime/retained-schemas.txt —— 它编译进 rime-sync-cli，钩子不另抄一份；
;   * 删除前由 CLI 同目录备份为 <原文件名>.rimesync-removed-<时间戳>；
;   * RIMESYNC_PRUNE_SCHEMAS=0 由 CLI 自己识别；
;   * 失败只记日志，绝不 Abort（清理失败不该让已经装好的输入法回滚）。
Function RimeSyncPruneSchemas
  Push $0
  Push $1

  StrCpy $RimeSyncPruneState "skipped"
  IfFileExists "$INSTDIR\rime-sync-cli.exe" 0 rimesync_prune_no_cli

  StrCpy $RimeSyncPruneState "running"
  ; 用户目录用播种阶段解析出来的真实路径（提权后 %APPDATA% 未必还是当前用户）；
  ; 为空时交给 CLI 自己按平台探测。
  StrCpy $1 ""
  StrCmp $RimeSyncRimeUserDir "" rimesync_prune_cmd
    StrCpy $1 '--user-dir "$RimeSyncRimeUserDir"'
  rimesync_prune_cmd:
  nsExec::ExecToStack /TIMEOUT=120000 '"$INSTDIR\rime-sync-cli.exe" prune-schemas $1'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 rimesync_prune_failed
    StrCpy $RimeSyncPruneState "ok"
    Goto rimesync_prune_log
  rimesync_prune_failed:
    StrCpy $RimeSyncPruneState "failed exit=$0"
    Goto rimesync_prune_log
  rimesync_prune_no_cli:
    StrCpy $RimeSyncPruneState "no-cli"
  rimesync_prune_log:
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=PRUNE state=$RimeSyncPruneState"
  Call RimeSyncLog
  DetailPrint "[rime-sync] [$R2] 阶段=PRUNE 非保留方案清理：$RimeSyncPruneState"
  Pop $1
  Pop $0
FunctionEnd

!macro NSIS_HOOK_POSTINSTALL
  ; 宏在安装段插入时才展开，此时 Tauri 已定义 MAINBINARYNAME / PRODUCTNAME / PRODUCTVERSION
  StrCpy $R6 "$INSTDIR\${MAINBINARYNAME}.exe"
  StrCpy $R7 "$SMPROGRAMS\${RIMESYNC_NAME}\${PRODUCTNAME}.lnk"

  ; 备份后缀 = 安装包版本。老模板/桩构建没有 PRODUCTVERSION 时退回内嵌小狼毫版本，
  ; 至少保证每次构建的备份名不同，不会覆盖上一版留下的备份。
  !ifdef PRODUCTVERSION
    StrCpy $RimeSyncSeedVersion "${PRODUCTVERSION}"
  !else
    StrCpy $RimeSyncSeedVersion "${RIMESYNC_WEASEL_DIRVER}"
  !endif
  StrCmp $RimeSyncSeedVersion "" 0 rimesync_post_have_version
    StrCpy $RimeSyncSeedVersion "unknown"
  rimesync_post_have_version:

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=POSTINSTALL 输入法已先安装，开始建快捷方式"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL begin shortcuts"
  Call RimeSyncLog

  ; 1) 自己的两个入口
  Call RimeSyncCreateOwnShortcuts
  ; 2) 有 Weasel 就把「同步设置」也放进它的程序组
  Call RimeSyncCreateWeaselShortcut
  ; 3) 推荐输入法配置：缺失才补，写进真实用户目录（见 RimeSyncSeedRimeConfig）
  Call RimeSyncSeedRimeConfig
  ; 3b) 0.6.21：清掉非保留方案（datadefault.yaml 的 schema_list 也在里面裁）。
  ;     必须在 SELF-CHECK 的部署之前：否则部署会按残留的 16 个方案重建 build。
  Call RimeSyncPruneSchemas
  ; 4) 安装结束自检：就绪 / 注册表 InstallDir / 默认布局 WeaselServer.exe 是硬性检查
  ;    （0.6.14 起任一不满足 = 安装失败：weasel-ready=no + 弹框 + 退出码 5，但绝不 Abort）；
  ;    另外报配置目录与数量 / 调一次部署 / build 产物是否更新
  Call RimeSyncSelfCheck

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=POSTINSTALL 完成"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL done"
  Call RimeSyncLog

  ; 本次没装输入法（只有 /SKIPIME 会走到这儿；未就绪且修不好已在 PREINSTALL 硬失败），
  ; 必须当面告诉用户（静默安装只写日志）
  StrCmp $RimeSyncWeaselKept "1" 0 rimesync_post_no_weasel_warning
    DetailPrint "[rime-sync] 警告：本次按 /SKIPIME 跳过了输入法安装"
    StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL warning=ime-skipped-by-switch ready=$RimeSyncWeaselReady"
    Call RimeSyncLog
    ${IfNot} ${Silent}
      MessageBox MB_OK|MB_ICONEXCLAMATION "rime-sync 已安装完成；按 /SKIPIME 请求，本次没有安装或修复小狼毫输入法。$\r$\n$\r$\n需要输入法时，请先手动安装小狼毫，再运行一次本安装包（不要带 /SKIPIME）。$\r$\n$\r$\n用户数据（%APPDATA%\Rime）不受影响。"
    ${EndIf}
  rimesync_post_no_weasel_warning:

  ; 收尾标记：成功写 phase=DONE（失败时没有这一行，便于区分「装完」与「中途退出」）。
  ; 0.6.14：自检硬性检查不通过 → 不写 DONE，改写 phase=SETUP failed，并再设一次退出码 5
  ; （Tauri 模板后续语句可能覆盖 SetErrorLevel，这里补一次最稳妥）。
  StrCmp $RimeSyncWeaselSelfCheckFail "1" rimesync_post_failed
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=DONE 安装完成；配置：新写入 $RimeSyncSeedWritten 个，沿用已有 $RimeSyncSeedReused 个；日志：$TEMP\rime-sync-setup.log"
  StrCpy $R3 "[rime-sync] $R2 phase=DONE setup finished configs=$RimeSyncSeedWritten backups=$RimeSyncSeedBackups weasel-kept=$RimeSyncWeaselKept weasel-ready=$RimeSyncWeaselReady install-dir=$RimeSyncWeaselRegInstallDir default-exe=$RimeSyncWeaselDefaultExe deploy=$RimeSyncDeployExit build=$RimeSyncBuildState build-source=$RimeSyncBuildSource prune=$RimeSyncPruneState warnings=$RimeSyncSelfCheckWarn"
  Call RimeSyncLog
  Goto rimesync_post_done
  rimesync_post_failed:
  SetErrorLevel 5
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=SETUP failed：自检未通过（输入法不可用），退出码 5；日志：$TEMP\rime-sync-setup.log"
  StrCpy $R3 "[rime-sync] $R2 phase=SETUP failed stage=SELF-CHECK exit=5 weasel-ready=$RimeSyncWeaselReady install-dir=$RimeSyncWeaselRegInstallDir default-exe=$RimeSyncWeaselDefaultExe root=$RimeSyncWeaselRootDir warnings=$RimeSyncSelfCheckWarn"
  Call RimeSyncLog
  rimesync_post_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 0) 先问「是否同时删除本地用户数据」（默认否；静默模式需 /PURGEDATA）
  ;    只记决定，删除放到本宏最后（等小狼毫卸载器跑完，免得它把文件写回去）
  Call un.RimeSyncAskPurgeUserData

  ; 1) 再处理小狼毫（仅当是我们装的；没标记/带 /KEEPWEASEL 时保留）
  ; 卸载器这时还在 $INSTDIR 里，后面 Tauri 会把程序目录删掉
  Call un.RimeSyncUninstallBundledWeasel

  ; 再删本安装器创建的东西；小狼毫的程序组只在它自己卸载时才消失
  Delete "$SMPROGRAMS\${RIMESYNC_NAME}\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\${RIMESYNC_NAME}\${RIMESYNC_SETUP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${RIMESYNC_NAME}"

  Delete "$SMPROGRAMS\小狼毫输入法\${RIMESYNC_SETUP_NAME}.lnk"
  Delete "$SMPROGRAMS\小狼毫輸入法\${RIMESYNC_SETUP_NAME}.lnk"
  Delete "$SMPROGRAMS\Weasel\${RIMESYNC_SETUP_NAME}.lnk"

  ; 旧版钩子（0.3.0 及以前）可能留下过这些名字
  Delete "$SMPROGRAMS\小狼毫\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\小狼毫输入法\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\Weasel\${RIMESYNC_NAME}.lnk"
  Delete "$DESKTOP\${RIMESYNC_NAME}.lnk"

  ; 安装顺序日志（静默安装的验收证据）
  Delete "${RIMESYNC_ORDER_LOG}"

  ; 0.6.15：预编译 build 产物是安装包带进来的，卸载时一并清掉（用户目录里的那份不动）
  RMDir /r "$INSTDIR\resources\prebuilt"

  ; 0.6.20：随包输入法安装器（resources\weasel\，程序自己修复输入法时要用）
  RMDir /r "$INSTDIR\resources\weasel"

  ; 旧版内嵌的小狼毫安装包与许可证文本（Tauri 也会清理 resources，这里显式兜底）
  Delete "$INSTDIR\\resources\${RIMESYNC_WEASEL_INSTALLER}"
  Delete "$INSTDIR\\resources\WEASEL-NOTICE.txt"
  Delete "$INSTDIR\\resources\GPL-3.0.txt"
  Delete "$INSTDIR\\resources\README.md"
  RMDir "$INSTDIR\\resources"

  ; 2) 最后按用户选择删除本地用户数据（%APPDATA% 里的两条路径；已在上一步确认）
  Call un.RimeSyncPurgeUserData
!macroend
