#!/usr/bin/env bash
# fetch-vendor-debs.sh — 抓取 fcitx5-rime 在目标发行版上的完整运行期依赖（.deb），
# 让 rime-sync 的 deb 包自带一个"断网也能装"的输入法依赖闭包。
#
# 用法:
#   packaging/linux/fetch-vendor-debs.sh                       # 默认 ubuntu:22.04
#   IMAGE=ubuntu:24.04 VERSION_ID=24.04 packaging/linux/fetch-vendor-debs.sh
#   APT_MIRROR=http://mirrors.aliyun.com/ubuntu packaging/linux/fetch-vendor-debs.sh
#   （用 http：基础镜像里没有 ca-certificates，https 源会因证书校验失败而拿不到索引）
#
# 产物（打包时由 tauri.conf.json 的 bundle.linux.deb.files 映射到
#       /usr/share/rime-sync/vendor/）:
#   packaging/linux/vendor/*.deb         依赖闭包（可整体 dpkg -i）
#   packaging/linux/vendor/VENDOR-INFO   发行版/版本/生成时间/包版本
#   packaging/linux/vendor/SHA256SUMS    安装期校验用
#
# 依赖: docker + 构建机能联网（安装期不需要网络）。
set -euo pipefail

IMAGE="${IMAGE:-ubuntu:22.04}"
VERSION_ID="${VERSION_ID:-22.04}"
DISTRO="${DISTRO:-ubuntu}"
# 顶层包：装完即具备 fcitx5-rime 输入法能力；依赖闭包由 apt 解析。
PKGS="${PKGS:-fcitx5 fcitx5-rime librime-data}"
# 可选：构建机在墙内时换源加速（默认用镜像自带 sources.list）。
APT_MIRROR="${APT_MIRROR:-}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENDOR="$HERE/vendor"

if ! command -v docker >/dev/null 2>&1; then
  echo "错误: 需要 docker（在容器内解析依赖，避免污染构建机）" >&2
  exit 1
fi

# 容器里没有宿主机的 systemd-resolved(127.0.0.53)，不显式给 --dns 会连域名都解析不了。
# 默认用 host 网络：部分 docker 环境的 bridge 网络没有出网/DNS（实测容器内
# getent hosts 报 Temporary failure in name resolution，即使显式 --dns 也不通）。
# 抓包只是构建期下载，用 host 网络最稳；需要隔离时用 NETWORK=bridge 覆盖。
NETWORK="${NETWORK:-host}"
DOCKER_ARGS=(--network "$NETWORK")

# 只清生成物，保留 vendor/README.md（它随包进 /usr/share/rime-sync/vendor/）
mkdir -p "$VENDOR"
rm -f "$VENDOR"/*.deb "$VENDOR/VENDOR-INFO" "$VENDOR/SHA256SUMS"

echo "==> 在 $IMAGE 内下载依赖闭包: $PKGS"
docker run --rm "${DOCKER_ARGS[@]}" -v "$VENDOR:/out" \
  -e PKGS="$PKGS" -e EXPECTED_DISTRO="$DISTRO" -e EXPECTED_VERSION_ID="$VERSION_ID" \
  -e APT_MIRROR="$APT_MIRROR" \
  "$IMAGE" bash -euxo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive

    # /etc/os-release 本身就是 shell 语法，source 比反复 sed + 转义引号可靠
    . /etc/os-release
    if [ "${VERSION_ID:-}" != "$EXPECTED_VERSION_ID" ]; then
      echo "错误: 镜像的 VERSION_ID=${VERSION_ID:-未知}，与期望的 $EXPECTED_VERSION_ID 不符" >&2
      exit 2
    fi

    if [ -n "$APT_MIRROR" ]; then
      echo "==> 换源: $APT_MIRROR"
      for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
        [ -f "$f" ] || continue
        sed -i "s|http://archive.ubuntu.com/ubuntu|$APT_MIRROR|g; s|http://security.ubuntu.com/ubuntu|$APT_MIRROR|g" "$f"
      done
    fi

    apt-get update -qq

    # apt 需要 archives 目录下的 partial 子目录，否则报 "Archives directory ... is missing"
    mkdir -p /tmp/dl/partial

    # --download-only + 私有 archives 目录：只下载不安装，依赖闭包由 apt 解析。
    apt-get install -y --no-install-recommends --download-only \
      -o Dir::Cache::archives=/tmp/dl -o Debug::NoLocking=1 $PKGS

    find /tmp/dl -maxdepth 1 -name "*.deb" -exec cp {} /out/ \;
    n=$(find /out -maxdepth 1 -name "*.deb" | wc -l)
    echo "==> 共 $n 个 .deb"

    {
      echo "# rime-sync 离线依赖清单，由 packaging/linux/fetch-vendor-debs.sh 生成"
      echo "distro=${ID:-unknown}"
      echo "version_id=$VERSION_ID"
      echo "codename=${VERSION_CODENAME:-unknown}"
      echo "generated=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "requested=$PKGS"
      echo "---packages---"
      for d in /out/*.deb; do
        printf "%s %s\\n" "$(dpkg-deb -f "$d" Package)" "$(dpkg-deb -f "$d" Version)"
      done | sort
    } > /out/VENDOR-INFO

    ( cd /out && sha256sum *.deb > SHA256SUMS )
    echo "==> 体积: $(du -sh /out | cut -f1)，包数: $n"
  '

echo "==> 完成: $VENDOR"
du -sh "$VENDOR"
