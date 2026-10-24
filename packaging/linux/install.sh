#!/bin/bash
# rime-sync 一键离线安装器
#
#   sudo ./install.sh                     # 自动识别发行版并安装
#   sudo ./install.sh --payload DIR       # 指定离线包根目录
#   sudo ./install.sh --uninstall [--yes] [--keep-ime] [--purge-data]
#                                         # 交给统一卸载脚本（默认保留用户数据；--purge-data 才删）
#
# 目录约定（.run 自解压后也是这个结构）：
#   install.sh
#   ubuntu-22.04/{rime-inputmethod_*.deb,rime-sync_*.deb,vendor/*.deb}
#   ubuntu-24.04/...
#   debian-12/...
#   debian-13/...
#
# 行为约定：
#   * 必须先装 rime-inputmethod，再装 rime-sync；任一步失败立即停止并保留日志
#   * 不启动任何应用、不写 autostart
#   * 卸载时默认保留用户数据（~/.local/share/fcitx5/rime、~/.config/com.rimesync.app、
#     ~/.config/rime-sync）；只有显式加 --purge-data 才随卸载一起删除（破坏性、不可恢复）
#   * 离线依赖只补缺失、不升级系统已装的包；失败判定只看本安装器解包的包，不把
#     系统里已有的半配置/多架构不一致算成 rime-sync 的失败
#   * 可重复执行：旧版已装时直接 dpkg -i 覆盖升级，不需要先卸载
#
# 选项：
#   --payload DIR     指定离线包根目录
#   --upgrade-vendor  允许把系统已装（ii）的依赖升级到离线包版本（默认不动系统库）
#   --uninstall       交给统一卸载脚本（--yes/--keep-ime/--purge-data 见下）
set -u

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
PAYLOAD=""
DO_UNINSTALL=0
ASSUME_YES=0
KEEP_IME=0
PURGE_DATA=0
UPGRADE_VENDOR=0
[ "${RIME_SYNC_UPGRADE_VENDOR:-0}" = "1" ] && UPGRADE_VENDOR=1

usage() {
  sed -n '2,/^set -u$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) DO_UNINSTALL=1 ;;
    --payload) shift; PAYLOAD=${1:-} ;;
    --payload=*) PAYLOAD=${1#--payload=} ;;
    --yes|-y) ASSUME_YES=1 ;;
    --keep-ime) KEEP_IME=1 ;;
    --purge-data) PURGE_DATA=1 ;;
    --upgrade-vendor) UPGRADE_VENDOR=1 ;;
    -h|--help) usage ;;
    *) echo "未知参数: $1（--help 查看用法）" >&2; exit 64 ;;
  esac
  shift
done

if [ "$(id -u)" -ne 0 ]; then
  echo "需要 root 权限。请用：sudo $0 $*" >&2
  echo "（安装会写入 /usr 与 /var/lib/dpkg，普通用户无法完成。）" >&2
  exit 1
fi

LOGDIR=/var/log/rime-sync
LOG=$LOGDIR/install.log
STEPS=$LOGDIR/install-steps.log
mkdir -p "$LOGDIR" || { echo "无法创建 $LOGDIR" >&2; exit 1; }
chmod 755 "$LOGDIR" 2>/dev/null || true
# 同时输出到终端和日志；失败时日志完整保留。
exec > >(tee -a "$LOG") 2>&1

say() { printf '[rime-sync] %s\n' "$*"; }
step() {
  local name=$1 line
  line=$(printf '%s %s epoch=%s' "$(date '+%F %T')" "$name" "$(date +%s)")
  printf '%s\n' "$line"
  mkdir -p "$LOGDIR" 2>/dev/null || true
  printf '%s\n' "$line" >>"$STEPS" 2>/dev/null || true
}
fail() {
  say "!! $*"
  say "---- 日志尾部（$LOG）----"
  tail -n 40 "$LOG" 2>/dev/null || true
  say "安装已停止。完整日志：$LOG"
  exit 3
}

