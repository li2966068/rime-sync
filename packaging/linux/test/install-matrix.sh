#!/usr/bin/env bash
# 容器化验收矩阵：Linux 离线安装器在不同 dpkg 状态下的行为。
#
#   packaging/linux/test/install-matrix.sh                  # 全部用例
#   packaging/linux/test/install-matrix.sh normal multiarch # 指定用例
#   BASE_13=debian:13 BASE_12=debian:12 packaging/linux/test/install-matrix.sh
#
# 每个用例都是全新的容器，安装全程 --network none（真离线）。
# 产物默认取本地已有的 .deb，可用环境变量覆盖：
#   IME_DEB_NEW / IME_DEB_OLD     当前/上一版 rime-inputmethod deb（debian-13）
#   IME_DEB_NEW_12                debian-12 的 IME deb（single-arch 用例）
#   SYNC_DEB_NEW / SYNC_DEB_OLD   当前/上一版 rime-sync deb
#   EXTRA_DEBS                    额外补进离线库的 .deb 目录（默认
#                                 packaging/linux/out/test-extra-debs，见下）
#
# 关于 EXTRA_DEBS：仓库里的 packaging/linux/vendor/ 是历史快照（10-22，对应
# IME 0.4.x）。IME 0.5.x 起新增 libqt5core5t64 / libpcre2-16-0 /
# libdouble-conversion3 依赖，正式 0.6.0 离线包由 fetch-vendor-debs.sh 现算闭包
# 会自带它们；本地跑矩阵时把这三个 .deb 放进 EXTRA_DEBS 目录即可（不会被提交）。
# 缺少时脚本会在预检阶段直接点名，而不是等到容器里装 IME 才报依赖不满足。
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
BASE_13=${BASE_13:-debian:13}
BASE_12=${BASE_12:-debian:12}
EXTRA_DEBS=${EXTRA_DEBS:-$REPO/packaging/linux/out/test-extra-debs}

say() { printf '==> %s\n' "$*"; }
die() { printf '!! %s\n' "$*" >&2; exit 1; }

pick_deb() { # $1=dir $2=glob $3=new|old
  local f best="" cur bv
  for f in "$1"/$2; do
    [ -f "$f" ] || continue
    bv=$(dpkg-deb -f "$f" Version 2>/dev/null) || continue
    if [ -z "$best" ]; then best=$f; continue; fi
    cur=$(dpkg-deb -f "$best" Version)
    if [ "$3" = "old" ]; then
      dpkg --compare-versions "$bv" lt "$cur" && best=$f
    else
      dpkg --compare-versions "$bv" gt "$cur" && best=$f
    fi
  done
  printf '%s' "$best"
}

command -v docker >/dev/null 2>&1 || die "需要 docker"
docker info >/dev/null 2>&1 || die "docker 不可用（当前用户没有权限？）"

IME_DEB_NEW=${IME_DEB_NEW:-$(pick_deb "$REPO/build/deb/debian-13" 'rime-inputmethod_*.deb' new)}
IME_DEB_OLD=${IME_DEB_OLD:-$(pick_deb "$REPO/build/deb/debian-13" 'rime-inputmethod_*.deb' old)}
IME_DEB_NEW_12=${IME_DEB_NEW_12:-$(pick_deb "$REPO/build/deb/debian-12" 'rime-inputmethod_*.deb' new)}
SYNC_DEB_NEW=${SYNC_DEB_NEW:-$(pick_deb "$REPO/packaging/out" 'rime-sync_*.deb' new)}
SYNC_DEB_OLD=${SYNC_DEB_OLD:-$(pick_deb "$REPO/packaging/out" 'rime-sync_*.deb' old)}

for v in IME_DEB_NEW IME_DEB_OLD IME_DEB_NEW_12 SYNC_DEB_NEW SYNC_DEB_OLD; do
  eval "p=\$$v"
  [ -n "$p" ] && [ -f "$p" ] || die "$v 没找到（可显式指定，见脚本头部）"
done

VENDOR13=$REPO/packaging/linux/vendor/debian-13
VENDOR12=$REPO/packaging/linux/vendor/debian-12
[ -d "$VENDOR13" ] || die "缺少 $VENDOR13"
[ -d "$VENDOR12" ] || die "缺少 $VENDOR12"

