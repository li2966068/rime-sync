#!/usr/bin/env bash
# verify-artifacts.sh —— 断言发布产物里"用户能看见的功能入口"仍然存在。
#
# 背景：v0.6.0 的 Windows 自编译 Weasel 失败后静默回退到上游官方安装包，
# 「Rime 同步…」入口从 Windows 版里消失，但发布仍然显示成功（见 docs/FALLBACK-POLICY.md）。
# 本脚本把"产物里必须有这条入口"变成可执行断言，release.yml 与 ci.yml 都调用它。
#
# 用法：
#   verify-artifacts.sh --dir dist                       # 校验产物目录（严格）
#   verify-artifacts.sh --dir dist --weasel-mode build    # 允许上游回退（红字告警）
#   verify-artifacts.sh --selftest                        # 自检：合成产物证明断言能抓出缺失
#
# 参数：
#   --dir DIR                 产物根目录（含 *.deb 与 *-setup.exe，可多层子目录）
#   --weasel-mode MODE        require|build|fetch。require（默认）下 WeaselServer.exe
#                             里搜不到「Rime 同步」即失败；build/fetch 允许上游回退，
#                             但会在日志与 summary 里红字告警
#   --allow-upstream-weasel   显式允许上游 Weasel（等价于 --weasel-mode build）
#   --strict-ime-bundle       额外要求 rime-sync-status / uninstall.sh 必须由
#                             rime-inputmethod deb 自带（默认按安装后布局，
#                             允许由 rime-sync deb 提供）
#   --selftest                构造合成产物自检（不需要真构建）
#
# 退出码：0 = 全部通过；1 = 有断言失败；2 = 用法/环境错误。
set -uo pipefail

PROG=$(basename "$0")
DIR=""
SELFTEST=0
STRICT_IME=0
WEASEL_MODE="require"
WEASEL_STRICT=1

STATUS_HELPER=/usr/bin/rime-sync-status
UNINSTALLER=/usr/lib/rime-sync/uninstall.sh
LOCALE_MO=usr/share/locale/zh_CN/LC_MESSAGES/fcitx5-rime.mo
WEASEL_CAPTION="输入法自建同步"

FAILED=0
CHECKS=0
WORK_DIR=""

say() { printf '%s\n' "$*"; }
ok() {
  CHECKS=$((CHECKS + 1))
  printf '  ok   %s\n' "$*"
}
bad() {
  CHECKS=$((CHECKS + 1))
  FAILED=$((FAILED + 1))
  printf '  FAIL %s\n' "$*"
  printf '::error title=verify-artifacts::%s\n' "$*"
}
warn() {
  printf '  WARN %s\n' "$*"
  printf '::warning title=verify-artifacts::%s\n' "$*"
}
summary() {
  [ -n "${GITHUB_STEP_SUMMARY:-}" ] || return 0
  printf '%s\n' "$*" >>"$GITHUB_STEP_SUMMARY"
}
die() {
  printf '%s: %s\n' "$PROG" "$*" >&2
  exit 2
}

usage() {
  sed -n '3,30p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)
      [ $# -ge 2 ] || die "--dir 需要一个目录参数"
      DIR=$2
      shift 2
      ;;
    --weasel-mode)
      [ $# -ge 2 ] || die "--weasel-mode 需要 require|build|fetch"
      WEASEL_MODE=$2
      shift 2
      ;;
    --allow-upstream-weasel)
      WEASEL_MODE=build
      shift
      ;;
    --strict-ime-bundle)
      STRICT_IME=1
      shift
      ;;
    --selftest)
      SELFTEST=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) die "未知参数：$1（--help 查看用法）" ;;
  esac
done

case "$WEASEL_MODE" in
  require) WEASEL_STRICT=1 ;;
  build | fetch) WEASEL_STRICT=0 ;;
  *) die "--weasel-mode 只接受 require|build|fetch，收到：$WEASEL_MODE" ;;
