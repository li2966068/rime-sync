#!/usr/bin/env bash
# build-offline-installer.sh -- assemble the "input method + sync tool" installer
# for a distribution, in a full (bundled dependency closure) and/or thin
# (dependencies resolved by apt at install time) variant.
#
#   packaging/linux/build-offline-installer.sh                 # trixie, both variants
#   packaging/linux/build-offline-installer.sh --thin          # thin only
#   packaging/linux/build-offline-installer.sh --full          # full only
#   packaging/linux/build-offline-installer.sh --skip-deps     # alias of --thin
#   SYNC_DEB=packaging/out/rime-sync_0.3.0_amd64.deb ...
#   IME_DEB=build/deb/debian-13/rime-inputmethod_0.4.0~trixie_amd64.deb ...
#
# Products (Linux is built for trixie only; the distro key stays a parameter):
#   packaging/linux/out/offline/<distro>/...       directory bundle (full)
#   packaging/linux/out/offline/<distro>-thin/...  directory bundle (thin)
#   packaging/linux/out/offline/install.sh         shared installer
#   packaging/linux/out/rime-sync-installer_<ver>.<suite>.run
#                                                  full: IME + sync + dependency closure
#   packaging/linux/out/rime-sync-installer_<ver>.<suite>.thin.run
#                                                  thin: IME + sync, deps via apt
#
# Both variants are the SAME installer (install.sh) and install rime-inputmethod
# first, then rime-sync. Only the dependency layer differs.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)
OUT_ROOT=${OUT_ROOT:-$HERE/out/offline}
RUN_DIR=${RUN_DIR:-$HERE/out}

VERSION=$(grep -m 1 '"version"' "$REPO_ROOT/src-tauri/tauri.conf.json" |
  sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
case "$VERSION" in [0-9]*) : ;; *) echo "!! bad version '$VERSION'" >&2; exit 1 ;; esac

usage() {
  sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
  exit 0
}

# VARIANTS holds "full" and/or "thin".
VARIANTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --full) VARIANTS+=("full"); shift ;;
    --thin | --skip-deps) VARIANTS+=("thin"); shift ;;
    --both | --all) VARIANTS=("full" "thin"); shift ;;
    -h | --help) usage ;;
    -*) echo "!! unknown option '$1' (--full|--thin|--skip-deps|--both)" >&2; exit 2 ;;
    *) break ;;
  esac
done
if [ "${#VARIANTS[@]}" -eq 0 ]; then
  VARIANTS=("full" "thin")
fi

suite_for() {
  case "$1" in
    debian-13) echo trixie ;;
    debian-12) echo bookworm ;; # legacy key: still understood if a caller passes it
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
  for d in debian-13; do
    if compgen -G "$REPO_ROOT/build/deb/$d/rime-inputmethod_*.deb" >/dev/null; then
      DISTROS+=("$d")
    fi
  done
fi
[ "${#DISTROS[@]}" -gt 0 ] || { echo "!! nothing to assemble" >&2; exit 1; }

mkdir -p "$OUT_ROOT" "$RUN_DIR"
cp -f "$HERE/install.sh" "$OUT_ROOT/install.sh"
cp -f "$HERE/uninstall.sh" "$OUT_ROOT/uninstall.sh"
chmod 0755 "$OUT_ROOT/install.sh" "$OUT_ROOT/uninstall.sh"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-run.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
FAILED=0

for d in "${DISTROS[@]}"; do
  suite=$(suite_for "$d") || { echo "!! unknown distro $d (only trixie/debian-13 is built)" >&2; exit 2; }
  # IME_DEB 是单发行版覆盖（例如本地验证要与已抓好的闭包配套时）；默认取最新一个。
  ime=${IME_DEB:-}
  [ -n "$ime" ] && [ -f "$ime" ] || ime=$(ls -1t "$REPO_ROOT/build/deb/$d"/rime-inputmethod_*.deb 2>/dev/null | head -n 1 || true)
  vendor=$REPO_ROOT/packaging/linux/vendor/$d
  [ -n "$ime" ] || { echo "!! $d: missing IME deb" >&2; exit 1; }

  for variant in "${VARIANTS[@]}"; do
    dir=$OUT_ROOT/$d
    runfile=$RUN_DIR/rime-sync-installer_${VERSION}.${suite}.run
    if [ "$variant" = "thin" ]; then
      dir=$OUT_ROOT/$d-thin
      runfile=$RUN_DIR/rime-sync-installer_${VERSION}.${suite}.thin.run
    elif [ ! -d "$vendor" ]; then
      # The full variant claims to be installable without network: no closure = no product.
      echo "!! $d: missing vendor deps for the full installer (run scripts/ime/fetch-vendor-deps.sh, or use --thin)" >&2
      FAILED=1
      continue
    fi

    rm -rf "$dir"
    mkdir -p "$dir"
    cp -f "$ime" "$dir/"
    "$HERE/prepare-sync-deb.sh" "$SYNC_DEB" "$dir/$(basename "$SYNC_DEB")"
    vendor_debs=0
    if [ "$variant" = "full" ]; then
      mkdir -p "$dir/vendor"
      cp -f "$vendor"/*.deb "$dir/vendor/"
      cp -f "$vendor/VENDOR-INFO" "$vendor/SHA256SUMS" "$dir/vendor/"
      vendor_debs=$(find "$dir/vendor" -name '*.deb' | wc -l)
    fi

    {
      echo "distro=$d"
      echo "suite=$suite"
      echo "variant=$variant"
      echo "app_version=$VERSION"
      echo "ime_deb=$(basename "$ime")"
      echo "sync_deb=$(basename "$SYNC_DEB")"
      echo "vendor_debs=$vendor_debs"
      echo "dir_bytes=$(du -sb "$dir" | cut -f1)"
      echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$dir/BUNDLE-INFO"

    # ---- self-extracting .run: stub + gzip tar of install.sh + <distro>/ ------
    stage=$WORK/stage-$variant
    rm -rf "$stage"
    mkdir -p "$stage"
    cp -f "$HERE/install.sh" "$HERE/uninstall.sh" "$stage/"
    cp -a "$dir" "$stage/$d"
    tar -C "$stage" -czf "$WORK/payload-$variant.tar.gz" .
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
      cat "$WORK/payload-$variant.tar.gz"
    } > "$runfile"
    chmod 0755 "$runfile"

    printf '%-12s %-7s %-5s ime=%s vendor=%-4s dir=%-7s run=%s\n' \
      "$d" "$suite" "$variant" \
      "$(stat -c %s "$ime")" \
      "$vendor_debs" \
      "$(du -sh "$dir" | cut -f1)" \
      "$(du -h "$runfile" | cut -f1)"
  done
done

echo
echo "==> directory bundles: $OUT_ROOT"
ls -1 "$OUT_ROOT" | grep -v '^install\.sh$' | grep -v '^uninstall\.sh$' || true
echo "==> .run installers: $RUN_DIR"
ls -lh "$RUN_DIR"/rime-sync-installer_*.run 2>/dev/null | awk '{print "    "$5" "$9}'
exit "$FAILED"
