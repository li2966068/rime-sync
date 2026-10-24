#!/bin/bash
# Runs INSIDE the target container (see install-matrix.sh on the host).
#
#   install-scenario.sh <case>
#
# Mounts:
#   /bundle      current offline bundle  (install.sh + <distro>/{ime,sync}.deb + vendor/)
#   /bundle-old  previous release bundle (upgrade case only)
#   /fixtures    writable scratch dir
#
# Cases:
#   normal            正常离线安装（单架构，无历史状态）
#   broken-unrelated  系统里有一个与本安装无关的半配置包，installer 不许中止
#   multiarch         i386 已启用 + 同包多架构版本不一致（libbrotli1），installer 不许升级已有库
#   upgrade           旧版已装（IME/sync）→ 直接 dpkg -i 覆盖升级，不要求先卸载
#   single-arch       没有 i386 的纯单架构系统（debian-12）仍要装成功
set -u

CASE=${1:?usage: install-scenario.sh <case>}
BUNDLE=${BUNDLE:-/bundle}
BUNDLE_OLD=${BUNDLE_OLD:-/bundle-old}
FIX=${FIXTURES:-/fixtures}   # 只读挂载：宿主机构建好的测试包（/tmp 上构建，权限正常）

say() { printf '[%s] %s\n' "$CASE" "$*"; }
ok()  { printf '  ok: %s\n' "$*"; }
fail() { printf 'FAIL[%s]: %s\n' "$CASE" "$*"; exit 1; }

pkg_status()  { dpkg-query -W -f='${Status}' "$1" 2>/dev/null || true; }
pkg_version() { dpkg-query -W -f='${Version}' "$1" 2>/dev/null || true; }

assert_installed() {
  local p=$1 st
  st=$(pkg_status "$p")
  [ "$st" = "install ok installed" ] || fail "$p 状态='$st'（期望 install ok installed）"
  ok "$p $(pkg_version "$p") 已安装并配置"
}
assert_absent() { [ -z "$(pkg_status "$1")" ] || fail "$1 不该存在：$(pkg_status "$1")"; ok "$1 不存在"; }

run_installer() { # $@ = extra args
  local rc
  say "运行：bash $BUNDLE/install.sh $*"
  bash "$BUNDLE/install.sh" "$@" >/tmp/installer.out 2>&1
  rc=$?
  grep -E '^\[rime-sync\]' /tmp/installer.out | tail -n 28
  [ "$rc" -eq 0 ] || { echo '---- installer 输出尾部 ----'; tail -n 60 /tmp/installer.out; fail "installer 退出码 $rc"; }
  ok "installer 退出码 0"
}

run_installer_expect_fail() { # $1 = 期望的退出码，其余是参数
  local want=$1 rc; shift
  say "运行（期望退出码 $want）：bash $BUNDLE/install.sh $*"
  bash "$BUNDLE/install.sh" "$@" >/tmp/installer.out 2>&1
  rc=$?
  grep -E '^\[rime-sync\]' /tmp/installer.out | tail -n 80
  [ "$rc" -eq "$want" ] || { echo '---- installer 输出尾部 ----'; tail -n 60 /tmp/installer.out; fail "installer 退出码 $rc（期望 $want）"; }
  # 完整打印安装器给出的诊断块（截断的 tail 会漏掉前面的 dpkg 原文与多架构清单）
  sed -n '/离线依赖配置失败/,/本次离线包改动过的系统包/p' /var/log/rime-sync/install.log
  ok "installer 退出码 $rc（符合预期，非 0 且措辞明确）"
}

deb_files() { # $1 = .deb -> 文件路径列表（去掉目录）
  dpkg-deb -c "$1" | awk '{print $6}' | sed 's#^\./##' | grep -v '/$' | sort
}

setup_user_data() {
  useradd -m -s /bin/bash tester 2>/dev/null || true
  mkdir -p /home/tester/.local/share/fcitx5/rime /home/tester/.config/com.rimesync.app /home/tester/.config/rime-sync
  printf 'patch:\n  schema_list:\n    - luna_pinyin\n' >/home/tester/.local/share/fcitx5/rime/default.custom.yaml
  printf 'user dict line 1\nuser dict line 2\n' >/home/tester/.local/share/fcitx5/rime/luna_pinyin.userdb.txt
  printf '{"device_id":"test-device","last_sync":"2026-01-01T00:00:00Z"}\n' >/home/tester/.config/com.rimesync.app/config.json
  printf '{"cursor":42}\n' >/home/tester/.config/rime-sync/state.json
  chown -R tester:tester /home/tester 2>/dev/null || true
  ( cd /home/tester && find . -type f -print0 | sort -z | xargs -0 sha256sum ) >/tmp/userdata.before
}

