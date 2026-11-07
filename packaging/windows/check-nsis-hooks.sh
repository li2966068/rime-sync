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
# 验证范围：NSIS 编译期（语法、标签、!ifdef 分支、File 内嵌路径、未引用函数告警）
# + 静态结构断言（B 方案：安装器不碰输入法）。
# 不验证：运行期行为——由 packaging/windows/test-nsis-hooks-wine.sh（wine）与
#         .github/scripts/e2e-windows-ime.ps1（真 Windows）负责。
#
# 「B 方案」：输入法的一切判断与安装从 NSIS 搬到程序
#   （crates/core/src/ime_state.rs 的 ensure(Mode::Install)）。因此本脚本的核心
#   断言从「钩子里必须有就绪判定/自检」反转为「钩子里必须没有这些」：
#     * 没有 Weasel 安装调用、prebuilt 铺设、prune、自检、部署、文本匹配；
#     * 没有 StrStrW / rimesync_vnum / rimesync_punct / ensure-ime / check-ime /
#       RIMESYNC_PREBUILT 之类的判据与解析；
#     * 没有 config 模板播种（default.custom.yaml / weasel.custom.yaml / schemas）；
#     * POSTINSTALL 不设退出码、不弹框、不判定输入法。
#   同时保留对「安装器本职」的断言：OS 版本门槛、WebView2 检查/安装、卸旧版
#   rime-sync、快捷方式、$INSTDIR\resources、卸载时的用户数据询问与清理。
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
REPO=$(cd "$PKG/.." && pwd)
STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1

# ---------------------------------------------------------------------------
# 1) 静态结构断言（不需要 makensis）
# ---------------------------------------------------------------------------
# RIMESYNC_HOOKS：给「反向验证」用（对临时改坏的副本跑同一套断言，不动工作区文件）
HOOKS="${RIMESYNC_HOOKS:-$PKG/nsis-hooks.nsh}"
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