. /etc/os-release 2>/dev/null || true
OS_ID=${ID:-unknown}
OS_VER=${VERSION_ID:-unknown}
OS_PRETTY=${PRETTY_NAME:-$OS_ID $OS_VER}

# ------------------------------------------------------------------ uninstall
if [ "$DO_UNINSTALL" -eq 1 ]; then
  # 卸载逻辑只有一份：/usr/lib/rime-sync/uninstall.sh（随 rime-sync 包安装）；
  # 离线包/ .run 自解压目录里也带同名脚本，未装或已半卸时用同目录那一份。
  UNINSTALLER=/usr/lib/rime-sync/uninstall.sh
  [ -f "$UNINSTALLER" ] || UNINSTALLER=$SCRIPT_DIR/uninstall.sh
  if [ ! -f "$UNINSTALLER" ]; then
    say "找不到统一卸载脚本：/usr/lib/rime-sync/uninstall.sh 与 $SCRIPT_DIR/uninstall.sh 都不存在"
    say "可改用：apt remove rime-sync；或 dpkg -r rime-inputmethod"
    exit 4
  fi
  uninstall_args=()
  [ "$ASSUME_YES" -eq 1 ] && uninstall_args+=(--yes)
  [ "$KEEP_IME" -eq 1 ] && uninstall_args+=(--keep-ime)
  [ "$PURGE_DATA" -eq 1 ] && uninstall_args+=(--purge-data)
  step "exec uninstall.sh"
  say "调用卸载脚本：$UNINSTALLER"
  exec bash "$UNINSTALLER" "${uninstall_args[@]}"
fi

say "rime-sync 离线安装器"
say "系统: $OS_PRETTY"

case "$OS_ID:$OS_VER" in
  ubuntu:22.04) DISTRO_KEY=ubuntu-22.04 ;;
  ubuntu:24.04) DISTRO_KEY=ubuntu-24.04 ;;
  debian:12)    DISTRO_KEY=debian-12 ;;
  debian:13)    DISTRO_KEY=debian-13 ;;
  *)            DISTRO_KEY="$OS_ID-$OS_VER" ;;
esac

if [ -z "$PAYLOAD" ]; then
  if [ -d "$SCRIPT_DIR/payload" ]; then
    PAYLOAD=$SCRIPT_DIR/payload
  else
    PAYLOAD=$SCRIPT_DIR
  fi
fi
PAYLOAD=$(cd "$PAYLOAD" && pwd)

