#!/bin/bash
# 随包预编译产物复制前的 sha256 校验回归测试（纯 shell、离线、不需要 root / docker）。
#
# 背景：postinst 会把 /usr/share/rime-sync/prebuilt/{build,config} 里的文件复制到
# 用户 Rime 目录。包里的 MANIFEST.json 记着每个文件的 sha256 与 size，
# Rust 侧 crates/core/src/prebuilt.rs 的 verify_manifest 会按它校验；
# shell 侧以前直接 install 复制，坏文件（下载损坏 / 打包串了）也会照装，
# 用户只会看到"输入法缺文件"，查不出原因。
#
# 本测试把 postinst 里的三个函数抽出来跑，逐例断言：
#   1. 源文件被篡改（sha256 不符 / 大小不符 / 清单里没有）→ **不复制**；
#   2. 源文件完好 → 正常复制；
#   3. 清单缺失 → 不校验（沿用 Rust 侧口径：旧版 payload 不算失败）；
#   4. 清单损坏（解析不出条目）→ 判定为清单坏，不复制任何文件。
#
# 用法：bash packaging/linux/test/test-postinst-prebuilt-sha256.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
postinst=$here/../postinst
tmp=$(mktemp -d "${TMPDIR:-/tmp}/postinst-sha256.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

if [ ! -f "$postinst" ]; then
    echo "找不到 $postinst" >&2
    exit 1
fi

pass=0
fail=0

# 从脚本里抽出指定函数（从 "name() {" 到行首 "}"）。
# 要求：函数内部不能有以 "}" 开头的行（辅助逻辑必须内联在函数体里）。
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

extract "$postinst" prebuilt_manifest_load "$tmp/manifest_load.sh"
extract "$postinst" prebuilt_manifest_missing "$tmp/manifest_missing.sh"
extract "$postinst" prebuilt_verify_file "$tmp/verify_file.sh"
extract "$postinst" prebuilt_copy_dir "$tmp/copy_dir.sh"

# 抽出的函数依赖 say() / PREBUILT_LOG / 日期，这些在测试里自己造。
cat >"$tmp/env.sh" <<'EOF'
say() { printf '[say] %s\n' "$*" >&2; }
PREBUILT_LOG=${PREBUILT_LOG:-}
export PREBUILT_LOG
EOF

# 造一个假 payload：build/ + config/ 各一个文件 + MANIFEST.json
make_payload() { # $1=根目录
    local root=$1
    mkdir -p "$root/build" "$root/config"
    printf 'build-content\n' >"$root/build/a.bin"
    printf 'config-content\n' >"$root/config/b.yaml"
    {
        printf '{\n  "schema": 1,\n  "generated_at": "2026-01-01T00:00:00Z",\n'
        printf '  "files": [\n'
        local f
        for f in build/a.bin config/b.yaml; do
            printf '    { "path": "%s", "bytes": %s, "sha256": "%s" },\n' \
                "$f" "$(stat -c%s "$root/$f")" \
                "$(sha256sum "$root/$f" | cut -d' ' -f1)"
        done
        printf '  ]\n}\n'
    } >"$root/MANIFEST.json"
}

# check <描述> <期望> <实际>
check() {
    local desc=$1 expect=$2 got=$3
    if [ "$got" = "$expect" ]; then
        printf '  OK   %s -> %s\n' "$desc" "$got"
        pass=$((pass + 1))
    else
        printf '  FAIL %s -> 期望 [%s] 实际 [%s]\n' "$desc" "$expect" "$got"
        fail=$((fail + 1))
    fi
}

# 跑一次复制：$1=payload 根 $2=目标目录 $3=模式；输出 "copied kept failed corrupt"
run_copy() { # $1=payload 根 $2=目标目录 $3=mode
    local root=$1 dst=$2 mode=$3 cache=""
    PREBUILT_LOG="$tmp/prebuilt.log"
    export PREBUILT_LOG
    : >"$tmp/prebuilt.log"
    cache="$tmp/manifest.tsv"
    if . "$tmp/env.sh" && . "$tmp/manifest_load.sh" && . "$tmp/verify_file.sh" && . "$tmp/copy_dir.sh"; then
        prebuilt_manifest_load "$root/MANIFEST.json" "$cache"
        rc=$?
        case "$rc" in
            0) # 清单可用 → 复制前按 sha256 校验
                prebuilt_copy_dir "$root/build" "$dst/build" "$mode" "$(id -un):$(id -gn)" "$cache" build
                ;;
            1) # 清单缺失 → 不校验（沿用 Rust 侧口径），照常复制
                prebuilt_copy_dir "$root/build" "$dst/build" "$mode" "$(id -un):$(id -gn)" "" build
                ;;
            *) # 清单损坏 → 不复制任何文件
                printf '0 0 0 0\n'
                ;;
        esac
    fi
}

