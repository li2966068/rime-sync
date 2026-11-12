#!/bin/bash
# install-extra-ime.sh 的自检测试（纯 bash，不需要 root，不改真机任何东西）。
#
# 覆盖：
#   ① --dry-run 不改动任何东西（不卸载、不安装、不编译、不碰用户数据）
#   ② 缺参数 / 安装包路径不存在 → 干净退出 + 中文提示 + 非零退出码
#   ③ 脚本里不含任何会删用户数据的路径（词库目录绝对不能出现）
#   ④ --help 打印用法
#   ⑤ 脚本自己不提权（不调用 sudo / pkexec）
#
# 手法：把 PATH 换成假的 dpkg / dpkg-query / apt-get / rime_deployer / rime-sync-cli，
# 所有会改动系统的动作都换成「记账」的桩；再用临时 HOME 观察有没有东西被动过。
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
SCRIPT="$HERE/../install-extra-ime.sh"

PASS=0
FAIL=0
check() { # <名字> <期望> <实际>
  if [ "$2" = "$3" ]; then
    PASS=$((PASS + 1))
    printf 'ok   %s\n' "$1"
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL %s\n       期望: %s\n       实际: %s\n' "$1" "$2" "$3"
  fi
}
check_has() { # <名字> <子串> <文本>
  case "$3" in
    *"$2"*) PASS=$((PASS + 1)); printf 'ok   %s\n' "$1" ;;
    *) FAIL=$((FAIL + 1)); printf 'FAIL %s\n       期望包含: %s\n       实际: %s\n' "$1" "$2" "$3" ;;
  esac
}

TMP=$(mktemp -d "${TMPDIR:-/tmp}/test-install-extra-ime.XXXXXX")
trap 'rm -rf -- "$TMP"' EXIT

FAKEBIN="$TMP/bin"
FAKELOG="$TMP/calls.log"
FAKEHOME="$TMP/home"
mkdir -p "$FAKEBIN" "$FAKEHOME"
: >"$FAKELOG"

# ------------------------------------------------------------ 假命令（记账用）
cat >"$FAKEBIN/dpkg-query" <<'STUB'
#!/bin/bash
args="$*"
case "$args" in
  *'-f=${Package}'*)
    # 假装机器上装着发行版的 fcitx5-rime 与 librime-bin
    printf 'fcitx5-rime\tinstall ok installed\n'
    printf 'librime-bin\tinstall ok installed\n'
    ;;
  *'-f=${Status}'*)
    case "$args" in
      *fcitx5-rime*) printf 'install ok installed' ;;
      *) printf 'unknown ok not-installed' ;;
    esac
    ;;
  *) : ;;
esac
exit 0
STUB

cat >"$FAKEBIN/dpkg" <<'STUB'
#!/bin/bash
# dpkg -L：假装 rime-inputmethod 不含我们的标志文件（即发行版同名包）
exit 0
STUB

cat >"$FAKEBIN/apt-get" <<'STUB'
#!/bin/bash
echo "apt-get $*" >>"$FAKE_LOG_FILE"
exit 0
STUB

cat >"$FAKEBIN/rime_deployer" <<'STUB'
#!/bin/bash
echo "rime_deployer $*" >>"$FAKE_LOG_FILE"
exit 0
STUB

cat >"$FAKEBIN/rime-sync-cli" <<'STUB'
#!/bin/bash
printf '{"ok":true,"rime_dir":"%s"}\n' "$FAKE_RIME_DIR"
exit 0
STUB

