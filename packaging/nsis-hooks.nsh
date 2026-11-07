; rime-sync · NSIS 安装钩子
; 由 src-tauri/tauri.conf.json 的 bundle.windows.nsis.installerHooks 引用。
; 离线变体：packaging/nsis-hooks-offline.nsh 先 !define RIMESYNC_EMBED_WEBVIEW
; 再 !include 本文件（由 src-tauri/tauri.offline.conf.json 选用）。
;
; 职责边界（B 方案）：安装器**只负责程序本体**。
;   装：Windows 版本门槛 → WebView2 检查/安装 → 静默卸载旧版 rime-sync →
;       Tauri 拷主程序与 resources → 开始菜单快捷方式 → DONE 日志。
;   卸：问「是否删除本地用户数据」→ 清快捷方式与随包载荷 → Tauri 卸主程序。
;
; 输入法（小狼毫 / Weasel）**完全不归安装器管**：不装、不判、不铺 prebuilt、
; 不跑部署、不 prune、不自检、不设退出码、不弹框。这些全部搬到程序里
; （crates/core/src/ime_state.rs 的 ensure(Mode::Install)，见 docs/WINDOWS-IME-BOOTSTRAP.md）：
;   * 程序首次启动（后台线程）自举：Weasel 缺失/不是我们的构建 → 随包安装器
;     /S（静默、不重启）装回 → 播 config 模板（缺失才写）→ 铺 prebuilt → patch data →
;     需要时部署 → assess；
;   * 安装 Weasel 需要管理员：普通用户启动时程序会弹阻塞对话框 + 设置页红条
;     （「修复 / 以管理员重启」），绝不静默失败。
; 为什么这么改：/S 静默安装时 Tauri 根本不执行 NSIS 钩子（0.6.19 实测），
; 旧钩子还把「输入法就绪」当硬失败（退出码 5），于是静默升级必然装出「没有任何
; 输入法」的机器。唯一可靠的时机是程序自己启动时。
;
; 安装后自动启动：
;   * 交互式安装：Tauri 完成页的「运行 rime-sync」复选框**默认勾选**（本文件刻意
;     不覆盖它的默认状态），用户点完成后程序立刻启动并自举输入法；
;   * 静默安装 /S：NSIS 不显示完成页，也不会启动程序 —— 由用户首次手动启动或
;     开机自启（--autostart）触发同一套自举。
;
; 升级路径（老用户）：旧版卸载器会连带卸掉小狼毫，新版安装器**不装回**；
; 从「升级完成」到「首次启动程序」之间机器上没有输入法（用户已接受）。首次启动
; 需要管理员权限才能把输入法装回来，程序会明确提示。
;
; 验收证据（静默安装也能事后复核）：$TEMP\rime-sync-setup.log（UTF-16LE）
;   阶段名：OS-CHECK / WEBVIEW-CHECK / WEBVIEW-INSTALL / UNINSTALL-OLD / POSTINSTALL / DONE。
;   卸载日志：$TEMP\rime-sync-uninstall.log（不随 $INSTDIR 删除）。
;
; 安装器命令行参数：
;   /SKIPWEBVIEW  缺 WebView2 也不阻断（不联网、不弹框、只写日志）
;
; Windows 版本门槛：${AtLeastWin10} 且 CurrentBuildNumber >= 17134（Windows 10 1803），
; 低于门槛弹框说明并 Abort（唯一会中止安装的两条路径之一，另一条是缺 WebView2）。
;
; 卸载时用户数据的处理（默认保留，必须显式确认才删）：
;   - 交互卸载（非静默）：MessageBox MB_YESNO|MB_ICONEXCLAMATION 询问
;     「是否同时删除本地用户数据？」，默认「否」（直接关掉窗口也算否）。
;   - 静默卸载（/S）：只有命令行带 /PURGEDATA 才删，否则一律保留。
;   - 删除范围严格限定两条：%APPDATA%\Rime 与 %APPDATA%\com.rimesync.app。
;     先 RMDir /r，再检查是否真的消失；失败只写卸载日志并继续卸载，绝不 Abort。
;   - 小狼毫由用户自己安装/卸载：rime-sync 的卸载器不再连带卸它。
;
; 前提（见 tauri.conf.json 的 bundle.windows.nsis）：
;   installMode = perMachine → 安装器以管理员运行，$SMPROGRAMS / $INSTDIR 都是
;   「所有用户」上下文（Tauri 的 SetContext 宏已 SetShellVarContext all）。
;
; 注意：本文件的 Function 会在 !include 处立即编译，而 ${MAINBINARYNAME} /
; ${PRODUCTNAME} 要等 Tauri 模板在 include 之后才定义，所以这两个符号只在
; 宏体里使用，由宏在插入时展开后经 $R6 / $R7 传给函数。

