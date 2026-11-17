#!/bin/bash
# Rime 用户目录解析回归测试（纯 shell、离线、不需要 root / docker）。
#
# 背景：fcitx5-rime 可以在 ~/.config/fcitx5/conf/rime.conf 里用 UserDataDirPath
# 指定用户数据目录。fcitx5 真正读写的正是那个目录，不是默认的
# ~/.local/share/fcitx5/rime。postinst 以前硬编码默认目录，于是受管 custom.yaml
# 与预编译产物都 seed 到了没人读的目录，用户真正在用的目录反而缺文件
#（表现为「切不到五笔 / check-ime 恒 failed」）。
#
# 解析规则只有一处权威实现：crates/core/src/rime_dir.rs 的
# parse_fcitx5_user_data_dir。shell 侧（postinst / uninstall.sh / install-extra-ime.sh）
# 必须与它逐条一致，否则安装器算出的目录与卸载器算出的目录不是同一个，
# 卸载 --purge-data 就会删错目录。
#
# 本测试把 postinst 与 uninstall.sh 里的解析函数分别抽出来跑，逐例断言：
#   1. 两边结果一致；
#   2. 结果等于期望值。
#
# 用法：bash packaging/linux/test/test-postinst-rime-dir.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
postinst=$here/../postinst
uninstaller=$here/../uninstall.sh
extra=$here/../install-extra-ime.sh
installer=$here/../install.sh
tmp=$(mktemp -d "${TMPDIR:-/tmp}/postinst-rime-dir.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

for f in "$postinst" "$uninstaller" "$extra" "$installer"; do
    if [ ! -f "$f" ]; then
        echo "找不到 $f" >&2
        exit 1
    fi
done

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

extract "$postinst" resolve_config_home "$tmp/postinst_config.sh"
extract "$postinst" parse_rime_user_data_dir "$tmp/postinst_parse.sh"
extract "$postinst" rime_user_dir_from_cli "$tmp/postinst_cli.sh"
extract "$postinst" resolve_rime_user_dir "$tmp/postinst_resolve.sh"
extract "$uninstaller" resolve_config_home "$tmp/uninst_config.sh"
extract "$extra" resolve_config_home "$tmp/extra_config.sh"
extract "$extra" parse_rime_user_data_dir "$tmp/extra_parse.sh"
extract "$extra" rime_user_dir_from_cli "$tmp/extra_cli.sh"
extract "$extra" resolve_rime_user_dir "$tmp/extra_resolve.sh"
extract "$uninstaller" parse_rime_user_data_dir "$tmp/uninst_parse.sh"
extract "$uninstaller" rime_user_dir_from_cli "$tmp/uninst_cli.sh"
extract "$uninstaller" rime_user_dir "$tmp/uninst_resolve.sh"
extract "$installer" resolve_config_home "$tmp/inst_config.sh"
extract "$installer" parse_rime_user_data_dir "$tmp/inst_parse.sh"
extract "$installer" rime_user_dir_from_cli "$tmp/inst_cli.sh"
extract "$installer" resolve_rime_user_dir "$tmp/inst_resolve.sh"

# 假的 rime-sync-cli：默认「没装」（退出码 127 语义 = 命令不可用），
# 让测试走本地回退解析这条路径。
mkdir -p "$tmp/bin"
cat >"$tmp/bin/rime-sync-cli" <<'EOF'
#!/bin/sh
exit 127
EOF
chmod +x "$tmp/bin/rime-sync-cli"

# 假的 rime-sync-cli：返回权威 JSON，用于验证 CLI 优先路径。
mkdir -p "$tmp/bin2"
cat >"$tmp/bin2/rime-sync-cli" <<EOF
#!/bin/sh
cat <<'JSON'
{
  "path": "/cli/authoritative",
  "source": "fcitx5-rime:UserDataDirPath"
}
JSON
EOF
chmod +x "$tmp/bin2/rime-sync-cli"

pass=0
fail=0

# run_in <脚本目录前缀> <函数名> <HOME>
# 在子 shell 里装载抽出的函数并执行，避免两边函数名互相覆盖。
run_in() { # $1=前缀 $2=函数 $3=HOME
    (
        PATH="$tmp/bin:$PATH"
        # 只装载存在的抽取文件（install-extra-ime.sh 那边只抽了 resolve_config_home）
        for f in config parse cli resolve; do
            [ -f "$1/$f.sh" ] && . "$1/$f.sh"
        done
        "$2" "$3"
    )
}

mkdir -p "$tmp/p" "$tmp/u" "$tmp/e" "$tmp/i"
cp "$tmp/postinst_config.sh" "$tmp/p/config.sh"
cp "$tmp/postinst_parse.sh" "$tmp/p/parse.sh"
cp "$tmp/postinst_cli.sh" "$tmp/p/cli.sh"
cp "$tmp/postinst_resolve.sh" "$tmp/p/resolve.sh"
cp "$tmp/uninst_config.sh" "$tmp/u/config.sh"
cp "$tmp/extra_config.sh" "$tmp/e/config.sh"
cp "$tmp/extra_parse.sh" "$tmp/e/parse.sh"
cp "$tmp/extra_cli.sh" "$tmp/e/cli.sh"
cp "$tmp/extra_resolve.sh" "$tmp/e/resolve.sh"
cp "$tmp/uninst_parse.sh" "$tmp/u/parse.sh"
cp "$tmp/uninst_cli.sh" "$tmp/u/cli.sh"
cp "$tmp/uninst_resolve.sh" "$tmp/u/resolve.sh"
cp "$tmp/inst_config.sh" "$tmp/i/config.sh"
cp "$tmp/inst_parse.sh" "$tmp/i/parse.sh"
cp "$tmp/inst_cli.sh" "$tmp/i/cli.sh"
cp "$tmp/inst_resolve.sh" "$tmp/i/resolve.sh"

# check <描述> <期望> <HOME>
# 四个实现（postinst / uninstall.sh / install.sh）必须给出同一个目录。
check() {
    local desc=$1 expect=$2 chome=$3 got_p got_u got_e got_i
    got_p=$(run_in "$tmp/p" resolve_rime_user_dir "$chome" | tr -d '\n')
    got_u=$(run_in "$tmp/u" rime_user_dir "$chome" | tr -d '\n')
    got_e=$(run_in "$tmp/e" resolve_rime_user_dir "$chome" | tr -d '\n')
    got_i=$(run_in "$tmp/i" resolve_rime_user_dir "$chome" | tr -d '\n')
    if [ "$got_p" = "$expect" ] && [ "$got_u" = "$expect" ] \
        && [ "$got_e" = "$expect" ] && [ "$got_i" = "$expect" ]; then
        printf '  OK   %s -> %s\n' "$desc" "$got_p"
        pass=$((pass + 1))
    else
        printf '  FAIL %s -> 期望 [%s] postinst=[%s] uninstall=[%s] extra-ime=[%s] install=[%s]\n' \
            "$desc" "$expect" "$got_p" "$got_u" "$got_e" "$got_i"
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

# ---- 以下 5 例对应用户复现的五种写法：必须与 Rust 侧结果完全一致 ----
printf 'UserDataDirPath: /tmp/colon-form\n' >"$conf"
check "冒号分隔（无空格）" "/tmp/colon-form" "$home"
printf "UserDataDirPath='/tmp/single-quoted'\n" >"$conf"
check "单引号包裹的绝对路径" "/tmp/single-quoted" "$home"
printf 'UserDataDirPath=~\n' >"$conf"
check "裸 ~ 指 HOME（不是默认目录）" "$home" "$home"
printf 'UserDataDirPath=relrime\n' >"$conf"
check "相对路径按未设置处理（回落默认目录）" "$home/.local/share/fcitx5/rime" "$home"
printf 'UserDataDirPath=/tmp/dir=with=equals\n' >"$conf"
check "路径里含 = 时不截断" "/tmp/dir=with=equals" "$home"

# ---- 配置根目录（XDG）----
# 三个 shell 脚本取配置目录的地方统一走 resolve_config_home：
# 「XDG_CONFIG_HOME 优先，缺省 ~/.config」。这里断言 postinst 与 uninstall.sh
# 两边的解析结果一致，且与期望值相符。
check_config_home() { # $1=描述 $2=期望 $3=HOME
    local desc=$1 expect=$2 chome=$3 got_p got_u got_e got_i
    got_p=$(run_in "$tmp/p" resolve_config_home "$chome" | tr -d '\n')
    got_u=$(run_in "$tmp/u" resolve_config_home "$chome" | tr -d '\n')
    got_e=$(run_in "$tmp/e" resolve_config_home "$chome" | tr -d '\n')
    got_i=$(run_in "$tmp/i" resolve_config_home "$chome" | tr -d '\n')
    if [ "$got_p" = "$expect" ] && [ "$got_u" = "$expect" ] \
        && [ "$got_e" = "$expect" ] && [ "$got_i" = "$expect" ]; then
        printf '  OK   %s -> %s\n' "$desc" "$got_p"
        pass=$((pass + 1))
    else
        printf '  FAIL %s -> 期望 [%s] postinst=[%s] uninstall=[%s] extra-ime=[%s] install=[%s]\n' \
            "$desc" "$expect" "$got_p" "$got_u" "$got_e" "$got_i"
        fail=$((fail + 1))
    fi
}

xdg_home=$tmp/xdg-home
mkdir -p "$xdg_home"
check_config_home "没有 XDG 变量时用 ~/.config" "$xdg_home/.config" "$xdg_home"

# ~/.profile 里的 export XDG_CONFIG_HOME
printf 'export XDG_CONFIG_HOME=%s/profile-cfg\n' "$xdg_home" >"$xdg_home/.profile"
check_config_home "~/.profile 里的 export XDG_CONFIG_HOME" "$xdg_home/profile-cfg" "$xdg_home"
rm -f "$xdg_home/.profile"

# ~/.bashrc 里的裸赋值
printf 'XDG_CONFIG_HOME=%s/bashrc-cfg\n' "$xdg_home" >"$xdg_home/.bashrc"
check_config_home "~/.bashrc 里的 XDG_CONFIG_HOME" "$xdg_home/bashrc-cfg" "$xdg_home"
rm -f "$xdg_home/.bashrc"

# ~/.pam_environment（Debian/Ubuntu 默认会读它）
mkdir -p "$xdg_home"
printf 'XDG_CONFIG_HOME=%s/pam-cfg\n' "$xdg_home" >"$xdg_home/.pam_environment"
check_config_home "~/.pam_environment 里的 XDG_CONFIG_HOME" "$xdg_home/pam-cfg" "$xdg_home"
rm -f "$xdg_home/.pam_environment"

# ~/.config/environment（systemd 用户级）
mkdir -p "$xdg_home/.config"
printf 'XDG_CONFIG_HOME=%s/env-cfg\n' "$xdg_home" >"$xdg_home/.config/environment"
check_config_home "~/.config/environment 里的 XDG_CONFIG_HOME" "$xdg_home/env-cfg" "$xdg_home"
rm -f "$xdg_home/.config/environment"

# 显式注入优先于一切
out_p=$(RIMESYNC_TARGET_CONFIG_HOME=/explicit/cfg run_in "$tmp/p" resolve_config_home "$xdg_home" | tr -d '\n')
out_u=$(RIMESYNC_TARGET_CONFIG_HOME=/explicit/cfg run_in "$tmp/u" resolve_config_home "$xdg_home" | tr -d '\n')
if [ "$out_p" = "/explicit/cfg" ] && [ "$out_u" = "/explicit/cfg" ]; then
    printf '  OK   显式注入的 RIMESYNC_TARGET_CONFIG_HOME 优先 -> %s\n' "$out_p"
    pass=$((pass + 1))
else
    printf '  FAIL 显式注入优先 -> postinst=[%s] uninstall=[%s]\n' "$out_p" "$out_u"
    fail=$((fail + 1))
fi

# XDG 生效时，rime.conf 也要从 XDG 配置根下读（否则 UserDataDirPath 读不到）
xdg_cfg=$tmp/xdg-cfg
mkdir -p "$xdg_cfg/fcitx5/conf"
printf 'UserDataDirPath=/tmp/from-xdg\n' >"$xdg_cfg/fcitx5/conf/rime.conf"
xdg_user=$tmp/xdg-user
mkdir -p "$xdg_user"
printf 'XDG_CONFIG_HOME=%s\n' "$xdg_cfg" >"$xdg_user/.profile"
check "XDG 配置根下的 rime.conf 生效" "/tmp/from-xdg" "$xdg_user"
rm -f "$xdg_user/.profile"

# ---- CLI 优先路径：rime-sync-cli detect --json 给的权威值应被采用 ----
cli_p=$(
    PATH="$tmp/bin2:$PATH"
    . "$tmp/p/config.sh"
    . "$tmp/p/parse.sh"
    . "$tmp/p/cli.sh"
    . "$tmp/p/resolve.sh"
    resolve_rime_user_dir "$home" | tr -d '\n'
)
cli_u=$(
    PATH="$tmp/bin2:$PATH"
    . "$tmp/u/config.sh"
    . "$tmp/u/parse.sh"
    . "$tmp/u/cli.sh"
    . "$tmp/u/resolve.sh"
    rime_user_dir "$home" | tr -d '\n'
)
cli_e=$(
    PATH="$tmp/bin2:$PATH"
    . "$tmp/e/config.sh"
    . "$tmp/e/parse.sh"
    . "$tmp/e/cli.sh"
    . "$tmp/e/resolve.sh"
    resolve_rime_user_dir "$home" | tr -d '\n'
)
cli_i=$(
    PATH="$tmp/bin2:$PATH"
    . "$tmp/i/config.sh"
    . "$tmp/i/parse.sh"
    . "$tmp/i/cli.sh"
    . "$tmp/i/resolve.sh"
    resolve_rime_user_dir "$home" | tr -d '\n'
)
if [ "$cli_p" = "/cli/authoritative" ] && [ "$cli_u" = "/cli/authoritative" ] \
    && [ "$cli_e" = "/cli/authoritative" ] && [ "$cli_i" = "/cli/authoritative" ]; then
    printf '  OK   CLI 权威值优先 -> %s\n' "$cli_p"
    pass=$((pass + 1))
else
    printf '  FAIL CLI 权威值优先 -> postinst=[%s] uninstall=[%s] extra-ime=[%s] install=[%s]\n' \
        "$cli_p" "$cli_u" "$cli_e" "$cli_i"
    fail=$((fail + 1))
fi

# 三个脚本的 resolve_config_home 必须**逐字一致**（只看缩进差异）：
# 少改一个就会导致安装器与卸载器算到不同的配置目录。
# 比代码行（剔除注释行与空行；注释文字允许各脚本写得略有不同）
sig_of() { # $1=抽取文件
    sed -e 's/^[[:space:]]*//' -e '/^$/d' -e 's/[[:space:]]*#.*$//' -e '/^$/d' "$1" |
        sed -e 's/[[:space:]]*$//'
}
sig_p=$(sig_of "$tmp/postinst_config.sh")
sig_u=$(sig_of "$tmp/uninst_config.sh")
sig_e=$(sig_of "$tmp/extra_config.sh")
sig_i=$(sig_of "$tmp/inst_config.sh")
if [ "$sig_p" = "$sig_u" ] && [ "$sig_p" = "$sig_e" ] && [ "$sig_p" = "$sig_i" ]; then
    printf '  OK   四个脚本的 resolve_config_home 逐字一致\n'
    pass=$((pass + 1))
else
    printf '  FAIL 四个脚本的 resolve_config_home 不一致\n'
    fail=$((fail + 1))
fi

# install.sh（.run 离线安装器入口）与 postinst 的三个解析函数必须**逐字一致**：
# 离线安装器以前硬编码 ~/.local/share/fcitx5/rime，把配置写到了 fcitx5 根本不读的
# 目录；两者不一致就会重演同一个缺陷。
# 说明：uninstall.sh 的对应函数改过名字（rime_user_dir）且有一处引号差异，
# 所以这里只锁 install.sh ↔ postinst 这一对（install.sh 的副本就是从 postinst 复制的）。
sig_same() { # $1=描述 $2=文件A $3=文件B
    local desc=$1 a b
    a=$(sig_of "$2")
    b=$(sig_of "$3")
    if [ "$a" = "$b" ]; then
        printf '  OK   %s 逐字一致\n' "$desc"
        pass=$((pass + 1))
    else
        printf '  FAIL %s 不一致（%s vs %s）\n' "$desc" "$2" "$3"
        diff -u "$2" "$3" | sed -n '1,40p'
        fail=$((fail + 1))
    fi
}
sig_same "install.sh 与 postinst 的 resolve_config_home" \
    "$tmp/postinst_config.sh" "$tmp/inst_config.sh"
sig_same "install.sh 与 postinst 的 parse_rime_user_data_dir" \
    "$tmp/postinst_parse.sh" "$tmp/inst_parse.sh"
sig_same "install.sh 与 postinst 的 rime_user_dir_from_cli" \
    "$tmp/postinst_cli.sh" "$tmp/inst_cli.sh"
sig_same "install.sh 与 postinst 的 resolve_rime_user_dir" \
    "$tmp/postinst_resolve.sh" "$tmp/inst_resolve.sh"

# install-extra-ime.sh（GUI 通过 pkexec 调的额外输入法安装器）同样必须与 postinst
# 逐字一致：它以前直接取 CLI 的 detect --json 的 path 字段，没有「known: 修正」，
# 于是 rime.conf 里的 UserDataDirPath 指向的目录还不存在时，本脚本与 postinst
# 会算出**不同的** Rime 目录，受管 custom.yaml 与预编译产物被写进没人读的目录。
sig_same "install-extra-ime.sh 与 postinst 的 resolve_config_home" \
    "$tmp/postinst_config.sh" "$tmp/extra_config.sh"
sig_same "install-extra-ime.sh 与 postinst 的 parse_rime_user_data_dir" \
    "$tmp/postinst_parse.sh" "$tmp/extra_parse.sh"
sig_same "install-extra-ime.sh 与 postinst 的 rime_user_dir_from_cli" \
    "$tmp/postinst_cli.sh" "$tmp/extra_cli.sh"
sig_same "install-extra-ime.sh 与 postinst 的 resolve_rime_user_dir" \
    "$tmp/postinst_resolve.sh" "$tmp/extra_resolve.sh"

# install-extra-ime.sh 必须真的走 resolve_rime_user_dir（而不是自己解析 CLI 的 path）。
if grep -q 'resolve_rime_user_dir "\$TARGET_HOME"' "$extra"; then
    printf '  OK   install-extra-ime.sh 使用 resolve_rime_user_dir 解析用户目录\n'
    pass=$((pass + 1))
else
    printf '  FAIL install-extra-ime.sh 没有用 resolve_rime_user_dir\n'
    fail=$((fail + 1))
fi

# install.sh 不得再硬编码默认 Rime 用户目录（缺陷回归护栏）：
# 硬编码会把配置写进 fcitx5 根本不读的目录。
if grep -q 'RIME_USER_DIR="\$TARGET_HOME/\.local/share/fcitx5/rime"' "$installer"; then
    printf '  FAIL install.sh 仍硬编码默认 Rime 用户目录\n'
    fail=$((fail + 1))
else
    printf '  OK   install.sh 不再硬编码默认 Rime 用户目录\n'
    pass=$((pass + 1))
fi
if grep -q 'resolve_rime_user_dir' "$installer"; then
    printf '  OK   install.sh 使用 resolve_rime_user_dir 解析用户目录\n'
    pass=$((pass + 1))
else
    printf '  FAIL install.sh 没有 resolve_rime_user_dir\n'
    fail=$((fail + 1))
fi

printf '\n通过 %d 项，失败 %d 项\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
