; rime-sync · NSIS 安装钩子
; 由 src-tauri/tauri.conf.json 的 bundle.windows.nsis.installerHooks 引用。
; Tauri 在 installer.nsi / uninstaller.nsi 里以 !ifmacrodef 方式插入这些宏，
; 因此本文件只定义宏与私有函数，不覆盖 Tauri 自己的安装逻辑。
;
; 前提（见 tauri.conf.json 的 bundle.windows.nsis）：
;   installMode = perMachine  → 安装器以管理员运行，$SMPROGRAMS / $INSTDIR 都是
;   「所有用户」上下文（Tauri 的 SetContext 宏已 SetShellVarContext all）。
;   内嵌的小狼毫官方安装包由 bundle.resources 装到 $INSTDIR\resources\。
;
; 安装结束**不启动** rime-sync：Tauri 模板在完成页有「运行」复选框，
; 这里把它的默认状态改成不勾选（MUI_FINISHPAGE_RUN_NOTCHECKED），
; 用户想用只能自己从开始菜单点开。
;
; 注意：本文件的 Function 会在 !include 处立即编译，而 ${MAINBINARYNAME} /
; ${PRODUCTNAME} 要等 Tauri 模板在 include 之后才定义，所以这两个符号只在
; 宏体里使用，由宏在插入时展开后经 $R6 / $R7 传给函数。

!define RIMESYNC_NAME "Rime 同步"
!define RIMESYNC_SETUP_NAME "同步设置"
!define RIMESYNC_WEASEL_INSTALLER "weasel-0.17.4.0-installer.exe"
!define RIMESYNC_WEASEL_DIRVER "0.17.4"

; 完成页「运行 rime-sync」复选框默认不勾选（不自动启动主程序）
!define MUI_FINISHPAGE_RUN_NOTCHECKED

