#!/usr/bin/env bash
# 在构建镜像里执行：编译、安装（DESTDIR）、ldd 检查、可选的加载冒烟测试。
# 只由 scripts/ime/build-fcitx5-addon.sh 调用，不直接在宿主机跑。
set -euo pipefail

SRC=${SRC:-/src}
OUT=${OUT:-/out}
PREFIX=${PREFIX:-/usr}
LIBDIR=${LIBDIR:-lib/$(dpkg-architecture -qDEB_HOST_MULTIARCH)}
JOBS=${JOBS:-$(nproc)}
SMOKE=${RIMESYNC_SMOKE:-1}
WORK=/work

log() { printf '\n########## %s\n' "$*"; }

rm -rf "$WORK" "$OUT/root"
mkdir -p "$WORK" "$OUT/root"

log "configure"
cmake -S "$SRC" -B "$WORK/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$PREFIX" \
  -DCMAKE_INSTALL_LIBDIR="$LIBDIR" \
  -DRIMESYNC_BUILD_FCITX5_ADDON=ON \
  -DRIMESYNC_BUILD_STANDALONE_CONFIG=ON

log "build ($JOBS jobs)"
cmake --build "$WORK/build" -j"$JOBS"

log "install -> $OUT/root$PREFIX"
DESTDIR="$OUT/root" cmake --install "$WORK/build"
find "$OUT/root" -type f | sed "s|^$OUT/root||" | sort | tee "$OUT/artifacts.txt"

log "ldd rimesync.so"
SO="$OUT/root$PREFIX/$LIBDIR/fcitx5/rimesync.so"
test -f "$SO" || { echo "!! missing $SO"; exit 1; }
ldd "$SO" | tee "$OUT/ldd-rimesync.so.txt"
if grep -q 'not found' "$OUT/ldd-rimesync.so.txt"; then
  echo "!! unresolved shared libraries in rimesync.so"; exit 1
fi

log "install into $PREFIX (same paths a real install would use)"
cp -a "$OUT/root$PREFIX/." "$PREFIX/"

if [ "$SMOKE" = "1" ]; then
  log "smoke test: fcitx5 loads the addon and its config page is readable"
  OUT="$OUT" PREFIX="$PREFIX" LIBDIR="$LIBDIR" bash "$SRC/docker/smoke-test.sh"
else
  log "smoke test skipped (RIMESYNC_SMOKE=0)"
fi

log "done"
