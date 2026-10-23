; rime-sync · NSIS 安装钩子
; 由 src-tauri/tauri.conf.json 的 bundle.windows.nsis.installerHooks 引用。
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
; 验收证据：除了 DetailPrint，安装过程还会把带时间戳的顺序日志追加到
;   $INSTDIR\install-ime-order.log
; （静默安装看不到 DetailPrint，这个文件是唯一可复核的证据；卸载时删除）
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
;   用户数据 %APPDATA%\Rime 与 rime-sync 自己的配置/备份**始终不动**：
;   本文件没有任何针对 $APPDATA 的 Delete/RMDir，Weasel 0.17.4 的卸载器同样只删
;   安装目录、程序组与注册表（见 third_party/weasel/output/install.nsi 的 Section "Uninstall"）。
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

; /KEEPWEASEL 用 ${GetOptions} 解析（Tauri 模板也 include 了 FileFunc.nsh，
; 该头文件自带 FILEFUNC_INCLUDED 保护，重复 include 安全）。
!include "FileFunc.nsh"

!define RIMESYNC_NAME "Rime 同步"
!define RIMESYNC_SETUP_NAME "同步设置"
!define RIMESYNC_WEASEL_INSTALLER "weasel-0.17.4.0-installer.exe"
!define RIMESYNC_WEASEL_DIRVER "0.17.4"
!define RIMESYNC_ORDER_LOG "$INSTDIR\install-ime-order.log"
!define RIMESYNC_UNINSTALL_LOG "$TEMP\rime-sync-uninstall.log"

; 所有权标记（卸载时读同一个键）；64 位视图，与 perMachine 安装上下文一致。
!define RIMESYNC_REG_KEY "Software\rime-sync"

; 构建期由 scripts/ime/build-weasel.ps1 放到 packaging/windows/vendor/。
; 不存在时不直接报错：允许“离线兜底/根本没编译”的构建继续，只给编译告警，
; 运行时再退回旧的 resources 布局（旧布局在 PREINSTALL 阶段通常拿不到文件）。
!define RIMESYNC_WEASEL_EXE "${__FILEDIR__}\windows\vendor\${RIMESYNC_WEASEL_INSTALLER}"

!if /FileExists "${RIMESYNC_WEASEL_EXE}"
  !define RIMESYNC_WEASEL_EMBEDDED
!else
  !warning "rime-sync: vendored weasel installer not found, IME will not be installed (run scripts/ime/build-weasel.ps1)"
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
  ClearErrors
  FileOpen $R4 "${RIMESYNC_ORDER_LOG}" a
  IfErrors rimesync_log_done
  FileWrite $R4 "$R3$\r$\n"
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
  IfFileExists "$APPDATA\Rime\*.*" rimesync_seed_dir_ready
    DetailPrint "[rime-sync] $APPDATA\Rime 不存在（用户尚未初始化小狼毫），跳过推荐配置写入"
    StrCpy $R3 "[rime-sync] $R2 seed skipped: no Rime user dir"
    Call RimeSyncLog
    Return
  rimesync_seed_dir_ready:
  IfFileExists "$APPDATA\Rime\default.custom.yaml" rimesync_seed_skip_default
    CopyFiles /SILENT "$INSTDIR\resources\default.custom.yaml" "$APPDATA\Rime\default.custom.yaml"
    DetailPrint "[rime-sync] 写入推荐 default.custom.yaml（每页 8 个候选 / 4 个方案）"
    StrCpy $R3 "[rime-sync] $R2 seed default.custom.yaml"
    Call RimeSyncLog
  rimesync_seed_skip_default:
  IfFileExists "$APPDATA\Rime\weasel.custom.yaml" rimesync_seed_skip_weasel
    CopyFiles /SILENT "$INSTDIR\resources\weasel.custom.yaml" "$APPDATA\Rime\weasel.custom.yaml"
    DetailPrint "[rime-sync] 写入推荐 weasel.custom.yaml（候选横向排列）"
    StrCpy $R3 "[rime-sync] $R2 seed weasel.custom.yaml"
    Call RimeSyncLog
  rimesync_seed_skip_weasel:
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
    StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL ownership marker written"
    Call RimeSyncLog
    Return
  rimesync_mark_failed:
    DetailPrint "[rime-sync] 警告：写小狼毫所有权标记失败，卸载 rime-sync 时将保留小狼毫"
    StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL ownership marker write failed"
    Call RimeSyncLog
