#!/usr/bin/env bash
# check-nsis-hooks.sh —— 在 Linux/macOS 上单独编译 packaging/nsis-hooks*.nsh。
#
# 为什么需要：钩子文件靠 Tauri 模板提供的 PRODUCTNAME / MAINBINARYNAME /
# MAINBINARYSRCPATH 与两个 NSIS_HOOK_* 插入点才能编译，平时只能在 CI 的 Windows
# runner（pnpm tauri build）里被间接验证。本脚本把这些桩起来，用 makensis -WX
# （告警即错误）编译「在线」与「离线」两套钩子，几分钟内就能发现语法/宏错误。
#
# 用法：
#   bash packaging/windows/check-nsis-hooks.sh            # 没有 makensis 时打印 SKIP 并退出 0
#   bash packaging/windows/check-nsis-hooks.sh --strict   # 没有 makensis 视为失败（CI 用）
#
# 验证范围：NSIS 编译期（语法、标签、!ifdef 分支、File 内嵌路径、未引用函数告警）。
# 不验证：运行期行为（注册表探测、弹框、ExecWait 退出码）—— 那需要真 Windows。
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1

# ---------------------------------------------------------------------------
# 1) 静态结构断言（不需要 makensis）
#    编译只能证明「语法对」，证明不了「卸载命令长得对 / 超时在 / 失败不 Abort」。
#    这里按函数体做文本断言，改坏了会直接指出是哪一条（--strict 与非 strict 都跑）。
#    0.6.11 起：卸载旧版 → 全新安装，POSTINSTALL 按新版无条件写配置（覆盖前备份）。
#    0.6.13 起：Weasel 就绪判定（注册表 + exe）、未就绪自动修复且失败硬退出、
#               配置写进 current 用户目录、POSTINSTALL 自检（部署 + build 产物）。
# ---------------------------------------------------------------------------
HOOKS="$PKG/nsis-hooks.nsh"
static_fail=0
static_count=0

assert_has() { # <说明> <固定字符串>
  static_count=$((static_count + 1))
  if ! grep -qF -- "$2" "$HOOKS"; then
    printf 'FAIL static  %s\n           缺少：%s\n' "$1" "$2" >&2
    static_fail=1
  fi
}

fn_body() { # <函数名> → 该函数体（到同列 FunctionEnd 为止）
  awk -v fn="$1" '
    $0 == "Function " fn { inside = 1 }
    inside { print }
    inside && $0 == "FunctionEnd" { exit }
  ' "$HOOKS"
}

assert_fn_has() { # <函数名> <说明> <固定字符串>
  static_count=$((static_count + 1))
  if ! fn_body "$1" | grep -qF -- "$3"; then
    printf 'FAIL static  %s\n           函数 %s 里缺少：%s\n' "$2" "$1" "$3" >&2
    static_fail=1
  fi
}

assert_count() { # <说明> <固定字符串> <最少出现次数>（整文件，用于宏体里的调用）
  static_count=$((static_count + 1))
  n=$(grep -cF -- "$2" "$HOOKS" || true)
  if [ "${n:-0}" -lt "$3" ]; then
    printf 'FAIL static  %s\n           整个钩子文件里 "%s" 只出现 %s 处（应 ≥ %s）\n' "$1" "$2" "${n:-0}" "$3" >&2
    static_fail=1
  fi
}

assert_fn_count() { # <函数名> <说明> <固定字符串> <最少出现次数>
  static_count=$((static_count + 1))
  n=$(fn_body "$1" | grep -cF -- "$3" || true)
  if [ "${n:-0}" -lt "$4" ]; then
    printf 'FAIL static  %s\n           函数 %s 里 "%s" 只出现 %s 处（应 ≥ %s）\n' "$2" "$1" "$3" "${n:-0}" "$4" >&2
    static_fail=1
  fi
}

assert_fn_not_has() { # <函数名> <说明> <禁止出现的固定字符串>
  static_count=$((static_count + 1))
  if fn_body "$1" | grep -qF -- "$3"; then
    printf 'FAIL static  %s\n           函数 %s 里不该有：%s\n' "$2" "$1" "$3" >&2
    static_fail=1
  fi
}

assert_fn_no_abort() { # <函数名>
  static_count=$((static_count + 1))
  if fn_body "$1" | grep -qE '^[[:space:]]*Abort\b'; then
    printf 'FAIL static  %s 里出现 Abort（卸载/写配置失败只许记日志）\n' "$1" >&2
    static_fail=1
  elif [ -z "$(fn_body "$1")" ]; then
    printf 'FAIL static  找不到函数 %s\n' "$1" >&2
    static_fail=1
  fi
}

