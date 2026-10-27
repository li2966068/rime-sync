#!/usr/bin/env bash
# Build the Rime input method (librime + fcitx5-rime + the Rime Sync fcitx5
# config page addon + vendored schema data) from the vendored sources inside
# Docker, then pack a distribution-specific rime-inputmethod deb.
#
#   scripts/ime/build-linux.sh                       # debian:13 (trixie)
#   IMAGE=debian:13   scripts/ime/build-linux.sh
#   ALL=1             scripts/ime/build-linux.sh     # trixie (kept as a driver)
#   JOBS=8 STAGING=/tmp/stg scripts/ime/build-linux.sh
#
# Supported targets (each is a *separate* build; fcitx5 5.0 and 5.1 are not
# ABI compatible, so an artifact built for one release must never be reused on
# another). 0.6.6 起只发布 trixie：
#   debian:13    -> debian-13    / trixie
# 需要再加发行版时，在这里补一条 + packaging/linux/build-offline-installer.sh
# 的 suite_for，并把 release.yml 的 ime matrix 加回去即可（DISTRO_KEY/SUITE 仍是参数）。
#
# Everything happens in the container: the host needs only docker (which is what
# the ubuntu-22.04 GitHub runner already has, so CI can call this script as-is).
#
# Outputs, per distribution:
#   build/ime-staging/<distro>/   root/ DESTDIR tree, build.log, build-info.txt,
#                                 deps.txt, ldd-*.txt, smoke logs, rime-smoke
#   build/deb/<distro>/rime-inputmethod_<ver>~<suite>_amd64.deb
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)

IMAGE=${IMAGE:-debian:13}

# Multi-distribution driver: ALL=1 or IMAGES="debian:13".
if [ -z "${_RIME_BUILD_CHILD:-}" ]; then
  IMAGES=${IMAGES:-}
  if [ "${ALL:-0}" = "1" ]; then
    IMAGES="debian:13"
  fi
  if [ -n "$IMAGES" ]; then
    rc=0
    for img in $IMAGES; do
      echo "########## $img"
      _RIME_BUILD_CHILD=1 IMAGE="$img" "$0" || rc=1
    done
    exit "$rc"
  fi
fi

case "$IMAGE" in
  debian:13) DEF_KEY=debian-13; DEF_SUITE=trixie ;;
  *)
    echo "!! unsupported IMAGE '$IMAGE' (Linux ships trixie only: use debian:13)" >&2
    exit 2
    ;;
esac
DISTRO_KEY=${DISTRO_KEY:-$DEF_KEY}
SUITE=${SUITE:-$DEF_SUITE}

JOBS=${JOBS:-$(nproc)}
STAGING=${STAGING:-$REPO_ROOT/build/ime-staging/$DISTRO_KEY}
OUT=${OUT:-$REPO_ROOT/build/deb/$DISTRO_KEY}

# APT_MIRROR is a base URL used for both Ubuntu and Debian, e.g.
#   http://mirrors.aliyun.com   (default: keep the image's official sources)
APT_MIRROR=${APT_MIRROR:-}
case "$APT_MIRROR" in
  "") : ;;
  http://*|https://*) : ;;
  *) APT_MIRROR="http://$APT_MIRROR" ;;
esac
# apt inside the image build has no IPv6 route on some hosts (buildkit resolves
# AAAA while the host only has v4). auto = force IPv4 only when the host itself
# has no IPv6 default route; set to 0/1 to override.
APT_FORCE_IPV4=${APT_FORCE_IPV4:-auto}
if [ "$APT_FORCE_IPV4" = "auto" ]; then
  if ip -6 route show default 2>/dev/null | grep -q .; then
    APT_FORCE_IPV4=
  else
    APT_FORCE_IPV4=1
    echo "==> no IPv6 default route; forcing apt to IPv4 inside the image build"
  fi
fi

BASE_TAG=$(printf '%s' "$IMAGE" | tr -c 'a-zA-Z0-9_.-' '_')
if [ -z "${BUILDER_IMAGE:-}" ]; then
  BUILDER_IMAGE="rime-ime-builder:$BASE_TAG"
fi

mkdir -p "$STAGING" "$OUT"

# Containers on the default bridge cannot always reach the host resolver (this
# sandbox is one such case). Probe once and fall back to the host network.
# DOCKER_NETWORK only drives "docker run"; see BUILD_NETWORK below for "docker build".
if [ -z "${DOCKER_NETWORK:-}" ]; then
  PROBE_HOST=archive.ubuntu.com
  case "$IMAGE" in debian:*) PROBE_HOST=deb.debian.org ;; esac
  if docker run --rm "$IMAGE" getent hosts "$PROBE_HOST" >/dev/null 2>&1; then
    DOCKER_NETWORK=bridge
  else
    DOCKER_NETWORK=host
    echo "==> container DNS unavailable on the default bridge; using --network host"
  fi
fi

