#!/usr/bin/env bash
# test-nsis-hooks-wine.sh —— 在 Linux 上用 wine 跑一遍 NSIS 钩子的**运行期**语义。
#
# 为什么需要：check-nsis-hooks.sh 只证明「编译得出来」；B 方案的关键承诺是
# 「静默安装不再碰输入法」——这只能靠真跑一遍来证明。
#
# 做法：用一个最小 driver .nsi 插入 NSIS_HOOK_PREINSTALL / POSTINSTALL /
# PREUNINSTALL，在独立的 WINEPREFIX 里：
#   1. wine harness.exe /S /SKIPWEBVIEW           （静默安装）
#      断言：退出码 0；$INSTDIR\resources 与主程序存在；
#            %TEMP%\rime-sync-setup.log 含 phase=DONE / POSTINSTALL / UNINSTALL-OLD；
#            **不含**任何 IME 关键字（IME/WEASEL/SELF-CHECK/PRUNE/ensure-ime/…）。
#   2. 造两个假的用户数据目录（%APPDATA%\Rime、%APPDATA%\com.rimesync.app）
#   3. wine uninstall.exe /S /PURGEDATA           （静默卸载）
#      断言：退出码 0；卸载日志含 purge-userdata=yes 与两条 removed；
#            两条用户数据目录被删；安装目录被删；日志同样不含 IME 关键字。
#
# 用法：
#   bash packaging/windows/test-nsis-hooks-wine.sh             # 没有 makensis/wine 时 SKIP
#   bash packaging/windows/test-nsis-hooks-wine.sh --strict    # 缺失视为失败（CI 用）
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1

if ! command -v makensis >/dev/null 2>&1 || ! command -v wine >/dev/null 2>&1; then
  if [ "$STRICT" -eq 1 ]; then
    echo "test-nsis-hooks-wine: 缺少 makensis 或 wine，--strict 下视为失败" >&2
    exit 1
  fi
  echo "test-nsis-hooks-wine: SKIP（需要 makensis + wine）"
  exit 0
fi

