#!/usr/bin/env bash
# Pack the staged Rime input method payload into
#   rime-inputmethod_<version>~<suite>_amd64.deb
# together with the vendored Rime schema data (/usr/share/rime-data).
#
#   scripts/ime/package-ime-deb.sh
#   STAGING=build/ime-staging/debian-13 OUT=build/deb/debian-13 \
#     SUITE=trixie DISTRO_KEY=debian-13 scripts/ime/package-ime-deb.sh
#
# The Debian version gets a "~<suite>" suffix so artifacts of different
# distributions can never be mistaken for one another (0.4.0~bookworm <
# 0.4.0~trixie). 0.6.6 起只发布 trixie，但后缀与参数保留：这个 deb 现在装在
# 两个 .run 里（不再是独立发布资产），将来加发行版不需要改脚本。
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
# not exist is harmless) so a package built for an older suite can never
# coexist with a t64 librime by accident.
# Debian 12/13 split the distro rime data into per-recipe packages; we ship the
# same files under /usr/share/rime-data, so apt (unlike plain dpkg -i) would
# fail with "trying to overwrite ... default.yaml". Keep this list in sync with
# the recipes vendored under third_party/rime-data.
CONFLICTS=${CONFLICTS:-"fcitx5-rime, librime1, librime1t64, librime-data, rime-data, rime-prelude, rime-essay, rime-luna-pinyin, rime-cangjie, rime-stroke, rime-terra-pinyin, rime-bopomofo, rime-wubi"}
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
install -d "$PKGROOT/usr/share/doc/$PKG"
# Every vendored recipe (any third_party/rime-data/* dir holding *.yaml / *.txt)
# is flattened, so adding a schema upstream only means vendoring its repo.
for recipe_dir in "$DATA_SRC"/*/; do
  recipe=$(basename "$recipe_dir")
  # *.yaml and *.txt both matter: rime-essay ships only essay.txt and would be
  # skipped by a yaml-only filter.
  find "$recipe_dir" -maxdepth 1 -type f \( -name '*.yaml' -o -name '*.txt' \) \
    -exec cp -f {} "$DATA_DST"/ \;
  cp -f "$recipe_dir/LICENSE" "$PKGROOT/usr/share/doc/$PKG/copyright.$recipe" 2>/dev/null || true
done
[ -f "$DATA_DST/essay.txt" ] || { echo "!! third_party/rime-data/rime-essay/essay.txt missing" >&2; exit 1; }
# Every schema named in default.yaml must ship here: librime's deployer treats a
# missing schema as a deployment failure (exit 1), so fail the build instead.
# Pure shell on purpose: this script has no other runtime dependency besides dpkg.
schema_ids=$(sed -n 's/^[[:space:]]*-[[:space:]]*schema:[[:space:]]*\([A-Za-z0-9_][A-Za-z0-9_]*\)[[:space:]]*$/\1/p' "$DATA_DST/default.yaml")
[ -n "$schema_ids" ] || { echo "!! default.yaml has no schema_list" >&2; exit 1; }
missing=""
for schema in $schema_ids; do
  [ -f "$DATA_DST/$schema.schema.yaml" ] || missing="$missing $schema"
done
[ -z "$missing" ] || { echo "!! schema_list names schemas that are not packaged:$missing" >&2; exit 1; }
echo "schema_list: $(printf '%s\n' "$schema_ids" | wc -l) schemas, all packaged"
# ------------------------------------------------- schema custom 模板 --------
# 16 个随包方案的 <schema>.custom.yaml：给引擎加 rimesync_vnum / rimesync_punct，
# 并把 punctuator 半角主字符改成单值 commit。安装器（postinst / install.sh）只在
# 用户 Rime 目录缺文件时写入，升级默认不动，见 docs/INPUT-ENHANCEMENTS.md。
# 构建时优先现场生成（保证与 vendored schema 一致）；没有 python3+PyYAML 时退回
# 仓库里已提交的产物（packaging/rime/schemas），数量与方案表对齐后仍然打包。
GEN=$REPO_ROOT/scripts/ime/gen-schema-custom.py
TEMPLATES_DST=$PKGROOT/usr/share/rime-sync/defaults/schemas
mkdir -p "$TEMPLATES_DST"
if python3 -c 'import yaml' >/dev/null 2>&1; then
  python3 "$GEN" --out "$TEMPLATES_DST" >/dev/null
  echo "schema custom 模板：现场生成 $(find "$TEMPLATES_DST" -maxdepth 1 -name '*.custom.yaml' | wc -l) 个"
else
  echo "!! python3/PyYAML 不可用，改用仓库里已生成的 packaging/rime/schemas/*.custom.yaml"
  cp -f "$REPO_ROOT"/packaging/rime/schemas/*.custom.yaml "$TEMPLATES_DST"/
fi
templ_count=$(find "$TEMPLATES_DST" -maxdepth 1 -name '*.custom.yaml' | wc -l)
schema_count=$(printf '%s\n' "$schema_ids" | wc -l)
[ "$templ_count" -eq "$schema_count" ] ||
  { echo "!! schema custom 模板数 $templ_count != schema_list $schema_count" >&2; exit 1; }
for schema in $schema_ids; do
  test -f "$TEMPLATES_DST/$schema.custom.yaml" ||
    { echo "!! 缺少 $schema.custom.yaml" >&2; exit 1; }
done
echo "schema custom 模板: $templ_count 个 -> /usr/share/rime-sync/defaults/schemas"
# ------------------------------------------------ 预编译 build 产物 --------
# 有些机器上 rime 部署永远不产出 build/（Windows 上实测 WeaselDeployer /deploy
# 返回 0 但什么都不写；Linux 上也会被用户目录权限 / 插件版本搞坏）。
# packaging/rime/prebuilt 是构建期用 rime_deployer 真实部署出来的产物：
#   build/   luna_pinyin / wubi86 及其依赖方案的 *.schema.yaml + *.bin
#   config/  生成这套产物时用的 default.custom.yaml + <schema>.custom.yaml
# 用户目录缺 build 时由 rime-sync 的 postinst / install.sh 铺过去。
# 生成：scripts/ime/gen-prebuilt-build.py（--check 供 CI 校验）。
PREBUILT_SRC=$REPO_ROOT/packaging/rime/prebuilt
PREBUILT_DST=$PKGROOT/usr/share/rime-sync/prebuilt
if [ -d "$PREBUILT_SRC/build" ]; then
  install -d "$PREBUILT_DST"
  cp -a "$PREBUILT_SRC/build" "$PREBUILT_SRC/config" "$PREBUILT_DST/"
  cp -f "$PREBUILT_SRC/MANIFEST.json" "$PREBUILT_DST/" 2>/dev/null || true
  prebuilt_schemas=$(sed -n 's/.*{schema:[[:space:]]*\([A-Za-z0-9_]*\)}.*/\1/p' \
    "$PREBUILT_DST/config/default.custom.yaml")
  [ -n "$prebuilt_schemas" ] ||
    { echo "!! prebuilt 的 config/default.custom.yaml 没有 schema_list" >&2; exit 1; }
  for pschema in $prebuilt_schemas; do
    pfile="$PREBUILT_DST/build/$pschema.schema.yaml"
    test -f "$pfile" || { echo "!! prebuilt 缺 $pschema.schema.yaml" >&2; exit 1; }
    for pmarker in rimesync_vnum rimesync_punct; do
      grep -q "$pmarker" "$pfile" ||
        { echo "!! prebuilt 的 $pschema.schema.yaml 缺 $pmarker（重跑 scripts/ime/gen-prebuilt-build.py）" >&2; exit 1; }
    done
    test -f "$DATA_DST/$pschema.schema.yaml" ||
      { echo "!! /usr/share/rime-data 里没有 $pschema.schema.yaml（prebuilt 与随包数据不一致）" >&2; exit 1; }
  done
  echo "prebuilt: $(find "$PREBUILT_DST" -type f | wc -l) 个文件 / $(du -sh "$PREBUILT_DST" | cut -f1) -> /usr/share/rime-sync/prebuilt"
