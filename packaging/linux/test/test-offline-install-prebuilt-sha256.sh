#!/bin/bash
# 离线安装器（packaging/linux/install.sh）的随包预编译产物 sha256 校验回归测试。
#
# 背景：deb 的 postinst 已经会按 MANIFEST.json 校验随包产物的 sha256 再复制，
# 但**离线安装器**（install.sh，用户不装 deb、直接跑 .run 时用的那条路）以前
# 是直接 install 复制、不校验的 —— 同一份产物，两条安装路径的校验口径不一致：
# 走 deb 会拦下损坏的文件，走离线包不会。
#
# 本测试断言两件事：
#   1. install.sh 里的三个校验函数与 postinst **逐字一致**（口径不许分叉）；
#   2. 校验真的生效：被篡改的文件不复制，完好的文件照常复制。
#
# 用法：bash packaging/linux/test/test-offline-install-prebuilt-sha256.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
install_sh=$here/../install.sh
postinst=$here/../postinst
tmp=$(mktemp -d "${TMPDIR:-/tmp}/offline-sha256.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

if [ ! -f "$install_sh" ]; then
    echo "找不到 $install_sh" >&2
    exit 1
fi

pass=0
fail=0

ok() { pass=$((pass + 1)); echo "  OK   $1"; }
ng() { fail=$((fail + 1)); echo "  FAIL $1"; }

# 从脚本里抽出指定函数（从 "name() {" 到行首 "}"）。
extract() { # $1=脚本路径 $2=函数名 $3=输出文件
    awk -v want="$2" '
      $0 ~ "^" want "\\(\\) \\{" { f = 1 }
      f { print }
      f && /^\}/{ exit }
    ' "$1" >"$3"
    if ! grep -q "^$2()" "$3"; then
        echo "在 $1 里找不到 $2 函数" >&2
        exit 1
    fi
}

extract "$install_sh" prebuilt_manifest_load "$tmp/manifest_load.sh"
extract "$install_sh" prebuilt_manifest_missing "$tmp/manifest_missing.sh"
extract "$install_sh" prebuilt_verify_file "$tmp/verify_file.sh"
extract "$install_sh" prebuilt_copy_dir "$tmp/copy_dir.sh"

cat >"$tmp/env.sh" <<EOF
say() { printf '[say] %s\n' "\$*" >&2; }
PREBUILT_LOG=$tmp/prebuilt.log
export PREBUILT_LOG
EOF

sha_of() { sha256sum "$1" | cut -d' ' -f1; }

# 造一个假 payload：build/ + config/ 各一个文件 + MANIFEST.json
make_payload() { # $1=根目录
    local root=$1
    mkdir -p "$root/build" "$root/config"
    printf 'BINARY-BUILD' >"$root/build/a.bin"
    printf 'CONFIG-YAML' >"$root/config/b.yaml"
    cat >"$root/MANIFEST.json" <<EOF
{
  "version": "0.6.33",
  "generated_at": "2026-11-16T00:00:00Z",
  "files": [
    {
      "path": "build/a.bin",
      "bytes": $(stat -c%s "$root/build/a.bin"),
      "sha256": "$(sha_of "$root/build/a.bin")"
    },
    {
      "path": "config/b.yaml",
      "bytes": $(stat -c%s "$root/config/b.yaml"),
      "sha256": "$(sha_of "$root/config/b.yaml")"
    }
  ]
}
EOF
}

# ---- 断言 1：三个校验函数与 postinst 逐字一致 ------------------------------
echo "== install.sh 与 postinst 的校验函数一致 =="
# $1=函数名 $2=install.sh 抽出的文件名
check_same() { # $1=函数名 $2=install.sh 抽出的文件名
    extract "$postinst" "$1" "$tmp/postinst_$1.sh"
    if diff -q "$tmp/postinst_$1.sh" "$tmp/$2" >/dev/null 2>&1; then
        ok "$1 与 postinst 逐字一致"
    else
        ng "$1 与 postinst 不一致（口径分叉了）"
        diff "$tmp/postinst_$1.sh" "$tmp/$2" | head -10
    fi
}
check_same prebuilt_manifest_load manifest_load.sh
check_same prebuilt_manifest_missing manifest_missing.sh
check_same prebuilt_verify_file verify_file.sh

# ---- 断言 2：校验真的生效 ---------------------------------------------------
echo "== 校验生效 =="
payload="$tmp/payload"
make_payload "$payload"
cache="$tmp/cache"

# shellcheck disable=SC1090
. "$tmp/env.sh"
# shellcheck disable=SC1091
. "$tmp/manifest_load.sh"
# shellcheck disable=SC1091
. "$tmp/manifest_missing.sh"
# shellcheck disable=SC1091
. "$tmp/verify_file.sh"
# shellcheck disable=SC1091
. "$tmp/copy_dir.sh"

prebuilt_manifest_load "$payload/MANIFEST.json" "$cache"
rc=$?
if [ "$rc" -eq 0 ] && [ -s "$cache" ]; then
    ok "清单解析成功"
else
    ng "清单解析失败 rc=$rc"
fi

# 完好文件 → 复制
dst="$tmp/dst1"
out=$(prebuilt_copy_dir "$payload/build" "$dst" auto root:root "$cache" build)
set -- $out
if [ "$1" = "1" ] && [ "$4" = "0" ]; then
    ok "完好文件被复制 -> $out"
else
    ng "完好文件复制结果不对 -> $out"
fi
if [ -f "$dst/a.bin" ]; then
    ok "目标目录里有 build/a.bin"
else
    ng "目标目录里没有 build/a.bin"
fi

# 被篡改的文件 → 不复制
printf 'TAMPERED!' >"$payload/build/a.bin"
dst="$tmp/dst2"
out=$(prebuilt_copy_dir "$payload/build" "$dst" auto root:root "$cache" build)
set -- $out
if [ "$1" = "0" ] && [ "$4" = "1" ]; then
    ok "被篡改的文件不复制 -> $out"
else
    ng "被篡改文件的复制结果不对 -> $out"
fi
if [ ! -f "$dst/a.bin" ]; then
    ok "被篡改的文件没落到目标目录"
else
    ng "被篡改的文件落到了目标目录"
fi

# 清单缺失 → 不校验，照常复制
payload2="$tmp/payload2"
make_payload "$payload2"
rm -f "$payload2/MANIFEST.json"
cache2="$tmp/cache2"
prebuilt_manifest_load "$payload2/MANIFEST.json" "$cache2"
rc=$?
if [ "$rc" -eq 1 ]; then
    ok "缺失的清单返回 1（=不校验）"
else
    ng "缺失的清单返回了 $rc，应为 1"
fi
dst="$tmp/dst3"
out=$(prebuilt_copy_dir "$payload2/build" "$dst" auto root:root "" build)
set -- $out
if [ "$1" = "1" ]; then
    ok "清单缺失时照常复制 -> $out"
else
    ng "清单缺失时复制结果不对 -> $out"
fi

# 清单损坏 → 判定为解析失败
payload3="$tmp/payload3"
make_payload "$payload3"
printf 'not json at all\n' >"$payload3/MANIFEST.json"
cache3="$tmp/cache3"
prebuilt_manifest_load "$payload3/MANIFEST.json" "$cache3"
rc=$?
if [ "$rc" -eq 2 ]; then
    ok "损坏的清单被判定为解析失败"
else
    ng "损坏的清单返回了 $rc，应为 2"
fi

echo
echo "通过 $pass 项，失败 $fail 项"
[ "$fail" -eq 0 ]
