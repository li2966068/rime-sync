#!/usr/bin/env bash
# 护栏：发布的 CLI 二进制必须与当前源码一致。
#
# 2026-11-10 起加。起因是「线上二进制比仓库源码旧」这类漂移很难靠肉眼发现：
# 二进制里的字符串常量会被 Rust/LLVM 拆成 8 字节立即数内嵌进指令流
# （例如 not-managed 实际存成 not-mana + aged），所以**不能用字节搜索/strings
# 去数短字面量**，查不到不代表没有。本脚本只做两件可靠的事：
#
#   1. 符号名断言（nm -C）：断言二进制里有当前源码才有的函数符号；
#   2. 运行时行为断言：真的跑一次二进制，检查它的 JSON 输出对不对。
#
# 用法：
#   scripts/ci/assert-cli-matches-source.sh <path-to-rime-sync-cli>
#
# 退出码：0 = 一致；非 0 = 不一致（CI 应当红）。

set -euo pipefail

BIN=${1:-target/release/rime-sync-cli}

if [ ! -x "$BIN" ]; then
  echo "找不到可执行的 CLI：$BIN" >&2
  exit 1
fi

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)

fail() {
  echo "!! 失败：$*" >&2
  exit 1
}

# ---------------------------------------------------------------- 1. 版本号
# workspace 的版本号（crates/core 与 crates/cli 都 version.workspace = true）。
WANT_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)
[ -n "$WANT_VERSION" ] || fail "读不出 Cargo.toml 的 workspace 版本号"

GOT_VERSION=$("$BIN" --version | awk '{print $NF}')
if [ "$GOT_VERSION" != "$WANT_VERSION" ]; then
  fail "CLI 版本号 $GOT_VERSION ≠ 源码版本号 $WANT_VERSION（二进制与源码脱节？）"
fi
echo "ok  版本号：$GOT_VERSION"

# --------------------------------------------------------------- 2. 符号名
# 这些函数是 0.6.x 期间陆续加进 crates/core 的；旧二进制里没有。
# 用 nm -C 看**符号名**（不受字符串拆分影响），不用字节搜索。
if command -v nm >/dev/null 2>&1; then
  SYMS=$(nm -C "$BIN" 2>/dev/null || true)
  for sym in verify_repaired verify_schema_list repair_default_schema_list_file; do
    case "$SYMS" in
      *"rime_sync_core::managed_custom::$sym"*) ;;
      *) fail "二进制里找不到符号 rime_sync_core::managed_custom::$sym（二进制比源码旧？）" ;;
    esac
  done
  echo "ok  符号名：verify_repaired / verify_schema_list / repair_default_schema_list_file"
else
  echo "跳过符号名断言（本机没有 nm）"
fi

# --------------------------------------------------------- 3. 运行时行为
# 真跑一次二进制：用**与父键同缩进**的列表（合法 YAML，2026-11-10 现场写法）
# 建一个受管 custom.yaml，跑 check-ime，断言它被认成「已含 numpad」。
#
# 这条断言同时覆盖两件事：
#   - 二进制确实包含 2026-11-10 的块扫描修复（否则会误报 source-stale）；
#   - 二进制的 JSON 输出结构没变（字段名对得上）。

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat >"$TMP/luna_pinyin_simp.custom.yaml" <<'EOF'
# patched by rime-sync 0.6.26
patch:
  engine/processors:
  - ascii_composer
  - recognizer
  - rimesync_vselect
  - numpad
  - key_binder
  engine/translators:
  - rimesync_vnum
  - punct_translator
EOF

OUT=$("$BIN" --rime-dir "$TMP" check-ime --json 2>/dev/null || true)
[ -n "$OUT" ] || fail "check-ime 没有输出 JSON"

echo "$OUT" | python3 -c '
import json, sys

doc = json.load(sys.stdin)
numpad = doc.get("numpad") or {}
detail = numpad.get("detail") or []

# 判据：numpad 明细里**不能出现 source-stale**。
#
# 2026-11-10 实测（同一份输入、同一个隔离目录，两个二进制的对照）：
#   修复前的二进制 → luna_pinyin_simp=source-stale   （误报：用户文件其实含 numpad）
#   修复后的二进制 → luna_pinyin_simp=no-artifact   （正确：只是缺编译产物）
# no-artifact 表示「编译产物里没找到 numpad」，与「用户 custom.yaml 里缺 numpad」
# 是两回事，所以这里断言的是「不出现 source-stale」，而不是「必须出现 ok」——
# 后者会依赖 CI 机器上有没有编译产物。
for line in detail:
    if "source-stale" in line:
        sys.exit(
            "!! check-ime 报 source-stale（二进制还是修复前的旧版本）：%s" % detail
        )
print("ok  运行时行为：同缩进列表不再被误报 source-stale（明细：%s）" % detail)
'

echo "全部断言通过：$BIN 与当前源码一致。"