# docker build 的网络模式与 docker run 不是同一套：buildkit 只接受
# default/host/none，传 "bridge" 会直接失败
# （ERROR: network mode "bridge" not supported by buildkit）。
# 所以构建默认不传 --network（= buildkit 的 default），只有显式
# BUILD_NETWORK=host 时（例如需要内网镜像源）才传。
# buildkit's default (bridge) network hits the same broken resolver: when the
# run probe above had to fall back to host, default the image build to host too.
# An explicit BUILD_NETWORK always wins.
BUILD_NETWORK=${BUILD_NETWORK:-}
if [ -z "$BUILD_NETWORK" ] && [ "$DOCKER_NETWORK" = "host" ]; then
  BUILD_NETWORK=host
  echo "==> container DNS needs the host resolver; using --network host for docker build"
fi
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

echo "==> image         : $IMAGE"
echo "==> distro key    : $DISTRO_KEY  (suite $SUITE)"
echo "==> builder image : $BUILDER_IMAGE"
echo "==> staging       : $STAGING"
echo "==> deb output    : $OUT"
echo "==> jobs          : $JOBS"
[ -n "$APT_MIRROR" ] && echo "==> apt mirror    : $APT_MIRROR"
if [ -n "$BUILD_NETWORK" ]; then
  echo "==> build network : $BUILD_NETWORK"
else
  echo "==> build network : default (no --network passed to docker build)"
fi

echo "==> building dependency image (docker caches it on later runs)"
if ! docker build -t "$BUILDER_IMAGE" ${BUILD_NETWORK_ARGS[@]+"${BUILD_NETWORK_ARGS[@]}"} \
      --build-arg BASE_IMAGE="$IMAGE" --build-arg APT_MIRROR="$APT_MIRROR" \
      --build-arg APT_FORCE_IPV4="$APT_FORCE_IPV4" \
      -f "$HERE/Dockerfile" "$HERE" >"$STAGING/docker-build.log" 2>&1; then
  echo "!! docker build failed; tail of $STAGING/docker-build.log:" >&2
  tail -n 60 "$STAGING/docker-build.log" >&2
  exit 1
fi
tail -n 1 "$STAGING/docker-build.log"

START=$(date +%s)
echo "==> compiling in container"
if ! docker run --rm --network "$DOCKER_NETWORK" \
      -e JOBS="$JOBS" \
      -e BASE_IMAGE="$IMAGE" \
      -e SRC=/src -e OUT=/out \
      -v "$REPO_ROOT":/src:ro \
      -v "$STAGING":/out \
      -v "$HERE":/scripts:ro \
      "$BUILDER_IMAGE" bash /scripts/container-build.sh 2>&1 | tee "$STAGING/build.log"; then
  echo "!! container build failed; see $STAGING/build.log" >&2
  exit 1
fi
ELAPSED=$(($(date +%s) - START))

PLUGIN=$STAGING/root/usr/lib/x86_64-linux-gnu/fcitx5/rime.so
ADDON_SO=$(find "$STAGING/root/usr/lib" -type f -name rimesync.so -print -quit 2>/dev/null || true)
ADDON_CONF=$STAGING/root/usr/share/fcitx5/addon/rimesync.conf
ADDON_BIN=$STAGING/root/usr/bin/rime-sync-open-setup

echo
echo "==> done in $ELAPSED s (docker image build excluded)"
echo "==> staging size : $(du -sh "$STAGING/root" | cut -f1)"
if [ -f "$PLUGIN" ]; then
  echo "==> plugin       : $PLUGIN ($(stat -c %s "$PLUGIN") bytes)"
  echo "==> ldd problems : $(grep -c 'not found' "$STAGING/ldd-rime.so.txt" || true)"
else
  echo "!! plugin not found at $PLUGIN" >&2
  exit 1
fi
# The config page addon is the whole point of the packaging change: bail out
# before packing an IME deb that fcitx5-configtool cannot show a page for.
for f in "$ADDON_SO" "$ADDON_CONF" "$ADDON_BIN"; do
  if [ -z "$f" ] || [ ! -f "$f" ]; then
    echo "!! fcitx5 config page file missing from staging: ${f:-rimesync.so}" >&2
    exit 1
  fi
done
if [ ! -x "$ADDON_BIN" ]; then
  echo "!! $ADDON_BIN is not executable" >&2
  exit 1
fi
echo "==> config page  : $ADDON_SO ($(stat -c %s "$ADDON_SO") bytes)"
echo "==> config page  : $ADDON_CONF ($(grep -c . "$ADDON_CONF") metadata lines)"
echo "==> config page  : $ADDON_BIN (0755)"

echo
echo "==> packaging $DISTRO_KEY deb"
STAGING="$STAGING" OUT="$OUT" SUITE="$SUITE" DISTRO_KEY="$DISTRO_KEY" \
  "$HERE/package-ime-deb.sh"

# Keep the rime-smoke probe next to the deb for the offline install test.
cp -f "$STAGING/rime-smoke" "$OUT/rime-smoke" 2>/dev/null || true

echo
echo "==> build-info.txt"
sed 's/^/    /' "$STAGING/build-info.txt"
