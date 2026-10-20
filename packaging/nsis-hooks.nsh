; rime-sync · NSIS 安装钩子
; 由 src-tauri/tauri.conf.json 的 bundle.windows.nsis.installerHooks 引用。
; Tauri 在 installer.nsi / uninstaller.nsi 中以 !ifmacrodef 方式插入以下宏，
; 因此这里只定义需要的钩子，不覆盖 Tauri 自己的安装逻辑。

!define RIMESYNC_SHORTCUT_NAME "Rime 同步"
!define RIMESYNC_EXE "rime-sync.exe"

; 在指定程序组里建快捷方式（目录不存在则创建）
!macro RIMESYNC_SHORTCUT_INTO GROUP_DIR
  CreateDirectory "${GROUP_DIR}"
  CreateShortcut "${GROUP_DIR}\Rime 同步.lnk" "$INSTDIR\${RIMESYNC_EXE}" "" \
    "$INSTDIR\${RIMESYNC_EXE}" 0 SW_SHOWNORMAL "" "Rime 词库与配置的 WebDAV 双向同步"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  IfFileExists "$INSTDIR\${RIMESYNC_EXE}" 0 rimesync_postinstall_done
    ; 1) 自己的程序组
    !insertmacro RIMESYNC_SHORTCUT_INTO "$SMPROGRAMS\Rime 同步"

    ; 2) 尝试归入 Weasel（小狼毫）程序组：中英文两种组名都探测
    IfFileExists "$SMPROGRAMS\小狼毫\*.*" 0 rimesync_try_weasel_en
      CreateShortcut "$SMPROGRAMS\小狼毫\Rime 同步.lnk" "$INSTDIR\${RIMESYNC_EXE}" "" \
        "$INSTDIR\${RIMESYNC_EXE}" 0 SW_SHOWNORMAL "" "Rime 词库与配置的 WebDAV 双向同步"
    rimesync_try_weasel_en:
    IfFileExists "$SMPROGRAMS\Weasel\*.*" 0 rimesync_postinstall_done
      CreateShortcut "$SMPROGRAMS\Weasel\Rime 同步.lnk" "$INSTDIR\${RIMESYNC_EXE}" "" \
        "$INSTDIR\${RIMESYNC_EXE}" 0 SW_SHOWNORMAL "" "Rime 词库与配置的 WebDAV 双向同步"
  rimesync_postinstall_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 删除本安装器创建的快捷方式；不删除 Weasel 组本身，也不动用户词典目录
  Delete "$SMPROGRAMS\Rime 同步\Rime 同步.lnk"
  RMDir "$SMPROGRAMS\Rime 同步"
  Delete "$SMPROGRAMS\小狼毫\Rime 同步.lnk"
  Delete "$SMPROGRAMS\Weasel\Rime 同步.lnk"
  Delete "$DESKTOP\Rime 同步.lnk"
!macroend