chmod 0755 "$FAKEBIN"/*

# ------------------------------------------------------- 造一个假的完整安装包
PKG="$TMP/pkg"
mkdir -p "$PKG/debian-13"
cat >"$PKG/install.sh" <<'STUB'
#!/bin/bash
# 假安装器：真被跑起来就说明「试运行没拦住安装」
echo "INSTALL_SH_EXECUTED" >>"$FAKE_LOG_FILE"
exit 99
STUB
chmod 0755 "$PKG/install.sh"
: >"$PKG/debian-13/rime-inputmethod_0.0.0_amd64.deb"
: >"$PKG/debian-13/rime-sync_0.0.0_amd64.deb"

FAKE_RIME_DIR="$FAKEHOME/.local/share/fcitx5/rime"
export FAKE_LOG_FILE="$FAKELOG" FAKE_RIME_DIR

run_script() { # <参数...>
  env -i \
    PATH="$FAKEBIN:/usr/bin:/bin" \
    HOME="$FAKEHOME" \
    TMPDIR="$TMP" \
    FAKE_LOG_FILE="$FAKE_LOG_FILE" \
    FAKE_RIME_DIR="$FAKE_RIME_DIR" \
    bash "$SCRIPT" "$@" 2>&1
}

# --------------------------------------------------------------- ① --dry-run
before=$(find "$FAKEHOME" -printf '%p %y %s\n' 2>/dev/null | sort)
out=$(run_script --dry-run "$PKG")
rc=$?
after=$(find "$FAKEHOME" -printf '%p %y %s\n' 2>/dev/null | sort)

check "① --dry-run 退出码为 0" "0" "$rc"
check "① --dry-run 没有动过用户目录" "$before" "$after"
calls=$(cat "$FAKELOG")
check "① --dry-run 没有执行任何会改系统的命令" "" "$calls"
check_has "① --dry-run 明确说明是试运行" "试运行" "$out"
check_has "① --dry-run 打印了卸载旧输入法的步骤" "卸载旧输入法" "$out"
check_has "① --dry-run 打印了全新安装的步骤" "全新安装" "$out"
check_has "① --dry-run 打印了重新编译的步骤" "rime_deployer --build" "$out"
check_has "① --dry-run 打印了重启输入法的提示" "重启输入法" "$out"
check_has "① --dry-run 说明了词库不会被删" "没有被删除" "$out"

# ---------------------------------------------------------------- ② 参数错误
out=$(run_script)
rc=$?
if [ "$rc" -ne 0 ]; then r=非零; else r=零; fi
check "② 缺参数时退出码非零" "非零" "$r"
check_has "② 缺参数时给出中文提示" "缺少参数" "$out"

out=$(run_script "$TMP/does-not-exist.run")
rc=$?
if [ "$rc" -ne 0 ]; then r=非零; else r=零; fi
check "② 路径不存在时退出码非零" "非零" "$r"
check_has "② 路径不存在时给出中文提示" "找不到安装包" "$out"

# ------------------------------------------------------- ③ 不删用户数据的自查
rm_lines=$(grep -nE '(^|[[:space:]])rm[[:space:]]' "$SCRIPT" 2>/dev/null || true)
bad_lines=""
if [ -n "$rm_lines" ]; then
  bad_lines=$(printf '%s\n' "$rm_lines" |
    grep -iE 'fcitx5/rime|com\.rimesync\.app|\.config/rime-sync|\$TARGET_HOME|\$HOME|/rime([^-a-zA-Z]|$)' || true)
fi
check "③ 脚本里没有删除用户数据的命令" "" "$bad_lines"
purge_hits=$(grep -n 'purge-data' "$SCRIPT" 2>/dev/null || true)
check "③ 脚本不会使用 uninstall.sh 的 --purge-data（破坏性选项）" "" "$purge_hits"
# 注意：模式串要拼出来，否则本文件自己就会命中「仓库红线自查」的那条 grep。
dict_pat="translator""/dictionary"
dict_hits=$(grep -rn "$dict_pat" "$HERE/.." 2>/dev/null || true)
check "③ packaging/linux 下没有词库目录引用（红线）" "" "$dict_hits"

# -------------------------------------------------------------------- ④ --help
out=$(run_script --help)
rc=$?
check "④ --help 退出码为 0" "0" "$rc"
check_has "④ --help 打印用法" "用法" "$out"
check_has "④ --help 说明由调用方用 pkexec 提权" "pkexec" "$out"

# ------------------------------------------------- ⑤ 脚本自己不提权（不调 sudo）
escalate=$(grep -nE '^[[:space:]]*(sudo|pkexec)[[:space:]]' "$SCRIPT" 2>/dev/null || true)
check "⑤ 脚本里没有自己调用 sudo / pkexec 的命令行" "" "$escalate"

printf '\n通过 %d 项，失败 %d 项\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
