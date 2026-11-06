#!/usr/bin/env bash
# librime 构建缓存（Linux，容器内使用）的写入与命中校验。
#
# 背景：scripts/ime/build-linux.sh 在 docker 里编 librime（scripts/ime/container-build.sh
# 的 1/6 步）。构建目录 /work 是临时的，每次都要重配 + 重编。这里把 **cmake --install 的
# 安装前缀**（DESTDIR 布局）连同 manifest.json 放进宿主挂进来的缓存目录，命中就跳过编译。
#
#   librime-build-cache.sh verify <cache-dir> <src-dir> <libdir> <logging> <base-image>
#       exit 0 = 命中，打印 cache=hit
#       exit 1 = 不可用，打印 cache=invalid reason=...
#       verify 只判定、不删目录（调用方按「判 invalid 就删」处理，和 Windows 侧一致）。
#
#   librime-build-cache.sh write <cache-dir> <src-dir> <libdir> <logging> <base-image>
#       <cache-dir>/root 已经是从 cmake --install 出来的 DESTDIR 树时，写 manifest.json。
#
# 判定依据：librime 源码树 / rimesync-extra 插件源码 / 构建脚本与 Dockerfile 的哈希、
# librime 版本、安装树整体哈希，外加 librime.so 里必须有的插件符号
# （rime_require_module_rimesync_extra + 四个组件类）。任何一项不符都当作不可用 —— 绝不拿
# 过期 .so 出包。
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: librime-build-cache.sh verify|write <cache-dir> <src-dir> <libdir> <logging> <base-image>
USAGE
}

if [ "$#" -ne 6 ]; then
  usage
  exit 2
fi

ACTION=$1
CACHE=$2
SRC=$3
LIBDIR=$4
LOGGING=$5
BASE_IMAGE=$6

PREFIX=/usr
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
LIBRIME_SRC=$SRC/third_party/librime
MANIFEST=$CACHE/manifest.json
ROOT=$CACHE/root
# 缓存里的 librime.so 必须带的插件标记。用**符号名**而不是字符串常量：GCC 会把
# "rimesync_vnum" 这种 16 字节以内的字面量直接变成立即数，二进制里根本不留连续字节，
# 而 rimesync-extra 的类符号（Itanium mangling 后仍含类名）与 RIME_REGISTER_MODULE 生成的
# rime_require_module_rimesync_extra 一定在 .dynstr 里。
PLUGIN_MARKERS="rime_require_module_rimesync_extra VnumTranslator PunctProcessor VselectProcessor NumpadProcessor"

say() { printf '%s\n' "$*"; }

# 目录内容指纹：相对路径 + 每个文件 sha256，排序后再取一次 sha256。
# 排除 deps/build/dist/.git：它们要么是构建产物，要么是别处克隆来的。
tree_sha256() {
  local dir=$1
  [ -d "$dir" ] || { say ""; return 0; }
  ( cd "$dir" \
      && find . \( -name deps -o -name build -o -name dist -o -name .git \) -prune -o -type f -print0 \
      | LC_ALL=C sort -z \
      | xargs -0 -r sha256sum \
      | sha256sum | cut -d' ' -f1 )
}

# 构建配方指纹：容器内真正决定 librime 产物的四个脚本。
recipe_sha256() {
  local files=() f
  for f in container-build.sh build-linux.sh librime-build-cache.sh Dockerfile; do
    [ -f "$SCRIPT_DIR/$f" ] && files+=("$f")
  done
  if [ "${#files[@]}" -eq 0 ]; then say ""; return 0; fi
  ( cd "$SCRIPT_DIR" && sha256sum "${files[@]}" | LC_ALL=C sort | sha256sum | cut -d' ' -f1 )
}

librime_version() {
  sed -n 's/^[[:space:]]*set(rime_version[[:space:]]\+\([0-9][0-9A-Za-z.-]*\)[[:space:]]*)/\1/p' \
    "$LIBRIME_SRC/CMakeLists.txt" 2>/dev/null | head -n 1
}

