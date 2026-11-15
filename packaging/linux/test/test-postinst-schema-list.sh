#!/bin/bash
# postinst 的「方案列表补齐」回归测试（纯 shell、离线、不需要 root / docker）。
#
# 背景：用户的 default.custom.yaml 里的 schema_list 就是「装了哪些方案」的清单，
# librime 只按 schema_list 生成 build/ —— 清单里没有的方案，一次全量重新部署
# 就会连编译产物一起被清掉，用户正在用的方案会「凭空消失」。
#
# 保留清单（packaging/rime/retained-schemas.txt）里的方案必须出现在这个清单里。
# 补齐逻辑本来在 crates/core/src/ime_state.rs 的 ensure 里（Windows 首启走它，
# 所以 Windows 有这个补齐），但 Linux 的 postinst 只调 repair-customs（不管
# schema_list）与 prune-schemas（只删不补），走不到 ensure ⇒ Linux 装/升级后
# 保留方案永远补不进来。
#
# 本测试做两件事：
#   ① 断言 postinst 里存在 repair_schema_list 函数，且 configure 段在
#      prune_extra_schemas **之后** 调用它（顺序反了会把刚补上的方案又裁掉）；
#   ② 断言 CLI 的 repair-schema-list 子命令真能把保留方案补进 schema_list，
#      且幂等（第二次跑不重复追加）、不改用户原有条目与顺序、备份原文件。
#
# 用法：bash packaging/linux/test/test-postinst-schema-list.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
postinst=$here/../postinst
repo=$(cd "$here/../../.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/postinst-schema-list.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0
check() {
    desc=$1 expect=$2 got=$3
    if [ "$got" = "$expect" ]; then
        printf '  OK   %s\n' "$desc"
        pass=$((pass + 1))
    else
        printf '  FAIL %s\n         期望 [%s]\n         实得 [%s]\n' "$desc" "$expect" "$got"
        fail=$((fail + 1))
    fi
}

# ---------------------------------------------------------------- ① postinst 结构
echo "== postinst 结构 =="

if [ ! -f "$postinst" ]; then
    echo "找不到 postinst：$postinst" >&2
    exit 1
fi

check "postinst 里有 repair_schema_list 函数" "yes" \
    "$(grep -q '^repair_schema_list()' "$postinst" && echo yes || echo no)"

check "repair_schema_list 调的是 repair-schema-list 子命令" "yes" \
    "$(grep -q 'repair-schema-list --user-dir' "$postinst" && echo yes || echo no)"

# 顺序：configure 段里 repair_schema_list 的调用行号必须大于 prune_extra_schemas 的
# 调用行号 —— prune 只删不补，先补后裁会把刚补上的方案又裁掉。
prune_line=$(grep -n '^    prune_extra_schemas "\$RIME_USER_DIR"' "$postinst" | head -1 | cut -d: -f1)
repair_line=$(grep -n '^    repair_schema_list "\$RIME_USER_DIR"' "$postinst" | head -1 | cut -d: -f1)
if [ -n "$prune_line" ] && [ -n "$repair_line" ] && [ "$repair_line" -gt "$prune_line" ]; then
    order=ok
else
    order="bad (prune=$prune_line repair=$repair_line)"
fi
check "configure 段里 repair_schema_list 在 prune_extra_schemas 之后" "ok" "$order"

# ---------------------------------------------------------------- ② CLI 子命令行为
echo
echo "== CLI repair-schema-list 行为 =="

cli_bin="$repo/target/debug/rime-sync-cli"
if [ ! -x "$cli_bin" ]; then
    # CI 的 packaging job 不构建 Rust 二进制 ⇒ 这里只跑结构断言（①）。
    echo "（跳过 CLI 行为断言：没有 $cli_bin，先 cargo build -p rime-sync-cli 才有）"
    echo
    if [ "$fail" -eq 0 ]; then
        echo "postinst 结构断言全部通过（$pass 项）"
        exit 0
    fi
    echo "失败 $fail 项（通过 $pass 项）"
    exit 1
fi

data_dir=/usr/share/rime-data
if [ ! -d "$data_dir" ]; then
    # CI 上不一定装了 rime-data（打包脚本 job 不装输入法本体）。
    # 没有 data 目录就没有「方案本体」，CLI 的门禁会一个都不补 ⇒
    # 行为断言无法验证，但 postinst 的结构断言（①）仍然有意义，照跑。
    echo "（跳过 CLI 行为断言：本机没有 $data_dir）"
    echo
    if [ "$fail" -eq 0 ]; then
        echo "postinst 结构断言全部通过（$pass 项）"
        exit 0
    fi
    echo "失败 $fail 项（通过 $pass 项）"
    exit 1
fi

# 保留清单（与 crates/core/src/prune.rs 的 RETAINED_SCHEMAS_TXT 同源）
retained="$repo/packaging/rime/retained-schemas.txt"
if [ ! -f "$retained" ]; then
    echo "找不到保留清单：$retained" >&2
    exit 1
fi
# 取清单里的方案 id（忽略注释与空行）
mapfile -t keep_ids < <(grep -v '^[[:space:]]*#' "$retained" | grep -v '^[[:space:]]*$')

# --- 场景 A：用户 schema_list 里缺保留方案 ⇒ 补齐，且原有条目与顺序不变
user_dir="$tmp/user"
mkdir -p "$user_dir"
printf 'patch:\n  schema_list:\n    - {schema: luna_pinyin}\n' >"$user_dir/default.custom.yaml"

# 期望补进来的数量 = 保留清单里「data 目录中真实存在、且用户的列表里还没有」的个数。
# （CLI 有门禁：data 里不存在的方案不补 —— 补了会让 librime 整次部署失败。）
# 注意：必须在跑命令**之前**算 —— 跑完就补齐了，再数就永远�� 0。
expect_added=0
for id in "${keep_ids[@]}"; do
    [ -f "$data_dir/$id.schema.yaml" ] || continue
    grep -q "schema: $id" "$user_dir/default.custom.yaml" && continue
    expect_added=$((expect_added + 1))
done

out=$("$cli_bin" repair-schema-list --user-dir "$user_dir" --data-dir "$data_dir" 2>&1)
added=$(printf '%s\n' "$out" | sed -n 's/^repair_added=//p' | head -1)

check "缺失的保留方案被补进来（个数与期望一致）" "$expect_added" "$added"

check "用户原有条目 luna_pinyin 仍在" "yes" \
    "$(grep -q 'schema: luna_pinyin' "$user_dir/default.custom.yaml" && echo yes || echo no)"

check "luna_pinyin 仍在第一位（原有顺序不变）" "yes" \
    "$(grep -q -m1 'schema: luna_pinyin' "$user_dir/default.custom.yaml" && \
      [ "$(grep -n -m1 'schema: luna_pinyin' "$user_dir/default.custom.yaml" | cut -d: -f1)" -lt \
        "$(grep -n -m1 'schema: luna_pinyin_simp' "$user_dir/default.custom.yaml" | cut -d: -f1)" ] \
      && echo yes || echo no)"

check "原文件被备份成 .rimesync-orig" "yes" \
    "$([ -f "$user_dir/default.custom.yaml.rimesync-orig" ] && echo yes || echo no)"

check "备份内容等于补齐前的原文件" "yes" \
    "$(printf 'patch:\n  schema_list:\n    - {schema: luna_pinyin}\n' | \
       diff -q - "$user_dir/default.custom.yaml.rimesync-orig" >/dev/null && echo yes || echo no)"

# --- 场景 B：幂等 —— 第二次跑不重复追加
before=$(cat "$user_dir/default.custom.yaml")
out2=$("$cli_bin" repair-schema-list --user-dir "$user_dir" --data-dir "$data_dir" 2>&1)
added2=$(printf '%s\n' "$out2" | sed -n 's/^repair_added=//p' | head -1)
after=$(cat "$user_dir/default.custom.yaml")

check "第二次跑：不再追加任何方案" "0" "$added2"
check "第二次跑：文件一个字节没动" "same" \
    "$([ "$before" = "$after" ] && echo same || echo different)"

# --- 场景 C：用户目录里没有 default.custom.yaml ⇒ 安全跳过，不凭空创建
empty_dir="$tmp/empty"
mkdir -p "$empty_dir"
out3=$("$cli_bin" repair-schema-list --user-dir "$empty_dir" --data-dir "$data_dir" 2>&1)
skipped=$(printf '%s\n' "$out3" | sed -n 's/^repair_skipped=//p' | head -1)

check "缺 default.custom.yaml：跳过（不报错）" "1" "$skipped"
check "缺 default.custom.yaml：不凭空创建文件" "no" \
    "$([ -f "$empty_dir/default.custom.yaml" ] && echo yes || echo no)"

echo
if [ "$fail" -eq 0 ]; then
    echo "全部通过（$pass 项）"
    exit 0
fi
echo "失败 $fail 项（通过 $pass 项）"
exit 1