# ---- 用例 1：源文件完好 → 正常复制 ----
root=$tmp/case1
make_payload "$root"
dst=$tmp/case1-dst
out=$(run_copy "$root" "$dst" auto)
check "完好文件被复制" "1 0 0 0" "$out"
if [ -f "$dst/build/a.bin" ]; then
    printf '  OK   目标目录里有 build/a.bin\n'
    pass=$((pass + 1))
else
    printf '  FAIL 目标目录里没有 build/a.bin\n'
    fail=$((fail + 1))
fi

# ---- 用例 2：源文件内容被篡改（sha256 不符）→ 不复制 ----
root=$tmp/case2
make_payload "$root"
printf 'TAMPERED\n' >"$root/build/a.bin"
dst=$tmp/case2-dst
out=$(run_copy "$root" "$dst" auto)
check "被篡改的文件不复制" "0 0 0 1" "$out"
if [ ! -f "$dst/build/a.bin" ]; then
    printf '  OK   被篡改的文件没落到目标目录\n'
    pass=$((pass + 1))
else
    printf '  FAIL 被篡改的文件被复制了\n'
    fail=$((fail + 1))
fi
if grep -q "corrupt file=build/a.bin" "$tmp/prebuilt.log"; then
    printf '  OK   prebuilt.log 里记了 corrupt 行\n'
    pass=$((pass + 1))
else
    printf '  FAIL prebuilt.log 里没有 corrupt 行：%s\n' "$(cat "$tmp/prebuilt.log")"
    fail=$((fail + 1))
fi

# ---- 用例 3：大小被改（sha256 与 size 都不符）→ 不复制 ----
root=$tmp/case3
make_payload "$root"
printf 'x' >>"$root/build/a.bin"
dst=$tmp/case3-dst
out=$(run_copy "$root" "$dst" auto)
check "大小变化的文件不复制" "0 0 0 1" "$out"
if [ ! -f "$dst/build/a.bin" ]; then
    printf '  OK   大小变化的文件没落到目标目录\n'
    pass=$((pass + 1))
else
    printf '  FAIL 大小变化的文件被复制了\n'
    fail=$((fail + 1))
fi

# ---- 用例 4：清单里没有这个文件 → 不复制（无法校验） ----
root=$tmp/case4
make_payload "$root"
printf 'extra\n' >"$root/build/extra.bin"
dst=$tmp/case4-dst
out=$(run_copy "$root" "$dst" auto)
check "清单外的文件不复制" "1 0 0 1" "$out"
if [ ! -f "$dst/build/extra.bin" ]; then
    printf '  OK   清单外的文件没落到目标目录\n'
    pass=$((pass + 1))
else
    printf '  FAIL 清单外的文件被复制了\n'
    fail=$((fail + 1))
fi

# ---- 用例 5：目标目录已有同名文件 → 沿用不覆盖（既有策略，2026-11-02 定案） ----
root=$tmp/case5
make_payload "$root"
dst=$tmp/case5-dst
mkdir -p "$dst/build"
printf 'USER-OWN\n' >"$dst/build/a.bin"
out=$(run_copy "$root" "$dst" auto)
check "已有同名文件时沿用不覆盖" "0 1 0 0" "$out"
if [ "$(cat "$dst/build/a.bin")" = "USER-OWN" ]; then
    printf '  OK   用户原有文件内容没被改\n'
    pass=$((pass + 1))
else
    printf '  FAIL 用户原有文件被覆盖了\n'
    fail=$((fail + 1))
fi

# ---- 用例 6：清单缺失 → 不校验（沿用 Rust 侧口径，不算失败） ----
root=$tmp/case6
make_payload "$root"
rm -f "$root/MANIFEST.json"
dst=$tmp/case6-dst
out=$(run_copy "$root" "$dst" auto)
check "清单缺失时按原样复制（不校验）" "1 0 0 0" "$out"
if [ -f "$dst/build/a.bin" ]; then
    printf '  OK   清单缺失时文件照常复制\n'
    pass=$((pass + 1))
else
    printf '  FAIL 清单缺失时文件没复制\n'
    fail=$((fail + 1))
fi

