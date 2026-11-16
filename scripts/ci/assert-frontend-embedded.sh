#!/usr/bin/env bash
# 断言：桌面二进制**自带界面文件**（前端产物已打进程序里）。
#
# 为什么要有这道关：
# Tauri v2 有两种模式。不带 custom-protocol feature 时是 **dev 模式** —— 前端产物
# 不打进程序，程序运行时去 devUrl（http://localhost:1420）现场取；那台机器上没开
# vite 的话，界面就是一片白（2026-11-16 踩过，还一度误判成自己代码的问题）。
#
# 判据：Tauri 把前端产物以「assets/index-<hash>.js」这样的相对路径嵌进二进制，
# 在二进制里搜得到这个路径，就说明界面确实在里面。
#
# 用法：
#   assert-frontend-embedded.sh <二进制路径>
#   assert-frontend-embedded.sh --selftest     用合成产物证明这道断言双向有效
set -euo pipefail

assert_embedded() {
  local bin=$1
  if [ ! -f "$bin" ]; then
    echo "::error::找不到 $bin"
    return 1
  fi
  # 只对**桌面程序**用这道断言：命令行工具（rime-sync-cli）本来就没有界面，
  # 拿它来跑这道断言必然被拦住 —— 那不是缺陷，是用错了对象。
  local base
  base=$(basename "$bin")
  case "$base" in
    rime-sync-cli|rime-sync-cli.exe)
      echo "::error::$base 是命令行工具，没有界面 —— 这道断言只适用于桌面程序（rime-sync-app）"
      return 1
      ;;
  esac
  local hits
  hits=$(strings "$bin" | grep -cE 'assets/index-[A-Za-z0-9_-]+\.(js|css)' || true)
  if [ "$hits" -lt 1 ]; then
    echo "::error::桌面二进制里搜不到前端资源（assets/index-*.js|css）"
    echo "::error::这个程序是 dev 模式 —— 界面没打包进去，装到用户机器上会白屏"
    return 1
  fi
  echo "ok：$bin 内含前端资源（命中 $hits 处）"
}

if [ "${1:-}" = "--selftest" ]; then
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT

  # 1) 好二进制：含前端资源路径 → 断言必须通过
  {
    printf 'PK\x03\x04 fake-pe-header\n'
    printf '/usr/local/bin/rime-sync-app\0'
    printf 'assets/index-Ba0sFDfN.js\0'
    printf 'assets/index-Ba0sFDfN.css\0'
  } > "$work/good.bin"

  # 2) 坏二进制：dev 模式，界面没打包 → 断言必须报错
  {
    printf 'PK\x03\x04 fake-pe-header\n'
    printf '/usr/local/bin/rime-sync-app\0'
    printf 'http://localhost:1420\0'
  } > "$work/bad.bin"

  if ! assert_embedded "$work/good.bin"; then
    echo "!! 自检失败：含前端资源的二进制应当通过断言" >&2
    exit 1
  fi
  if assert_embedded "$work/bad.bin" >/dev/null 2>&1; then
    echo "!! 自检失败：dev 模式（无前端资源）的二进制应当被断言拦住" >&2
    exit 1
  fi
  # 3) 文件不存在 → 也必须报错
  if assert_embedded "$work/nope.bin" >/dev/null 2>&1; then
    echo "!! 自检失败：不存在的文件应当被断言拦住" >&2
    exit 1
  fi
  # 4) 命令行工具 → 必须报错（它本来就没有界面，拿它跑这道断言是用错了对象）
  cp "$work/bad.bin" "$work/rime-sync-cli"
  if assert_embedded "$work/rime-sync-cli" >/dev/null 2>&1; then
    echo "!! 自检失败：命令行工具应当被断言拦住（这道断言只适用于桌面程序）" >&2
    exit 1
  fi
  echo "自检通过：好二进制放行，dev 模式二进制、缺失文件、命令行工具都被拦住。"
  exit 0
fi

if [ $# -ne 1 ]; then
  echo "用法：$0 <二进制路径> | $0 --selftest" >&2
  exit 2
fi

assert_embedded "$1"