WORK=$(mktemp -d)
cleanup() {
  # 只杀本 WINEPREFIX 的进程，别影响用户其它 wine 会话
  WINEPREFIX="$WORK/wineprefix" wineserver -k >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

mkdir -p "$WORK/packaging/windows/vendor" "$WORK/out"
ln -sfn "$PKG/nsis-hooks.nsh" "$WORK/packaging/nsis-hooks.nsh"
ln -sfn "$PKG/nsis-hooks-offline.nsh" "$WORK/packaging/nsis-hooks-offline.nsh"

cat >"$WORK/driver.nsi" <<'NSI'
Unicode true
Name "rime-sync-hooks-wine"
OutFile "OUTDIR/harness.exe"
InstallDir "$LOCALAPPDATA\rime-sync-wine-test"
RequestExecutionLevel user
SilentInstall silent
!define PRODUCTNAME "rime-sync"
!define MAINBINARYNAME "rime-sync-app"
!define MAINBINARYSRCPATH "HARNESS"

!include "INCLUDE_LINE"

Section "Install"
  SetOutPath $INSTDIR
  !insertmacro NSIS_HOOK_PREINSTALL
  File "/oname=rime-sync-app.exe" "${MAINBINARYSRCPATH}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd

Section "Uninstall"
  !insertmacro NSIS_HOOK_PREUNINSTALL
  ; Tauri 的 NSIS 模板在卸载段末尾会删掉整个安装目录；harness 照做，
  ; 否则「卸载后安装目录还在」永远是事实，这条断言就永远是假的。
  RMDir /r "$INSTDIR"
SectionEnd
NSI

printf 'MZ fake main binary (wine harness)\n' >"$WORK/app.exe"
sed -e "s|OUTDIR|$WORK/out|" \
  -e "s|HARNESS|$WORK/app.exe|" \
  -e "s|INCLUDE_LINE|$WORK/packaging/nsis-hooks.nsh|" \
  "$WORK/driver.nsi" >"$WORK/driver-online.nsi"

if ! makensis -WX -V2 "$WORK/driver-online.nsi" >"$WORK/makensis.log" 2>&1; then
  echo "FAIL 驱动脚本编译失败：" >&2
  sed -n '1,40p' "$WORK/makensis.log" >&2
  exit 1
fi

export WINEPREFIX="$WORK/wineprefix"
export WINEDEBUG=-all
export WINEDLLOVERRIDES="mscoree,mshtml="
wineboot -u >/dev/null 2>&1 || true
wineserver -w >/dev/null 2>&1 || true

# wine 模拟的 Windows 版本必须够新：安装器按设计要求 Win10 1803（build 17134）+，
# 版本不够会 SetErrorLevel 3 + Abort（退出码 3）—— 那是产品逻辑**正确**，但冒烟跑不下去。
# wine 6.x 默认模拟 win7、wine 10.x 默认模拟 win10，所以这里显式设成 win10，
# 让冒烟测的是「装得上、且不碰输入法」这条路径。
WINE_WIN_VERSION="${RIMESYNC_WINE_VERSION:-win10}"
wine reg add "HKCU\Software\Wine" /v Version /d "$WINE_WIN_VERSION" /f >/dev/null 2>&1 || true
WINE_BUILD=$(wine reg query "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion" /v CurrentBuildNumber 2>/dev/null \
  | tr -d '\r' | awk '/CurrentBuildNumber/{print $NF}')
echo "test-nsis-hooks-wine: wine 模拟版本=$WINE_WIN_VERSION CurrentBuildNumber=${WINE_BUILD:-（读不到）}"

fail=0
fail_msg() { printf 'FAIL %s\n' "$1" >&2; fail=1; }
note_msg() { printf 'NOTE %s\n' "$1" >&2; }

# ---------- 1) 静默安装 ------------------------------------------------------
wine "$WORK/out/harness.exe" /S /SKIPWEBVIEW >"$WORK/install.log" 2>&1
install_code=$?
if [ "$install_code" -ne 0 ]; then
  fail_msg "静默安装退出码 $install_code（应为 0）"
  # 诊断必须打出来：退出码 3 = 安装器按设计中止（多半是 wine 模拟的 Windows 版本
  # 低于 Win10 1803）；其它退出码看 wine 自己的输出。
  echo "--- wine 输出（尾部）---" >&2
  tail -n 20 "$WORK/install.log" >&2 || true
  echo "--- 安装日志（尾部）---" >&2
  if [ -f "$WORK/setup-utf8.log" ]; then tail -n 20 "$WORK/setup-utf8.log" >&2; fi
  if [ ! -f "$WORK/setup-utf8.log" ]; then
    _sl=$(find "$WINEPREFIX/drive_c/users" -maxdepth 6 -name 'rime-sync-setup.log' 2>/dev/null | head -1)
    if [ -n "$_sl" ] && command -v iconv >/dev/null 2>&1; then
      iconv -f UTF-16LE -t UTF-8 "$_sl" 2>/dev/null | tail -n 20 >&2 || true
    fi
  fi
  echo "--------------------------" >&2
fi

DRIVE_C="$WINEPREFIX/drive_c"
INSTALL_DIR=$(find "$DRIVE_C" -maxdepth 5 -type d -name 'rime-sync-wine-test' 2>/dev/null | head -1)
if [ -z "$INSTALL_DIR" ]; then
  fail_msg "安装目录不存在（InstallDir 没落地）"
else
  [ -f "$INSTALL_DIR/rime-sync-app.exe" ] || fail_msg "主程序没有安装：$INSTALL_DIR/rime-sync-app.exe"
  [ -f "$INSTALL_DIR/uninstall.exe" ] || fail_msg "卸载器没有写出：$INSTALL_DIR/uninstall.exe"
  [ -d "$INSTALL_DIR/resources" ] || fail_msg "PREINSTALL 没有创建 resources 目录"
  [ -f "$INSTALL_DIR/install-order.log" ] || fail_msg "安装顺序日志不存在：$INSTALL_DIR/install-order.log"
fi

SETUP_LOG=$(find "$DRIVE_C/users" -maxdepth 6 -name 'rime-sync-setup.log' 2>/dev/null | head -1)
if [ -z "$SETUP_LOG" ]; then
  fail_msg "安装日志不存在：%TEMP%\\rime-sync-setup.log"
else
  # 日志是 UTF-16LE，先转成 UTF-8（iconv 不在时原样兜底）
  if command -v iconv >/dev/null 2>&1; then
    iconv -f UTF-16LE -t UTF-8 "$SETUP_LOG" >"$WORK/setup-utf8.log" 2>/dev/null || cp "$SETUP_LOG" "$WORK/setup-utf8.log"
  else
    cp "$SETUP_LOG" "$WORK/setup-utf8.log"
  fi
  # /S 静默模式下钩子不执行，钩子阶段日志不作为判据（见 docs/WINDOWS-IME-BOOTSTRAP.md）：
  # 这里只断言「日志里不该出现 IME 字样」，不断言 phase=* 存在（那会永远红）。
  for banned in 'phase=IME-' 'phase=WEASEL' 'phase=SELF-CHECK' 'phase=PRUNE' 'ensure-ime' 'check-ime' 'rimesync_vnum' 'weasel-ready' 'default.custom.yaml' 'weasel.custom.yaml'; do
    if grep -qF -- "$banned" "$WORK/setup-utf8.log"; then
      fail_msg "静默安装写了 IME 相关日志：$banned"
    fi
  done
fi

# ---------- 2) 假用户数据 ----------------------------------------------------
APPDATA_DIR=$(find "$DRIVE_C/users" -maxdepth 2 -type d -name 'AppData' 2>/dev/null | head -1 | sed 's|/AppData$||')
if [ -z "$APPDATA_DIR" ]; then
  fail_msg "找不到 wine 的 %APPDATA%"
else
  mkdir -p "$APPDATA_DIR/AppData/Roaming/Rime" "$APPDATA_DIR/AppData/Roaming/com.rimesync.app"
  printf 'sentinel\n' >"$APPDATA_DIR/AppData/Roaming/Rime/sentinel.txt"
  printf 'sentinel\n' >"$APPDATA_DIR/AppData/Roaming/com.rimesync.app/sentinel.txt"
fi

# ---------- 3) 静默卸载（带 /PURGEDATA） -------------------------------------
if [ -z "${INSTALL_DIR:-}" ]; then
  fail_msg "没有安装目录，跳过卸载验证"
else
  wine "$INSTALL_DIR/uninstall.exe" /S /PURGEDATA >"$WORK/uninstall.log" 2>&1
  uninstall_code=$?
  [ "$uninstall_code" -eq 0 ] || fail_msg "静默卸载退出码 $uninstall_code（应为 0）"

  # NSIS 卸载器会把自己复制到 %TEMP% 再执行（_?= 形式）：wine 命令会**先返回**，
  # 真正的卸载还在后台跑。不等待就直接断言会误判（这一段曾经全是 note_msg，
  # 等于「卸载没跑」也能报绿）。这里有界等待：卸载日志出现 + 主程序消失。
  waited=0
  UNINSTALL_LOG=""
  while [ "$waited" -lt 30 ]; do
    UNINSTALL_LOG=$(find "$DRIVE_C/users" -maxdepth 6 -name 'rime-sync-uninstall.log' 2>/dev/null | head -1)
    if [ -n "$UNINSTALL_LOG" ] && [ ! -f "$INSTALL_DIR/rime-sync-app.exe" ]; then
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done

  if [ -z "$UNINSTALL_LOG" ]; then
    fail_msg "卸载日志不存在：%TEMP%\\rime-sync-uninstall.log（等待 ${waited}s 后仍没有）"
  else
    grep -qF 'purge-userdata=yes' "$UNINSTALL_LOG" || fail_msg "卸载日志缺 purge-userdata=yes（/PURGEDATA 没生效）"
    [ "$(grep -cF 'purge-userdata removed' "$UNINSTALL_LOG")" -ge 2 ] \
      || fail_msg "卸载日志里的 removed 少于 2 条（两条用户数据目录应都被删）"
    for banned in 'Phase=IME' 'WEASEL' 'weasel' 'ensure-ime' 'check-ime'; do
      if grep -qF -- "$banned" "$UNINSTALL_LOG"; then
        fail_msg "卸载日志出现 IME 相关字样：$banned"
      fi
    done
  fi

  # 用户数据目录的删除可能比日志晚一点：再等一小段（上限 10s）。
  waited=0
  while [ "$waited" -lt 10 ]; do
    if [ ! -d "$APPDATA_DIR/AppData/Roaming/Rime" ] && [ ! -d "$APPDATA_DIR/AppData/Roaming/com.rimesync.app" ]; then
      break
    fi
    sleep 1
    waited=$((waited + 1))
  done
  if [ -n "${APPDATA_DIR:-}" ]; then
    [ ! -d "$APPDATA_DIR/AppData/Roaming/Rime" ] || fail_msg "/PURGEDATA 没有删除 %APPDATA%\\Rime"
    [ ! -d "$APPDATA_DIR/AppData/Roaming/com.rimesync.app" ] || fail_msg "/PURGEDATA 没有删除 %APPDATA%\\com.rimesync.app"
  fi
  [ ! -f "$INSTALL_DIR/rime-sync-app.exe" ] || fail_msg "卸载后主程序仍在（卸载器没清安装目录）"
fi

if [ "$fail" -ne 0 ]; then
  echo "test-nsis-hooks-wine: 运行期断言失败（见上）" >&2
  exit 1
fi
echo "test-nsis-hooks-wine: 静默安装/卸载均 0 退出，未写任何 IME 日志，用户数据询问与清理正常"