; /SKIPWEBVIEW 用 ${GetOptions} 解析（Tauri 模板也 include 了 FileFunc.nsh，
; 该头文件自带 FILEFUNC_INCLUDED 保护，重复 include 安全）。
!include "FileFunc.nsh"
; 系统版本判定：${AtLeastWin10}（Win10/Win11 都为真）+ CurrentBuildNumber 补 1803 门槛。
; WinVer.nsh 自带 ___WINVER__NSH___ 保护，Tauri 模板若已 include 也不冲突。
!include "WinVer.nsh"
; WinVer.nsh 的 LogicLib 扩展需要 LogicLib.nsh（自带 LOGICLIB_INCLUDED 保护）。
!include "LogicLib.nsh"

!define RIMESYNC_NAME "Rime 同步"
!define RIMESYNC_SETUP_NAME "同步设置"
!define RIMESYNC_ORDER_LOG "$INSTDIR\install-order.log"
!define RIMESYNC_UNINSTALL_LOG "$TEMP\rime-sync-uninstall.log"

; 统一安装日志（UTF-16LE）：%TEMP% 下这一份在安装结束后仍可复核，失败路径（Abort 前）也写。
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

; 升级即全新安装：先静默卸载旧版同步程序。卸载器最多等 120 秒
; （nsExec /TIMEOUT），超时就结束进程继续装，绝不 Abort。
!define RIMESYNC_UNINSTALL_TIMEOUT_MS "120000"

; 卸载时用户数据的处理决定（1 = 删除，0 = 保留）与来源（prompt / /PURGEDATA / none）。
; 用全局 Var 而不是寄存器：Section Uninstall 里要跨两次函数调用保存它。
!ifndef RIMESYNC_USERDATA_VARS
  !define RIMESYNC_USERDATA_VARS
  Var /GLOBAL RimeSyncPurgeUserData
  Var /GLOBAL RimeSyncPurgeSource
!endif

; 升级流程要跨函数保存的状态。寄存器（$0-$9 / $R0-$R9）会被 Call 链里的
; RimeSyncStamp / RimeSyncLog 冲掉，只能用全局 Var 保存。
!ifndef RIMESYNC_UPGRADE_VARS
  !define RIMESYNC_UPGRADE_VARS
  Var /GLOBAL RimeSyncOldUninstKey      ; 命中的 Uninstall 子键名（或 (instdir)）
  Var /GLOBAL RimeSyncOldUninstRaw      ; 该子键的 UninstallString（未归一化）
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
; 追加到 $INSTDIR\install-order.log
; ---------------------------------------------------------------------------
Function RimeSyncLog
  ; $R3 = 一行文本（调用方已写入 $R2 时间戳与 phase=）
  ; 同时写两份：旧的验收文件路径 + 统一 setup 日志（UTF-16LE）
  ClearErrors
  FileOpen $R4 "${RIMESYNC_ORDER_LOG}" a
  IfErrors rimesync_log_setup
    ; 写位置挪到末尾：NSIS 的 "a" 模式只保证文件存在，写入位置可能仍在 0。
    ; 不挪的话每次都从文件头覆盖，日志只剩最后一行（实测 wine 与真机同源二进制）。
    FileSeek $R4 0 END
    FileWrite $R4 "$R3$\r$\n"
    FileClose $R4
  rimesync_log_setup:
  ClearErrors
  FileOpen $R4 "${RIMESYNC_SETUP_LOG}" a
  IfErrors rimesync_log_done
    ; 同上：追加前先把写位置挪到文件末尾。
    FileSeek $R4 0 END
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
  ; 同上：写位置挪到末尾，否则每行都从文件头覆盖（实测日志只剩最后一行 + 残渣）。
  FileSeek $R4 0 END
  FileWrite $R4 "$R3$\r$\n"
  FileClose $R4
  rimesync_ulog_${UN}done:
