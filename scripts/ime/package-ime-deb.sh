#!/usr/bin/env bash
# Pack the staged Rime input method payload into
#   rime-inputmethod_<version>~<suite>_amd64.deb
# together with the vendored Rime schema data (/usr/share/rime-data).
#
#   scripts/ime/package-ime-deb.sh
#   STAGING=build/ime-staging/ubuntu-22.04 OUT=build/deb/ubuntu-22.04 \
#     SUITE=jammy DISTRO_KEY=ubuntu-22.04 scripts/ime/package-ime-deb.sh
#
# The Debian version gets a "~<suite>" suffix so the four distribution builds
# can never be mistaken for one another (0.4.0~jammy < 0.4.0~noble < 0.4.0).
# Version base comes from src-tauri/tauri.conf.json (single source of truth for
# the app version); the package is deliberately not versioned separately.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)

STAGING=${STAGING:-$REPO_ROOT/build/ime-staging}
ROOT=$STAGING/root
SUITE=${SUITE:-}
DISTRO_KEY=${DISTRO_KEY:-native}
OUT=${OUT:-$REPO_ROOT/build/deb/$DISTRO_KEY}
PKG=rime-inputmethod
ARCH=amd64

CONF=$REPO_ROOT/src-tauri/tauri.conf.json
VERSION=$(grep -m 1 '"version"' "$CONF" | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
case "$VERSION" in
  [0-9]*) : ;;
  *) echo "!! could not read version from $CONF (got '$VERSION')" >&2; exit 1 ;;
esac
PKG_VERSION=$VERSION
[ -n "$SUITE" ] && PKG_VERSION="${VERSION}~${SUITE}"

test -d "$ROOT" || { echo "!! staging tree missing: $ROOT (run scripts/ime/build-linux.sh first)" >&2; exit 1; }
test -f "$ROOT/usr/lib/x86_64-linux-gnu/fcitx5/rime.so" ||
  { echo "!! rime.so missing from staging" >&2; exit 1; }

# The Rime Sync config page is a second, independent fcitx5 addon. If any of
# these three files is missing, fcitx5-configtool shows no "Rime Sync" entry
# (no gear icon, no page) — the exact regression this check exists for.
ADDON_SO=$(find "$ROOT/usr/lib" -type f -name rimesync.so -print -quit 2>/dev/null || true)
ADDON_CONF=$ROOT/usr/share/fcitx5/addon/rimesync.conf
ADDON_BIN=$ROOT/usr/bin/rime-sync-open-setup
test -n "$ADDON_SO" ||
  { echo "!! rimesync.so missing from staging (fcitx5 config page not built)" >&2; exit 1; }
test -f "$ADDON_CONF" ||
  { echo "!! $ADDON_CONF missing from staging" >&2; exit 1; }
test -f "$ADDON_BIN" ||
  { echo "!! $ADDON_BIN missing from staging" >&2; exit 1; }
test -x "$ADDON_BIN" ||
  { echo "!! $ADDON_BIN is not executable in staging" >&2; exit 1; }
grep -qx 'Configurable=True' "$ADDON_CONF" ||
  { echo "!! $ADDON_CONF is not Configurable=True" >&2; exit 1; }
test -f "$REPO_ROOT/third_party/rime-data/rime-prelude/default.yaml" ||
  { echo "!! third_party/rime-data is missing the vendored schema data" >&2; exit 1; }

# Runtime deps: resolved from the staged binaries during the docker build, so
# the list matches the image the artifact was compiled in. Override with DEPS.
DEPS=${DEPS:-}
if [ -z "$DEPS" ] && [ -f "$STAGING/deps.txt" ]; then
  DEPS=$(paste -sd, "$STAGING/deps.txt")
fi
if [ -z "$DEPS" ]; then
  DEPS="libc6, libstdc++6, libgcc-s1, libfcitx5core7, libfcitx5config6, libfcitx5utils2"
fi

