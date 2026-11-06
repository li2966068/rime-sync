#!/usr/bin/env bash
# check-portable-zip.sh —— 便携 zip 打包的静态断言（0.6.22）。
#
# 为什么需要：便携包没有安装器，resources/prebuilt 与 resources/weasel 只能由
# packaging/portable-zip.ps1 自己复制。Linux CI 跑不了 PowerShell，这里按文本
# 断言关键步骤，并由 check-nsis-hooks.sh 统一调用（--strict 下一起失败）。
#
# 断言内容：
#   * 整个 Tauri resources 目录被复制进便携包；
#   * resources/prebuilt 必带，缺失/空目录直接 throw；
#   * resources/weasel/weasel-*.exe 会被收集，缺失至少给 warning + README 提示；
#   * core 的 fix-ime 能识别便携版并提示「手动安装小狼毫」。
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
SCRIPT="$PKG/portable-zip.ps1"
IME="$PKG/../crates/core/src/ime.rs"

fail=0
count=0
assert_has() { # <说明> <固定字符串> [文件]
  count=$((count + 1))
  local label=$1 needle=$2 file="$SCRIPT"
  if [ $# -ge 3 ]; then file=$3; fi
  if [ ! -f "$file" ]; then
    printf 'FAIL portable-zip  %s\n           文件不存在：%s\n' "$label" "$file" >&2
    fail=1
    return
  fi
  if ! grep -qF -- "$needle" "$file"; then
    printf 'FAIL portable-zip  %s\n           缺少：%s（%s）\n' "$label" "$needle" "$file" >&2
    fail=1
  fi
}

assert_has "复制 Tauri resources 到便携包" 'Copy-Item -LiteralPath $resources -Destination $stagedResources'
assert_has "resources/prebuilt 必带" 'resources/prebuilt'
assert_has "prebuilt 缺失时直接失败" 'throw "便携包必须含 resources/prebuilt'
assert_has "prebuilt 为空目录也失败" 'resources/prebuilt 是空目录'
assert_has "收集 weasel-*.exe" 'weasel-*.exe'
assert_has "weasel 缺失时告警" 'fix-ime 无法自动装回输入法'
assert_has "README 带手动安装提示" '手动安装小狼毫（Weasel）'
assert_has "便携版检测函数" 'pub fn looks_portable' "$IME"
assert_has "fix-ime 便携版提示" '便携版不内置输入法安装器' "$IME"

if [ "$fail" -ne 0 ]; then
  echo "check-portable-zip: 便携 zip 静态断言失败（见上）" >&2
  exit 1
fi
printf 'ok   portable-zip %s 条断言（resources/prebuilt 必带 + weasel 安装器 + 便携版提示）\n' "$count"