FunctionEnd
!macroend
!insertmacro RIMESYNC_FN_UNINSTALL_LOG "un."

; ---------------------------------------------------------------------------
; 系统版本门槛：Windows 10 1803（10.0.17134）及以上
;   ${AtLeastWin10} 只看主版本号（Win10 / Win11 都为真），构建号再拦 1803 以下。
;   低于门槛：写日志 → 弹框说明 → Abort（不写任何文件）。
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
; WebView2 探测
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
;   2) 注册表没写（精简/绿色安装）时，退回程序文件
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
    ; 在线包：只给下载地址，绝不自动下载
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
; 升级即全新安装：停进程 → 卸旧同步程序 → 清旧所有权标记 → 交还 Tauri。
;   任何一步失败都只写日志、绝不 Abort：装上一个能用的新版，比半路退出好。
;   阶段名统一记 UNINSTALL-OLD（含被卸载对象、退出码、耗时毫秒）。
;   输入法不在这里处理（B 方案：程序首次启动时自举）。
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
;   "C:\Program Files\rime-sync\uninstall.exe"   （NSIS 卸载器，带引号）
;   C:\rime-sync\uninstall.exe                     （万一没带引号，整体当路径）
; 带附加参数（"x.exe" /S）的写法不猜，直接判无效，交给调用方兜底。
; 路径不存在同样判无效（旧版被手动删掉的情况）。
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
FunctionEnd