FunctionEnd

; ---------------------------------------------------------------------------
; PREINSTALL：先装小狼毫（在 Tauri 拷主程序与 resources 之前）
; 已安装则跳过（避免把用户现有小狼毫重装/降级）；失败只写日志，不阻断主安装。
; 只有“原本没有、由本次安装装上”的情况才写所有权标记。
; ---------------------------------------------------------------------------
Function RimeSyncInstallWeaselPre
  Call RimeSyncFindWeaselRoot
  StrCmp $R8 "" rimesync_pre_install
    DetailPrint "[rime-sync] 已检测到小狼毫（$R8），跳过内嵌安装包"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL skip existing weasel"
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
    IfFileExists "$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}" 0 rimesync_pre_checked
      StrCpy $R0 "$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}"
    rimesync_pre_checked:
  !endif

  StrCmp $R0 "" 0 rimesync_pre_run
    DetailPrint "[rime-sync] 警告：未找到内嵌的小狼毫安装包，跳过输入法安装"
    Call RimeSyncStamp
    StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL missing weasel installer"
    Call RimeSyncLog
    Return

  rimesync_pre_run:
  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=PREINSTALL 开始静默安装小狼毫"
  StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL begin weasel install"
  Call RimeSyncLog

  ClearErrors
  ExecWait '"$R0" /S' $0
  IfErrors rimesync_pre_launch_failed

  Call RimeSyncStamp
  DetailPrint "[rime-sync] [$R2] 阶段=PREINSTALL 小狼毫安装器退出码 $0"
  StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL weasel installer exit=$0"
  Call RimeSyncLog
  StrCmp $0 "0" rimesync_pre_ok
    DetailPrint "[rime-sync] 警告：小狼毫安装器退出码 $0（不阻断 rime-sync 安装，也不写所有权标记）"
    Goto rimesync_pre_cleanup

  rimesync_pre_launch_failed:
    Call RimeSyncStamp
    DetailPrint "[rime-sync] [$R2] 警告：无法启动小狼毫安装器（不阻断 rime-sync 安装）"
    StrCpy $R3 "[rime-sync] $R2 phase=PREINSTALL exec failed"
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
  DetailPrint "[rime-sync] [$R2] 阶段=PREUNINSTALL 开始卸载（用户数据 %APPDATA%\Rime 始终保留）"
  StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL begin keep-userdata"
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
    DetailPrint "[rime-sync] 小狼毫已静默卸载；用户数据 %APPDATA%\Rime 保留"
    StrCpy $R3 "[rime-sync] $R2 phase=PREUNINSTALL weasel uninstalled, user data kept"
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
; 宏：Tauri 插入点
; ---------------------------------------------------------------------------
!macro NSIS_HOOK_PREINSTALL
  ; 先装满输入法，再让 Tauri 拷主程序（宏返回后 Tauri 才 File 主程序 / resources）
  CreateDirectory "$INSTDIR\resources"
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
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 先处理小狼毫（仅当是我们装的；没标记/带 /KEEPWEASEL 时保留）
  ; 必须放在最前面：$INSTDIR 这时还在，后面 Tauri 会把程序目录删掉
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
  Delete "$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}"
  Delete "$INSTDIR\resources\WEASEL-NOTICE.txt"
  Delete "$INSTDIR\resources\GPL-3.0.txt"
  Delete "$INSTDIR\resources\README.md"
  RMDir "$INSTDIR\resources"
!macroend