json_get() {
  # 单行 "key": "value" 的取值（容器里没有 jq，只用 sed）
  sed -n 's/^[[:space:]]*"'"$2"'":[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

# 返回缺失的插件标记（空 = 齐全）
missing_plugin_markers() {
  local so=$1 miss=""
  for marker in $PLUGIN_MARKERS; do
    grep -aq "$marker" "$so" || miss="$miss $marker"
  done
  printf '%s' "${miss# }"
}

find_librime_so() {
  local dir=$ROOT$PREFIX/$LIBDIR
  [ -d "$dir" ] || return 0
  find "$dir" -maxdepth 1 -type f -name 'librime.so.1.*' -print 2>/dev/null | head -n 1
}

# ---------------------------------------------------------------------------
# write
# ---------------------------------------------------------------------------
write_manifest() {
  if [ ! -d "$ROOT" ]; then
    say "!! write: $ROOT 不存在（cmake --install 没跑成？）"
    exit 1
  fi
  local so version tree plugin recipe so_sha so_bytes
  so=$(find_librime_so)
  if [ -z "$so" ]; then
    say "!! write: $ROOT$PREFIX/$LIBDIR 下没有 librime.so.1.*，缓存不可写"
    exit 1
  fi
  local missing
  missing=$(missing_plugin_markers "$so")
  if [ -n "$missing" ]; then
    say "!! write: $(basename "$so") 里找不到插件符号：$missing（插件没编进去），缓存不可写"
    exit 1
  fi
  version=$(librime_version); [ -n "$version" ] || version=unknown
  tree=$(tree_sha256 "$LIBRIME_SRC")
  plugin=$(tree_sha256 "$LIBRIME_SRC/plugins/rimesync-extra")
  recipe=$(recipe_sha256)
  so_sha=$(sha256sum "$so" | cut -d' ' -f1)
  so_bytes=$(wc -c <"$so" | tr -d ' ')

  : >"$MANIFEST"
  {
    printf '{\n'
    printf '  "schema": "1",\n'
    printf '  "platform": "linux",\n'
    printf '  "created_at": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '  "librime_version": "%s",\n' "$version"
    printf '  "librime_tree_sha256": "%s",\n' "$tree"
    printf '  "librime_plugin_sha256": "%s",\n' "$plugin"
    printf '  "recipe_sha256": "%s",\n' "$recipe"
    printf '  "libdir": "%s",\n' "$LIBDIR"
    printf '  "logging": "%s",\n' "$LOGGING"
    printf '  "base_image": "%s",\n' "$BASE_IMAGE"
    printf '  "librime_so": "%s",\n' "${so#"$ROOT"}"
    printf '  "librime_so_sha256": "%s",\n' "$so_sha"
    printf '  "librime_so_bytes": "%s",\n' "$so_bytes"
    printf '  "install_sha256": "%s"\n' "$(tree_sha256 "$ROOT")"
    printf '}\n'
  } >"$MANIFEST"
  say "[cache] manifest: $MANIFEST"
  say "[cache] librime $version  so=$so_sha  install=$(json_get "$MANIFEST" install_sha256)"
}

# ---------------------------------------------------------------------------
# verify
# ---------------------------------------------------------------------------
invalid() {
  say "cache=invalid reason=$1"
  exit 1
}

verify_cache() {
  [ -f "$MANIFEST" ] || invalid no-manifest

  local version tree plugin recipe
  version=$(librime_version); [ -n "$version" ] || version=unknown
  tree=$(tree_sha256 "$LIBRIME_SRC")
  plugin=$(tree_sha256 "$LIBRIME_SRC/plugins/rimesync-extra")
  recipe=$(recipe_sha256)

  [ "$(json_get "$MANIFEST" schema)" = "1" ] || invalid manifest-schema
  [ "$(json_get "$MANIFEST" librime_version)" = "$version" ] || invalid inputs-changed:librime_version
  [ "$(json_get "$MANIFEST" librime_tree_sha256)" = "$tree" ] || invalid inputs-changed:librime_tree
  [ "$(json_get "$MANIFEST" librime_plugin_sha256)" = "$plugin" ] || invalid inputs-changed:plugin
  [ "$(json_get "$MANIFEST" recipe_sha256)" = "$recipe" ] || invalid inputs-changed:scripts
  [ "$(json_get "$MANIFEST" libdir)" = "$LIBDIR" ] || invalid inputs-changed:libdir
  [ "$(json_get "$MANIFEST" logging)" = "$LOGGING" ] || invalid inputs-changed:logging
  [ "$(json_get "$MANIFEST" base_image)" = "$BASE_IMAGE" ] || invalid inputs-changed:base_image

  local so recorded_so
  recorded_so=$(json_get "$MANIFEST" librime_so)
  [ -n "$recorded_so" ] || invalid manifest-no-so
  so=$ROOT$recorded_so
  [ -f "$so" ] || invalid artifact-missing:"$recorded_so"
  [ "$(sha256sum "$so" | cut -d' ' -f1)" = "$(json_get "$MANIFEST" librime_so_sha256)" ] \
    || invalid artifact-changed:"$recorded_so"

  local missing
  missing=$(missing_plugin_markers "$so")
  [ -z "$missing" ] || invalid plugin-missing:"${missing// /,}"

  [ "$(tree_sha256 "$ROOT")" = "$(json_get "$MANIFEST" install_sha256)" ] || invalid artifact-changed:install-tree

  say "[cache] librime $version  so=$recorded_so  install=$(json_get "$MANIFEST" install_sha256)"
  say "cache=hit"
}

case "$ACTION" in
  verify) verify_cache ;;
  write) write_manifest ;;
  *)
    usage
    exit 2
    ;;
esac