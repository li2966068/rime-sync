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
; 阶段名（phase=）：OS-CHECK / WEBVIEW-CHECK / WEBVIEW-INSTALL / IME-INSTALL /
;                   POSTINSTALL / DONE；失败路径同样写日志。
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
; 写入推荐 Rime 配置：只在用户 Rime 目录缺文件时复制，绝不覆盖用户已有配置。
; per-machine 安装下 $APPDATA 属于提权用户（通常是安装者本人）；若该目录还不存在
; （用户尚未初始化小狼毫），只记录日志，模板仍留在 $INSTDIR\resources\ 里。
; ---------------------------------------------------------------------------
Function RimeSyncSeedRimeConfig
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL seed rime config"
  Call RimeSyncLog
  ; 0.6.10：用户目录不存在时不再整体跳过——建出来再写（写进去的只是 *.custom.yaml，
  ; 小狼毫首次部署会照常合并；旧行为会让“刚装完还没启动过输入法”的机器什么都拿不到）。
  CreateDirectory "$APPDATA\Rime"
  IfFileExists "$APPDATA\Rime\*.*" rimesync_seed_dir_ready
    DetailPrint "[rime-sync] 无法创建 $APPDATA\Rime，跳过推荐配置写入"
    StrCpy $R3 "[rime-sync] $R2 seed skipped: cannot create Rime user dir"
    Call RimeSyncLog
    Return
  rimesync_seed_dir_ready:
  IfFileExists "$APPDATA\Rime\default.custom.yaml" rimesync_seed_skip_default
    ; 推荐配置列了 16 个随包方案（含五笔）。官方 Weasel 安装包的 data 没有五笔，
    ; schema_list 里列一个缺失方案会让 librime 整次部署失败（exit 1），所以先确认
    ; 已装小狼毫的 data 里真有 wubi86.schema.yaml；没有就跳过，保留 Weasel 自己的方案表。
    Call RimeSyncFindWeaselRoot
    StrCmp $R8 "" rimesync_seed_default_gate
    IfFileExists "$R8\data\wubi86.schema.yaml" rimesync_seed_default_ok rimesync_seed_default_gate
    rimesync_seed_default_gate:
      DetailPrint "[rime-sync] 小狼毫 data 没有 wubi86（官方安装包不含五笔），跳过推荐 default.custom.yaml，避免部署失败"
      StrCpy $R3 "[rime-sync] $R2 seed skipped default.custom.yaml: no wubi86 in $R8\data"
      Call RimeSyncLog
      Goto rimesync_seed_skip_default
    rimesync_seed_default_ok:
      CopyFiles /SILENT "$INSTDIR\\resources\default.custom.yaml" "$APPDATA\Rime\default.custom.yaml"
      DetailPrint "[rime-sync] 写入推荐 default.custom.yaml（每页 8 个候选 / 16 个方案：拼音·五笔·仓颉·笔画·注音）"
      StrCpy $R3 "[rime-sync] $R2 seed default.custom.yaml"
      Call RimeSyncLog
  rimesync_seed_skip_default:
  IfFileExists "$APPDATA\Rime\weasel.custom.yaml" rimesync_seed_skip_weasel
    CopyFiles /SILENT "$INSTDIR\\resources\weasel.custom.yaml" "$APPDATA\Rime\weasel.custom.yaml"
    DetailPrint "[rime-sync] 写入推荐 weasel.custom.yaml（候选横向排列）"
    StrCpy $R3 "[rime-sync] $R2 seed weasel.custom.yaml"
    Call RimeSyncLog
  rimesync_seed_skip_weasel:
  ; 随包的 schema 增强配置（v 数值转换 / 中文标点直出）。当前 Windows 安装包还没有
  ; resources\schemas 目录（需要 tauri.conf.json 的 resources 映射），所以先探测：
  ; 有模板才写，写入规则与 default.custom.yaml 一致——用户目录里已有同名文件就跳过。
  IfFileExists "$INSTDIR\\resources\schemas\*.*" rimesync_schema_seed_root
    StrCpy $R3 "[rime-sync] $R2 seed skipped: no resources\schemas"
    Call RimeSyncLog
    Goto rimesync_schema_seed_done
  rimesync_schema_seed_root:
  ; 模板引用的方案必须真在 Weasel 的 data 里，否则部署会失败。
  ; 0.6.10：改成**逐个模板**判断（luna_pinyin.custom.yaml 只要求 data 里有 luna_pinyin.schema.yaml），
  ; 不再一刀切要求 wubi86——否则老版本小狼毫（data 无五笔）上 v 模式/标点直出会被全部跳过。
  Call RimeSyncFindWeaselRoot
  StrCmp $R8 "" rimesync_schema_seed_done
  Push $R5
  Push $R6
  Push $R4
  FindFirst $R5 $R6 "$INSTDIR\resources\schemas\*.custom.yaml"
  rimesync_schema_seed_loop:
  StrCmp $R6 "" rimesync_schema_seed_end
    IfFileExists "$APPDATA\Rime\$R6" rimesync_schema_seed_next
      ; <schema>.custom.yaml 只有在 Weasel data 里有 <schema>.schema.yaml 时才写，
      ; 避免部署时找不到方案（老版本小狼毫没有五笔，但仍能吃到 luna 系的增强）。
      StrCpy $R4 $R6 -12
      IfFileExists "$R8\data\$R4.schema.yaml" rimesync_schema_seed_copy rimesync_schema_seed_missing
      rimesync_schema_seed_copy:
        CopyFiles /SILENT "$INSTDIR\resources\schemas\$R6" "$APPDATA\Rime\$R6"
        IfFileExists "$APPDATA\Rime\$R6" rimesync_schema_seed_copied rimesync_schema_seed_copyfail
        rimesync_schema_seed_copied:
          DetailPrint "[rime-sync] 写入 $R6（v 数值转换 / 中文标点直出）"
          StrCpy $R3 "[rime-sync] $R2 seed $R6 ok"
          Call RimeSyncLog
          Goto rimesync_schema_seed_next
        rimesync_schema_seed_copyfail:
          DetailPrint "[rime-sync] 复制 $R6 失败（目标 $APPDATA\Rime）"
          StrCpy $R3 "[rime-sync] $R2 seed $R6 FAILED copy"
          Call RimeSyncLog
          Goto rimesync_schema_seed_next
      rimesync_schema_seed_missing:
        StrCpy $R3 "[rime-sync] $R2 seed skip $R6: no $R4.schema.yaml in data"
        Call RimeSyncLog
    rimesync_schema_seed_next:
    FindNext $R5 $R6
    Goto rimesync_schema_seed_loop
  rimesync_schema_seed_end:
  FindClose $R5
  Pop $R6
  Pop $R5
  rimesync_schema_seed_done:
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
; PREINSTALL：先装小狼毫（在 Tauri 拷主程序与 resources 之前）
; 已安装则跳过（避免把用户现有小狼毫重装/降级）；失败只写日志，不阻断主安装。
; 只有“原本没有、由本次安装装上”的情况才写所有权标记。
; ---------------------------------------------------------------------------
Function RimeSyncInstallWeaselPre
  ; /SKIPIME：已有输入法时强制跳过（当前逻辑本来就会跳过“已存在”，
  ; 这个开关是给「已装但探测不到 / 用户不想动输入法」的场景用的显式出口）
  ${GetOptions} $CMDLINE "/SKIPIME" $R1
  IfErrors rimesync_pre_no_skipime
    Call RimeSyncStamp
    DetailPrint "[rime-sync] 收到 /SKIPIME：跳过内嵌小狼毫安装"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL skip=1 switch=/SKIPIME"
    Call RimeSyncLog
    Return
  rimesync_pre_no_skipime:

  Call RimeSyncFindWeaselRoot
  StrCmp $R8 "" rimesync_pre_install
    DetailPrint "[rime-sync] 已检测到小狼毫（$R8），跳过内嵌安装包"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL skip existing weasel"
    Call RimeSyncLog
    Return

  rimesync_pre_install:
  StrCpy $R0 ""
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
    ; 兜底：旧布局（resources 里带安装包）只在 POSTINSTALL 之后才存在，
    ; 但这里仍检查一次，便于“自定义模板先解压 resources”的场景。
    IfFileExists "$INSTDIR\\resources\${RIMESYNC_WEASEL_INSTALLER}" 0 rimesync_pre_checked
      StrCpy $R0 "$INSTDIR\\resources\${RIMESYNC_WEASEL_INSTALLER}"
    rimesync_pre_checked:
  !endif

  StrCmp $R0 "" 0 rimesync_pre_run
    DetailPrint "[rime-sync] 警告：未找到内嵌的小狼毫安装包，跳过输入法安装"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL missing weasel installer"
    Call RimeSyncLog
    Return

  rimesync_pre_run:
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=PREINSTALL 开始静默安装小狼毫"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL begin weasel install"
  Call RimeSyncLog

  ClearErrors
  ExecWait '"$R0" /S' $0
  IfErrors rimesync_pre_launch_failed

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=PREINSTALL 小狼毫安装器退出码 $0"
  StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL weasel installer exit=$0"
  Call RimeSyncLog
  StrCmp $0 "0" rimesync_pre_ok
    DetailPrint "[rime-sync] 警告：小狼毫安装器退出码 $0（不阻断 rime-sync 安装，也不写所有权标记）"
    Goto rimesync_pre_cleanup

  rimesync_pre_launch_failed:
    Call RimeSyncStamp
    DetailPrint "[rime-sync] [$R2] 警告：无法启动小狼毫安装器（不阻断 rime-sync 安装）"
    StrCpy $R3 "[rime-sync] $R2 phase=IME-INSTALL exec failed"
    Call RimeSyncLog
    Goto rimesync_pre_cleanup

  rimesync_pre_ok:
    DetailPrint "[rime-sync] 小狼毫静默安装完成"
    ; 装完再探测一次：注册表/默认目录此时才出现 WeaselServer.exe。
    ; 探测不到也照样写标记（安装器已成功退出，说明是我们装的），
    ; 卸载时再用 UninstallString 兜底。
    Call RimeSyncFindWeaselRoot
    Call RimeSyncMarkBundledWeasel

  rimesync_pre_cleanup:
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
  ; 0.6.5 顺序：系统版本 → WebView2 → 输入法 → 交还 Tauri 拷主程序。
  ; 三步都在 Tauri File 主程序之前，任何一步 Abort 都不会留下半个安装。
  Call RimeSyncCheckWindows
  CreateDirectory "$INSTDIR\\resources"
  Call RimeSyncEnsureWebView2
  Call RimeSyncInstallWeaselPre
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; 宏在安装段插入时才展开，此时 Tauri 已定义 MAINBINARYNAME / PRODUCTNAME
  StrCpy $R6 "$INSTDIR\${MAINBINARYNAME}.exe"
  StrCpy $R7 "$SMPROGRAMS\${RIMESYNC_NAME}\${PRODUCTNAME}.lnk"

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=POSTINSTALL 输入法已先安装，开始建快捷方式"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL begin shortcuts"
  Call RimeSyncLog

  ; 1) 自己的两个入口
  Call RimeSyncCreateOwnShortcuts
  ; 2) 有 Weasel 就把「同步设置」也放进它的程序组
  Call RimeSyncCreateWeaselShortcut
  ; 3) 推荐输入法配置（仅在用户目录缺文件时写入）
  Call RimeSyncSeedRimeConfig

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=POSTINSTALL 完成"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL done"
  Call RimeSyncLog

  ; 收尾标记：安装日志里必须有 phase=DONE（失败时不会有，便于区分「装完」与「中途退出」）
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=DONE 安装完成；日志：$TEMP\rime-sync-setup.log"
  StrCpy $R3 "[rime-sync] $R2 phase=DONE setup finished"
  Call RimeSyncLog
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

  ; 旧版内嵌的小狼毫安装包与许可证文本（Tauri 也会清理 resources，这里显式兜底）
  Delete "$INSTDIR\\resources\${RIMESYNC_WEASEL_INSTALLER}"
  Delete "$INSTDIR\\resources\WEASEL-NOTICE.txt"
  Delete "$INSTDIR\\resources\GPL-3.0.txt"
  Delete "$INSTDIR\\resources\README.md"
  RMDir "$INSTDIR\\resources"

  ; 2) 最后按用户选择删除本地用户数据（%APPDATA% 里的两条路径；已在上一步确认）
  Call un.RimeSyncPurgeUserData
!macroend