# EXTRA_DEBS 支持按发行版分子目录：$EXTRA_DEBS/debian-13/*.deb、$EXTRA_DEBS/debian-12/*.deb，
# 也兼容平铺（$EXTRA_DEBS/*.deb）。同名包在两个发行版版本不同，必须分开。
extra_debs_for() { # $1=distro
  local d=$1 f
  for f in "$EXTRA_DEBS/$d"/*.deb "$EXTRA_DEBS"/*.deb; do
    [ -f "$f" ] && printf '%s\n' "$f"
  done
}

WORK=$(mktemp -d "$REPO/packaging/linux/out/installer-test.XXXXXX") || die "无法创建临时目录"
# fixtures 走 /tmp：仓库所在的挂载点会忽略 chmod（权限恒为 777），而 dpkg-deb 要求
# DEBIAN 目录权限 <=0755，所以测试包必须在普通文件系统上构建。
FIXDIR=$(mktemp -d "${TMPDIR:-/tmp}/rime-installer-fixtures.XXXXXX") || die "无法创建 fixtures 目录"
cleanup() { rm -rf "$WORK" "$FIXDIR"; }
trap cleanup EXIT
say "工作目录：$WORK（fixtures: $FIXDIR）"

# 复制 vendor（硬链接，秒级）；容器只读挂载，不会污染仓库
link_vendor() { cp -al "$1" "$2" 2>/dev/null || cp -a "$1" "$2"; }

# 把 EXTRA_DEBS 里"vendor 还没有的包"补进离线库，并重建 SHA256SUMS
add_extra_debs() { # $1=vendor dir $2=distro
  local dest=$1 distro=$2 f pkg added=0
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    pkg=$(dpkg-deb -f "$f" Package 2>/dev/null) || continue
    compgen -G "$dest/${pkg}_*.deb" >/dev/null && continue
    cp -f "$f" "$dest/"
    added=$((added + 1))
  done < <(extra_debs_for "$distro")
  if [ "$added" -gt 0 ]; then
    ( cd "$dest" && rm -f SHA256SUMS && sha256sum *.deb >SHA256SUMS )
  fi
  say "  补入 $added 个本地 vendor 缺的 .deb（EXTRA_DEBS=$EXTRA_DEBS/$distro）"
}

stage_bundle() { # $1=dest $2=distro $3=ime deb $4=sync deb $5=vendor dir
  local dest=$1 d=$2
  mkdir -p "$dest/$d"
  cp -f "$REPO/packaging/linux/install.sh" "$REPO/packaging/linux/uninstall.sh" "$dest/"
  chmod 0755 "$dest/install.sh" "$dest/uninstall.sh"
  cp -f "$3" "$dest/$d/"
  bash "$REPO/packaging/linux/prepare-sync-deb.sh" "$4" "$dest/$d/$(basename "$4")" >/dev/null
  link_vendor "$5" "$dest/$d/vendor"
  add_extra_debs "$dest/$d/vendor" "$d"
  [ -r "$dest/$d/vendor/SHA256SUMS" ] || die "$dest/$d/vendor 缺少 SHA256SUMS"
  printf 'test bundle: %s %s\n' "$d" "$(basename "$3")" >"$dest/$d/BUNDLE-INFO"
}

BROTLI_VER=$(dpkg-deb -f "$VENDOR13"/libbrotli1_*_amd64.deb Version) || die "读不到离线库里 libbrotli1 的版本"

# fixtures 必须在 /tmp 构建：仓库所在的挂载点忽略 chmod（权限恒为 777），
# 而 dpkg-deb 要求 DEBIAN 目录权限 <=0755。容器只读挂载这个目录。
build_fixtures() {
  local b
  # 1) 系统侧 libbrotli1:amd64 原版（多架构用例的"健康起点"）
  cp -f "$VENDOR13"/libbrotli1_*_amd64.deb "$FIXDIR/"
  # 2) 同版本 i386 占位包：Multi-Arch: same、无文件、无依赖，只占住 i386 那一侧
  b=$FIXDIR/build-i386
  rm -rf "$b"; mkdir -p "$b/DEBIAN"
  chmod 0755 "$b" "$b/DEBIAN"
  cat >"$b/DEBIAN/control" <<EOF
Package: libbrotli1
Version: $BROTLI_VER
Architecture: i386
Multi-Arch: same
Maintainer: rime-sync test <test@example.invalid>
Section: libs
Priority: optional
Description: empty i386 libbrotli1 (test fixture)
EOF
  chmod 0644 "$b/DEBIAN/control"
  dpkg-deb --root-owner-group -b "$b" "$FIXDIR/libbrotli1_i386.deb" >/dev/null \
    || die "构造 libbrotli1:i386 fixture 失败"
  # 3) 与本安装无关、postinst 必失败的包（半配置包用例）
  b=$FIXDIR/build-broken
  rm -rf "$b"; mkdir -p "$b/DEBIAN"
  chmod 0755 "$b" "$b/DEBIAN"
  cat >"$b/DEBIAN/control" <<'EOF'
Package: zz-rime-test-broken
Version: 1.0
Architecture: all
Maintainer: rime-sync test <test@example.invalid>
Section: misc
Priority: optional
Description: unrelated half-configured package (test fixture)
EOF
  printf '#!/bin/sh\nexit 1\n' >"$b/DEBIAN/postinst"
  chmod 0644 "$b/DEBIAN/control"; chmod 0755 "$b/DEBIAN/postinst"
  dpkg-deb --root-owner-group -b "$b" "$FIXDIR/zz-rime-test-broken.deb" >/dev/null \
    || die "构造半配置包 fixture 失败"
}
build_fixtures

say "产物："
say "  IME new:  $(basename "$IME_DEB_NEW")  ($(dpkg-deb -f "$IME_DEB_NEW" Version))"
say "  IME old:  $(basename "$IME_DEB_OLD")  ($(dpkg-deb -f "$IME_DEB_OLD" Version))"
say "  SYNC new: $(basename "$SYNC_DEB_NEW") ($(dpkg-deb -f "$SYNC_DEB_NEW" Version))"
say "  SYNC old: $(basename "$SYNC_DEB_OLD") ($(dpkg-deb -f "$SYNC_DEB_OLD" Version))"

# ---------------------------------------------------------------- bundles
say "组装离线包："
stage_bundle "$WORK/bundle-new"    debian-13 "$IME_DEB_NEW"    "$SYNC_DEB_NEW" "$VENDOR13"
stage_bundle "$WORK/bundle-old"    debian-13 "$IME_DEB_OLD"    "$SYNC_DEB_OLD" "$VENDOR13"
stage_bundle "$WORK/bundle-single" debian-12 "$IME_DEB_NEW_12" "$SYNC_DEB_NEW" "$VENDOR12"

# ---------------------------------------------------------------- preflight
# IME 的运行期依赖必须由"离线库 + 基础镜像"覆盖，否则用例注定在 dpkg -i 处失败。
preflight() { # $1=image $2=ime deb $3=vendor dir
  local image=$1 ime=$2 vendor=$3 missing=() p
  docker image inspect "$image" >/dev/null 2>&1 || docker pull -q "$image" >/dev/null || die "无法获取镜像 $image"
  local base_pkgs
  base_pkgs=$(docker run --rm "$image" dpkg-query -W -f='${Package}\n')
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    compgen -G "$vendor/${p}_*.deb" >/dev/null && continue
    grep -qx -- "$p" <<<"$base_pkgs" && continue
    missing+=("$p")
  done < <(dpkg-deb -f "$ime" Depends | tr ',' '\n' | sed 's/^ *//;s/(.*//;s/ *$//' | grep -v '^$')
  if [ "${#missing[@]}" -gt 0 ]; then
    printf '!! %s 的依赖在"离线库 + %s"里没有：%s\n' "$(basename "$ime")" "$image" "${missing[*]}" >&2
    die "把对应的 .deb 放进 EXTRA_DEBS=$EXTRA_DEBS 后重跑（正式 0.6.0 离线包由 fetch-vendor-debs.sh 现算闭包，不会缺）"
  fi
  say "  预检通过：$image + 离线库覆盖 $(basename "$ime") 的全部 Depends"
}
preflight "$BASE_13" "$IME_DEB_NEW" "$WORK/bundle-new/debian-13/vendor"
preflight "$BASE_12" "$IME_DEB_NEW_12" "$WORK/bundle-single/debian-12/vendor"

# multiarch：把离线库里的 libbrotli1 升一个版本，模拟用户现场（离线包比系统新），
# 其它包保持真实内容；系统侧用同一 .deb 的原版装出"两个架构版本一致"的健康状态。
say "构造 multiarch 离线库（libbrotli1 比系统新一版）："
stage_bundle "$WORK/bundle-multiarch" debian-13 "$IME_DEB_NEW" "$SYNC_DEB_NEW" "$VENDOR13"
MV=$WORK/bundle-multiarch/debian-13/vendor
orig=$(ls "$MV"/libbrotli1_*_amd64.deb | head -1)
[ -n "$orig" ] || die "multiarch 用例：离线库里没有 libbrotli1"
base_ver=$(dpkg-deb -f "$orig" Version)
new_ver="${base_ver}+multiarch1"
# 原版留给容器当"系统已有的健康状态"，离线库里换成高一版
cp -f "$orig" "$FIXDIR/$(basename "$orig")" || die "复制 libbrotli1 fixture 失败"
rm -f "$MV"/libbrotli1_*_amd64.deb
rm -rf "$FIXDIR/brotli-root"
dpkg-deb -R "$FIXDIR/$(basename "$orig")" "$FIXDIR/brotli-root" || die "解包 libbrotli1 失败"
find "$FIXDIR/brotli-root" -type d -exec chmod 0755 {} +
find "$FIXDIR/brotli-root" -type f -exec chmod 0644 {} +
sed -i -E "s/^Version: .*/Version: $new_ver/" "$FIXDIR/brotli-root/DEBIAN/control"
dpkg-deb --root-owner-group -b "$FIXDIR/brotli-root" "$MV/libbrotli1_${new_ver}_amd64.deb" >/dev/null \
  || die "重建 libbrotli1（$new_ver）失败"
( cd "$MV" && rm -f SHA256SUMS && sha256sum *.deb >SHA256SUMS )
say "  系统侧 libbrotli1=$base_ver，离线包=$new_ver"

# ---------------------------------------------------------------- cases
run_case() { # $1=case $2=image $3=bundle $4=bundle_old(optional)
  local case=$1 image=$2 bundle=$3 old=${4:-}
  docker image inspect "$image" >/dev/null 2>&1 || docker pull -q "$image" >/dev/null || die "无法获取镜像 $image"
  local args=(-v "$bundle:/bundle:ro" -v "$HERE:/test:ro" -v "$FIXDIR:/fixtures:ro")
  [ -n "$old" ] && args+=(-v "$old:/bundle-old:ro")
  docker run --rm --network none "${args[@]}" "$image" \
    bash /test/install-scenario.sh "$case"
}

declare -a PASSED=() FAILED=()
ALL_CASES=(normal broken-unrelated multiarch prebroken-multiarch upgrade single-arch)
CASES=("$@")
[ "${#CASES[@]}" -gt 0 ] || CASES=("${ALL_CASES[@]}")

for c in "${CASES[@]}"; do
  echo
  echo "=================== case: $c ==================="
  start=$(date +%s)
  case "$c" in
    normal)           run_case normal           "$BASE_13" "$WORK/bundle-new" ;;
    broken-unrelated) run_case broken-unrelated "$BASE_13" "$WORK/bundle-new" ;;
    multiarch)        run_case multiarch        "$BASE_13" "$WORK/bundle-multiarch" ;;
    prebroken-multiarch) run_case prebroken-multiarch "$BASE_13" "$WORK/bundle-multiarch" ;;
    upgrade)          run_case upgrade          "$BASE_13" "$WORK/bundle-new" "$WORK/bundle-old" ;;
    single-arch)      run_case single-arch      "$BASE_12" "$WORK/bundle-single" ;;
    *) die "未知用例 $c（可选：${ALL_CASES[*]}）" ;;
  esac
  rc=$?
  dur=$(( $(date +%s) - start ))
  if [ "$rc" -eq 0 ]; then
    PASSED+=("$c ($dur s)")
    echo "------------------- PASS: $c (${dur}s) -------------------"
  else
    FAILED+=("$c ($dur s)")
    echo "------------------- FAIL: $c (${dur}s, rc=$rc) -------------------"
  fi
done

echo
echo "=================== summary ==================="
printf 'PASS %s\n' "${PASSED[@]:-（无）}"
if [ "${#FAILED[@]}" -gt 0 ]; then
  printf 'FAIL %s\n' "${FAILED[@]}"
  exit 1
fi
echo "全部用例通过"
