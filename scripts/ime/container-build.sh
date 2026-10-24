#!/usr/bin/env bash
# Executed *inside* the build container by scripts/ime/build-linux.sh.
# Nothing here runs on the host.
set -euo pipefail

SRC=${SRC:-/src}
OUT=${OUT:-/out}
JOBS=${JOBS:-$(nproc)}
BASE_IMAGE=${BASE_IMAGE:-unknown}
PREFIX=/usr
LIBDIR=lib/$(dpkg-architecture -qDEB_HOST_MULTIARCH)
ROOT=$OUT/root
WORK=/work
RIME_DATA_DIR=/usr/share/rime-data

log() { printf '\n########## %s\n' "$*"; }
stamp() { date +%s; }
T_ALL=$(stamp)

log "0/6 prepare ($(nproc) cpus, multiarch $LIBDIR)"
cd /    # the image's WORKDIR is $WORK, which is about to be removed
rm -rf "$ROOT" "$WORK"
mkdir -p "$ROOT" "$WORK"
cmake --version | head -n 1
g++ --version | head -n 1

# librime 1.17.0 calls google::IsGoogleLoggingInitialized(), which only exists
# in glog >= 0.6. Ubuntu 22.04 ships glog 0.5.0, so there either logging is
# turned off (upstream-supported option, RIME_ENABLE_LOGGING is compiled out)
# or the build fails. Detect the symbol instead of hardcoding a distribution.
GLOG_HEADER=$(ls /usr/include/glog/logging.h 2>/dev/null | head -n 1)
GLOG_LIB=$(ls /usr/lib/*/libglog.so* 2>/dev/null | head -n 1)
# glog 0.5.0 exports the symbol but does not declare it, so test the header.
if [ -n "$GLOG_HEADER" ] && grep -q IsGoogleLoggingInitialized "$GLOG_HEADER"; then
  ENABLE_LOGGING=ON
else
  ENABLE_LOGGING=OFF
fi
echo "glog: ${GLOG_LIB:-none} -> ENABLE_LOGGING=$ENABLE_LOGGING"

# ---------------------------------------------------------------- librime ----
log "1/6 configure + build librime 1.17.0"
T0=$(stamp)
# -DLINUX=ON: librime only links Boost.regex when the never-defined upstream
# variable LINUX is true; without it librime.so is built with undefined
# boost::re_detail symbols and fails to link any executable that uses it.
cmake -S "$SRC/third_party/librime" -B "$WORK/librime" \
  -DLINUX=ON \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=$PREFIX \
  -DCMAKE_INSTALL_LIBDIR=$LIBDIR \
  -DBUILD_SHARED_LIBS=ON \
  -DBUILD_MERGED_PLUGINS=ON \
  -DBUILD_TEST=OFF \
  -DBUILD_DATA=ON \
  -DBUILD_SAMPLE=OFF \
  -DENABLE_LOGGING=$ENABLE_LOGGING
cmake --build "$WORK/librime" -j"$JOBS"
DESTDIR="$ROOT" cmake --install "$WORK/librime"
# Drop the staged tree into the container's /usr so pkg-config, cmake and ldd
# see real paths while fcitx5-rime is built against it.
cp -a "$ROOT$PREFIX/." "$PREFIX/"
ldconfig
T1=$(stamp)
echo "librime build+install: $((T1 - T0)) s"

# ----------------------------------------------------------- fcitx5-rime ----
log "2/6 configure + build fcitx5-rime 5.0.16"
echo "system rime.pc -> $(pkg-config --modversion rime) ($(pkg-config --variable=includedir rime))"
T2=$(stamp)
cmake -S "$SRC/third_party/fcitx5-rime" -B "$WORK/fcitx5-rime" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=$PREFIX \
  -DCMAKE_INSTALL_LIBDIR=$LIBDIR \
  -DRIME_DATA_DIR=$RIME_DATA_DIR
cmake --build "$WORK/fcitx5-rime" -j"$JOBS"
DESTDIR="$ROOT" cmake --install "$WORK/fcitx5-rime"
# The addon is unusable in a Chinese session without its catalogs: every option
# (including the UserDataDirPath added in FORK.md) would fall back to English.
# Assert the staged .mo exists AND carries the new labels, so a broken po ->
# msgfmt path fails the build instead of silently shipping an English UI.
RIME_MO="$ROOT$PREFIX/share/locale/zh_CN/LC_MESSAGES/fcitx5-rime.mo"
test -f "$RIME_MO" || { echo "!! missing $RIME_MO"; exit 1; }
for label in '用户数据目录（留空 = 默认）' '打开用户目录'; do
  grep -qa -- "$label" "$RIME_MO" || {
    echo "!! $RIME_MO does not contain '$label'"; exit 1; }
done
echo "locale: $(find "$ROOT$PREFIX/share/locale" -name 'fcitx5-rime.mo' | wc -l) catalogs, zh_CN has the UserDataDirPath labels"
T3=$(stamp)
echo "fcitx5-rime build+install: $((T3 - T2)) s"

# ---------------------------------------- fcitx5 config page (rimesync addon) ----
# The config page is a separate fcitx5 SharedLibrary addon. It used to be built
# only by packaging/fcitx5-addon/docker (a standalone addon deb), so the IME deb
# shipped rime.so alone and users had no "Rime Sync" page in fcitx5-configtool.
# -DCMAKE_INSTALL_LIBDIR=$LIBDIR is required: fcitx5 looks for addons in
# <libdir>/fcitx5, and GNUInstallDirs would otherwise install into usr/lib/fcitx5
# on Debian/Ubuntu (multiarch triplet lives only in dpkg's build flags).
log "3/6 configure + build fcitx5 config page addon (rimesync)"
T_ADDON=$(stamp)
cmake -S "$SRC/packaging/fcitx5-addon" -B "$WORK/rimesync-addon" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=$PREFIX \
  -DCMAKE_INSTALL_LIBDIR=$LIBDIR \
  -DRIMESYNC_BUILD_FCITX5_ADDON=ON \
  -DRIMESYNC_BUILD_STANDALONE_CONFIG=OFF
cmake --build "$WORK/rimesync-addon" -j"$JOBS"
DESTDIR="$ROOT" cmake --install "$WORK/rimesync-addon"
T_ADDON_END=$(stamp)

# Which Qt major the addon actually linked (the image ships exactly one of
# qtbase5-dev / qt6-base-dev; print the resolved one so the build log is
# unambiguous). QT_CACHE holds e.g. "Qt5Core_DIR".
QT_CACHE=$(grep -m 1 -oE '^Qt[56]Core_DIR' "$WORK/rimesync-addon/CMakeCache.txt" || true)
if [ -z "$QT_CACHE" ]; then
  echo "!! no Qt[56]Core_DIR in the addon CMake cache"; exit 1
fi
QT_MAJOR=${QT_CACHE#Qt}; QT_MAJOR=${QT_MAJOR%Core_DIR}
echo "addon Qt major: $QT_MAJOR ($(pkg-config --modversion "Qt${QT_MAJOR}Core"))"

RS_SO=$ROOT$PREFIX/$LIBDIR/fcitx5/rimesync.so
RS_CONF=$ROOT$PREFIX/share/fcitx5/addon/rimesync.conf
RS_BIN=$ROOT$PREFIX/bin/rime-sync-open-setup
for f in "$RS_SO" "$RS_CONF" "$RS_BIN"; do
  test -f "$f" || { echo "!! missing $f"; exit 1; }
done
test -x "$RS_BIN" || { echo "!! $RS_BIN is not executable"; exit 1; }
grep -qx 'Configurable=True' "$RS_CONF" || { echo "!! rimesync.conf is not Configurable=True"; exit 1; }
grep -qx 'Library=rimesync' "$RS_CONF" || { echo "!! rimesync.conf does not point at Library=rimesync"; exit 1; }
# -DRIMESYNC_BUILD_STANDALONE_CONFIG=OFF keeps the Qt Widgets fallback program
# out of the runtime package; assert it did not leak in.
if find "$ROOT" -name 'rime-sync-config' -print -quit | grep -q .; then
  echo "!! standalone rime-sync-config leaked into the runtime tree"; exit 1
fi
echo "addon: $RS_SO ($(stat -c %s "$RS_SO") bytes), conf ok, $RS_BIN 0755"

# ------------------------------------------------------------- strip/trim ----
if [ "${STRIP:-1}" = "1" ]; then
  find "$ROOT$PREFIX/$LIBDIR" -name '*.so*' -type f -exec strip --strip-unneeded {} +
fi

# --------------------------------------------------------------- verify -----
log "4/6 strip/trim + link check"
export LD_LIBRARY_PATH="$ROOT$PREFIX/$LIBDIR"
RIME_SO=$ROOT$PREFIX/$LIBDIR/fcitx5/rime.so
LIBRIME_SO=$(find "$ROOT$PREFIX/$LIBDIR" -maxdepth 1 -name 'librime.so.1.*' -type f | head -n 1)
test -f "$RIME_SO" || { echo "missing $RIME_SO"; exit 1; }
test -f "$LIBRIME_SO" || { echo "missing librime.so.1.*"; exit 1; }
ldd "$RIME_SO" | tee "$OUT/ldd-rime.so.txt"
if grep -q 'not found' "$OUT/ldd-rime.so.txt"; then
  echo "!! unresolved shared libraries in rime.so"; exit 1
fi
ldd "$LIBRIME_SO" | tee "$OUT/ldd-librime.so.txt"
if grep -q 'not found' "$OUT/ldd-librime.so.txt"; then
  echo "!! unresolved shared libraries in librime.so"; exit 1
fi
# The config page addon is a Qt client; a missing Qt runtime here would also
# silently remove it from the deb's Depends, so fail loudly instead.
ldd "$RS_SO" | tee "$OUT/ldd-rimesync.so.txt"
if grep -q 'not found' "$OUT/ldd-rimesync.so.txt"; then
  echo "!! unresolved shared libraries in rimesync.so"; exit 1
fi
if ! grep -q "libQt${QT_MAJOR}Core\.so" "$OUT/ldd-rimesync.so.txt"; then
  echo "!! rimesync.so is not linked against libQt${QT_MAJOR}Core"; exit 1
fi

log "4b/6 dlopen check"
python3 - "$RIME_SO" "$RS_SO" <<'PY'
import ctypes, sys
for path in sys.argv[1:]:
    ctypes.CDLL(path)
    print("dlopen OK:", path)
PY

# ----------------------------------------------------------------- smoke ----
log "5/6 smoke: load addon in fcitx5 under Xvfb"
cp -a "$ROOT/." /
ldconfig
# Install the vendored schema data exactly where the addon looks for it
# (fcitx5-rime is configured with -DRIME_DATA_DIR=$RIME_DATA_DIR).
# Without it the addon loads but cannot deploy, so the image is not usable.
install -d "$RIME_DATA_DIR"
# Flatten every vendored recipe (any third_party/rime-data/* directory holding
# *.yaml / *.txt) into RIME_DATA_DIR, exactly like the Weasel installer and the deb do.
for recipe_dir in "$SRC"/third_party/rime-data/*/; do
  # *.yaml and *.txt both matter: rime-essay ships only essay.txt and would be
  # skipped by a yaml-only filter.
  find "$recipe_dir" -maxdepth 1 -type f \( -name '*.yaml' -o -name '*.txt' \) \
    -exec cp -f {} "$RIME_DATA_DIR"/ \;
done
[ -f "$RIME_DATA_DIR/essay.txt" ] || { echo "!! essay.txt missing from $RIME_DATA_DIR"; exit 1; }
echo "rime data: $(find "$RIME_DATA_DIR" -maxdepth 1 -type f | wc -l) files from $(find "$SRC/third_party/rime-data" -mindepth 1 -maxdepth 1 -type d | wc -l) recipes"
export DISPLAY=:99
Xvfb :99 -screen 0 640x480x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
XVFB_PID=$!
sleep 2
# The rime addon is OnDemand=True, so it is only dlopen-ed when it becomes the
# active input method; make it the default IM of the default group.
mkdir -p /root/.config/fcitx5
cat >/root/.config/fcitx5/profile <<PROFILE
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=rime

[Groups/0/Items/0]
Name=rime
Layout=

[GroupOrder]
0=Default
PROFILE
timeout 90 dbus-run-session -- fcitx5 --replace --verbose "*=5" >/tmp/fcitx5.log 2>&1 || true
kill "$XVFB_PID" 2>/dev/null || true
grep -in 'rime' /tmp/fcitx5.log | tee "$OUT/smoke-fcitx5.log" || true
if ! grep -q "Loaded addon rime" /tmp/fcitx5.log; then
  echo "!! fcitx5 did not load the rime addon"; tail -n 40 /tmp/fcitx5.log; exit 1
fi
if grep -iE 'failed to load|undefined symbol|cannot open shared object|not a valid' /tmp/fcitx5.log; then
  echo "!! fcitx5 failed to load a module"; exit 1
fi
if grep -q 'deploy failure' /tmp/fcitx5.log; then
  echo "!! rime deploy reported failure inside fcitx5"; tail -n 40 /tmp/fcitx5.log; exit 1
fi
RIME_USER_DIR=/root/.local/share/fcitx5/rime
if ! find "$RIME_USER_DIR/build" -name '*.bin' -print -quit 2>/dev/null | grep -q .; then
  echo "!! rime deploy produced no build/*.bin in $RIME_USER_DIR"
  find "$RIME_USER_DIR" -maxdepth 2 -type f 2>/dev/null | head -20
  exit 1
fi
echo "smoke: fcitx5 loaded addon rime and deployed $(find "$RIME_USER_DIR/build" -name '*.bin' | wc -l) table(s)"

log "5b/6 smoke: Rime Sync config page loaded by fcitx5"
# Reuse the addon's own smoke test so the assertions cannot drift from
# packaging/fcitx5-addon/docker: it checks the three installed paths, then starts
# fcitx5 under Xvfb + dbus, asserts "Loaded addon rimesync" (rimesync.conf is
# OnDemand=True, so it is only dlopen-ed when its config page is requested) and
# then makes the exact DBus calls fcitx5-configtool uses for a Configurable
# addon page — GetConfig/SetConfig on fcitx://config/addon/rimesync.
if ! OUT="$OUT" PREFIX="$PREFIX" LIBDIR="$LIBDIR" \
     bash "$SRC/packaging/fcitx5-addon/docker/smoke-test.sh"; then
  echo "!! fcitx5 did not expose the Rime Sync config page"
  tail -n 40 /tmp/fcitx5.log || true
  exit 1
fi

log "5c/6 smoke: librime deploy + luna_pinyin candidate"
rm -rf /tmp/rime-smoke-user
g++ -O2 -std=c++11 -o "$OUT/rime-smoke" "$SRC/scripts/ime/rime-smoke.cc" -lrime
if ! "$OUT/rime-smoke" "$RIME_DATA_DIR" /tmp/rime-smoke-user | tee "$OUT/rime-smoke.log"; then
  echo "!! rime deploy / candidate smoke failed"; exit 1
fi
if ! grep -q 'PASS: candidate .* found' "$OUT/rime-smoke.log"; then
  echo "!! candidate smoke did not pass"; exit 1
fi

# ------------------------------------------------------------------ deps ----
log "6/6 resolve runtime package dependencies"
find "$ROOT" -name '*.so*' -type f | while read -r so; do
  ldd "$so" | awk '/=> \// {print $3}' || true
done | sort -u | while read -r lib; do
  real=$(readlink -f "$lib")
  case "$real" in "$ROOT"/*|*/librime.so.*) continue ;; esac
  # /lib is a symlink to /usr/lib in these images while dpkg records the /lib
  # path, so try both spellings before giving up.
  pkg=""
  for cand in "$lib" "$real"; do
    pkg=$(dpkg -S "$cand" 2>/dev/null | head -n 1 | cut -d: -f1) || pkg=""
    if [ -n "$pkg" ]; then break; fi
  done
  printf '%s\n' "${pkg:-$(basename "$real")}"