assert_user_data_intact() {
  ( cd /home/tester && find . -type f -print0 | sort -z | xargs -0 sha256sum ) >/tmp/userdata.after
  diff -u /tmp/userdata.before /tmp/userdata.after || fail "用户目录内容被安装器改动"
  ok "用户目录未变：~/.local/share/fcitx5/rime、~/.config/com.rimesync.app、~/.config/rime-sync（$(wc -l </tmp/userdata.before) 个文件）"
}

assert_rime_data() {
  local n
  n=$(find /usr/share/rime-data -maxdepth 1 -type f | wc -l)
  [ "$n" -gt 10 ] || fail "/usr/share/rime-data 只有 $n 个文件"
  ok "/usr/share/rime-data $n 个文件"
  if command -v rime_deployer >/dev/null 2>&1; then
    rm -rf /tmp/deployer-user
    rime_deployer --build /tmp/deployer-user /usr/share/rime-data >/tmp/deployer.log 2>&1 \
      || { tail -n 20 /tmp/deployer.log; fail "rime_deployer --build 失败"; }
    ok "rime_deployer --build 成功（$(find /tmp/deployer-user/build -name '*.bin' | wc -l) 个 .bin）"
  fi
}

assert_no_system_wide_configure() {
  # 旧实现把无关包的状态也算进来；这里确认脚本里不再有 "dpkg --configure -a" 这条命令
  # （注释里提到它是可以的，所以要按命令行首匹配）。
  if grep -qE '^[[:space:]]*dpkg[[:space:]]+--configure[[:space:]]+-a' "$BUNDLE/install.sh"; then
    fail "install.sh 里仍有 dpkg --configure -a 命令"
  fi
  ok "install.sh 不再调用 dpkg --configure -a"
}

# ------------------------------------------------------------------ cases
case_normal() {
  run_installer
  assert_installed rime-inputmethod
  assert_installed rime-sync
  assert_rime_data
  assert_no_system_wide_configure
  [ -z "$(dpkg --audit 2>&1)" ] || { dpkg --audit; fail "dpkg --audit 报告异常"; }
  ok "dpkg --audit 干净"
  # 离线包不许升级系统已有包
  local up
  up=$(grep -cE ' upgrade ' /var/log/dpkg.log 2>/dev/null || true)
  [ "${up:-0}" -eq 0 ] || { grep -E ' upgrade ' /var/log/dpkg.log | head -5; fail "dpkg 日志里有 $up 次 upgrade"; }
  ok "没有任何 upgrade（只补缺失包）"
}

case_broken_unrelated() {
  [ -f "$FIX/zz-rime-test-broken.deb" ] || fail "缺少 /fixtures/zz-rime-test-broken.deb"
  dpkg -i "$FIX/zz-rime-test-broken.deb" >/tmp/broken.out 2>&1 || true
  say "无关的半配置包状态：$(pkg_status zz-rime-test-broken)"
  case "$(pkg_status zz-rime-test-broken)" in
    *"half-configured"*|*"half-installed"*|*"unpacked"*|*"triggers-pending"*|*"triggers-awaited"*) : ;;
    *) cat /tmp/broken.out; fail "测试前置失败：zz-rime-test-broken 没进半配置状态" ;;
  esac

  run_installer                      # 旧实现会在这里 fail "配置离线依赖失败"
  assert_installed rime-inputmethod
  assert_installed rime-sync
  assert_rime_data
  # 无关包保持原样（我们不碰它，也不因它中止）
  case "$(pkg_status zz-rime-test-broken)" in
    *"half-configured"*|*"half-installed"*) ok "无关包仍未被配置（installer 没有替系统做主）" ;;
    *) fail "installer 改动了无关包：$(pkg_status zz-rime-test-broken)" ;;
  esac
  if grep -q 'zz-rime-test-broken' /var/log/rime-sync/install.log 2>/dev/null; then
    fail "install.log 里提到无关包，说明判定仍受它影响"
  fi
  ok "install.log 没有提及无关包"
}

