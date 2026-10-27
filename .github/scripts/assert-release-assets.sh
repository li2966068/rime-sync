#!/usr/bin/env bash
# assert-release-assets.sh -- assert the published asset inventory (0.6.6 layout).
#
# 0.6.6 起 Linux 与 Windows 都只发「一个包 = 输入法本体 + 同步工具」，并各自区分全量/非全量：
#   Linux   rime-sync-installer_<ver>.trixie.run       full  （内置依赖闭包）
#           rime-sync-installer_<ver>.trixie.thin.run  thin  （apt 装依赖）
#           rime-inputmethod_*.deb 不再作为独立资产（它只在两个 .run 内）
#   Windows rime-sync_<ver>_x64-setup.exe              非全量
#           rime-sync_<ver>_x64-offline-setup.exe      全量（内嵌 WebView2）
#
# 输入是 gh release view 的两列输出（资产名 + 字节数），所以本脚本可以离线自检：
#   gh release view "$TAG" --json assets --jq '.assets[] | "\(.name) \(.size)"' > assets.txt
#   assert-release-assets.sh --assets assets.txt --version 0.6.6
#
# 参数：
#   --assets FILE          资产清单（每行 "<名字> <字节数>"）
#   --version VER          应用版本（tauri.conf.json）
#   --suite SUITE          发行版 suite，默认 trixie
#   --full-min-mib N       全量 .run 体积下限，默认 100
#   --thin-min-mib N       thin .run 体积下限，默认 5
#   --thin-max-mib N       thin .run 体积上限，默认 60（超了说明依赖闭包被打进去了）
#   --require-sync-deb 0   允许草稿 Release 里没有 rime-sync deb（转正前会被删掉）
#
# 退出码：0 = 全部通过；1 = 有断言失败；2 = 用法错误。
set -uo pipefail

PROG=$(basename "$0")
ASSETS=""
VERSION=""
SUITE=trixie
FULL_MIN=100
THIN_MIN=5
THIN_MAX=60
REQUIRE_SYNC_DEB=1

FAILED=0
CHECKS=0

say() { printf '%s\n' "$*"; }
ok() { CHECKS=$((CHECKS + 1)); printf '  ok   %s\n' "$*"; }
bad() {
  CHECKS=$((CHECKS + 1))
  FAILED=$((FAILED + 1))
  printf '  FAIL %s\n' "$*"
  printf '::error title=assert-release-assets::%s\n' "$*"
}
die() { printf '%s: %s\n' "$PROG" "$*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --assets) [ $# -ge 2 ] || die "--assets 需要文件"; ASSETS=$2; shift 2 ;;
    --version) [ $# -ge 2 ] || die "--version 需要版本号"; VERSION=$2; shift 2 ;;
    --suite) [ $# -ge 2 ] || die "--suite 需要 suite"; SUITE=$2; shift 2 ;;
    --full-min-mib) [ $# -ge 2 ] || die "--full-min-mib 需要数字"; FULL_MIN=$2; shift 2 ;;
    --thin-min-mib) [ $# -ge 2 ] || die "--thin-min-mib 需要数字"; THIN_MIN=$2; shift 2 ;;
    --thin-max-mib) [ $# -ge 2 ] || die "--thin-max-mib 需要数字"; THIN_MAX=$2; shift 2 ;;
    --require-sync-deb) [ $# -ge 2 ] || die "--require-sync-deb 需要 0|1"; REQUIRE_SYNC_DEB=$2; shift 2 ;;
    -h | --help)
      sed -n '2,/^set -uo pipefail$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "未知参数：$1" ;;
  esac
done

[ -n "$ASSETS" ] || die "缺少 --assets"
[ -f "$ASSETS" ] || die "资产清单不存在：$ASSETS"
[ -n "$VERSION" ] || die "缺少 --version"
case "${FULL_MIN}${THIN_MIN}${THIN_MAX}" in *[!0-9]*) die "体积阈值只接受整数（MiB）" ;; esac

size_of() { awk -v n="$1" '$1==n {print $2; exit}' "$ASSETS"; }
mib() { echo $(( $1 / 1024 / 1024 )); }

say "资产清单：$ASSETS（version=$VERSION suite=$SUITE）"

# ---------------------------------------------------- Linux: exactly two .run
run_count=$(awk '$1 ~ /^rime-sync-installer_/ {c++} END {print c+0}' "$ASSETS")
if [ "$run_count" -eq 2 ]; then
  ok "Linux 安装器恰好 2 个 .run（全量 + thin）"