# 卸载旧版：抽成函数、静默 + 原地卸载（_?=）、120 秒超时、先停进程
assert_has "PREINSTALL 第一步调用卸载旧版"        "Call RimeSyncUninstallOldVersion"
assert_has "旧 rime-sync：路径带引号 + /S + 原地 _?=" '"$R9" /S _?=$0'
assert_has "旧小狼毫：路径带引号 + /S"                    '"$R9" /S'
assert_has "卸载前先 taskkill 同步程序"           "taskkill /IM rime-sync-app.exe /F"
assert_has "卸载前先 taskkill 输入法服务"         "taskkill /IM WeaselServer.exe /F"
assert_has "卸载等待有 120 秒超时"                '!define RIMESYNC_UNINSTALL_TIMEOUT_MS "120000"'
assert_has "超时通过 nsExec /TIMEOUT 生效"        'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_UNINSTALL_TIMEOUT_MS}'
assert_has "阶段日志 UNINSTALL-OLD"               "phase=UNINSTALL-OLD"
assert_has "旧输入法会被卸载（标记+卸载命令）"      'StrCpy $R9 $RimeSyncOldWeaselCmd'
assert_has "旧小狼毫仍在时给出警告标记"            'StrCpy $RimeSyncWeaselKept "1"'

# POSTINSTALL：无条件写配置 + 覆盖前备份 + DONE 报数
assert_has "已有配置一律复用（不覆盖不删除）"     'rimesync_wcfg_reuse'
assert_has "复用计数 result=reuse"               'StrCpy $3 "reuse"'
assert_has "复制失败记录 failed"                 'rimesync_wcfg_failed'
assert_has "DONE 阶段报配置文件数/备份数"          "phase=DONE setup finished configs=\$RimeSyncSeedWritten backups=\$RimeSyncSeedBackups"
assert_has "用户数据不动：只写三类 *.custom.yaml"   'seed skip schemas: no templates'

# 0.6.13：Weasel 就绪判定（注册表 + exe，两条同时成立才算就绪）
assert_fn_count RimeSyncWeaselReady "就绪判定读 InstallDir（64+32 两个视图）" \
  'ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"' 2
assert_fn_count RimeSyncWeaselReady "就绪判定读 WeaselRoot（64+32 两个视图）" \
  'ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"' 2
assert_fn_has RimeSyncWeaselReady "就绪判定同时读 32 位视图" 'SetRegView 32'
assert_fn_has RimeSyncWeaselReady "就绪判定看进程证据" 'Call RimeSyncWeaselServerRunning'
assert_fn_has RimeSyncWeaselReady "就绪判定看 TSF 证据" 'Call RimeSyncWeaselTsfProbe'
assert_fn_has RimeSyncWeaselReadyFromDir "命中条件：目录里有 WeaselServer.exe" \
  'Call RimeSyncWeaselRootFromDir'
assert_fn_has RimeSyncWeaselReadyFromDir "探测时不冲掉依据标签（$1 压栈）" 'Push $1'
assert_has "就绪判定要求 WeaselServer.exe 真的存在" 'IfFileExists "$R5\WeaselServer.exe"'
assert_has "任意版本号目录（weasel-*）也认" 'FindFirst $0 $1 "$R5\weasel-*"'
assert_has "注册表有值但没有 exe 也算未就绪" 'no-WeaselServer.exe'
assert_has "未就绪原因是 registry-missing" 'StrCpy $RimeSyncWeaselReadyReason "registry-missing"'

