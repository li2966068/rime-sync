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

log "0/5 prepare ($(nproc) cpus, multiarch $LIBDIR)"
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
log "1/5 configure + build librime 1.17.0"
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
log "2/5 configure + build fcitx5-rime 5.0.16"
echo "system rime.pc -> $(pkg-config --modversion rime) ($(pkg-config --variable=includedir rime))"
T2=$(stamp)
cmake -S "$SRC/third_party/fcitx5-rime" -B "$WORK/fcitx5-rime" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=$PREFIX \
  -DCMAKE_INSTALL_LIBDIR=$LIBDIR \
  -DRIME_DATA_DIR=$RIME_DATA_DIR
cmake --build "$WORK/fcitx5-rime" -j"$JOBS"
DESTDIR="$ROOT" cmake --install "$WORK/fcitx5-rime"
T3=$(stamp)
echo "fcitx5-rime build+install: $((T3 - T2)) s"

# ------------------------------------------------------------- strip/trim ----
if [ "${STRIP:-1}" = "1" ]; then
  find "$ROOT$PREFIX/$LIBDIR" -name '*.so*' -type f -exec strip --strip-unneeded {} +
fi

# --------------------------------------------------------------- verify -----
log "3/5 link check"
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

log "3b/5 dlopen check"
python3 - "$RIME_SO" <<'PY'
import ctypes, sys
ctypes.CDLL(sys.argv[1])
print("dlopen OK:", sys.argv[1])
PY

# ----------------------------------------------------------------- smoke ----
log "4/5 smoke: load addon in fcitx5 under Xvfb"
cp -a "$ROOT/." /
ldconfig
# Install the vendored schema data exactly where the addon looks for it
# (fcitx5-rime is configured with -DRIME_DATA_DIR=$RIME_DATA_DIR).
# Without it the addon loads but cannot deploy, so the image is not usable.
install -d "$RIME_DATA_DIR"
cp -f "$SRC/third_party/rime-data/rime-prelude/"*.yaml "$RIME_DATA_DIR"/
cp -f "$SRC/third_party/rime-data/rime-essay/essay.txt" "$RIME_DATA_DIR"/
cp -f "$SRC/third_party/rime-data/rime-luna-pinyin/"*.yaml "$RIME_DATA_DIR"/
echo "rime data: $(find "$RIME_DATA_DIR" -maxdepth 1 -type f | wc -l) files"
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

log "4b/5 smoke: librime deploy + luna_pinyin candidate"
rm -rf /tmp/rime-smoke-user
g++ -O2 -std=c++11 -o "$OUT/rime-smoke" "$SRC/scripts/ime/rime-smoke.cc" -lrime
if ! "$OUT/rime-smoke" "$RIME_DATA_DIR" /tmp/rime-smoke-user | tee "$OUT/rime-smoke.log"; then
  echo "!! rime deploy / candidate smoke failed"; exit 1
fi
if ! grep -q 'PASS: candidate .* found' "$OUT/rime-smoke.log"; then
  echo "!! candidate smoke did not pass"; exit 1
fi

# ------------------------------------------------------------------ deps ----
log "5/5 resolve runtime package dependencies"
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
  echo "smoke_log:"
  sed 's/^/  /' "$OUT/smoke-fcitx5.log"
} >"$OUT/build-info.txt"
echo
cat "$OUT/build-info.txt"
