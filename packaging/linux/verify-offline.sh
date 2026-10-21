#!/usr/bin/env bash
# verify-offline.sh -- acceptance test for an offline bundle on one or more
# distribution images. Everything is installed with "--network none".
#
#   packaging/linux/verify-offline.sh                 # all built distributions
#   packaging/linux/verify-offline.sh ubuntu-22.04
#   APT_MIRROR=http://mirrors.aliyun.com packaging/linux/verify-offline.sh
#
# Evidence is written to build/evidence/<distro>/.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
APT_MIRROR=${APT_MIRROR:-}

image_for() {
  case "$1" in
    ubuntu-22.04) echo ubuntu:22.04 ;;
    ubuntu-24.04) echo ubuntu:24.04 ;;
    debian-12)    echo debian:12 ;;
    debian-13)    echo debian:13 ;;
    *) return 1 ;;
  esac
}
suite_for() {
  case "$1" in
    ubuntu-22.04) echo jammy ;;
    ubuntu-24.04) echo noble ;;
    debian-12)    echo bookworm ;;
    debian-13)    echo trixie ;;
  esac
}

DISTROS=("$@")
if [ "${#DISTROS[@]}" -eq 0 ]; then
  DISTROS=()
  for d in ubuntu-22.04 ubuntu-24.04 debian-12 debian-13; do
    [ -d "$REPO_ROOT/packaging/linux/out/offline/$d" ] && DISTROS+=("$d")
  done
fi
[ "${#DISTROS[@]}" -gt 0 ] || { echo "!! no offline bundles; run build-offline-installer.sh" >&2; exit 1; }

VERSION=$(grep -m1 '"version"' "$REPO_ROOT/src-tauri/tauri.conf.json" |
  sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')

SIZES=$REPO_ROOT/build/evidence/SIZES.txt
mkdir -p "$REPO_ROOT/build/evidence"

for d in "${DISTROS[@]}"; do
  image=$(image_for "$d") || { echo "!! unknown distro $d" >&2; exit 2; }
  suite=$(suite_for "$d")
  ev=$REPO_ROOT/build/evidence/$d
  mkdir -p "$ev"
  tag=rime-offline-test:$d
  runfile=$REPO_ROOT/packaging/linux/out/rime-sync-installer_${VERSION}~${suite}.run

  echo "===== $d ($image) ====="
  echo "==> building test image (target + Xvfb)"
  docker build -t "$tag" --network host \
    --build-arg BASE_IMAGE="$image" --build-arg APT_MIRROR="$APT_MIRROR" \
    -f "$HERE/test/Dockerfile" "$HERE/test" >"$ev/test-image.log" 2>&1 ||
    { tail -n 30 "$ev/test-image.log"; exit 1; }

  echo "==> offline install with --network none (self-extracting .run)"
  docker run --rm --network none \
    -v "$runfile:/installer.run:ro" \
    -v "$REPO_ROOT/build/deb/$d:/probe:ro" \
    -v "$HERE/test/verify-in-container.sh:/verify.sh:ro" \
    "$tag" bash -c 'bash /installer.run && bash /verify.sh' 2>&1 | tee "$ev/offline-install.log"
  grep -q 'deploy_tables=' "$ev/offline-install.log"
  grep -q 'PASS' "$ev/offline-install.log" || { echo "!! $d verification failed" >&2; exit 1; }

  ime=$(ls -1 "$REPO_ROOT/build/deb/$d"/rime-inputmethod_*.deb | head -1)
  sync_deb=$(ls -1 "$REPO_ROOT/packaging/linux/out/offline/$d"/rime-sync_*.deb | head -1)
  vendor=$REPO_ROOT/packaging/linux/vendor/$d
  {
    echo "[$d]"
    echo "image=$image suite=$suite"
    echo "ime_deb=$(basename "$ime") bytes=$(stat -c %s "$ime")"
    echo "sync_deb=$(basename "$sync_deb") bytes=$(stat -c %s "$sync_deb")"
    echo "vendor_debs=$(find "$vendor" -name '*.deb' | wc -l) bytes=$(du -sb "$vendor" | cut -f1)"
    echo "bundle_bytes=$(du -sb "$REPO_ROOT/packaging/linux/out/offline/$d" | cut -f1)"
    echo "run_bytes=$(stat -c %s "$runfile")"
  } | tee "$ev/sizes.txt"
  echo
done

{
  for d in ubuntu-22.04 ubuntu-24.04 debian-12 debian-13; do
    [ -f "$REPO_ROOT/build/evidence/$d/sizes.txt" ] && cat "$REPO_ROOT/build/evidence/$d/sizes.txt"
  done
} >"$SIZES"

echo "==> evidence: $REPO_ROOT/build/evidence"
echo "==> sizes   : $SIZES"