# 0.6.13：未就绪 → 停进程 → 残留目录改名留证 → 内嵌安装器（300s）→ 复查 → 硬失败
assert_fn_has RimeSyncInstallWeaselPre "未就绪走修复流程" 'Call RimeSyncBackupLeftoverWeasel'
assert_fn_has RimeSyncInstallWeaselPre "修复前先停输入法服务" "taskkill /IM WeaselServer.exe /F"
assert_fn_count RimeSyncInstallWeaselPre "修复走带 300 秒超时的等待" 'Call RimeSyncRunWeaselInstaller' 1
assert_fn_has RimeSyncInstallWeaselPre "内嵌安装器静默 + 防重启" '"$R0" /S /NOREBOOT'
assert_fn_has RimeSyncInstallWeaselPre "安装后必须复查就绪" 'Call RimeSyncWeaselReady'
assert_fn_has RimeSyncInstallWeaselPre "已就绪分支写清依据" 'phase=IME-CHECK ready=yes'
assert_fn_has RimeSyncInstallWeaselPre "未就绪分支写清原因" 'phase=IME-CHECK ready=no'
assert_fn_has RimeSyncInstallWeaselPre "已就绪仍然跳过安装" 'action=skip'
assert_has "残留目录改名成 Rime.bak-<时间戳>" '.bak-$R2'
assert_has "改名结果写进日志" 'phase=IME-REPAIR rename'
assert_has "安装器等待上限 300 秒" '!define RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS "300000"'
assert_has "300 秒超时真的传给 nsExec" \
  'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS}'
assert_fn_has RimeSyncInstallWeaselPre "修复仍不就绪写 WEASEL-INSTALL failed" \
  'WEASEL-INSTALL failed reason=$RimeSyncWeaselReadyReason installer-exit=$R4'
assert_fn_has RimeSyncInstallWeaselPre "缺少载荷也是硬失败" 'WEASEL-INSTALL failed reason=payload-missing'
assert_fn_has RimeSyncAbortWeasel "硬失败设非零退出码" 'SetErrorLevel 5'
assert_fn_has RimeSyncAbortWeasel "硬失败必须 Abort" 'Abort'
assert_has "硬失败路径在安装器后可达" 'Call RimeSyncAbortWeasel'

# 0.6.13：配置播种必须写进 current 用户目录（不能是 ProgramData）
assert_fn_count RimeSyncSeedRimeConfig "播种切到 current 上下文" 'SetShellVarContext current' 1
assert_fn_has RimeSyncSeedRimeConfig "解析出的用户目录存进全局" 'StrCpy $RimeSyncRimeUserDir "$APPDATA"'
assert_fn_has RimeSyncSeedRimeConfig "默认配置写进该目录" 'CreateDirectory "$RimeSyncRimeUserDir"'
assert_fn_has RimeSyncSeedRimeConfig "weasel.custom.yaml 也写该目录" 'StrCpy $1 "$RimeSyncRimeUserDir"'
assert_fn_not_has RimeSyncSeedRimeConfig '播种里不再直接用 $APPDATA 拼路径' '$APPDATA\Rime'

# 0.6.13：安装结束自检（部署 + build 产物 + 弹框/日志）
assert_fn_has RimeSyncSelfCheck "自检再判一次就绪" 'Call RimeSyncWeaselReady'
assert_fn_has RimeSyncSelfCheck "自检调用部署" 'WeaselDeployer.exe" /deploy'
assert_has "部署等待上限 20 秒" '!define RIMESYNC_DEPLOY_TIMEOUT_MS "20000"'
assert_has "20 秒超时真的传给 nsExec" 'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_DEPLOY_TIMEOUT_MS}'
assert_fn_has RimeSyncSelfCheck "自检检查 build 里的 luna_pinyin" 'build\luna_pinyin.schema.yaml'
assert_fn_has RimeSyncSelfCheck "自检写 phase=SELF-CHECK" 'phase=SELF-CHECK'
assert_fn_has RimeSyncSelfCheck "自检报配置目录与数量" 'configdir=$0 configs=$RimeSyncSeedWritten'
assert_fn_has RimeSyncSelfCheck "自检未更新只告警" 'warning=build-not-updated'
assert_fn_has RimeSyncSelfCheck "非静默时弹简报框" 'MessageBox MB_OK|MB_ICONINFORMATION'
assert_count "POSTINSTALL 调用自检" 'Call RimeSyncSelfCheck' 1
assert_has "DONE 行带自检字段" 'weasel-ready=$RimeSyncWeaselReady deploy=$RimeSyncDeployExit'

# 失败一律只记日志：这些函数体里不能有 Abort
for fn in \
  RimeSyncRunAndWait \
  RimeSyncRunWeaselInstaller \
  RimeSyncNormalizeUninstallPath \
  RimeSyncUninstallOldRimeSync \
  RimeSyncUninstallOldWeasel \
  RimeSyncUninstallOldVersion \
  RimeSyncWriteConfigFile \
  RimeSyncSeedRimeConfig \
  RimeSyncSelfCheck \
  RimeSyncBackupLeftoverWeasel \
  RimeSyncWeaselReady; do
  assert_fn_no_abort "$fn"