# The addon was compiled against Fcitx5Core 5.x; make that floor explicit.
DEPS=$(printf '%s' "$DEPS" | sed -E \
  -e 's/libfcitx5core7([, ]|$)/libfcitx5core7 (>= 5.0.6)\1/' \
  -e 's/libfcitx5config6([, ]|$)/libfcitx5config6 (>= 5.0.6)\1/' \
  -e 's/libfcitx5utils2([, ]|$)/libfcitx5utils2 (>= 5.0.6)\1/')

# We ship the same librime soname the distribution packages do, and we ship
# the schema data itself. Debian/Ubuntu time_t rebuilds renamed librime1 to
# librime1t64; list both unconditionally (Conflicts with a package that does
# not exist is harmless) so a package built for jammy can never coexist with a
# t64 librime by accident.
CONFLICTS=${CONFLICTS:-"fcitx5-rime, librime1, librime1t64, librime-data"}
if [ -f "$STAGING/conflicts-extra.txt" ]; then
  while read -r extra; do
    [ -n "$extra" ] || continue
    case ", $CONFLICTS," in
      *", $extra,"*) : ;;
      *) CONFLICTS="$CONFLICTS, $extra" ;;
    esac
  done <"$STAGING/conflicts-extra.txt"
fi

# Assemble in a real temp filesystem: the repo may live on a fuseblk/NTFS mount
# that ignores chmod, and dpkg-deb insists on sane directory modes.
PKGROOT=$(mktemp -d "${TMPDIR:-/tmp}/rime-ime-deb.XXXXXX")
trap 'rm -rf "$PKGROOT"' EXIT
DEB=$OUT/${PKG}_${PKG_VERSION}_${ARCH}.deb
mkdir -p "$OUT"
rm -f "$DEB"
cp -a "$ROOT/." "$PKGROOT/"

# Runtime package only: development files stay in the staging tree (they are
# there so future rime plugins can be built against this librime) but shipping
# headers/.pc/cmake in a plain runtime deb is packaging noise.
rm -rf "$PKGROOT/usr/include" "$PKGROOT/usr/lib/x86_64-linux-gnu/pkgconfig" \
       "$PKGROOT/usr/share/cmake"

# ---------------------------------------------------------- schema data ------
# Flat install, exactly the layout librime expects under RIME_DATA_DIR.
DATA_SRC=$REPO_ROOT/third_party/rime-data
DATA_DST=$PKGROOT/usr/share/rime-data
install -d "$DATA_DST"
cp -f "$DATA_SRC/rime-prelude/"*.yaml "$DATA_DST"/
cp -f "$DATA_SRC/rime-essay/essay.txt" "$DATA_DST"/
cp -f "$DATA_SRC/rime-luna-pinyin/"*.yaml "$DATA_DST"/
install -d "$PKGROOT/usr/share/doc/$PKG"
for d in rime-prelude rime-essay rime-luna-pinyin; do
  cp -f "$DATA_SRC/$d/LICENSE" "$PKGROOT/usr/share/doc/$PKG/copyright.$d" 2>/dev/null || true
done
cp "$REPO_ROOT/third_party/librime/LICENSE" "$PKGROOT/usr/share/doc/$PKG/copyright.librime" 2>/dev/null || true
cp "$REPO_ROOT/third_party/fcitx5-rime/LICENSES/LGPL-2.1-or-later.txt" \
   "$PKGROOT/usr/share/doc/$PKG/copyright.fcitx5-rime" 2>/dev/null || true
{
  echo "Rime input method for fcitx5, built from the sources vendored in third_party/."
  echo "Distribution target: ${DISTRO_KEY} (${SUITE:-unknown suite})"
  echo
  echo "Upstream:"
  echo "  librime      https://github.com/rime/librime (tag 1.17.0, BSD-3-Clause)"
  echo "  fcitx5-rime  https://github.com/fcitx/fcitx5-rime (tag 5.0.16, LGPL-2.1-or-later)"
  echo "  rime data    https://github.com/rime/rime-prelude, rime-essay, rime-luna-pinyin (LGPL-3.0)"
  echo
  echo "Schema data is installed to /usr/share/rime-data so the input method can"
  echo "deploy and type Chinese immediately after installation, without network."
} >"$PKGROOT/usr/share/doc/$PKG/README"