; ---------------------------------------------------------------------------
; 探测：$R8 = 小狼毫安装根目录（含 WeaselServer.exe），未安装则空字符串
; 说明：Weasel 0.17 的安装根目录是 <Program Files>\Rime\weasel-0.17.4，
;       老版本是 <Program Files>\Rime\Weasel；自定义目录无法枚举，由程序组探测兜底。
; ---------------------------------------------------------------------------
Function RimeSyncFindWeaselRoot
  StrCpy $R8 ""
  ; 1) 注册表：Weasel 写 HKLM\Software\Rime\Weasel 的 WeaselRoot / InstallDir。
  ;    自定义安装目录只能靠这一条；64/32 位视图都读（Weasel 的 NSIS 是 32 位，
  ;    但 WeaselSetup 是原生 x64，两个视图都可能命中）。读完恢复 64 位视图
  ;    （本工程只出 x64/arm64 安装包，与 Tauri SetContext 的默认一致）。
  SetRegView 64
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"
  IfFileExists "$R5\WeaselServer.exe" rimesync_root_set_reg
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"
  IfFileExists "$R5\WeaselServer.exe" rimesync_root_set_reg
  SetRegView 32
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"
  IfFileExists "$R5\WeaselServer.exe" rimesync_root_set_reg
  ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"
  IfFileExists "$R5\WeaselServer.exe" rimesync_root_set_reg
  SetRegView 64
  ; 2) 默认安装目录
  StrCmp $PROGRAMFILES64 "" rimesync_root_pf32
    IfFileExists "$PROGRAMFILES64\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}\WeaselServer.exe" rimesync_root_set64ver
    IfFileExists "$PROGRAMFILES64\Rime\Weasel\WeaselServer.exe" rimesync_root_set64
  rimesync_root_pf32:
    IfFileExists "$PROGRAMFILES\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}\WeaselServer.exe" rimesync_root_set32ver
    IfFileExists "$PROGRAMFILES\Rime\Weasel\WeaselServer.exe" rimesync_root_set32
  Goto rimesync_root_done
  rimesync_root_set_reg:
    StrCpy $R8 "$R5"
    Goto rimesync_root_done
  rimesync_root_set64ver:
    StrCpy $R8 "$PROGRAMFILES64\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}"
    Goto rimesync_root_done
  rimesync_root_set64:
    StrCpy $R8 "$PROGRAMFILES64\Rime\Weasel"
    Goto rimesync_root_done
  rimesync_root_set32ver:
    StrCpy $R8 "$PROGRAMFILES\Rime\weasel-${RIMESYNC_WEASEL_DIRVER}"
    Goto rimesync_root_done
  rimesync_root_set32:
    StrCpy $R8 "$PROGRAMFILES\Rime\Weasel"
  rimesync_root_done:
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
; 静默安装内嵌的小狼毫：<installer> /S
; 已安装则跳过（避免把用户现有小狼毫重装/降级）；失败只写 DetailPrint，不阻断主安装。
; ---------------------------------------------------------------------------
Function RimeSyncInstallWeasel
  Call RimeSyncFindWeaselRoot
  StrCmp $R8 "" rimesync_wi_install
    DetailPrint "[rime-sync] 已检测到小狼毫（$R8），跳过内嵌安装包"
    Return

  rimesync_wi_install:
  IfFileExists "$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}" rimesync_wi_found_res
  IfFileExists "$INSTDIR\${RIMESYNC_WEASEL_INSTALLER}" rimesync_wi_found_root
    DetailPrint "[rime-sync] 警告：未找到内嵌的小狼毫安装包（$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}），跳过输入法安装"
    Return

  rimesync_wi_found_res:
    StrCpy $R0 "$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}"
    Goto rimesync_wi_run

  rimesync_wi_found_root:
    StrCpy $R0 "$INSTDIR\${RIMESYNC_WEASEL_INSTALLER}"

  rimesync_wi_run:
  DetailPrint "[rime-sync] 静默安装小狼毫：$R0 /S"
  ClearErrors
  ExecWait '"$R0" /S' $0
  IfErrors rimesync_wi_launch_failed
  StrCmp $0 "0" rimesync_wi_ok
    DetailPrint "[rime-sync] 警告：小狼毫安装器退出码 $0（不阻断 rime-sync 安装）"
    Return

  rimesync_wi_launch_failed:
    DetailPrint "[rime-sync] 警告：无法启动小狼毫安装器（不阻断 rime-sync 安装）"
    Return

  rimesync_wi_ok:
    DetailPrint "[rime-sync] 小狼毫静默安装完成"
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
; 宏：Tauri 插入点
; ---------------------------------------------------------------------------
!macro NSIS_HOOK_POSTINSTALL
  ; 宏在安装段插入时才展开，此时 Tauri 已定义 MAINBINARYNAME / PRODUCTNAME
  StrCpy $R6 "$INSTDIR\${MAINBINARYNAME}.exe"
  StrCpy $R7 "$SMPROGRAMS\${RIMESYNC_NAME}\${PRODUCTNAME}.lnk"
  ; 1) 先装小狼毫（静默），这样它的程序组已存在，第 3 步才能准确归位
  Call RimeSyncInstallWeasel
  ; 2) 自己的两个入口
  Call RimeSyncCreateOwnShortcuts
  ; 3) 有 Weasel 就把「同步设置」也放进它的程序组
  Call RimeSyncCreateWeaselShortcut
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 只删本安装器创建的东西；小狼毫本体、词库与它的程序组**默认保留**
  ; （卸载说明见 docs/THIRD-PARTY.md 与安装包内 resources\WEASEL-NOTICE.txt）
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

  ; 内嵌的小狼毫安装包与许可证文本（Tauri 也会清理 resources，这里显式兜底）
  Delete "$INSTDIR\resources\${RIMESYNC_WEASEL_INSTALLER}"
  Delete "$INSTDIR\resources\WEASEL-NOTICE.txt"
  Delete "$INSTDIR\resources\GPL-3.0.txt"
  Delete "$INSTDIR\resources\README.md"
  RMDir "$INSTDIR\resources"
!macroend