case_multiarch() {
  dpkg --add-architecture i386
  [ "$(dpkg --print-foreign-architectures)" = "i386" ] || fail "i386 架构未启用"

  local amd64_ver vendor_ver fixture=""
  for f in "$FIX"/libbrotli1_*_amd64.deb; do [ -f "$f" ] && fixture=$f; done
  if [ -z "$(pkg_version libbrotli1:amd64)" ]; then
    [ -n "$fixture" ] || fail "缺少 /fixtures/libbrotli1_*_amd64.deb"
    dpkg -i "$fixture" >/tmp/brotli-amd64.out 2>&1 || { cat /tmp/brotli-amd64.out; fail "libbrotli1:amd64 基线安装失败"; }
  fi
  amd64_ver=$(pkg_version libbrotli1:amd64)
  [ -n "$amd64_ver" ] || fail "系统里没有 libbrotli1:amd64"
  vendor_ver=$(dpkg-deb -f "$BUNDLE"/debian-13/vendor/libbrotli1_*.deb Version)
  say "系统 libbrotli1:amd64=$amd64_ver；离线包 libbrotli1=$vendor_ver"
  [ "$amd64_ver" != "$vendor_ver" ] || fail "测试前置无效：离线包版本与系统相同，不会产生升级压力"
  [ "$(dpkg --print-foreign-architectures)" = "i386" ] || fail "i386 架构未启用"

  # 同版本 i386 占位包：系统此时是"两个架构一致"的健康状态，
  # 旧的安装器会把 amd64 升级到 $vendor_ver，于是 i386 版本落后 → configure 全链条失败。
  [ -f "$FIX/libbrotli1_i386.deb" ] || fail "缺少 /fixtures/libbrotli1_i386.deb"
  [ "$(dpkg-deb -f "$FIX/libbrotli1_i386.deb" Version)" = "$amd64_ver" ] \
    || fail "i386 fixture 版本与 amd64 不一致，测试前置无效"
  dpkg -i "$FIX/libbrotli1_i386.deb" >/tmp/i386.out 2>&1 || { cat /tmp/i386.out; fail "i386 测试包安装失败"; }
  say "多架构前置：libbrotli1:amd64=$(pkg_version libbrotli1:amd64) i386=$(pkg_version libbrotli1:i386)"

  run_installer
  assert_installed rime-inputmethod
  assert_installed rime-sync
  assert_rime_data

  [ "$(pkg_version libbrotli1:amd64)" = "$amd64_ver" ] \
    || fail "libbrotli1:amd64 被升级到 $(pkg_version libbrotli1:amd64)（应为 $amd64_ver）"
  [ "$(pkg_status libbrotli1:amd64)" = "install ok installed" ] \
    || fail "libbrotli1:amd64 状态=$(pkg_status libbrotli1:amd64)"
  ok "libbrotli1:amd64 仍是 $amd64_ver（未被升级）"
  # 离线库里其它"系统已有"的库同样不许被升级
  local up
  up=$(grep -cE ' upgrade ' /var/log/dpkg.log 2>/dev/null || true)
  [ "${up:-0}" -eq 0 ] || { grep -E ' upgrade ' /var/log/dpkg.log | head -5; fail "$up 个系统包被升级"; }
  ok "dpkg 日志 0 次 upgrade（系统库一个都没动）"
  if grep -q '检测到外部架构' /var/log/rime-sync/install.log; then
    ok "installer 识别到多架构系统并采用保守策略"
  else
    fail "install.log 里没有多架构提示"
  fi
}

