#!/usr/bin/env bash
# verify-offline.sh -- acceptance test for the Linux installers on Debian 13 (trixie).
#
# Linux ships exactly two installers, both = IME + sync tool, in this order:
#   rime-sync-installer_<ver>.trixie.run        full  (bundled dependency closure)
#   rime-sync-installer_<ver>.trixie.thin.run   thin  (deps resolved by apt)
#
# Assertions per variant:
#   full       docker run --network none -> install succeeds, rime deploys, order IME -> sync
#   thin       docker run --network none -> FAILS FAST with the "use the full package" advice
#              docker run (network)      -> install succeeds, same assertions as full
#
#   packaging/linux/verify-offline.sh                    # both variants
#   packaging/linux/verify-offline.sh --variant full
#   packaging/linux/verify-offline.sh --variant thin --skip-online
#   APT_MIRROR=http://mirrors.aliyun.com packaging/linux/verify-offline.sh
#
# Evidence is written to build/evidence/<distro>/.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
APT_MIRROR=${APT_MIRROR:-}
VARIANT=both
SKIP_ONLINE=${SKIP_ONLINE:-0}
# docker run 的网络：在线（thin）用 bridge，部分 docker 环境没有 bridge 出网/DNS 时用 host。
RUN_NETWORK=${RUN_NETWORK:-bridge}

DISTRO_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --variant) shift; VARIANT=${1:-both}; shift ;;
    --variant=*) VARIANT=${1#--variant=}; shift ;;
    --skip-online) SKIP_ONLINE=1; shift ;;
    full | thin) VARIANT=$1; shift ;;
    debian-13 | debian-12) DISTRO_ARGS+=("$1"); shift ;;
    -h | --help)
      sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "!! unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$VARIANT" in both | full | thin) : ;; *) echo "!! --variant must be full|thin|both" >&2; exit 2 ;; esac

# buildkit 的 docker build 只接受 default/host/none，传 "bridge" 会直接失败
# （ERROR: network mode "bridge" not supported by buildkit）。默认不传 --network，
# 只有显式 BUILD_NETWORK=host 时才传（例如需要内网镜像源）。
BUILD_NETWORK=${BUILD_NETWORK:-}
case "$BUILD_NETWORK" in
  ""|default|host|none) : ;;
  *)
    echo "!! unsupported BUILD_NETWORK '$BUILD_NETWORK' (use default|host|none)" >&2
    exit 2
    ;;
esac
if [ "$BUILD_NETWORK" = "default" ]; then BUILD_NETWORK=; fi
BUILD_NETWORK_ARGS=()
if [ -n "$BUILD_NETWORK" ]; then
  BUILD_NETWORK_ARGS=(--network "$BUILD_NETWORK")
fi

image_for() {
  case "$1" in
    debian-13) echo debian:13 ;;
    *) return 1 ;;
  esac
}
suite_for() {
  case "$1" in
    debian-13) echo trixie ;;
  esac
}

DISTROS=()
if [ "${#DISTRO_ARGS[@]}" -gt 0 ]; then
  DISTROS=("${DISTRO_ARGS[@]}")
else
  for d in debian-13; do
    [ -d "$REPO_ROOT/packaging/linux/out/offline/$d" ] && DISTROS+=("$d")
  done
fi
[ "${#DISTROS[@]}" -gt 0 ] || { echo "!! no bundles; run build-offline-installer.sh" >&2; exit 1; }

