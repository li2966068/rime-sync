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
assert_has "备份名 <file>.bak-<版本>"             '.bak-$RimeSyncSeedVersion"'
assert_has "同名备份追加 -1/-2…"                  '.bak-$RimeSyncSeedVersion-$4'
assert_has "覆盖前先 Rename（不删用户文件）"       'Rename "$1\$2" "$5"'
assert_has "DONE 阶段报配置文件数/备份数"          "phase=DONE setup finished configs=\$RimeSyncSeedWritten backups=\$RimeSyncSeedBackups"
assert_has "用户数据不动：只写三类 *.custom.yaml"   'seed skip schemas: no templates'

# 失败一律只记日志：这些函数体里不能有 Abort
for fn in \
  RimeSyncRunAndWait \
  RimeSyncNormalizeUninstallPath \
  RimeSyncUninstallOldRimeSync \
  RimeSyncUninstallOldWeasel \
  RimeSyncUninstallOldVersion \
  RimeSyncWriteConfigFile \
  RimeSyncSeedRimeConfig; do
  assert_fn_no_abort "$fn"
done

if [ "$static_fail" -ne 0 ]; then
  echo "check-nsis-hooks: 静态结构断言失败（见上）" >&2
  exit 1
fi
printf 'ok   static  %s 条结构断言（卸载命令/120s 超时/失败不 Abort/备份命名）\n' "$static_count"

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