case_upgrade() {
  setup_user_data
  say "步骤 A：先装旧版（$(basename "$BUNDLE_OLD"/debian-13/rime-inputmethod_*.deb) / $(basename "$BUNDLE_OLD"/debian-13/rime-sync_*.deb)）"
  run_installer --payload "$BUNDLE_OLD"
  assert_installed rime-inputmethod
  assert_installed rime-sync
  local old_ime_ver old_sync_ver new_ime_ver new_sync_ver
  old_ime_ver=$(pkg_version rime-inputmethod)
  old_sync_ver=$(pkg_version rime-sync)
  new_ime_ver=$(dpkg-deb -f "$BUNDLE"/debian-13/rime-inputmethod_*.deb Version)
  new_sync_ver=$(dpkg-deb -f "$BUNDLE"/debian-13/rime-sync_*.deb Version)
  say "旧版：rime-inputmethod=$old_ime_ver rime-sync=$old_sync_ver"
  say "新版：rime-inputmethod=$new_ime_ver rime-sync=$new_sync_ver"
  dpkg --compare-versions "$new_ime_ver" gt "$old_ime_ver" || fail "测试前置无效：新 IME 版本不高于旧版"
  dpkg --compare-versions "$new_sync_ver" gt "$old_sync_ver" || fail "测试前置无效：新 sync 版本不高于旧版"

  say "步骤 B：直接重跑同一个 installer（不卸载、不 purge）"
  run_installer
  assert_installed rime-inputmethod
  assert_installed rime-sync
  [ "$(pkg_version rime-inputmethod)" = "$new_ime_ver" ] || fail "IME 版本没升到 $new_ime_ver（$(pkg_version rime-inputmethod)）"
  [ "$(pkg_version rime-sync)" = "$new_sync_ver" ] || fail "sync 版本没升到 $new_sync_ver（$(pkg_version rime-sync)）"
  ok "版本已升级：rime-inputmethod $old_ime_ver -> $new_ime_ver，rime-sync $old_sync_ver -> $new_sync_ver"

  awk -v o="$old_ime_ver" -v n="$new_ime_ver" \
      '$3 == "upgrade" && $4 ~ /^rime-inputmethod(:|$)/ && $5 == o && $6 == n { f = 1 } END { exit !f }' \
      /var/log/dpkg.log \
    || { grep 'rime-inputmethod' /var/log/dpkg.log | tail -5; fail "dpkg.log 里没有原地 upgrade 记录"; }
  awk -v o="$old_sync_ver" -v n="$new_sync_ver" \
      '$3 == "upgrade" && $4 ~ /^rime-sync(:|$)/ && $5 == o && $6 == n { f = 1 } END { exit !f }' \
      /var/log/dpkg.log \
    || { grep 'rime-sync' /var/log/dpkg.log | tail -5; fail "rime-sync 没有原地 upgrade 记录"; }
  ok "dpkg.log 记录为 upgrade（不是 remove+install）"

  local old_deb new_deb removed=0
  for pair in "rime-inputmethod" "rime-sync"; do
    if [ "$pair" = "rime-inputmethod" ]; then
      old_deb=$(ls "$BUNDLE_OLD"/debian-13/rime-inputmethod_*.deb)
      new_deb=$(ls "$BUNDLE"/debian-13/rime-inputmethod_*.deb)
    else
      old_deb=$(ls "$BUNDLE_OLD"/debian-13/rime-sync_*.deb)
      new_deb=$(ls "$BUNDLE"/debian-13/rime-sync_*.deb)
    fi
    deb_files "$old_deb" >/tmp/old.files
    deb_files "$new_deb" >/tmp/new.files
    comm -23 /tmp/old.files /tmp/new.files >/tmp/removed.files || true
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      removed=$((removed + 1))
      [ -e "/$f" ] && fail "旧版已删除的路径仍在磁盘上：/$f"
    done </tmp/removed.files
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      [ -e "/$f" ] || fail "新版应该装上的文件缺失：/$f"
    done </tmp/new.files
    ok "$(basename "$new_deb")：$(wc -l </tmp/new.files) 个文件全部就位，旧版独有路径 $(wc -l </tmp/removed.files) 个已清理"
  done
  [ "${removed:-0}" -ge 0 ] || true

  local stale
  stale=$(find /usr /etc -name '*.dpkg-old' -o -name '*.dpkg-dist' -o -name '*.dpkg-new' 2>/dev/null | head -5)
  [ -z "$stale" ] || fail "升级留下了 conffile 残留：$stale"
  ok "没有 .dpkg-old/.dpkg-dist/.dpkg-new 残留"

  assert_user_data_intact
  assert_rime_data
}