macro_body() { # <宏名> → 宏体（到 !macroend 为止）
  awk -v m="!macro $1" '
    $0 == m { inside = 1 }
    inside { print }
    inside && $0 == "!macroend" { exit }
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

assert_macro_has() { # <宏名> <说明> <固定字符串>
  static_count=$((static_count + 1))
  if ! macro_body "$1" | grep -qF -- "$3"; then
    printf 'FAIL static  %s\n           宏 %s 里缺少：%s\n' "$2" "$1" "$3" >&2
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

assert_macro_not_has() { # <宏名> <说明> <禁止出现的固定字符串>
  static_count=$((static_count + 1))
  if macro_body "$1" | grep -qF -- "$3"; then
    printf 'FAIL static  %s\n           宏 %s 里不该有：%s\n' "$2" "$1" "$3" >&2
    static_fail=1
  fi
}

assert_absent() { # <说明> <固定字符串>：整文件里必须已经不存在
  static_count=$((static_count + 1))
  if grep -qF -- "$2" "$HOOKS"; then
    printf 'FAIL static  %s\n           不该再出现：%s\n' "$1" "$2" >&2
    static_fail=1
  fi
}

assert_fn_order() { # <函数名> <先出现> <后出现> <说明>
  static_count=$((static_count + 1))
  a=$(fn_body "$1" | grep -nF -- "$2" | head -1 | cut -d: -f1)
  b=$(fn_body "$1" | grep -nF -- "$3" | head -1 | cut -d: -f1)
  if [ -z "$a" ] || [ -z "$b" ] || [ "$a" -ge "$b" ]; then
    printf 'FAIL static  %s\n           %s 里 "%s"(行 %s) 必须先于 "%s"(行 %s)\n' \
      "$4" "$1" "$2" "${a:-缺}" "$3" "${b:-缺}" >&2
    static_fail=1
  fi
}

assert_macro_order() { # <宏名> <先出现> <后出现> <说明>
  static_count=$((static_count + 1))
  a=$(macro_body "$1" | grep -nF -- "$2" | head -1 | cut -d: -f1)
  b=$(macro_body "$1" | grep -nF -- "$3" | head -1 | cut -d: -f1)
  if [ -z "$a" ] || [ -z "$b" ] || [ "$a" -ge "$b" ]; then
    printf 'FAIL static  %s\n           %s 里 "%s"(行 %s) 必须先于 "%s"(行 %s)\n' \
      "$4" "$1" "$2" "${a:-缺}" "$3" "${b:-缺}" >&2
    static_fail=1
  fi
}

# 安装侧正文：整份钩子去掉 NSIS_HOOK_PREUNINSTALL。
# 卸载要清掉随包载荷（resources\weasel 与安装器文件本身），那些名字只允许出现在卸载段。
install_side() {
  awk '
    $0 == "!macro NSIS_HOOK_PREUNINSTALL" { skip = 1 }
    !skip { print }
    skip && $0 == "!macroend" { skip = 0 }
  ' "$HOOKS"
}

assert_install_side_absent() { # <说明> <固定字符串>
  static_count=$((static_count + 1))
  if install_side | grep -qF -- "$2"; then
    printf 'FAIL static  %s\n           安装侧不该出现：%s\n' "$1" "$2" >&2
    static_fail=1
  fi
}

assert_fn_no_abort() { # <函数名>
  static_count=$((static_count + 1))
  if fn_body "$1" | grep -qE '^[[:space:]]*Abort\b'; then
    printf 'FAIL static  %s 里出现 Abort（卸载/写日志失败只许记日志）\n' "$1" >&2
    static_fail=1
  elif [ -z "$(fn_body "$1")" ]; then
    printf 'FAIL static  找不到函数 %s\n' "$1" >&2
    static_fail=1
  fi
}

assert_file_has() { # <说明> <文件> <固定字符串>
  static_count=$((static_count + 1))
  if ! grep -qF -- "$3" "$2" 2>/dev/null; then
    printf 'FAIL static  %s\n           文件 %s 里缺少：%s\n' "$1" "$2" "$3" >&2
    static_fail=1
  fi
}

# ---------------------------------------------------------------------------
# 1a) 安装器本职：OS 版本 / WebView2 / 卸旧版 / 快捷方式 / resources
# ---------------------------------------------------------------------------
assert_has "PREINSTALL 有系统版本检查"        "Call RimeSyncCheckWindows"
assert_has "PREINSTALL 有 WebView2 检查"      "Call RimeSyncEnsureWebView2"
assert_has "PREINSTALL 卸载旧版 rime-sync"    "Call RimeSyncUninstallOldVersion"
assert_has "PREINSTALL 创建 resources 目录"   'CreateDirectory "$INSTDIR\resources"'
assert_macro_order NSIS_HOOK_PREINSTALL 'Call RimeSyncCheckWindows' 'Call RimeSyncUninstallOldVersion' \
  "系统版本检查必须在卸旧版之前（先卸载再 Abort 会留下半装状态）"
assert_macro_order NSIS_HOOK_PREINSTALL 'Call RimeSyncUninstallOldVersion' 'CreateDirectory "$INSTDIR\resources"' \
  "resources 目录必须在卸旧版之后创建"

# OS 门槛：Win10 1803，太低 → 弹框 + 退出码 3 + Abort
assert_has "OS 门槛用 AtLeastWin10"           '${AtLeastWin10}'
assert_has "OS 门槛构建号 17134"              '!define RIMESYNC_MIN_WIN10_BUILD "17134"'
assert_fn_has RimeSyncCheckWindows "过低时设退出码 3" 'SetErrorLevel 3'
assert_fn_has RimeSyncCheckWindows "过低时 Abort"     'Abort'

# WebView2：四个注册表位置 + 文件兜底 + /SKIPWEBVIEW + 离线内嵌
assert_has "WebView2 GUID 已定义"             '!define RIMESYNC_WEBVIEW_GUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"'
assert_has "WebView2 在线地址只打印不下载"    '!define RIMESYNC_WEBVIEW_URL "https://go.microsoft.com/fwlink/?linkid=2124703"'
assert_fn_has RimeSyncFindWebView2 "读 HKLM 64 位视图" 'SetRegView 64'
assert_fn_has RimeSyncFindWebView2 "读 32 位视图"      'SetRegView 32'
assert_fn_has RimeSyncFindWebView2 "程序文件兜底"      'msedgewebview2.exe'
assert_fn_has RimeSyncEnsureWebView2 "/SKIPWEBVIEW 开关" '${GetOptions} $CMDLINE "/SKIPWEBVIEW" $R0'
assert_fn_has RimeSyncEnsureWebView2 "缺 WebView2 时退出码 4" 'SetErrorLevel 4'
assert_has "离线变体内嵌 WebView2 安装器"     'File "/oname=${RIMESYNC_WEBVIEW_INSTALLER}" "${RIMESYNC_WEBVIEW_EXE}"'
assert_has "离线载荷存在性在编译期检查"       '!ifdef RIMESYNC_WEBVIEW_PAYLOAD'

# 升级：静默 + 原地 _?= + 120 秒超时 + 先停进程；失败只记日志
assert_has "旧 rime-sync：路径带引号 + /S + 原地 _?=" '"$R9" /S _?=$0'
assert_has "卸载前先 taskkill 同步程序"       "taskkill /IM rime-sync-app.exe /F"
assert_has "卸载等待有 120 秒超时"            '!define RIMESYNC_UNINSTALL_TIMEOUT_MS "120000"'
assert_has "超时通过 nsExec /TIMEOUT 生效"    'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_UNINSTALL_TIMEOUT_MS}'
assert_has "阶段日志 UNINSTALL-OLD"           "phase=UNINSTALL-OLD"
assert_fn_no_abort RimeSyncUninstallOldRimeSync
assert_fn_no_abort RimeSyncUninstallOldVersion
assert_fn_has RimeSyncUninstallOldRimeSync "注册表兜底扫 32 + 64 位视图" '!insertmacro RIMESYNC_SCAN_OUR_UNINST 32'

# 快捷方式：两个入口（主程序 + --setup）；POSTINSTALL 调用一次
assert_fn_has RimeSyncCreateOwnShortcuts "建主程序快捷方式" '${RIMESYNC_NAME}.lnk"'
assert_fn_has RimeSyncCreateOwnShortcuts "建 --setup 快捷方式" '"--setup"'
assert_macro_has NSIS_HOOK_POSTINSTALL "POSTINSTALL 建快捷方式" 'Call RimeSyncCreateOwnShortcuts'
assert_count "POSTINSTALL 只建自己的快捷方式" 'Call RimeSyncCreateOwnShortcuts' 1

# 卸载：询问用户数据（交互默认否、静默 /PURGEDATA）+ 只删两条明确路径
assert_fn_has un.RimeSyncAskPurgeUserData "交互询问（默认否）" 'MessageBox MB_YESNO|MB_ICONEXCLAMATION'
assert_fn_has un.RimeSyncAskPurgeUserData "静默需 /PURGEDATA" '${GetOptions} $CMDLINE "/PURGEDATA"'
assert_fn_has un.RimeSyncPurgeUserData "删除 %APPDATA%\Rime" 'StrCpy $0 "$1\Rime"'
assert_fn_has un.RimeSyncPurgeUserData "删除 %APPDATA%\com.rimesync.app" 'StrCpy $0 "$1\com.rimesync.app"'
assert_macro_has NSIS_HOOK_PREUNINSTALL "卸载时问用户数据" 'Call un.RimeSyncAskPurgeUserData'
assert_macro_has NSIS_HOOK_PREUNINSTALL "最后才删用户数据" 'Call un.RimeSyncPurgeUserData'
assert_macro_order NSIS_HOOK_PREUNINSTALL 'Call un.RimeSyncAskPurgeUserData' 'Call un.RimeSyncPurgeUserData' \
  "先问决定，最后再删"
assert_fn_no_abort un.RimeSyncPurgeDir
assert_fn_no_abort un.RimeSyncPurgeUserData

# 安装后自动启动：不勾选 NOTCHECKED（交互安装完成页默认启动程序，首次启动才会自举输入法）
assert_absent "不再关闭完成页的「运行」复选框" 'MUI_FINISHPAGE_RUN_NOTCHECKED'

# ---------------------------------------------------------------------------
# 1b) B 方案核心：钩子里不许再有任何输入法判断 / 安装 / 产物 / 自检
# ---------------------------------------------------------------------------
# 函数与宏：整套 Weasel 判定、安装、回滚、自检、prebuilt、prune、JSON 解析都不许存在
for gone in \
  RimeSyncWeaselReady \
  RimeSyncWeaselReadyFromDir \
  RimeSyncWeaselRootFromDir \
  RimeSyncFindWeaselRoot \
  RimeSyncFindWeaselGroup \
  RimeSyncFindWeaselUninstallString \
  RimeSyncWeaselTsfProbe \
  RimeSyncWeaselServerRunning \
  RimeSyncWeaselLogEvidence \
  RimeSyncWeaselPreflight \
  RimeSyncInstallWeaselPre \
  RimeSyncMarkBundledWeasel \
  RimeSyncRunWeaselInstaller \
  RimeSyncBackupLeftoverWeasel \
  RimeSyncBackupLeftoverDir \
  RimeSyncRollbackWeasel \
  RimeSyncAbortWeasel \
  RimeSyncScanDefaultLayout \
  RimeSyncSeedRimeConfig \
  RimeSyncWriteConfigFile \
  RimeSyncLogSeedResult \
  RimeSyncFileContains \
  RimeSyncEnsureIme \
  RimeSyncCheckIme \
  RimeSyncPrebuiltMode \
  RimeSyncSelfCheck \
  RimeSyncPruneSchemas \
  un.RimeSyncUninstallBundledWeasel; do
  assert_absent "钩子不再定义 \${gone}" "Function ${gone}"
  assert_absent "钩子不再调用 \${gone}" "Call ${gone}"
done

# 关键字匹配：任何 RimeSync* 函数名含输入法相关关键字即禁止（防止换名字绕过枚举）
assert_absent "钩子里不再有含 IME 关键字的 RimeSync* 函数定义" 'Function RimeSync.*Weasel'
assert_absent "钩子里不再有 Call RimeSync.*Install" 'Call RimeSync.*Install'
assert_absent "钩子里不再有 Call RimeSync.*Ime" 'Call RimeSync.*Ime'
assert_absent "钩子里不再有 Call RimeSync.*Seed" 'Call RimeSync.*Seed'

# 关键字兜底：换名字绕过枚举也必须被抓住（RimeSync* 函数名含输入法关键字即禁止）
if grep -qE "(Function|Call)[[:space:]]+RimeSync[A-Za-z]*(Weasel|Ime|IME|Tsf|Seed|Prune|SelfCheck|Prebuilt|Deploy|Contains)" "$HOOKS"; then
  printf "FAIL static  %s\n           不该再匹配：%s\n" "钩子里不再有含输入法关键字的 RimeSync* 函数" "(Function|Call) RimeSync*Weasel|Ime|Seed|Prune|SelfCheck|Prebuilt|Deploy|Contains" >&2
  static_fail=1
fi
static_count=$((static_count + 1))
# 判据/解析/开关：这些字符串一旦回到钩子里，就说明输入法逻辑又搬回安装器了
for banned in \
  'StrStrW' \
  'rimesync_vnum' \
  'rimesync_punct' \
  'ensure-ime' \
  'check-ime' \
  'RIMESYNC_PREBUILT' \
  'prune-schemas' \
  'RIMESYNC_WEASEL' \
  'WeaselServer.exe' \
  'WeaselServer.dll' \
  'rime.dll' \
  'WeaselDeployer' \
  'RIMESYNC_TSF_CLSID' \
  'SKIPIME' \
  'KEEPWEASEL' \
  'BundledWeasel' \
  'default.custom.yaml' \
  'weasel.custom.yaml' \
  'resources\schemas' \
  'phase=SELF-CHECK' \
  'phase=IME-CHECK' \
  'phase=IME-INSTALL' \
  'phase=PRUNE' \
  'weasel-ready=' \
  'build_schemas=' \
  'WriteRegStr' \
  'WriteRegExpandStr' \
  'InProcServer32' \
  'TextServiceFramework' \
  'CTF\TIP' \
  'SOFTWARE\Microsoft\CTF' \
; do
  assert_absent "钩子不再出现「${banned}」" "${banned}"
done

# M4：安装侧绝不允许再出现「跑随包 Weasel 安装器」的任何形状。
# 只按安装侧扫这两个名字：卸载段要删随包载荷（resources\weasel 与安装器文件本身）。
for banned in \
  'weasel-0.17.4.0-installer.exe' \
  'resources\weasel'; do
  assert_install_side_absent "安装侧不出现「${banned}」" "${banned}"
done
# /NOREBOOT 是随包安装器的参数：钩子里一旦出现就说明安装器又被拉回来了。
assert_absent "钩子不再出现 /NOREBOOT（Weasel 安装器只由程序跑）" '/NOREBOOT'
# 安装侧只允许一处 ExecWait：内嵌 WebView2 安装器。Weasel 安装器不是它的活，
# 把真实安装调用插回 PREINSTALL（或任何安装侧函数）必须在这里变红。
static_count=$((static_count + 1))
execwait_lines=$(install_side | grep -vE '^[[:space:]]*;' | grep -nF -- 'ExecWait' || true)
execwait_count=$(printf '%s\n' "$execwait_lines" | grep -c . || true)
if [ "${execwait_count:-0}" -ne 1 ] ||
  ! printf '%s\n' "$execwait_lines" | grep -qF -- '/silent /install'; then
  printf 'FAIL static  安装侧只允许一处 ExecWait（内嵌 WebView2：ExecWait ... /silent /install），实际 %s 处：\n%s\n' \
    "${execwait_count:-0}" "$execwait_lines" >&2
  static_fail=1
fi
assert_macro_not_has NSIS_HOOK_PREINSTALL "PREINSTALL 不直接执行程序" 'ExecWait'
assert_macro_not_has NSIS_HOOK_POSTINSTALL "POSTINSTALL 不直接执行程序" 'ExecWait'

# POSTINSTALL：建完快捷方式就结束 —— 不判定、不弹框、不设退出码
assert_macro_not_has NSIS_HOOK_POSTINSTALL "POSTINSTALL 不设退出码" 'SetErrorLevel'
assert_macro_not_has NSIS_HOOK_POSTINSTALL "POSTINSTALL 不弹框"     'MessageBox'
assert_macro_not_has NSIS_HOOK_POSTINSTALL "POSTINSTALL 不 Abort"   'Abort'
assert_macro_not_has NSIS_HOOK_POSTINSTALL "POSTINSTALL 不跑输入法自检" 'Call RimeSyncSelfCheck'
assert_has "成功收尾写 phase=DONE"             'phase=DONE setup finished note=ime-bootstrap-deferred-to-app'

# PREINSTALL 里只留 OS / WebView2 / 卸旧版 / resources
assert_macro_not_has NSIS_HOOK_PREINSTALL "PREINSTALL 不跑输入法安装" 'Call RimeSyncInstallWeaselPre'
assert_macro_not_has NSIS_HOOK_PREINSTALL "PREINSTALL 不跑就绪判定"   'Call RimeSyncWeaselReady'

# 失败一律只记日志：这些函数体里不能有 Abort
for fn in \
  RimeSyncRunAndWait \
  RimeSyncNormalizeUninstallPath \
  RimeSyncUninstallOldRimeSync \
  RimeSyncUninstallOldVersion; do
  assert_fn_no_abort "$fn"
done

# ---------------------------------------------------------------------------
# 1c) 程序侧必须真的接住了这份责任（否则删钩子 = 功能丢失）
# ---------------------------------------------------------------------------
CORE_IME_STATE="$REPO/crates/core/src/ime_state.rs"
CORE_IME="$REPO/crates/core/src/ime.rs"
TAURI_BOOT="$REPO/src-tauri/src/prebuilt_cmds.rs"
assert_file_has "core 里有 ensure 状态机"        "$CORE_IME_STATE" 'pub fn ensure('
assert_file_has "core 的 Mode 有 Install 入口"    "$CORE_IME_STATE" 'pub fn install() -> Self'
assert_file_has "core 有 config 模板播种"         "$CORE_IME_STATE" 'pub fn seed_templates('
assert_file_has "core 会调 ime::ensure（装 Weasel）" "$CORE_IME_STATE" 'ime::ensure('
assert_file_has "core 安装参数固定 /S /NOREBOOT"  "$CORE_IME" 'pub const INSTALLER_ARGS: [&str; 2] = ["/S", "/NOREBOOT"];'
assert_file_has "core 安装超时 300 秒"            "$CORE_IME" 'pub const INSTALL_TIMEOUT_SECS: u64 = 300;'
# M5：启动自举必须是 bootstrap 语义（装输入法 + 播模板，但部署 deferred），
# 不许再退回安装档 install()（那会每次开机 taskkill / 重载输入法）。
assert_file_has "程序启动走 ensure(Bootstrap)"    "$TAURI_BOOT" 'ime_state::Options::bootstrap()'
if grep -qF -- 'ime_state::Options::install()' "$TAURI_BOOT"; then
  printf 'FAIL static  启动自举又用回 install()（会 taskkill / 打断用户；应当用 bootstrap()）\n' >&2
  static_fail=1
fi
assert_file_has "core 的 bootstrap 语义装输入法"   "$CORE_IME_STATE" 'pub fn bootstrap() -> Self'

# 随包输入法安装器仍必须进 Tauri resources（由程序运行）
WIN_CONF="$REPO/src-tauri/tauri.windows.conf.json"
WEASEL_NAME="weasel-0.17.4.0-installer.exe"
assert_file_has "Tauri resources 带随包输入法安装器" "$WIN_CONF" \
  "\"../packaging/windows/vendor/$WEASEL_NAME\": \"resources/weasel/$WEASEL_NAME\""
assert_file_has "core 从 resources/weasel 找安装器" "$CORE_IME" 'join("resources").join("weasel")'
assert_file_has "core 里的安装器文件名与打包一致"     "$CORE_IME" \
  "pub const INSTALLER_FILE: &str = \"$WEASEL_NAME\";"

# 12 MB 的 Windows 安装器不能塞进基础配置：Linux 的 deb/AppImage 会白白变大。
if grep -qF -- "$WEASEL_NAME" "$REPO/src-tauri/tauri.conf.json"; then
  printf 'FAIL static  基础 tauri.conf.json 不该引用 %s（放进 tauri.windows.conf.json）\n' \
    "$WEASEL_NAME" >&2
  static_fail=1
fi

# 工作区里已有 vendor 安装器时，顺带验证载荷本身（CI 的 windows job 还会再验一次）。
vendor_exe="$PKG/windows/vendor/$WEASEL_NAME"
if [ -f "$vendor_exe" ]; then
  static_count=$((static_count + 1))
  size=$(wc -c <"$vendor_exe")
  magic=$(head -c 2 "$vendor_exe")
  if [ "$size" -lt 8000000 ]; then
    printf 'FAIL static  %s 只有 %s 字节（应 ≥ 8 MiB）：下载/编译出来的多半是错误页或占位文件\n' \
      "$vendor_exe" "$size" >&2
    static_fail=1
  elif [ "$magic" != "MZ" ]; then
    printf 'FAIL static  %s 不是 PE 文件（缺少 MZ 头）\n' "$vendor_exe" >&2
    static_fail=1
  else
    printf 'ok   vendor  %s（%s 字节，MZ 头）\n' "$WEASEL_NAME" "$size"
  fi
else
  printf 'note vendor  %s 不在工作区（Windows 打包前需先跑 fetch-weasel.ps1 / 自编译）\n' "$vendor_exe"
fi

# ---------------------------------------------------------------------------
# 1d) 随包方案清单与钩子解耦（清单只属于程序与模板，不再抄进 NSIS）
# ---------------------------------------------------------------------------
RETAINED_LIST="$PKG/rime/retained-schemas.txt"
static_count=$((static_count + 1))
if [ ! -f "$RETAINED_LIST" ]; then
  printf 'FAIL static  缺少保留清单 %s\n' "$RETAINED_LIST" >&2
  static_fail=1
else
  want=$(sed -e 's/#.*//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' "$RETAINED_LIST" | sed '/^$/d' | sort)
  have=$(cd "$PKG/rime/schemas" 2>/dev/null && ls *.custom.yaml 2>/dev/null | sed 's/\.custom\.yaml$//' | sort)
  if [ "$want" != "$have" ]; then
    printf 'FAIL static  方案模板与保留清单不一致\n           清单：%s\n           模板：%s\n' \
      "$(printf '%s' "$want" | tr '\n' ' ')" "$(printf '%s' "$have" | tr '\n' ' ')" >&2
    static_fail=1
  fi
  count=$(printf '%s\n' "$want" | wc -l)
  if [ "$count" -ne 3 ]; then
    printf 'FAIL static  保留清单应为 3 个方案，实际 %s\n' "$count" >&2
    static_fail=1
  fi
fi
assert_absent "钩子不把已移除方案写死" 'terra_pinyin'
assert_absent "钩子不把已移除方案写死（仓颉）" 'cangjie5'

if [ "$static_fail" -ne 0 ]; then
  echo "check-nsis-hooks: 静态结构断言失败（见上）" >&2
  exit 1
fi
printf 'ok   static  %s 条结构断言（安装器只管程序本体：OS/WebView2/卸旧版/快捷方式/卸载；输入法判定与安装全部在程序侧 ensure(Install/Bootstrap)）\n' \
  "$static_count"

# 0.6.22：便携 zip 的静态断言单独成脚本（resources/prebuilt 必带、weasel 安装器、
# 便携版提示）。这里一并跑，--strict 与非 strict 都会失败。
if ! bash "$HERE/check-portable-zip.sh"; then
  echo "check-nsis-hooks: 便携 zip 静态断言失败" >&2
  exit 1
fi

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