find_sync_deb() {
  ls -1t "$1"/rime-sync_*.deb "$1"/*/rime-sync_*.deb 2>/dev/null | head -n 1 || true
}

verify_installed() {
  local pkg=$1 status
  status=$(dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null || true)
  if [ "$status" != "install ok installed" ]; then
    say "!! $pkg 状态异常：${status:-未安装}"
    return 1
  fi
  say "OK: $pkg ($(dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null)) installed"
  return 0
}

# ------------------------------------------------------- offline set missing
if [ ! -d "$PAYLOAD/$DISTRO_KEY" ]; then
  say "没有找到本机（$DISTRO_KEY）对应的离线安装集。"
  say "当前离线包支持的发行版："
  for d in "$PAYLOAD"/ubuntu-22.04 "$PAYLOAD"/ubuntu-24.04 "$PAYLOAD"/debian-12 "$PAYLOAD"/debian-13; do
    [ -d "$d" ] && say "  - $(basename "$d")"
  done
  if [ "$ASSUME_YES" -eq 1 ]; then
    answer=y
  elif [ -t 0 ]; then
    read -r -p "是否改用联网 apt 安装发行版输入法并继续安装同步程序？[y/N] " answer
  else
    answer=n
  fi
  case "${answer:-n}" in
    y|Y|yes|YES) : ;;
    *)
      say "已取消。请换用对应发行版的离线包（如 $(basename "$PAYLOAD")_<distro>.run）。"
      exit 2
      ;;
  esac
  sync_deb=$(find_sync_deb "$PAYLOAD")
  [ -n "$sync_deb" ] || fail "离线包里没有 rime-sync_*.deb"
  say "联网 apt 安装：fcitx5 fcitx5-rime librime-data"
  step "apt install distro ime (network fallback)"
  apt-get update || fail "apt-get update 失败"
  apt-get install -y --no-install-recommends fcitx5 fcitx5-rime librime-data ||
    fail "apt 安装输入法失败"
  step "apt install rime-sync deb"
  apt-get install -y --no-install-recommends "$sync_deb" || fail "安装 $(basename "$sync_deb") 失败"
  verify_installed rime-sync || fail "rime-sync 未正确安装"
  say "安装完成，输入法设置或应用菜单里打开同步设置"
  exit 0
fi

DIR=$PAYLOAD/$DISTRO_KEY
IME_DEB=$(ls -1t "$DIR"/rime-inputmethod_*.deb 2>/dev/null | head -n 1 || true)
SYNC_DEB=$(ls -1t "$DIR"/rime-sync_*.deb 2>/dev/null | head -n 1 || true)
[ -n "$IME_DEB" ] || fail "$DIR 缺少 rime-inputmethod_*.deb"
[ -n "$SYNC_DEB" ] || fail "$DIR 缺少 rime-sync_*.deb"
say "离线集: $DIR"
say "  IME : $(basename "$IME_DEB")"
say "  SYNC: $(basename "$SYNC_DEB")"

# --------------------------------------------------- remove conflicting pkgs
for p in fcitx5-rime librime1t64 librime1 librime-data; do
  if dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed'; then
    say "移除与自带 librime/数据冲突的发行版包：$p"
    step "purge conflicting $p"
    dpkg --purge --force-depends "$p" || fail "无法移除冲突包 $p"
  fi
done

# ------------------------------------------------------- offline vendor deps
# 判定原则（只对"我们自己的包"负责）：
#   1) 解包前留一份 dpkg 全量快照（$DPKG_BEFORE），失败时据此给出回退说明；
#   2) 已经是 ii（install ok installed）的包一律不碰——离线包只补缺失，不升级系统库；
#      多架构系统上更严：同一包名任一架构已配置且版本与离线包不同时，连缺失架构也不补，
#      否则会立刻触发 "cannot be configured because <pkg>:<other-arch> is at a
#      different version"（0.6.0 现场：libbrotli1:amd64 被升到 deb13u1，i386 还是 b7）；
#   3) 只解包"状态非 ii"的包（未安装 / iU / iF / rc）；
#   4) 用 `dpkg --configure <pkg:arch>...` 只配置我们这批包，不再 `dpkg --configure -a`；
#   5) 失败判定只看我们这批包里"安装前是 ii 或本来不存在、现在却不是 ii"的包；
#      安装前就没配好的系统包只提示、不判失败。
export DEBIAN_FRONTEND=noninteractive
DPKG_BEFORE=$LOGDIR/dpkg-before-install.txt
DPKG_LIVE=$(mktemp "${TMPDIR:-/tmp}/rime-sync-dpkg-live.XXXXXX")
DPKG_OUT=$(mktemp "${TMPDIR:-/tmp}/rime-sync-dpkg-out.XXXXXX")
trap 'rm -f "$DPKG_LIVE" "$DPKG_OUT"' EXIT

OUR_PKGS=()
TOUCHED_FILES=()
declare -A DEB_PKG=() DEB_ARCH=() DEB_VER=()

dpkg_snapshot() {
  dpkg-query -W -f='${Package}\t${Architecture}\t${Version}\t${Status}\n' 2>/dev/null || true
}
dpkg_state_refresh() { dpkg_snapshot >"$DPKG_LIVE" 2>/dev/null || true; }
state_from() { # $1=快照文件 $2=pkg $3=arch -> "version<TAB>status"；空=dpkg 不认识
  awk -F'\t' -v p="$2" -v a="$3" '$1 == p && $2 == a { print $3 "\t" $4; exit }' "$1" 2>/dev/null
}
pkg_state()    { state_from "$DPKG_LIVE" "$1" "$2"; }
before_state() { state_from "$DPKG_BEFORE" "$1" "$2"; }
known_versions() {
  # 该包在任一架构上出现过的版本（去重）。注意不能只看 ii：dpkg 解包了 amd64 的新版本后，
  # 会把 i386 的同名包打成 half-configured，那正是"多架构版本不一致"的现场。
  awk -F'\t' -v p="$1" '$1 == p && $4 !~ /^deinstall/ { print $3 }' "$DPKG_LIVE" 2>/dev/null | sort -u
}
pkg_status_of() { # $1=pkg:arch
  local s
  s=$(pkg_state "${1%%:*}" "${1##*:}")
  printf '%s' "${s#*$'\t'}"
}
our_unconfigured() { # 我们这批包里还没到 ii 的（输出 pkg:arch）
  local id
  for id in "${OUR_PKGS[@]}"; do
    [ "$(pkg_status_of "$id")" = "install ok installed" ] || printf '%s\n' "$id"
  done
}

say_multiarch_advice() {
  local mismatches
  mismatches=$(dpkg_snapshot | awk -F'\t' '
      $4 ~ /^deinstall/ { next }
      { if (!($1 in ver)) { ver[$1] = $3; list[$1] = $2 ":" $3 }
        else if (ver[$1] != $3) { list[$1] = list[$1] " " $2 ":" $3; bad[$1] = 1 } }
      END { for (k in bad) print "     " k ": " list[k] }' | sort | head -n 8)
  [ -n "$FOREIGN_ARCHS" ] || return 0
  if [ -n "$mismatches" ]; then
    say "!! 检测到多架构版本不一致（dpkg 要求同一包的各架构版本相同）："
    printf '%s\n' "$mismatches"
    say "   这是系统已有包的状态，不是 rime-sync 安装内容造成的。可选修复："
    say "     1) 有网络：sudo apt --fix-broken install"
    say "     2) 不需要 32 位兼容：sudo dpkg --remove-architecture $FOREIGN_ARCHS"
    say "        （先 sudo apt purge '.*:$FOREIGN_ARCHS' 清掉 32 位包）"
    say "     3) 只看还有哪些包没配好：sudo dpkg --audit"
  else
    say "   当前外部架构（$FOREIGN_ARCHS）没有版本不一致的包。"
  fi
}

say_rollback_info() {
  local f pkg arch b now n=0 rest
  say "---- 本次离线包改动过的系统包（安装前 → 现在）----"
  for f in "${TOUCHED_FILES[@]}"; do
    pkg=${DEB_PKG[$f]}; arch=${DEB_ARCH[$f]}
    b=$(before_state "$pkg" "$arch"); now=$(pkg_state "$pkg" "$arch")
    b=${b//$'\t'/ }; now=${now//$'\t'/ }
    say "   $pkg:$arch  ${b:-<未安装>}  ->  ${now:-<未安装>}"
    n=$((n + 1))
  done
  [ "$n" -gt 0 ] || say "   （本次没有解包任何包）"
  say "   安装前 dpkg 快照：$DPKG_BEFORE"
  rest=()
  for f in "${TOUCHED_FILES[@]}"; do
    pkg=${DEB_PKG[$f]}; arch=${DEB_ARCH[$f]}
    if [ -z "$(before_state "$pkg" "$arch")" ]; then rest+=("$pkg:$arch"); fi
  done
  if [ "${#rest[@]}" -gt 0 ]; then
    say "   撤销本次新增的包（安装前 dpkg 里没有它们）："
    printf '        sudo dpkg -r %s\n' "${rest[@]:0:10}"
  fi
  say "   把某个包重新配好：sudo dpkg --configure <pkg:arch>"
  say "   让 apt 统一收尾（需要联网）：sudo apt --fix-broken install"
}

report_vendor_failure() { # $1... = 我们这批里未配置且本应可用的 pkg:arch
  local bad=("$@") p id
  say "!! 离线依赖配置失败：${#bad[@]} 个包没有配置完成"
  for id in "${bad[@]}"; do
    p=$(pkg_state "${id%%:*}" "${id##*:}")
    say "   - $id  ${p//$'\t'/ }"
  done
  if [ -s "$DPKG_OUT" ]; then
    say "---- dpkg 原文（尾部）----"
    tail -n 25 "$DPKG_OUT" | sed 's/^/   /'
  fi
  say "---- 系统范围内仍未配置的包（dpkg --audit，仅供参考）----"
  dpkg --audit 2>&1 | sed 's/^/   /' | head -n 20 || true
  say_multiarch_advice
  say_rollback_info
  say "!! 失败的是【系统已安装包的配置状态】（多架构版本不一致 / 半配置包），"
  say "   不是 rime-sync 的安装内容有问题；rime-inputmethod 与 rime-sync 本次没有被装上。"
  say "   按上面的建议修好后重新执行本安装器即可（已装好的依赖不会重复解包）。"
  say "安装已停止。完整日志：$LOG"
  exit 3
}

FOREIGN_ARCHS=$(dpkg --print-foreign-architectures 2>/dev/null | tr '\n' ' ' | sed -e 's/[[:space:]]*$//')
if [ -n "$FOREIGN_ARCHS" ]; then
  say "检测到外部架构：$FOREIGN_ARCHS（多架构系统：只补缺失包，不升级系统已有库）"
fi

if [ -d "$DIR/vendor" ] && compgen -G "$DIR/vendor/*.deb" >/dev/null; then
  COUNT=$(find "$DIR/vendor" -maxdepth 1 -name '*.deb' | wc -l)
  say "安装离线运行期依赖（$COUNT 个 .deb）"
  if [ -r "$DIR/vendor/SHA256SUMS" ]; then
    ( cd "$DIR/vendor" && sha256sum -c --quiet SHA256SUMS ) || fail "离线依赖校验失败（文件损坏或被改过）"
    say "SHA256SUMS 校验通过"
  fi

  dpkg_snapshot >"$DPKG_BEFORE" || fail "无法记录 dpkg 快照（$DPKG_BEFORE）"
  dpkg -l >"$LOGDIR/dpkg-l-before-install.txt" 2>/dev/null || true
  dpkg_state_refresh
  say "安装前 dpkg 快照：$DPKG_BEFORE（$(wc -l <"$DPKG_BEFORE") 条）；dpkg -l：$LOGDIR/dpkg-l-before-install.txt"

  step "select vendor deps ($COUNT)"
  unpack_candidates=()
  configured_skip=0
  multiarch_skip=0
  for f in "$DIR"/vendor/*.deb; do
    pkg=$(dpkg-deb -f "$f" Package 2>/dev/null) || fail "无法读取 $(basename "$f") 的 Package 字段"
    arch=$(dpkg-deb -f "$f" Architecture 2>/dev/null) || arch=$(dpkg --print-architecture)
    ver=$(dpkg-deb -f "$f" Version 2>/dev/null) || ver=""
    DEB_PKG[$f]=$pkg; DEB_ARCH[$f]=$arch; DEB_VER[$f]=$ver
    OUR_PKGS+=("$pkg:$arch")

    state=$(pkg_state "$pkg" "$arch")
    same_ver=${state%%$'\t'*}
    same_status=${state#*$'\t'}
    others=$(known_versions "$pkg" | grep -vxF -- "$same_ver" || true)

    if [ -z "$state" ]; then
      # dpkg 里完全没有这个包
      if [ -n "$FOREIGN_ARCHS" ] && [ -n "$others" ]; then
        multiarch_skip=$((multiarch_skip + 1))
        say "跳过 $pkg:$arch：$pkg 在其它架构已有 ${others//$'\n'/ }，与离线包 $ver 不同，补装会造成多架构版本冲突"
        continue
      fi
      unpack_candidates+=("$f")
    elif [ "$same_status" = "install ok installed" ]; then
      if [ "$UPGRADE_VENDOR" -eq 1 ] && ! dpkg --compare-versions "$ver" le "$same_ver" 2>/dev/null; then
        say "升级 $pkg:$arch：$same_ver -> $ver（--upgrade-vendor）"
        unpack_candidates+=("$f")
      else
        configured_skip=$((configured_skip + 1))
      fi
    else
      # 已解包但没配置（iU/iF），或只剩配置文件（rc）：重新解包即可补齐
      if [ -n "$FOREIGN_ARCHS" ] && [ -n "$others" ]; then
        multiarch_skip=$((multiarch_skip + 1))
        say "跳过 $pkg:$arch（$same_status ${same_ver}）：其它架构版本不同（${others//$'\n'/ }），不碰以免扩大冲突"
        continue
      fi
      unpack_candidates+=("$f")
    fi
  done
  say "待解包 ${#unpack_candidates[@]} 个；已配置跳过 $configured_skip 个；多架构保护跳过 $multiarch_skip 个"
  TOUCHED_FILES=("${unpack_candidates[@]}")

  step "dpkg unpack vendor deps (${#unpack_candidates[@]})"
  # dpkg --unpack 不按 Pre-Depends 排序（systemd-sysv 先于 systemd 是经典失败），
  # 所以逐包解包、把过早的留到下一轮；某一轮完全没有进展才是真的缺依赖。
  pending=("${unpack_candidates[@]}")
  pass=0
  while [ "${#pending[@]}" -gt 0 ]; do
    pass=$((pass + 1))
    [ "$pass" -le 6 ] || fail "离线依赖存在无法满足的 Pre-Depends（见 $LOG）"
    next=()
    for f in "${pending[@]}"; do
      if ! dpkg --unpack --force-confdef --force-confold "$f" >>"$LOG" 2>&1; then
        next+=("$f")
      fi
    done
    if [ "${#next[@]}" -eq "${#pending[@]}" ]; then
      fail "解包离线依赖失败（第 $pass 轮无进展，见 $LOG）"
    fi
    pending=("${next[@]}")
    say "解包第 $pass 轮：剩余 ${#pending[@]} 个"
    # Pre-Depends 只认"已配置"的包：先把我们自己这批里已经能配的配好，下一轮才能继续。
    dpkg_state_refresh
    mapfile -t cfg_now < <(our_unconfigured)
    if [ "${#cfg_now[@]}" -gt 0 ]; then
      dpkg --configure "${cfg_now[@]}" >>"$LOG" 2>&1 || true
    fi
  done

  # 只配置我们自己的包；不再 dpkg --configure -a（那会把系统里无关的坏状态算成我们的失败）
  dpkg_state_refresh
  cfg_pass=0
  cfg_prev=0
  while :; do
    mapfile -t cfg_now < <(our_unconfigured)
    n=${#cfg_now[@]}
    [ "$n" -eq 0 ] && break
    if [ "$cfg_pass" -gt 0 ] && [ "$n" -ge "$cfg_prev" ]; then break; fi
    [ "$cfg_pass" -lt 4 ] || break
    cfg_pass=$((cfg_pass + 1))
    cfg_prev=$n
    say "配置离线依赖（第 $cfg_pass 次，$n 个待配置）"
    : >"$DPKG_OUT"
    dpkg --configure "${cfg_now[@]}" >"$DPKG_OUT" 2>&1 || true
    cat "$DPKG_OUT" >>"$LOG"
    dpkg_state_refresh
  done

  dpkg_state_refresh
  mapfile -t still_bad < <(our_unconfigured)
  if [ "${#still_bad[@]}" -gt 0 ]; then
    mapfile -t ours_failed < <(
      for id in "${still_bad[@]}"; do
        b=$(before_state "${id%%:*}" "${id##*:}")
        if [ -z "$b" ] || [ "${b#*$'\t'}" = "install ok installed" ]; then printf '%s\n' "$id"; fi
      done
    )
    if [ "${#ours_failed[@]}" -gt 0 ]; then
      step "dpkg configure failed (${#ours_failed[@]})"
      report_vendor_failure "${ours_failed[@]}"
    fi
    say "注意：以下包在本安装之前就没有配置完成，属于系统已有状态，不是这次安装造成的："
    printf '   - %s\n' "${still_bad[@]}"
    say_multiarch_advice
    say_rollback_info
    say "   本次没有改动它们的版本，继续安装 rime-sync（若后续失败，按上面的命令修好再重跑）。"
  fi
  say "离线依赖就绪（本批 ${#OUR_PKGS[@]} 个包，本次解包 ${#unpack_candidates[@]} 个，未配置 0 个）"
  step "vendor deps ready"
else
  say "警告：$DIR/vendor 为空，跳过离线依赖（要求系统已具备 fcitx5 与 GTK/WebKit 运行库）"
fi

# --------------------------------------------------------------- strict order
step "dpkg -i $(basename "$IME_DEB")"
say "步骤 1/2：安装输入法 $(basename "$IME_DEB")（已装旧版时直接覆盖升级，不需要先卸载）"
if ! dpkg -i --force-confdef --force-confold "$IME_DEB"; then
  say_multiarch_advice
  say "!! 若上面的 dpkg 报错指向系统已有包（半配置 / 多架构版本不一致），按建议修好后重跑本安装器；"
  say "   rime-sync 自身的安装内容没有问题。"
  fail "输入法安装失败（$(basename "$IME_DEB")）"
fi
verify_installed rime-inputmethod || fail "rime-inputmethod 未正确安装"

step "dpkg -i $(basename "$SYNC_DEB")"
say "步骤 2/2：安装同步程序 $(basename "$SYNC_DEB")（覆盖升级，保留用户数据）"
if ! dpkg -i --force-confdef --force-confold "$SYNC_DEB"; then
  say_multiarch_advice
  say "!! 若上面的 dpkg 报错指向系统已有包（半配置 / 多架构版本不一致），按建议修好后重跑本安装器。"
  fail "同步程序安装失败（$(basename "$SYNC_DEB")）"
fi
verify_installed rime-sync || fail "rime-sync 未正确安装"

# --------------------------------------------------- recommended user config
# 只在调用者自己的 Rime 目录缺文件时写入，不覆盖用户已有配置。
TARGET_USER=${SUDO_USER:-}
if [ -n "$TARGET_USER" ] && [ -f /usr/share/rime-sync/defaults/default.custom.yaml ]; then
  TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
  if [ -n "$TARGET_HOME" ] && [ -d "$TARGET_HOME" ]; then
    RIME_USER_DIR="$TARGET_HOME/.local/share/fcitx5/rime"
    install -d -m 700 "$RIME_USER_DIR" 2>/dev/null || true
    chown -R "$TARGET_USER" "$TARGET_HOME/.local/share/fcitx5" 2>/dev/null || true
    if [ ! -f /usr/share/rime-data/wubi86.schema.yaml ]; then
      # 模板列了 16 个随包方案；/usr/share/rime-data 不是随包数据（例如只装了应用、
      # 用发行版 librime-data）时写进去会让 rime 因缺 schema 部署失败。
      say "跳过推荐输入法配置：/usr/share/rime-data 缺 wubi86.schema.yaml（不是随包方案），写入 16 个方案会导致部署失败"
    elif [ ! -f "$RIME_USER_DIR/default.custom.yaml" ]; then
      install -m 600 /usr/share/rime-sync/defaults/default.custom.yaml "$RIME_USER_DIR/default.custom.yaml" 2>/dev/null &&
        chown "$TARGET_USER" "$RIME_USER_DIR/default.custom.yaml" 2>/dev/null || true
      say "已写入推荐输入法配置：$RIME_USER_DIR/default.custom.yaml（每页 8 个候选 / 16 个方案：拼音·五笔·仓颉·笔画·注音）"
    else
      say "保留已有配置：$RIME_USER_DIR/default.custom.yaml（未覆盖）"
    fi
  fi
fi

say "输入法数据：/usr/share/rime-data（$(find /usr/share/rime-data -maxdepth 1 -type f 2>/dev/null | wc -l) 个文件）"
say "安装日志：$LOG"
say "安装完成，输入法设置或应用菜单里打开同步设置"
exit 0