done

if [ "$static_fail" -ne 0 ]; then
  echo "check-nsis-hooks: 静态结构断言失败（见上）" >&2
  exit 1
fi
printf 'ok   static  %s 条结构断言（卸载命令/120s+300s 超时/就绪判定/改名留证/自检/失败不 Abort）\n' "$static_count"

if ! command -v makensis >/dev/null 2>&1; then
  if [ "$STRICT" -eq 1 ]; then
    echo "check-nsis-hooks: 缺少 makensis（apt-get install nsis / makensis.org），--strict 下视为失败" >&2
    exit 1
  fi
  echo "check-nsis-hooks: SKIP（没有 makensis；在线包/离线包的真实校验在 CI 的 windows job）"
  exit 0
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/packaging/windows/vendor" "$WORK/out"
ln -sfn "$PKG/nsis-hooks.nsh" "$WORK/packaging/nsis-hooks.nsh"
ln -sfn "$PKG/nsis-hooks-offline.nsh" "$WORK/packaging/nsis-hooks-offline.nsh"

# 假载荷：只需存在即可（NSIS 编译期 File 会读它）。
printf 'MZ fake weasel installer (harness only)\n' \
  >"$WORK/packaging/windows/vendor/weasel-0.17.4.0-installer.exe"
# WebView2 用不可压缩数据：离线包体积应当比在线包正好大一个载荷，
# 这就是「离线变体确实内嵌了 Standalone、在线变体没有内嵌」的编译期证据。
head -c 1048576 /dev/urandom >"$WORK/packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe" 2>/dev/null ||
  dd if=/dev/urandom of="$WORK/packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe" bs=1024 count=1024 2>/dev/null
printf 'MZ' | dd of="$WORK/packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe" \
  bs=1 seek=0 conv=notrunc status=none

cat >"$WORK/harness.nsi" <<'NSI'
; 最小 harness：桩掉 Tauri 模板的变量与插入点，只编译 rime-sync 的钩子文件。
Unicode true
Name "rime-sync hooks harness"
OutFile "OUTDIR/harness.exe"
InstallDir "$PROGRAMFILES64\rime-sync"
RequestExecutionLevel admin
!define PRODUCTNAME "rime-sync"
!define MAINBINARYNAME "rime-sync-app"
!define MAINBINARYSRCPATH "HARNESS"

INCLUDE_LINE

Section "Install"
  SetOutPath $INSTDIR
  !insertmacro NSIS_HOOK_PREINSTALL
  File "/oname=rime-sync-app.exe" "${MAINBINARYSRCPATH}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd

Section "Uninstall"
  !insertmacro NSIS_HOOK_PREUNINSTALL
SectionEnd
NSI

fail=0
for variant in online offline; do
  hooks=nsis-hooks.nsh
  [ "$variant" = offline ] && hooks=nsis-hooks-offline.nsh
  sed -e "s|OUTDIR|$WORK/out|" \
    -e "s|HARNESS|$WORK/harness.nsi|" \
    -e "s|INCLUDE_LINE|!include \"$WORK/packaging/$hooks\"|" \
    "$WORK/harness.nsi" >"$WORK/harness-$variant.nsi"

  log="$WORK/$variant.log"
  if makensis -WX -V3 "$WORK/harness-$variant.nsi" >"$log" 2>&1; then
    size=$(wc -c <"$WORK/out/harness.exe")
    warn=$(grep -cE 'warning|Error' "$log" || true)
    printf 'ok   %-7s %9s bytes  warnings=%s\n' "$variant" "$size" "${warn:-0}"
    cp "$WORK/out/harness.exe" "$WORK/out/$variant.exe"
  else
    printf 'FAIL %-7s 编译失败：\n' "$variant"
    sed -n '1,40p' "$log"
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  online=$(wc -c <"$WORK/out/online.exe")
  offline=$(wc -c <"$WORK/out/offline.exe")
  delta=$((offline - online))
  printf 'delta    %8s bytes（离线包 - 在线包；假载荷 1048576 字节，应≈它）\n' "$delta"
  if [ "$delta" -lt 900000 ]; then
    echo "FAIL 离线包没有内嵌 WebView2 载荷（差值 $delta 太小）"
    fail=1
  fi
fi

[ "$fail" -eq 0 ] && echo "check-nsis-hooks: 两套钩子均 0 warning 0 error"
exit "$fail"