else
  echo "!! 缺少 packaging/rime/prebuilt（先跑 scripts/ime/gen-prebuilt-build.py）" >&2
  [ "${ALLOW_NO_PREBUILT:-0}" = "1" ] ||
    { echo "!! 拒绝构建不带预编译产物的包；确实要这样构建请设 ALLOW_NO_PREBUILT=1" >&2; exit 1; }
fi
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
  echo "  rime data    https://github.com/rime/rime-prelude, rime-essay, rime-luna-pinyin,"
  echo "               rime-cangjie, rime-stroke, rime-terra-pinyin, rime-bopomofo, rime-wubi (LGPL-3.0)"
  echo "               rime-pinyin-simp (Apache-2.0)"
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

# localepurge deletes *.mo files for locales outside its whitelist, which makes
# the fcitx5-rime settings page fall back to English. The labels are bilingual
# now, but tell the user how to get the pure-Chinese catalogs back.
cat >"$PKGROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if dpkg-query -W -f='${Status}' localepurge 2>/dev/null | grep -q 'install ok installed'; then
  echo "[rime-inputmethod] 检测到 localepurge：它会清理 /usr/share/locale 下的翻译文件。" >&2
  echo "[rime-inputmethod] 配置页标签已是中英双语；如需纯中文，请把 zh_CN 加入 /etc/locale.nopurge 后重装本包。" >&2
