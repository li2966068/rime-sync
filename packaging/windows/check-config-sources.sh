#!/usr/bin/env bash
# check-config-sources.sh —— 断言 config 模板**只有一份源**（A-4）。
#
# 背景：config 模板（default.custom.yaml / weasel.custom.yaml / <schema>.custom.yaml）
# 曾经有两份：
#   1. packaging/rime/schemas/ + packaging/rime/{default,weasel}.custom.yaml
#      （手写源，同时进 Windows 的 resources/ 与 Linux 的 deb）
#   2. packaging/rime/prebuilt/config/（gen-prebuilt-build.py 生成的拷贝）
# 两份迟早漂移：改了 1 忘了重新生成 2，或者反过来。
#
# 现在的约定（A-4）：
#   * **手写源** = packaging/rime/schemas/ + packaging/rime/{default,weasel}.custom.yaml
#   * **分发源** = packaging/rime/prebuilt/config/（生成物，进 Windows 资源）
#   * 程序侧只读分发源（payload 的 config/）；Windows 资源目录里不再带第二份。
#   * 本脚本断言两者**不漂移**。
#
# 用法：bash packaging/windows/check-config-sources.sh
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
REPO=$(cd "$PKG/.." && pwd)

SRC_DIR="$PKG/rime/schemas"
DST_DIR="$PKG/rime/prebuilt/config"
fail=0

printf 'check-config-sources: 源=%s 分发=%s\n' "$SRC_DIR" "$DST_DIR"

if [ ! -d "$DST_DIR" ]; then
  echo "FAIL config-sources  分发目录不存在：$DST_DIR" >&2
  echo "                   先跑 scripts/ime/gen-prebuilt-build.py 生成随包预编译产物" >&2
  exit 1
fi

# 1) 每个手写源的 <schema>.custom.yaml 必须在分发源里存在且逐字节相同。
for src in "$SRC_DIR"/*.custom.yaml; do
  [ -e "$src" ] || continue
  name=$(basename "$src")
  dst="$DST_DIR/$name"
  if [ ! -f "$dst" ]; then
    printf 'FAIL config-sources  分发源缺 %s（改了手写源忘了重新生成）\n' "$name" >&2
    fail=1
    continue
  fi
  if ! cmp -s "$src" "$dst"; then
    printf 'FAIL config-sources  %s 与手写源不一致（重新跑 gen-prebuilt-build.py）\n' "$name" >&2
    fail=1
  else
    printf 'ok   config-sources  %s 一致\n' "$name"
  fi
done

# 2) weasel.custom.yaml 也必须在分发源里（seed 路径靠它播外观模板）。
if [ -f "$PKG/rime/weasel.custom.yaml" ]; then
  if cmp -s "$PKG/rime/weasel.custom.yaml" "$DST_DIR/weasel.custom.yaml"; then
    printf 'ok   config-sources  weasel.custom.yaml 一致\n'
  else
    printf 'FAIL config-sources  weasel.custom.yaml 与手写源不一致\n' >&2
    fail=1
  fi
fi

# 3) 分发源里不能有多余的 <schema>.custom.yaml（手写源删了方案，生成物没更新）。
for dst in "$DST_DIR"/*.custom.yaml; do
  [ -e "$dst" ] || continue
  name=$(basename "$dst")
  case "$name" in
    default.custom.yaml|weasel.custom.yaml) continue ;;
  esac
  if [ ! -f "$SRC_DIR/$name" ]; then
    printf 'FAIL config-sources  分发源多出 %s（手写源已删；重新跑 gen-prebuilt-build.py）\n' "$name" >&2
    fail=1
  fi
done

# 4) Windows 资源目录里**不再**带第二份 schemas/（A-4 的核心断言）。
TAURI_CONF="$REPO/src-tauri/tauri.conf.json"
if grep -qF 'packaging/rime/schemas/' "$TAURI_CONF"; then
  printf 'FAIL config-sources  tauri.conf.json 仍把 packaging/rime/schemas/ 打进 resources/（两份源会漂移）\n' >&2
  fail=1
else
  printf 'ok   config-sources  tauri.conf.json 不再带第二份 schemas/\n'
fi

# 5) Linux deb 仍然用 packaging/rime/schemas（发行包渠道，与 Windows 无关），
#    但 default.custom.yaml 必须还在（deb 资源里引用它）。
if [ ! -f "$PKG/rime/default.custom.yaml" ]; then
  printf 'FAIL config-sources  缺少手写源 packaging/rime/default.custom.yaml\n' >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "check-config-sources: config 源不一致（见上）" >&2
  exit 1
fi
echo "check-config-sources: ok（config 模板只有一份手写源，分发副本一致）"