VERSION=$(grep -m1 '"version"' "$REPO_ROOT/src-tauri/tauri.conf.json" |
  sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')

SIZES=$REPO_ROOT/build/evidence/SIZES.txt
mkdir -p "$REPO_ROOT/build/evidence"
FAILED=0

wants() { [ "$VARIANT" = both ] || [ "$VARIANT" = "$1" ]; }

for d in "${DISTROS[@]}"; do
  image=$(image_for "$d") || { echo "!! unknown distro $d" >&2; exit 2; }
  suite=$(suite_for "$d")
  ev=$REPO_ROOT/build/evidence/$d
  mkdir -p "$ev"
  tag=rime-offline-test:$d
  full_run=$REPO_ROOT/packaging/linux/out/rime-sync-installer_${VERSION}.${suite}.run
  thin_run=$REPO_ROOT/packaging/linux/out/rime-sync-installer_${VERSION}.${suite}.thin.run

  echo "===== $d ($image) ====="
  echo "==> building test image (target + Xvfb)"
  docker build -t "$tag" ${BUILD_NETWORK_ARGS[@]+"${BUILD_NETWORK_ARGS[@]}"} \
    --build-arg BASE_IMAGE="$image" --build-arg APT_MIRROR="$APT_MIRROR" \
    -f "$HERE/test/Dockerfile" "$HERE/test" >"$ev/test-image.log" 2>&1 ||
    { tail -n 30 "$ev/test-image.log"; exit 1; }

  run_container() { # $1=runfile $2=log $3...=docker run 额外参数
    local runfile=$1 log=$2
    shift 2
    docker run --rm "$@" \
      -v "$runfile:/installer.run:ro" \
      -v "$REPO_ROOT/build/deb/$d:/probe:ro" \
      -v "$HERE/test/verify-in-container.sh:/verify.sh:ro" \
      "$tag" bash -c 'bash /installer.run && bash /verify.sh' 2>&1 | tee "$log"
  }

  if wants full; then
    [ -f "$full_run" ] || { echo "!! full installer missing: $full_run" >&2; exit 1; }
    echo "==> [full] offline install with --network none (self-extracting .run)"
    run_container "$full_run" "$ev/full-offline-install.log" --network none
    grep -q 'deploy_tables=' "$ev/full-offline-install.log" || { echo "!! $d full: no deploy_tables" >&2; FAILED=1; }
    grep -q 'PASS' "$ev/full-offline-install.log" || { echo "!! $d full verification failed" >&2; FAILED=1; }
    grep -q '安装包类型: 全量' "$ev/full-offline-install.log" || { echo "!! $d full: variant banner missing" >&2; FAILED=1; }
  fi

  if wants thin; then
    [ -f "$thin_run" ] || { echo "!! thin installer missing: $thin_run" >&2; exit 1; }
    echo "==> [thin] offline install with --network none must FAIL with guidance"
    if run_container "$thin_run" "$ev/thin-offline-install.log" --network none; then
      echo "!! $d thin: 断网时竟然安装成功（应该失败并提示改用全量包）" >&2
      FAILED=1
    fi
    grep -q '非全量包（thin）需要系统仓库或联网' "$ev/thin-offline-install.log" ||
      { echo "!! $d thin: 断网失败时没有打印 thin 指引" >&2; FAILED=1; }
    grep -q 'rime-sync-installer_<ver>.trixie.run' "$ev/thin-offline-install.log" ||
      { echo "!! $d thin: 断网失败时没有指向全量包" >&2; FAILED=1; }

    if [ "$SKIP_ONLINE" = "1" ]; then
      echo "==> [thin] online install skipped (--skip-online)"
    else
      echo "==> [thin] online install with --network $RUN_NETWORK"
      if ! run_container "$thin_run" "$ev/thin-online-install.log" --network "$RUN_NETWORK"; then
        echo "!! $d thin: 联网安装失败（可以用 RUN_NETWORK=host 重试）" >&2
        FAILED=1
      fi
      grep -q 'PASS' "$ev/thin-online-install.log" || { echo "!! $d thin online verification failed" >&2; FAILED=1; }
      grep -q '安装包类型: 非全量' "$ev/thin-online-install.log" || { echo "!! $d thin: variant banner missing" >&2; FAILED=1; }
      grep -q '步骤 1/2：安装输入法' "$ev/thin-online-install.log" || { echo "!! $d thin: IME step missing" >&2; FAILED=1; }
    fi
  fi

  ime=$(ls -1 "$REPO_ROOT/build/deb/$d"/rime-inputmethod_*.deb | head -1)
  sync_deb=$(ls -1 "$REPO_ROOT/packaging/linux/out/offline/$d"/rime-sync_*.deb | head -1)
  vendor=$REPO_ROOT/packaging/linux/vendor/$d
  {
    echo "[$d]"
    echo "image=$image suite=$suite"
    echo "ime_deb=$(basename "$ime") bytes=$(stat -c %s "$ime")"
    echo "sync_deb=$(basename "$sync_deb") bytes=$(stat -c %s "$sync_deb")"
    echo "vendor_debs=$(find "$vendor" -name '*.deb' 2>/dev/null | wc -l) bytes=$(du -sb "$vendor" 2>/dev/null | cut -f1)"
    echo "full_run_bytes=$(stat -c %s "$full_run" 2>/dev/null || echo 0)"
    echo "thin_run_bytes=$(stat -c %s "$thin_run" 2>/dev/null || echo 0)"
    echo "bundle_bytes=$(du -sb "$REPO_ROOT/packaging/linux/out/offline/$d" | cut -f1)"
  } | tee "$ev/sizes.txt"
  echo
done

{
  for d in "${DISTROS[@]}"; do
    [ -f "$REPO_ROOT/build/evidence/$d/sizes.txt" ] && cat "$REPO_ROOT/build/evidence/$d/sizes.txt"
  done
} >"$SIZES"

echo "==> evidence: $REPO_ROOT/build/evidence"
echo "==> sizes   : $SIZES"
exit "$FAILED"
