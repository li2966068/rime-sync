#!/usr/bin/env bash
# 在 docker 里编译 rime-sync 的 fcitx5 addon（rimesync.so + rimesync.conf），
# 安装到容器 /usr（/usr/lib/<triplet>/fcitx5、/usr/share/fcitx5/addon），
# 然后用 Xvfb + dbus 启动 fcitx5，走 fcitx5-configtool 用的同一套 DBus 接口
# 验证「配置页可读、可写回 ~/.config/rime-sync/config.json」。
#
#   scripts/ime/build-fcitx5-addon.sh
#   APT_MIRROR=mirrors.tuna.tsinghua.edu.cn scripts/ime/build-fcitx5-addon.sh
#   OUT=dist/fcitx5-addon scripts/ime/build-fcitx5-addon.sh --no-smoke
#
# 环境变量：BASE_IMAGE / IMAGE / APT_MIRROR / OUT / JOBS / BUILD_NETWORK
# 选项：--out DIR  --image TAG  --base-image TAG  --apt-mirror HOST
#       --no-smoke  --rebuild-image  -h
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
ADDON_DIR=$REPO_ROOT/packaging/fcitx5-addon
BASE_IMAGE=${BASE_IMAGE:-ubuntu:22.04}
APT_MIRROR=${APT_MIRROR:-}
IMAGE=${IMAGE:-}
OUT=${OUT:-$ADDON_DIR/build}
JOBS=${JOBS:-$(nproc)}
SMOKE=1
FORCE_IMAGE=0
# docker build 默认不传 --network（= buildkit 的 default）；buildkit 只接受
# default/host/none，传 bridge 会直接失败。本机 DNS 有问题时用 BUILD_NETWORK=host。
BUILD_NETWORK=${BUILD_NETWORK:-}
case "$BUILD_NETWORK" in
    ""|default|host|none) : ;;
    *)
        echo "不支持的 BUILD_NETWORK: $BUILD_NETWORK（可用 default|host|none）" >&2
        exit 2
        ;;
esac
if [ "$BUILD_NETWORK" = "default" ]; then BUILD_NETWORK=; fi

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT=$2; shift 2 ;;
        --image) IMAGE=$2; shift 2 ;;
        --base-image) BASE_IMAGE=$2; shift 2 ;;
        --apt-mirror) APT_MIRROR=$2; shift 2 ;;
        --no-smoke) SMOKE=0; shift ;;
        --rebuild-image) FORCE_IMAGE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "未知参数: $1" >&2; usage >&2; exit 2 ;;
    esac
done

command -v docker >/dev/null 2>&1 || { echo "需要 docker" >&2; exit 1; }
if [ -z "$IMAGE" ]; then
    IMAGE="rime-sync-fcitx5-addon-builder:$(printf '%s' "$BASE_IMAGE" | tr -c 'a-zA-Z0-9_.-' '_')"
fi

if [ "$FORCE_IMAGE" = 1 ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo ">>> 构建镜像 $IMAGE（base=$BASE_IMAGE, apt_mirror=${APT_MIRROR:-默认}）"
    docker build -t "$IMAGE" \
        ${BUILD_NETWORK:+--network "$BUILD_NETWORK"} \
        --build-arg BASE_IMAGE="$BASE_IMAGE" \
        --build-arg APT_MIRROR="$APT_MIRROR" \
        "$ADDON_DIR/docker"
fi

mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

echo ">>> 容器内编译 / 安装 / 冒烟测试"
docker run --rm \
    -e JOBS="$JOBS" \
    -e RIMESYNC_SMOKE="$SMOKE" \
    -v "$ADDON_DIR:/src:ro" \
    -v "$OUT:/out" \
    "$IMAGE" \
    bash /src/docker/ci-build.sh

echo
echo ">>> 安装树（$OUT/root）"
sed 's/^/    /' "$OUT/artifacts.txt"
echo ">>> ldd 缺失库: $(grep -c 'not found' "$OUT/ldd-rimesync.so.txt" || true)"
if [ "$SMOKE" = 1 ] && [ -f "$OUT/getconfig.txt" ]; then
    echo ">>> 配置页接口抓包: $OUT/getconfig.txt"
fi