esac

# ---------------------------------------------------------------- deb 断言

extract_deb() { # $1=deb $2=outdir
  rm -rf "$2"
  mkdir -p "$2"
  dpkg-deb -x "$1" "$2" 2>/dev/null
}

check_mode() { # $1=path $2=label $3=mode
  if [ ! -e "$1" ]; then
    bad "$2 不存在（$1）"
    return 1
  fi
  local mode
  mode=$(stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1" 2>/dev/null || echo '?')
  if [ "$mode" = "$3" ]; then
    ok "$2 存在且权限 $3"
    return 0
  fi
  bad "$2 权限是 $mode，期望 $3（缺执行位时 fcitx5 配置页的按钮会静默点不动）"
  return 1
}

check_ime_deb() { # $1=deb $2=workdir
  local deb=$1 root="$2/root"
  say "[ime] $(basename "$deb")"
  if ! extract_deb "$deb" "$root"; then
    bad "无法解开 deb：$deb"
    return
  fi

  # 1) 打过 patch 的 fcitx5-rime 必须在 rime.so 里保留「查看同步状态」按钮的路径。
  #    丢失即说明又用了未打 patch 的上游 fcitx5-rime 源码。
  local so
  so=$(find "$root" -type f -name 'rime.so' -print -quit 2>/dev/null)
  if [ -z "$so" ]; then
    bad "$(basename "$deb") 里没有 rime.so"
  elif grep -aqF "$STATUS_HELPER" "$so"; then
    ok "rime.so 内含 $STATUS_HELPER（查看同步状态入口）"
  else
    bad "rime.so 里没有 $STATUS_HELPER 字节序列：fcitx5-rime 的「查看同步状态」入口丢失（third_party/fcitx5-rime/src/rimeengine.h 未生效）"
  fi

  # 2) 中文译文：按钮在 zh_CN 下会退化成英文（或空白）。
  if [ -f "$root/$LOCALE_MO" ]; then
    ok "存在 $LOCALE_MO"
  else
    bad "缺少 $LOCALE_MO（fcitx5-rime 中文界面会退化）"
  fi

  # 3) 安装后布局里必须存在这两个可执行文件。默认允许由 rime-sync deb 提供
  #    （实际也是它提供的）；--strict-ime-bundle 时强制由本 deb 自带。
  if [ "$STRICT_IME" -eq 1 ]; then
    check_mode "$root$STATUS_HELPER" "IME deb 内 $STATUS_HELPER" 755 || true
    check_mode "$root$UNINSTALLER" "IME deb 内 $UNINSTALLER" 755 || true
  elif [ ! -e "$root$STATUS_HELPER" ]; then
    say "  info $STATUS_HELPER 不在 IME deb 里，改为校验安装后布局（rime-sync deb）"
  fi
}

check_sync_deb() { # $1=deb $2=workdir
  local deb=$1 root="$2/root"
  say "[sync] $(basename "$deb")"
  if ! extract_deb "$deb" "$root"; then
    bad "无法解开 deb：$deb"
    return
  fi
  check_mode "$root$STATUS_HELPER" "rime-sync deb 内 $STATUS_HELPER" 755 || true
  if [ -e "$root$UNINSTALLER" ]; then
    check_mode "$root$UNINSTALLER" "rime-sync deb 内 $UNINSTALLER" 755 || true
  fi
  if [ -f "$root/usr/bin/rime-sync-app" ]; then
    ok "存在 /usr/bin/rime-sync-app"
  else
    bad "rime-sync deb 里没有 /usr/bin/rime-sync-app"
  fi
}

# 安装后布局：两个 deb 装到一起才算齐。strict 模式下跳过这里（IME deb 必须自带）。
check_installed_layout() { # $1=ime_root $2=sync_root
  local ime=$1 sync=$2
  [ "$STRICT_IME" -eq 0 ] || return 0
  if [ -e "$ime$STATUS_HELPER" ]; then
    check_mode "$ime$STATUS_HELPER" "安装后 $STATUS_HELPER" 755 || true
  elif [ -e "$sync$STATUS_HELPER" ]; then
    check_mode "$sync$STATUS_HELPER" "安装后 $STATUS_HELPER" 755 || true
  else
    bad "安装后布局里没有 $STATUS_HELPER（fcitx5 配置页「查看同步状态」按钮会静默失效）"
  fi
  if [ -e "$ime$UNINSTALLER" ]; then
    check_mode "$ime$UNINSTALLER" "安装后 $UNINSTALLER" 755 || true
  elif [ -e "$sync$UNINSTALLER" ]; then
    check_mode "$sync$UNINSTALLER" "安装后 $UNINSTALLER" 755 || true
  else
    bad "安装后布局里没有 $UNINSTALLER（卸载入口丢失）"
  fi
}

# ---------------------------------------------------------------- Windows 断言

unpack_7z() { # $1=archive $2=outdir
  local archive=$1 out=$2
  rm -rf "$out"
  mkdir -p "$out"
  if command -v 7z >/dev/null 2>&1; then
    7z x -y -bso0 -bsp0 -o"$out" "$archive" >/dev/null 2>&1
  elif command -v 7za >/dev/null 2>&1; then
    7za x -y -bso0 -bsp0 -o"$out" "$archive" >/dev/null 2>&1
  else
    return 127
  fi
}

count_utf16le() { # $1=file $2=text
  python3 - "$1" "$2" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
print(data.count(sys.argv[2].encode('utf-16-le')))
PY
}

check_windows_setup() { # $1=setup.exe $2=workdir
  local setup=$1 work=$2
  say "[windows] $(basename "$setup")"

  if ! command -v 7z >/dev/null 2>&1 && ! command -v 7za >/dev/null 2>&1; then
    bad "缺少 7z/7za，无法解包 NSIS 安装包（CI 用 p7zip-full）"
    return
  fi

  if ! unpack_7z "$setup" "$work/l1"; then
    bad "7z 解不开 $(basename "$setup")（NSIS 安装包）"
    return
  fi

  local inner
  inner=$(find "$work/l1" -type f -iname 'weasel-*-installer.exe' -print -quit 2>/dev/null)
  if [ -z "$inner" ]; then
    bad "$(basename "$setup") 里没有内嵌 weasel-*-installer.exe：安装包不会带输入法"
    return
  fi
  ok "setup.exe 内嵌 $(basename "$inner")"

  if ! unpack_7z "$inner" "$work/l2"; then
    bad "7z 解不开内嵌的 $(basename "$inner")"
    return
  fi

  local server
  server=$(find "$work/l2" -type f -iname 'WeaselServer.exe' -print -quit 2>/dev/null)
  if [ -z "$server" ]; then
    bad "内嵌 weasel 安装包里没有 WeaselServer.exe"
    return
  fi
  ok "解出 WeaselServer.exe"

  local count
  count=$(count_utf16le "$server" "$WEASEL_CAPTION" 2>/dev/null || echo 0)
  if [ "${count:-0}" -gt 0 ] 2>/dev/null; then
    ok "WeaselServer.exe 内含 UTF-16LE「$WEASEL_CAPTION」×$count（Weasel 托盘入口存在）"
    return
  fi

  local msg="WeaselServer.exe 里搜不到 UTF-16LE「$WEASEL_CAPTION」：本版 Windows 安装包不含「Rime 同步」入口（上游官方 Weasel 回退）"
  if [ "$WEASEL_STRICT" -eq 1 ]; then
    bad "$msg（RIMESYNC_WEASEL_MODE=$WEASEL_MODE）"
  else
    warn "$msg（RIMESYNC_WEASEL_MODE=$WEASEL_MODE，已按参数放行；必须写进发布说明与 job summary）"
    summary "> [!CAUTION]"
    summary "> **本版未包含 Weasel 入口**（\`$WEASEL_CAPTION\` 缺失）：Windows 安装包用的是上游官方 Weasel。"
    summary "> 详见 docs/FALLBACK-POLICY.md；修复方式：让 scripts/ime/build-weasel.ps1 成功，或用 require 模式（会直接失败）。"
  fi
}

# ---------------------------------------------------------------- 主流程

cleanup() {
  [ -n "$WORK_DIR" ] && rm -rf "$WORK_DIR"
  WORK_DIR=""
}

verify_dir() {
  local dir=$1
  [ -d "$dir" ] || die "产物目录不存在：$dir"

  local work
  work=$(mktemp -d)
  WORK_DIR=$work
  trap cleanup EXIT

  local ime_debs=() sync_debs=() setups=()
  mapfile -t ime_debs < <(find "$dir" -type f -name 'rime-inputmethod_*.deb' | sort)
  mapfile -t sync_debs < <(find "$dir" -type f -name 'rime-sync_*.deb' | sort)
  mapfile -t setups < <(find "$dir" -type f -iname '*-setup.exe' | sort)

  say "产物目录：$dir"
  say "  rime-inputmethod deb: ${#ime_debs[@]} 个"
  say "  rime-sync deb:        ${#sync_debs[@]} 个"
  say "  windows setup.exe:    ${#setups[@]} 个"

  [ "${#ime_debs[@]}" -gt 0 ] || bad "没有找到 rime-inputmethod_*.deb"
  [ "${#sync_debs[@]}" -gt 0 ] || bad "没有找到 rime-sync_*.deb"
  [ "${#setups[@]}" -gt 0 ] || bad "没有找到 *-setup.exe"

  local i=0 deb
  for deb in "${ime_debs[@]}"; do
    i=$((i + 1))
    check_ime_deb "$deb" "$work/ime-$i"
  done

  local sync_root=""
  i=0
  for deb in "${sync_debs[@]}"; do
    i=$((i + 1))
    check_sync_deb "$deb" "$work/sync-$i"
    [ -n "$sync_root" ] || sync_root="$work/sync-$i/root"
  done

  if [ "${#ime_debs[@]}" -gt 0 ] && [ -n "$sync_root" ]; then
    check_installed_layout "$work/ime-1/root" "$sync_root"
  fi

  i=0
  for deb in "${setups[@]}"; do
    i=$((i + 1))
    check_windows_setup "$deb" "$work/win-$i"
  done

  say ""
  say "断言 $CHECKS 项，失败 $FAILED 项"
  summary "### verify-artifacts"
  summary ""
  if [ "$FAILED" -eq 0 ]; then
    summary "✅ $CHECKS 项断言通过（Weasel 模式：$WEASEL_MODE）"
    return 0
  fi
  summary "❌ $FAILED/$CHECKS 项断言失败（Weasel 模式：$WEASEL_MODE）——见 job 日志里的 ::error"
  return 1
}

# ---------------------------------------------------------------- 自检

build_deb() { # $1=root $2=deb $3=package $4=version
  mkdir -p "$1/DEBIAN"
  cat >"$1/DEBIAN/control" <<EOF
Package: $3
Version: $4
Architecture: amd64
Maintainer: selftest <selftest@example.com>
Description: verify-artifacts selftest fixture
EOF
  if ! dpkg-deb --root-owner-group --build "$1" "$2" >/dev/null 2>&1; then
    die "无法构造自检 deb（需要 dpkg-deb）：$2"
  fi
}

run_selftest() {
  command -v dpkg-deb >/dev/null 2>&1 || die "自检需要 dpkg-deb"
  command -v python3 >/dev/null 2>&1 || die "自检需要 python3"
  if ! command -v 7z >/dev/null 2>&1 && ! command -v 7za >/dev/null 2>&1; then
    die "自检需要 7z/7za（sudo apt-get install -y p7zip-full）"
  fi

  local base
  base=$(mktemp -d)
  local failures=0

  # ---- 构造"完整正确"的产物 ----
  local root_ime="$base/good/ime-root" root_sync="$base/good/sync-root"
  mkdir -p "$root_ime/usr/lib/x86_64-linux-gnu/fcitx5" \
    "$root_ime/usr/share/locale/zh_CN/LC_MESSAGES" \
    "$root_ime/usr/bin" "$root_ime/usr/lib/rime-sync" \
    "$root_sync/usr/bin"
  printf 'prefix\0/usr/bin/rime-sync-status\0suffix\0' >"$root_ime/usr/lib/x86_64-linux-gnu/fcitx5/rime.so"
  printf 'mock mo\0' >"$root_ime/$LOCALE_MO"
  printf '#!/bin/sh\nexit 0\n' >"$root_ime$STATUS_HELPER"
  printf '#!/bin/sh\nexit 0\n' >"$root_ime$UNINSTALLER"
  chmod 0755 "$root_ime$STATUS_HELPER" "$root_ime$UNINSTALLER"
  printf '#!/bin/sh\nexit 0\n' >"$root_sync$STATUS_HELPER"
  chmod 0755 "$root_sync$STATUS_HELPER"
  printf 'mock app' >"$root_sync/usr/bin/rime-sync-app"

  mkdir -p "$base/good/dist"
  build_deb "$root_ime" "$base/good/dist/rime-inputmethod_0.0.0~selftest_amd64.deb" rime-inputmethod 0.0.0
  build_deb "$root_sync" "$base/good/dist/rime-sync_0.0.0_amd64.deb" rime-sync 0.0.0

  # 假 setup.exe：zip → 内层 zip（weasel 安装包）→ WeaselServer.exe，与真实 NSIS 一样"套娃"。
  local weasel_dir="$base/good/weasel"
  mkdir -p "$weasel_dir"
  python3 - "$weasel_dir/WeaselServer.exe" "$WEASEL_CAPTION" <<'PY'
import sys
caption = sys.argv[2]
data = b'MZ' + b'\0' * 64 + caption.encode('utf-16-le') + b'\0' * 16
open(sys.argv[1], 'wb').write(data)
PY
  (cd "$weasel_dir" && 7z a -bso0 -bsp0 "$base/good/weasel-0.17.4.0-installer.exe" WeaselServer.exe >/dev/null 2>&1)
  mkdir -p "$base/good/resources"
  mv "$base/good/weasel-0.17.4.0-installer.exe" "$base/good/resources/"
  (cd "$base/good" && 7z a -bso0 -bsp0 "$base/good/dist/rime-sync_0.0.0_x64-setup.exe" resources/weasel-0.17.4.0-installer.exe >/dev/null 2>&1)

  SELFTEST_ARGS=(--strict-ime-bundle)
  expect_ok() { # $1=label $2=dir
    if bash "$0" --dir "$2" "${SELFTEST_ARGS[@]}" >"$2.log" 2>&1; then
      printf '  ok   自检：%s\n' "$1"
    else
      printf '  FAIL 自检：%s 应当通过却失败（日志 %s.log）\n' "$1" "$2"
      sed -n '1,40p' "$2.log"
      failures=$((failures + 1))
    fi
  }
  expect_fail() { # $1=label $2=dir $3=grep pattern
    if bash "$0" --dir "$2" "${SELFTEST_ARGS[@]}" >"$2.log" 2>&1; then
      printf '  FAIL 自检：%s 应当失败却通过\n' "$1"
      failures=$((failures + 1))
      return
    fi
    if grep -qF "$3" "$2.log"; then
      printf '  ok   自检：%s（被断言抓住）\n' "$1"
    else
      printf '  FAIL 自检：%s 失败了，但报错里没有「%s」\n' "$1" "$3"
      sed -n '1,40p' "$2.log"
      failures=$((failures + 1))
    fi
  }

  expect_ok "完整产物应通过" "$base/good"

  # ---- 逐个功能入口：少一个就必须红 ----
  mutate() { # $1=name
    rm -rf "$base/$1"
    cp -a "$base/good" "$base/$1"
  }
  rebuild() { # $1=case dir
    rm -f "$1/dist/rime-inputmethod_"*.deb "$1/dist/rime-sync_"*.deb
    build_deb "$1/ime-root" "$1/dist/rime-inputmethod_0.0.0~selftest_amd64.deb" rime-inputmethod 0.0.0
    build_deb "$1/sync-root" "$1/dist/rime-sync_0.0.0_amd64.deb" rime-sync 0.0.0
  }

  mutate no-rmieso-string
  python3 - "$base/no-rmieso-string/ime-root/usr/lib/x86_64-linux-gnu/fcitx5/rime.so" <<'PY'
import sys
path = sys.argv[1]
data = open(path, 'rb').read().replace(b'/usr/bin/rime-sync-status', b'/usr/bin/XXXXX-XXXXX-XXXXX')
open(path, 'wb').write(data)
PY
  rebuild "$base/no-rmieso-string"
  expect_fail "rime.so 丢了「查看同步状态」路径" "$base/no-rmieso-string" "rime.so 里没有 /usr/bin/rime-sync-status"

  mutate no-zh-mo
  rm -f "$base/no-zh-mo/ime-root/$LOCALE_MO"
  rebuild "$base/no-zh-mo"
  expect_fail "缺 zh_CN 译文" "$base/no-zh-mo" "缺少 $LOCALE_MO"

  mutate no-status-mode
  chmod 0644 "$base/no-status-mode/ime-root$STATUS_HELPER"
  rebuild "$base/no-status-mode"
  expect_fail "rime-sync-status 丢执行位" "$base/no-status-mode" "期望 755"

  mutate no-uninstaller
  rm -f "$base/no-uninstaller/ime-root$UNINSTALLER"
  rebuild "$base/no-uninstaller"
  expect_fail "缺 uninstall.sh" "$base/no-uninstaller" "不存在"

  mutate no-sync-status
  rm -f "$base/no-sync-status/sync-root$STATUS_HELPER"
  rebuild "$base/no-sync-status"
  expect_fail "rime-sync deb 缺 rime-sync-status" "$base/no-sync-status" "rime-sync deb 内 /usr/bin/rime-sync-status"

  mutate no-weasel-caption
  python3 - "$base/no-weasel-caption/weasel/WeaselServer.exe" <<'PY'
import sys
path = sys.argv[1]
data = open(path, 'rb').read().replace('Rime 同步'.encode('utf-16-le'), 'Rime XXXX'.encode('utf-16-le'))
open(path, 'wb').write(data)
PY
  (cd "$base/no-weasel-caption/weasel" && 7z a -bso0 -bsp0 "$base/no-weasel-caption/resources/weasel-0.17.4.0-installer.exe" WeaselServer.exe >/dev/null 2>&1)
  rm -f "$base/no-weasel-caption/dist/"*-setup.exe
  (cd "$base/no-weasel-caption" && 7z a -bso0 -bsp0 "$base/no-weasel-caption/dist/rime-sync_0.0.0_x64-setup.exe" resources/weasel-0.17.4.0-installer.exe >/dev/null 2>&1)
  expect_fail "Weasel 里没有「Rime 同步」" "$base/no-weasel-caption" "搜不到 UTF-16LE"

  printf '\n自检失败 %d 项\n' "$failures"
  rm -rf "$base"
  [ "$failures" -eq 0 ]
}

if [ "$SELFTEST" -eq 1 ]; then
  run_selftest
  exit $?
fi

[ -n "$DIR" ] || die "缺 --dir（或 --selftest）"
verify_dir "$DIR"
exit $?