done | sort -u | tee "$OUT/deps.txt"

# ------------------------------------------------ packaging extra conflicts ---
# Debian/Ubuntu time_t rebuilds renamed librime1 -> librime1t64. Our package
# ships the same soname, so it must conflict with whichever name exists here.
: >"$OUT/conflicts-extra.txt"
if apt-cache show librime1t64 >/dev/null 2>&1; then
  echo librime1t64 >>"$OUT/conflicts-extra.txt"
fi
echo "extra conflicts: $(paste -sd, "$OUT/conflicts-extra.txt" || true)"

# ------------------------------------------------------------- build info ---
T_END=$(stamp)
ROOT_SIZE=$(du -sb "$ROOT" | cut -f1)
{
  echo "base_image: $BASE_IMAGE"
  echo "built_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "total_seconds: $((T_END - T_ALL))"
  echo "librime_seconds: $((T1 - T0))"
  echo "fcitx5rime_seconds: $((T3 - T2))"
  echo "rimesync_addon_seconds: $((T_ADDON_END - T_ADDON))"
  echo "rimesync_qt_major: $QT_MAJOR ($(pkg-config --modversion "Qt${QT_MAJOR}Core" 2>/dev/null || echo unknown))"
  echo "rimesync_so_bytes: $(stat -c %s "$RS_SO")"
  echo "rimesync_conf: $(grep -E '^(Library|Type|OnDemand|Configurable)=' "$RS_CONF" | paste -sd'|')"
  echo "staging_bytes: $ROOT_SIZE"
  echo "librime_version: $(grep -o 'set(rime_version [0-9.]*' "$SRC/third_party/librime/CMakeLists.txt" | awk '{print $2}')"
  echo "fcitx5rime_version: $(grep -o 'project(fcitx5-rime VERSION [0-9.]*' "$SRC/third_party/fcitx5-rime/CMakeLists.txt" | awk '{print $3}')"
  echo "fcitx5_system_version: $(dpkg-query -W -f='${Version}' libfcitx5core7 2>/dev/null || echo unknown)"
  echo "rime_data_dir: $RIME_DATA_DIR"
  echo "rime_data_files: $(find "$RIME_DATA_DIR" -maxdepth 1 -type f | wc -l)"
  echo "rime_user_build_tables: $(find "$RIME_USER_DIR/build" -name '*.bin' 2>/dev/null | wc -l)"
  echo "extra_conflicts: $(paste -sd, "$OUT/conflicts-extra.txt" || true)"
  echo "rime_smoke:"
  sed 's/^/  /' "$OUT/rime-smoke.log"
  echo "enable_logging: $ENABLE_LOGGING (glog: ${GLOG_LIB:-none})"
  echo "ldd_rime_so:"
  sed 's/^/  /' "$OUT/ldd-rime.so.txt"
  echo "ldd_rimesync_so:"
  sed 's/^/  /' "$OUT/ldd-rimesync.so.txt"
  echo "rimesync_getconfig:"
  head -n 3 "$OUT/getconfig.txt" | sed 's/^/  /'
  echo "smoke_log:"
  sed 's/^/  /' "$OUT/smoke-fcitx5.log"
} >"$OUT/build-info.txt"
echo
cat "$OUT/build-info.txt"