# ---- 用例 7：清单损坏（解析不出条目）→ prebuilt_manifest_load 返回 2 ----
root=$tmp/case7
make_payload "$root"
printf 'not json at all\n' >"$root/MANIFEST.json"
cache=$tmp/case7.tsv
if (
    . "$tmp/env.sh"
    . "$tmp/manifest_load.sh"
    prebuilt_manifest_load "$root/MANIFEST.json" "$cache"
); then
    printf '  FAIL 损坏的清单应被判定为解析失败\n'
    fail=$((fail + 1))
else
    printf '  OK   损坏的清单被判定为解析失败\n'
    pass=$((pass + 1))
fi

# ---- 用例 8：清单缺失时 prebuilt_manifest_load 返回 1 ----
if (
    . "$tmp/env.sh"
    . "$tmp/manifest_load.sh"
    prebuilt_manifest_load "$tmp/nonexistent-MANIFEST.json" "$tmp/case8.tsv"
); then
    printf '  FAIL 缺失的清单应返回 1\n'
    fail=$((fail + 1))
else
    rc=$?
    if [ "$rc" = 1 ]; then
        printf '  OK   缺失的清单返回 1（=不校验）\n'
        pass=$((pass + 1))
    else
        printf '  FAIL 缺失的清单返回了 %s（期望 1）\n' "$rc"
        fail=$((fail + 1))
    fi
fi

# ---- 用例 9：清单里的 sha256 大写也能匹配（与 Rust 侧大小写不敏感一致） ----
root=$tmp/case9
make_payload "$root"
# 只把 sha256 字段里的十六进制小写字母转成大写（path 保持原样）
awk '/"sha256"/ { n = split($0, a, /"sha256": "/); pre = a[1]; rest = a[2]; k = index(rest, "\""); val = substr(rest, 1, k - 1); tail = substr(rest, k); print pre "\"sha256\": \"" toupper(val) "\"" tail; next } { print }' \
    "$root/MANIFEST.json" >"$root/MANIFEST.upper.json"
mv "$root/MANIFEST.upper.json" "$root/MANIFEST.json"
dst=$tmp/case9-dst
out=$(run_copy "$root" "$dst" auto)
check "大写 sha256 也能匹配（大小写不敏感）" "1 0 0 0" "$out"

# ---- 用例 10：config/ 前缀的条目也走同一套校验 ----
root=$tmp/case10
make_payload "$root"
printf 'TAMPERED-CONFIG\n' >"$root/config/b.yaml"
dst=$tmp/case10-dst
cache=$tmp/case10.tsv
out=$(
    PREBUILT_LOG="$tmp/prebuilt.log"
    export PREBUILT_LOG
    : >"$tmp/prebuilt.log"
    (
        . "$tmp/env.sh"
        . "$tmp/manifest_load.sh"
        . "$tmp/verify_file.sh"
        . "$tmp/copy_dir.sh"
        prebuilt_manifest_load "$root/MANIFEST.json" "$cache"
        prebuilt_copy_dir "$root/config" "$dst" "auto" "$(id -un):$(id -gn)" "$cache" config
    )
)
check "config/ 下的坏文件同样不复制" "0 0 0 1" "$out"
if [ ! -f "$dst/b.yaml" ]; then
    printf '  OK   config/ 下的坏文件没落到目标目录\n'
    pass=$((pass + 1))
else
    printf '  FAIL config/ 下的坏文件被复制了\n'
    fail=$((fail + 1))
fi

# ---- 用例 11：清单里有、payload 里没有的条目要能查出来（与 Rust 侧 missing 对齐） ----
root=$tmp/case11
make_payload "$root"
rm -f "$root/build/a.bin"
cache=$tmp/case11.tsv
out=$(
    . "$tmp/manifest_load.sh"
    . "$tmp/manifest_missing.sh"
    prebuilt_manifest_load "$root/MANIFEST.json" "$cache"
    prebuilt_manifest_missing "$cache" "$root"
)
if printf '%s' "$out" | grep -q 'build/a.bin:missing:'; then
    printf '  OK   清单里有、payload 里没有的条目被查出来\n'
    pass=$((pass + 1))
else
    printf '  FAIL 没查出来缺失条目：%s\n' "$out"
    fail=$((fail + 1))
fi
if printf '%s' "$out" | grep -q 'config/b.yaml:missing:'; then
    printf '  FAIL 不该把还存在的文件报成缺失：%s\n' "$out"
    fail=$((fail + 1))
else
    printf '  OK   还存在的文件没被误报成缺失\n'
    pass=$((pass + 1))
fi

printf '\n通过 %d 项，失败 %d 项\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