; ---------------------------------------------------------------------------
; 卸载旧版同步程序：$INSTDIR\uninstall.exe 优先，其次 Uninstall 注册项（两个视图）。
; 命令形如 "C:\Program Files\rime-sync\uninstall.exe" /S _?=C:\Program Files\rime-sync
; （_?= 的目录在 NSIS 卸载器里必须裸写、不引号；见 NSIS 手册的静默卸载示例）。
; 只写日志，不 Abort；日志行写在卸载之后 —— 旧卸载器会删掉 $INSTDIR\install-order.log。
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
; PREINSTALL 的一部分：把旧版同步程序清干净，后面就按全新安装走。
;   输入法不在这里处理：旧卸载器（0.6.13 及以前）可能连带卸掉小狼毫，
;   新版安装器不装回 —— 由程序首次启动时自举（见文件头）。
; ---------------------------------------------------------------------------
Function RimeSyncUninstallOldVersion
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=UNINSTALL-OLD begin policy=clean-install"
  Call RimeSyncLog

  Call RimeSyncKillRunningProcesses
  Call RimeSyncUninstallOldRimeSync

  ; 旧版写过的 HKLM\Software\rime-sync 所有权标记已经没人读了（新版由程序
  ; 自举输入法），这里兜底删掉，免得留下误导性的旧标记。
  SetRegView 64
  DeleteRegKey HKLM "Software\rime-sync"

  ; 旧卸载器可能把 $INSTDIR 一起删了，而后面还要把日志写回 $INSTDIR
  CreateDirectory "$INSTDIR"
  Call RimeSyncStamp
  StrCpy $R3 "[rime-sync] $R2 phase=UNINSTALL-OLD done（下一步：全新安装）"
  Call RimeSyncLog
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
; 卸载：询问是否删除本地用户数据（默认保留）
; 交互模式弹 MessageBox（默认「否」，关掉窗口也算否）；静默模式只在命令行带
; /PURGEDATA 时删。这里只记决定，真正的删除交给 un.RimeSyncPurgeUserData，
; 放在 Section Uninstall 末尾（免得别的清理过程把文件又写回去）。
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
  ; 顺序：系统版本 → WebView2 → 卸旧同步程序 → 建资源目录 → 交还 Tauri 拷主程序。
  ; 系统版本是只读检查，放在最前面（先卸载再 Abort 会把用户晾在
  ; 「旧版没了、新版没装」的半路）。
  ; 这里**不碰输入法**：不判定、不安装、不铺产物、不部署、不自检、不设退出码。
  Call RimeSyncCheckWindows
  Call RimeSyncEnsureWebView2
  Call RimeSyncUninstallOldVersion
  CreateDirectory "$INSTDIR\resources"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; 宏在安装段插入时才展开，此时 Tauri 已定义 MAINBINARYNAME / PRODUCTNAME。
  StrCpy $R6 "$INSTDIR\${MAINBINARYNAME}.exe"
  StrCpy $R7 "$SMPROGRAMS\${RIMESYNC_NAME}\${PRODUCTNAME}.lnk"

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=POSTINSTALL 建快捷方式（输入法由程序首次启动时自举）"
  StrCpy $R3 "[rime-sync] $R2 phase=POSTINSTALL begin shortcuts"
  Call RimeSyncLog

  ; 自己的两个入口
  Call RimeSyncCreateOwnShortcuts

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=DONE 安装完成；日志：$TEMP\rime-sync-setup.log"
  ; 收尾标记：成功写 phase=DONE（失败时没有这一行，便于区分「装完」与「中途退出」）。
  ; 安装器不判定输入法，也不设退出码：输入法由程序首次启动时自举并给出提示。
  StrCpy $R3 "[rime-sync] $R2 phase=DONE setup finished note=ime-bootstrap-deferred-to-app"
  Call RimeSyncLog
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 0) 先问「是否同时删除本地用户数据」（默认否；静默模式需 /PURGEDATA）
  ;    只记决定，删除放到本宏最后。
  Call un.RimeSyncAskPurgeUserData

  ; 1) 删本安装器创建的东西；小狼毫是用户自己装的，卸载 rime-sync 不再连带卸它。
  Delete "$SMPROGRAMS\${RIMESYNC_NAME}\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\${RIMESYNC_NAME}\${RIMESYNC_SETUP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${RIMESYNC_NAME}"

  Delete "$SMPROGRAMS\小狼毫输入法\${RIMESYNC_SETUP_NAME}.lnk"
  Delete "$SMPROGRAMS\小狼毫輸入法\${RIMESYNC_SETUP_NAME}.lnk"
  Delete "$SMPROGRAMS\Weasel\${RIMESYNC_SETUP_NAME}.lnk"

  ; 旧版钩子（0.3.0 及以前）可能留下过这些名字
  Delete "$SMPROGRAMS\小狼毫\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\小狼毫输入法\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\小狼毫輸入法\${RIMESYNC_NAME}.lnk"
  Delete "$SMPROGRAMS\Weasel\${RIMESYNC_NAME}.lnk"
  Delete "$DESKTOP\${RIMESYNC_NAME}.lnk"

  ; 安装顺序日志（静默安装的验收证据）
  Delete "${RIMESYNC_ORDER_LOG}"

  ; 随包载荷：预编译 build 产物与输入法安装器（用户目录里的那份不动）
  RMDir /r "$INSTDIR\resources\prebuilt"
  RMDir /r "$INSTDIR\resources\weasel"
  ; 旧版内嵌的小狼毫安装包与许可证文本（Tauri 也会清理 resources，这里显式兜底）
  Delete "$INSTDIR\resources\weasel-0.17.4.0-installer.exe"
  Delete "$INSTDIR\resources\WEASEL-NOTICE.txt"
  Delete "$INSTDIR\resources\GPL-3.0.txt"
  Delete "$INSTDIR\resources\README.md"
  RMDir "$INSTDIR\resources"

  ; 2) 最后按用户选择删除本地用户数据（%APPDATA% 里的两条路径；已在上一步确认）
  Call un.RimeSyncPurgeUserData
!macroend
