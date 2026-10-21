#!/usr/bin/env bash
# build-offline-installer.sh -- assemble one offline installer per distribution
# from the built IME deb, the rime-sync deb and the vendored dependency set.
#
#   packaging/linux/build-offline-installer.sh                  # all available
#   packaging/linux/build-offline-installer.sh ubuntu-22.04
#   SYNC_DEB=packaging/out/rime-sync_0.3.0_amd64.deb ...
#
# Products:
#   packaging/linux/out/offline/<distro>/...       directory bundle
#   packaging/linux/out/offline/install.sh         shared installer
#   packaging/linux/out/rime-sync-installer_<ver>~<suite>.run
#                                                  self-extracting installer
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
OUT_ROOT=${OUT_ROOT:-$HERE/out/offline}
RUN_DIR=${RUN_DIR:-$HERE/out}

VERSION=$(grep -m 1 '"version"' "$REPO_ROOT/src-tauri/tauri.conf.json" |
  sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
case "$VERSION" in [0-9]*) : ;; *) echo "!! bad version '$VERSION'" >&2; exit 1 ;; esac

suite_for() {
  case "$1" in
    ubuntu-22.04) echo jammy ;;
    ubuntu-24.04) echo noble ;;
    debian-12)    echo bookworm ;;
    debian-13)    echo trixie ;;
    *) return 1 ;;
  esac
}

SYNC_DEB=${SYNC_DEB:-}
if [ -z "$SYNC_DEB" ]; then
  SYNC_DEB=$(ls -1t "$REPO_ROOT"/packaging/out/rime-sync_*.deb 2>/dev/null | head -n 1 || true)
fi
[ -n "$SYNC_DEB" ] && [ -f "$SYNC_DEB" ] || {
  echo "!! rime-sync deb not found (set SYNC_DEB=...)" >&2; exit 1
}

DISTROS=("$@")
if [ "${#DISTROS[@]}" -eq 0 ]; then
  DISTROS=()
  for d in ubuntu-22.04 ubuntu-24.04 debian-12 debian-13; do
    if compgen -G "$REPO_ROOT/build/deb/$d/rime-inputmethod_*.deb" >/dev/null &&
       compgen -G "$REPO_ROOT/packaging/linux/vendor/$d/*.deb" >/dev/null; then
      DISTROS+=("$d")
    fi
  done
fi
[ "${#DISTROS[@]}" -gt 0 ] || { echo "!! nothing to assemble" >&2; exit 1; }

mkdir -p "$OUT_ROOT" "$RUN_DIR"
cp -f "$HERE/install.sh" "$OUT_ROOT/install.sh"
chmod 0755 "$OUT_ROOT/install.sh"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-run.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

for d in "${DISTROS[@]}"; do
  suite=$(suite_for "$d") || { echo "!! unknown distro $d" >&2; exit 2; }
  ime=$(ls -1t "$REPO_ROOT/build/deb/$d"/rime-inputmethod_*.deb 2>/dev/null | head -n 1 || true)
  vendor=$REPO_ROOT/packaging/linux/vendor/$d
  [ -n "$ime" ] || { echo "!! $d: missing IME deb" >&2; exit 1; }
  [ -d "$vendor" ] || { echo "!! $d: missing vendor deps" >&2; exit 1; }

  dir=$OUT_ROOT/$d
  rm -rf "$dir"
  mkdir -p "$dir/vendor"
  cp -f "$ime" "$dir/"
  "$HERE/prepare-sync-deb.sh" "$SYNC_DEB" "$dir/$(basename "$SYNC_DEB")"
  cp -f "$vendor"/*.deb "$dir/vendor/"
  cp -f "$vendor/VENDOR-INFO" "$vendor/SHA256SUMS" "$dir/vendor/"

  {
    echo "distro=$d"
    echo "suite=$suite"
    echo "app_version=$VERSION"
    echo "ime_deb=$(basename "$ime")"
    echo "sync_deb=$(basename "$SYNC_DEB")"
    echo "vendor_debs=$(find "$dir/vendor" -name '*.deb' | wc -l)"
    echo "dir_bytes=$(du -sb "$dir" | cut -f1)"
    echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$dir/BUNDLE-INFO"

  # ---- self-extracting .run: stub + gzip tar of install.sh + <distro>/ ------
  stage=$WORK/stage
  rm -rf "$stage"
  mkdir -p "$stage"
  cp -f "$HERE/install.sh" "$stage/install.sh"
  cp -a "$dir" "$stage/$d"
  tar -C "$stage" -czf "$WORK/payload.tar.gz" .
  runfile=$RUN_DIR/rime-sync-installer_${VERSION}~${suite}.run
  {
    cat <<'RUN_STUB'
#!/bin/bash
set -e
MARKER=__RIME_SYNC_PAYLOAD_BELOW__
LINE=$(awk -v m="$MARKER" '$0 == m { print NR + 1; exit }' "$0")
TMPDIR_RUN=$(mktemp -d)
trap 'rm -rf "$TMPDIR_RUN"' EXIT
tail -n +"$LINE" "$0" | tar -xzf - -C "$TMPDIR_RUN"
"$TMPDIR_RUN/install.sh" "$@"
exit $?
RUN_STUB
    echo "__RIME_SYNC_PAYLOAD_BELOW__"
    cat "$WORK/payload.tar.gz"
  } > "$runfile"
  chmod 0755 "$runfile"

  printf '%-14s %-9s ime=%s vendor=%-4s dir=%s run=%s\n' \
    "$d" "$suite" \
    "$(stat -c %s "$ime")" \
    "$(find "$dir/vendor" -name '*.deb' | wc -l)" \
    "$(du -sh "$dir" | cut -f1)" \
    "$(du -h "$runfile" | cut -f1)"
done

echo
echo "==> directory bundles: $OUT_ROOT"
ls -1 "$OUT_ROOT"
echo "==> .run installers: $RUN_DIR"
ls -lh "$RUN_DIR"/rime-sync-installer_*.run 2>/dev/null | awk '{print "    "$5" "$9}'