INSTALLED_KB=$(du -sk "$PKGROOT" | cut -f1)
mkdir -p "$PKGROOT/DEBIAN"
cat >"$PKGROOT/DEBIAN/control" <<EOF
Package: $PKG
Version: $PKG_VERSION
Architecture: $ARCH
Maintainer: rime-sync <dev@rimesync.invalid>
Installed-Size: $INSTALLED_KB
Depends: $DEPS
Conflicts: $CONFLICTS
Replaces: $CONFLICTS
Provides: fcitx5-rime
Section: utils
Priority: optional
Multi-Arch: same
Homepage: https://github.com/rime/librime
Description: Rime input method engine for fcitx5 (self-built)
 This package ships a self-built librime and the matching fcitx5-rime addon,
 plus the Rime schema data (prelude, essay, luna_pinyin), so the input method
 does not depend on the distribution's librime / fcitx5-rime / librime-data
 packages and works offline.
EOF

# The staging tree comes out of the container world-writable and this file
# system hands out umask 000; dpkg-deb rejects loose DEBIAN permissions, so
# normalise the whole tree right before packing.
find "$PKGROOT" -type d -exec chmod 0755 {} +
find "$PKGROOT" -type f -exec chmod 0644 {} +
if [ -d "$PKGROOT/usr/bin" ]; then
  find "$PKGROOT/usr/bin" -type f -exec chmod 0755 {} +
fi

# Everything under /usr is root-owned; --root-owner-group avoids fakeroot.
dpkg-deb --root-owner-group -Zxz --build "$PKGROOT" "$DEB" >/dev/null
cp "$PKGROOT/DEBIAN/control" "$OUT/control.txt"
cp "$OUT/control.txt" "$STAGING/control.txt" 2>/dev/null || true
# ------------------------------------------------------- deb contents ------
# dpkg-deb -c is the ground truth for what a user actually installs; assert the
# config page payload and its permissions straight out of the archive.
DEB_LIST=$(dpkg-deb -c "$DEB")
deb_assert() { # <path inside the package> <exact mode column> <description>
  local path=$1 want=$2 desc=$3 got
  got=$(printf '%s\n' "$DEB_LIST" | awk -v p="./$path" '$6 == p { print $1 }')
  if [ -z "$got" ]; then
    echo "!! $desc missing from $DEB: /$path" >&2
    exit 1
  fi
  if [ "$got" != "$want" ]; then
    echo "!! $desc has mode $got in $DEB (want $want): /$path" >&2
    exit 1
  fi
  printf '    ok %s /%s\n' "$got" "$path"
}
ADDON_SO_REL=${ADDON_SO#"$ROOT"/}
deb_assert "$ADDON_SO_REL" -rw-r--r-- "rimesync.so (fcitx5 config page addon)"
deb_assert usr/share/fcitx5/addon/rimesync.conf -rw-r--r-- "rimesync.conf (addon metadata)"
deb_assert usr/bin/rime-sync-open-setup -rwxr-xr-x "rime-sync-open-setup (action button target)"
if printf '%s\n' "$DEB_LIST" | grep -q 'rime-sync-config'; then
  echo "!! the standalone Qt widgets config page leaked into $DEB" >&2
  exit 1
fi

echo "==> $DEB ($(stat -c %s "$DEB") bytes)"
echo "==> version: $PKG_VERSION  suite: ${SUITE:-none}  distro: $DISTRO_KEY"
echo "==> deps: $DEPS"
echo "==> conflicts: $CONFLICTS"
echo "==> config page:"
printf '%s\n' "$DEB_LIST" | grep -E 'rimesync|rime-sync-open-setup' | sed 's/^/    /'
