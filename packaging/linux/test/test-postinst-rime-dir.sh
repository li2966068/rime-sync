#!/bin/bash
# postinst 的 Rime 用户目录解析回归测试（纯 shell、离线、不需要 root / docker）。
#
# 背景：fcitx5-rime 可以在 ~/.config/fcitx5/conf/rime.conf 里用 UserDataDirPath
# 指定用户数据目录。fcitx5 真正读写的正是那个目录，不是默认的
# ~/.local/share/fcitx5/rime。postinst 以前硬编码默认目录，于是受管 custom.yaml
# 与预编译产物都 seed 到了没人读的目录，用户真正在用的目录反而缺文件
#（表现为「切不到五笔 / check-ime 恒 failed」）。
# 本测试把 postinst 里的解析函数抽出来跑；规则必须与 crates/core/src/rime_dir.rs
# 的 parse_fcitx5_user_data_dir 保持一致。
#
# 用法：bash packaging/linux/test/test-postinst-rime-dir.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
postinst=$here/../postinst
tmp=$(mktemp -d "${TMPDIR:-/tmp}/postinst-rime-dir.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

if [ ! -f "$postinst" ]; then
    echo "找不到 postinst：$postinst" >&2
    exit 1
fi

# 从 postinst 里抽出 resolve_rime_user_dir 函数
awk '
  /^resolve_rime_user_dir\(\) \{/{ f = 1 }
  f { print }
  f && /^\}/{ exit }
' "$postinst" >"$tmp/rud.sh"

if ! grep -q '^resolve_rime_user_dir()' "$tmp/rud.sh"; then
    echo "postinst 里找不到 resolve_rime_user_dir 函数" >&2
    exit 1
fi

. "$tmp/rud.sh"

pass=0
fail=0
check() {
    desc=$1 expect=$2 home=$3
    got=$(resolve_rime_user_dir "$home")
    if [ "$got" = "$expect" ]; then
        printf '  OK   %s -> %s\n' "$desc" "$got"
        pass=$((pass + 1))
    else
        printf '  FAIL %s -> 期望 [%s] 实得 [%s]\n' "$desc" "$expect" "$got"
        fail=$((fail + 1))
    fi
}

home=$tmp/home
mkdir -p "$home/.config/fcitx5/conf"
conf=$home/.config/fcitx5/conf/rime.conf

check "没有 rime.conf 时用默认目录" "$home/.local/share/fcitx5/rime" "$home"
printf 'UserDataDirPath=/home/ljj/rime/\n' >"$conf"
check "绝对路径（末尾斜杠去掉）" "/home/ljj/rime" "$home"
printf 'UserDataDirPath = "/tmp/quoted"\n' >"$conf"
check "键值之间有空格 + 双引号" "/tmp/quoted" "$home"
printf "UserDataDirPath='~/single'\n" >"$conf"
check "单引号 + ~ 展开" "$home/single" "$home"
printf 'UserDataDirPath: ~/tilde-dir\n' >"$conf"
check "冒号分隔 + ~ 展开" "$home/tilde-dir" "$home"
printf 'UserDataDirPath=~\n' >"$conf"
check "单个 ~ 指 HOME" "$home" "$home"
printf 'UserDataDirPath=  ~/spaced  \n' >"$conf"
check "值两端有空格" "$home/spaced" "$home"
printf 'UserDataDirPath=\n' >"$conf"
check "空值回落默认目录" "$home/.local/share/fcitx5/rime" "$home"
printf '# UserDataDirPath=/commented\nOtherKey=1\n' >"$conf"
check "注释行不算" "$home/.local/share/fcitx5/rime" "$home"
printf '[Section]\nUserDataDirPath=/after-section\n' >"$conf"
check "section 之后的键" "/after-section" "$home"
printf 'UserDataDirPath=/first\nUserDataDirPath=/second\n' >"$conf"
check "取第一个出现的键" "/first" "$home"
check "HOME 为空" "" ""

printf '\n通过 %d 项，失败 %d 项\n' "$pass" "$fail"
[ "$fail" -eq 0 ]