fi
exit 0
EOF
chmod 0755 "$PKGROOT/DEBIAN/postinst"

# The staging tree comes out of the container world-writable and this file
# system hands out umask 000; dpkg-deb rejects loose DEBIAN permissions, so
# normalise the whole tree right before packing.
find "$PKGROOT" -type d -exec chmod 0755 {} +
find "$PKGROOT" -type f -exec chmod 0644 {} +
# Maintainer scripts must stay executable after the normalisation above.
if [ -f "$PKGROOT/DEBIAN/postinst" ]; then
  chmod 0755 "$PKGROOT/DEBIAN/postinst"
fi
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
deb_assert usr/share/rime-sync/defaults/schemas/luna_pinyin.custom.yaml -rw-r--r--   "schema custom 模板（luna_pinyin：rimesync_vnum / rimesync_punct）"
deb_assert usr/share/rime-sync/prebuilt/build/luna_pinyin.schema.yaml -rw-r--r-- \
  "预编译 build 产物（部署不产出的机器上直接铺到用户目录）"
# Without this .mo every option of the rime addon (including the new
# UserDataDirPath) is displayed in English even under LANG=zh_CN.UTF-8.
deb_assert usr/share/locale/zh_CN/LC_MESSAGES/fcitx5-rime.mo -rw-r--r-- \
  "zh_CN translation of the rime addon"
LC_CHECK=$(mktemp -d)
LC_ENTRIES=0
if dpkg-deb -x "$DEB" "$LC_CHECK" 2>/dev/null; then
  # .mo stores msgstr as plain UTF-8 bytes, so grep works without gettext on the
  # packaging host (whose only job here is to prove the strings are inside).
  for s in '用户数据目录（留空 = 默认）' '打开用户目录'; do
    if grep -qa -- "$s" "$LC_CHECK/usr/share/locale/zh_CN/LC_MESSAGES/fcitx5-rime.mo"; then
      LC_ENTRIES=$((LC_ENTRIES + 1))
    fi
  done
fi
rm -rf "$LC_CHECK"
if [ "$LC_ENTRIES" -lt 2 ]; then
  echo "!! zh_CN fcitx5-rime.mo in $DEB does not translate the new rime labels ($LC_ENTRIES/2)" >&2
  exit 1
fi
printf '    ok zh_CN mo translates UserDataDirPath/OpenUserDataDir (%s/2 strings)\n' "$LC_ENTRIES"
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