else
  bad "Linux 安装器有 $run_count 个 .run（期望 2：全量 + thin）——多出的大概率是旧发行版残留"
fi

full_name="rime-sync-installer_${VERSION}.${SUITE}.run"
thin_name="rime-sync-installer_${VERSION}.${SUITE}.thin.run"
full_size=$(size_of "$full_name")
thin_size=$(size_of "$thin_name")

if [ -z "$full_size" ]; then
  bad "缺少全量安装器（$full_name）"
else
  m=$(mib "$full_size")
  say "  $full_name: $m MiB"
  if [ "$full_size" -lt $(( FULL_MIN * 1024 * 1024 )) ]; then
    bad "$full_name 仅 $m MiB（< $FULL_MIN MiB）：依赖闭包没打进去"
  else
    ok "$full_name 体积达标（≥ $FULL_MIN MiB）"
  fi
fi

if [ -z "$thin_size" ]; then
  bad "缺少非全量安装器（$thin_name）"
else
  m=$(mib "$thin_size")
  say "  $thin_name: $m MiB"
  if [ "$thin_size" -lt $(( THIN_MIN * 1024 * 1024 )) ]; then
    bad "$thin_name 仅 $m MiB（< $THIN_MIN MiB）：连 IME + sync 都没打进去"
  elif [ "$thin_size" -gt $(( THIN_MAX * 1024 * 1024 )) ]; then
    bad "$thin_name 有 $m MiB（> $THIN_MAX MiB）：依赖闭包被打进 thin 包了"
  else
    ok "$thin_name 体积达标（$THIN_MIN–$THIN_MAX MiB）"
  fi
  if [ -n "$full_size" ] && [ "$thin_size" -gt $(( full_size / 2 )) ]; then
    bad "$thin_name 不小于全量包的一半：thin 去掉依赖后体积应显著更小"
  else
    ok "thin 包体积显著小于全量包"
  fi
fi

# --------------------------------------- no standalone IME deb on the release
ime_count=$(awk '$1 ~ /^rime-inputmethod_/ {c++} END {print c+0}' "$ASSETS")
if [ "$ime_count" -eq 0 ]; then
  ok "没有独立的 rime-inputmethod deb 资产（IME 只在 .run 内）"
else
  bad "release 里有 $ime_count 个 rime-inputmethod deb：0.6.6 起不再作为独立资产发布"
fi

# --------------------------------------------- rime-sync deb (internal only) --
if [ "$REQUIRE_SYNC_DEB" = "1" ]; then
  if [ -n "$(size_of "rime-sync_${VERSION}_amd64.deb")" ]; then
    ok "rime-sync deb 存在（内部传递用；转正前由 publish 删除）"
  else
    bad "缺少 rime-sync_${VERSION}_amd64.deb（ime/verify job 的内部输入）"
  fi
fi

# --------------------------------------------------- AppImage (still shipped)
for name in "rime-sync_${VERSION}_amd64.AppImage" "rime-sync_${VERSION}_amd64.AppImage.sig"; do
  if [ -n "$(size_of "$name")" ]; then
    ok "存在 $name"
  else
    bad "缺少 $name"
  fi
done

# ------------------------------------------- Windows: non-full + full, + sigs
for pair in "rime-sync_${VERSION}_x64-setup.exe:12" "rime-sync_${VERSION}_x64-offline-setup.exe:100"; do
  win_name=${pair%:*}
  min_mib=${pair##*:}
  s=$(size_of "$win_name")
  if [ -z "$s" ]; then
    bad "缺少 Windows 安装包（$win_name）"
  else
    m=$(mib "$s")
    say "  $win_name: $m MiB"
    if [ "$s" -lt $(( min_mib * 1024 * 1024 )) ]; then
      bad "$win_name 仅 $m MiB（< $min_mib MiB）：内嵌载荷（Weasel/WebView2）没打进去"
    else
      ok "$win_name 体积达标（≥ $min_mib MiB）"
    fi
  fi
done
for name in "rime-sync_${VERSION}_x64-setup.exe.sig" "rime-sync_${VERSION}_x64-offline-setup.exe.sig"; do
  if [ -n "$(size_of "$name")" ]; then
    ok "存在 $name"
  else
    bad "缺少 $name（updater / 离线包签名）"
  fi
done

say ""
say "断言 $CHECKS 项，失败 $FAILED 项"
[ "$FAILED" -eq 0 ]