# 用户现场复现：上一次安装把 libbrotli1:amd64 解包成了新版本（iU），i386 还停在旧版本。
# 这是"系统已经有病"的状态——安装器不能再装作没事，但也必须说清楚：
# 错的是系统已有包的配置，不是 rime-sync；并给出可执行的修复命令。
case_prebroken_multiarch() {
  dpkg --add-architecture i386
  local amd64_ver fixture=""
  for f in "$FIX"/libbrotli1_*_amd64.deb; do [ -f "$f" ] && fixture=$f; done
  [ -n "$fixture" ] || fail "缺少 /fixtures/libbrotli1_*_amd64.deb"
  dpkg -i "$fixture" >/tmp/brotli-amd64.out 2>&1 || { cat /tmp/brotli-amd64.out; fail "libbrotli1:amd64 基线安装失败"; }
  amd64_ver=$(pkg_version libbrotli1:amd64)
  [ -f "$FIX/libbrotli1_i386.deb" ] || fail "缺少 /fixtures/libbrotli1_i386.deb"
  [ "$(dpkg-deb -f "$FIX/libbrotli1_i386.deb" Version)" = "$amd64_ver" ] \
    || fail "i386 fixture 版本与 amd64 不一致，测试前置无效"
  dpkg -i "$FIX/libbrotli1_i386.deb" >/tmp/i386.out 2>&1 || { cat /tmp/i386.out; fail "i386 测试包安装失败"; }
  say "健康前置：libbrotli1 amd64=$amd64_ver i386=$amd64_ver"

  # 复刻上一版安装器造成的半配置状态：只解包、没配置
  dpkg --unpack "$BUNDLE"/debian-13/vendor/libbrotli1_*.deb >/tmp/unpack.out 2>&1 \
    || { cat /tmp/unpack.out; fail "复刻半配置状态失败"; }
  say "复刻后：amd64=$(pkg_version libbrotli1:amd64) 状态=$(pkg_status libbrotli1:amd64) / i386=$(pkg_version libbrotli1:i386)"

  local before_ver before_stat logmark
  before_ver=$(pkg_version libbrotli1:amd64)
  before_stat=$(pkg_status libbrotli1:amd64)
  say "安装器运行前 libbrotli1："
  dpkg-query -W -f='  ${Package}:${Architecture} ${Version} [${Status}]\n' libbrotli1
  logmark=$(wc -l </var/log/dpkg.log)

  run_installer_expect_fail 3

  say "安装器运行后 libbrotli1："
  dpkg-query -W -f='  ${Package}:${Architecture} ${Version} [${Status}]\n' libbrotli1
  say "install.log 里与 libbrotli1 相关的行："
  grep 'libbrotli1' /var/log/rime-sync/install.log | head -n 8 || true

  local pat
  for pat in \
    "cannot be configured because libbrotli1:i386 is at a different version" \
    "检测到多架构版本不一致" \
    "sudo apt --fix-broken install" \
    "sudo dpkg --remove-architecture" \
    "失败的是【系统已安装包的配置状态】" \
    "本次离线包改动过的系统包" \
    "不是 rime-sync 的安装内容有问题"; do
    grep -qF -- "$pat" /var/log/rime-sync/install.log || fail "诊断输出缺少：$pat"
  done
  ok "诊断齐备：dpkg 原文 + 多架构说明 + 修复命令 + 责任归属"
  assert_absent rime-inputmethod
  # 安装器没有再动那个包：版本与状态都是上一次安装留下的原样（unpacked = iU）
  [ "$(pkg_version libbrotli1:amd64)" = "$before_ver" ] \
    || fail "libbrotli1:amd64 版本被改成 $(pkg_version libbrotli1:amd64)（应为 $before_ver）"
  [ "$(pkg_status libbrotli1:amd64)" = "$before_stat" ] \
    || fail "libbrotli1:amd64 状态被改成 $(pkg_status libbrotli1:amd64)（应为 $before_stat）"
  ok "没有再次改动 libbrotli1:amd64（仍为 $before_ver / $before_stat）"
  # 安装器识别出"别的架构版本不同"并明确跳过
  grep -q '跳过 libbrotli1:amd64' /var/log/rime-sync/install.log \
    || fail "install.log 里没有多架构保护跳过记录"
  ok "install.log 记录：跳过 libbrotli1:amd64（多架构保护）"
  # 本次运行没有再解包/升级它（dpkg.log 里本次新增的行）
  if tail -n +$((logmark + 1)) /var/log/dpkg.log | grep -qE ' (unpack|upgrade|install) libbrotli1'; then
    tail -n +$((logmark + 1)) /var/log/dpkg.log | grep libbrotli1
    fail "本次安装又解包/升级了 libbrotli1"
  fi
  ok "本次运行没有解包/升级 libbrotli1（dpkg.log 无 unpack/upgrade/install 动作）"
}

case_single_arch() {
  [ -z "$(dpkg --print-foreign-architectures)" ] || fail "本用例要求没有外部架构，实际：$(dpkg --print-foreign-architectures)"
  say "外部架构：<空>"
  run_installer
  assert_installed rime-inputmethod
  assert_installed rime-sync
  assert_rime_data
  if grep -q '检测到外部架构' /var/log/rime-sync/install.log; then
    fail "单架构系统不该报多架构提示"
  fi
  ok "单架构系统路径正常（没有多架构分支介入）"
}

say "os=$(. /etc/os-release; echo "$PRETTY_NAME") arch=$(dpkg --print-architecture) foreign=[$(dpkg --print-foreign-architectures | tr '\n' ' ')]"
case "$CASE" in
  normal)            case_normal ;;
  broken-unrelated)  case_broken_unrelated ;;
  multiarch)         case_multiarch ;;
  prebroken-multiarch) case_prebroken_multiarch ;;
  upgrade)           case_upgrade ;;
  single-arch)       case_single_arch ;;
  *) fail "未知用例 $CASE" ;;
esac
say "PASS: $CASE"
