#!/bin/bash
# rime-sync 一键离线安装器
#
#   sudo ./install.sh                     # 自动识别发行版并安装
#   sudo ./install.sh --payload DIR       # 指定离线包根目录
#   sudo ./install.sh --uninstall         # 先卸同步、再卸输入法（保留用户数据）
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
#   * 卸载时保留 ~/.local/share/fcitx5/rime 与 ~/.config/rime-sync
set -u

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
PAYLOAD=""
DO_UNINSTALL=0
ASSUME_YES=0

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) DO_UNINSTALL=1 ;;
    --payload) shift; PAYLOAD=${1:-} ;;
    --payload=*) PAYLOAD=${1#--payload=} ;;
    --yes|-y) ASSUME_YES=1 ;;
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
  say "卸载 rime-sync 与 rime-inputmethod（先同步、后输入法）"
  step "uninstall rime-sync"
  dpkg -P rime-sync || say "rime-sync 未安装或卸载失败，继续"
  step "uninstall rime-inputmethod"
  dpkg -P rime-inputmethod || say "rime-inputmethod 未安装或卸载失败，继续"
  say "卸载完成。用户数据保留："
  say "  ~/.local/share/fcitx5/rime（Rime 用户词库）"
  say "  ~/.config/rime-sync（同步配置）"
  exit 0
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
if [ -d "$DIR/vendor" ] && compgen -G "$DIR/vendor/*.deb" >/dev/null; then
  COUNT=$(find "$DIR/vendor" -maxdepth 1 -name '*.deb' | wc -l)
  say "安装离线运行期依赖（$COUNT 个 .deb）"
  if [ -r "$DIR/vendor/SHA256SUMS" ]; then
    ( cd "$DIR/vendor" && sha256sum -c --quiet SHA256SUMS ) || fail "离线依赖校验失败"
    say "SHA256SUMS 校验通过"
  fi
  step "dpkg unpack vendor deps ($COUNT)"
  # dpkg --unpack does not order archives by Pre-Depends (systemd-sysv before
  # systemd is the classic failure), so unpack package-by-package and retry the
  # ones that were too early; a pass with no progress means a real gap.
  pending=("$DIR"/vendor/*.deb)
  pass=0
  while [ "${#pending[@]}" -gt 0 ]; do
    pass=$((pass + 1))
    [ "$pass" -le 5 ] || fail "离线依赖存在无法满足的 Pre-Depends（见 $LOG）"
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
    # Pre-Depends are only satisfied by a *configured* package; configure what
    # can be configured now so the next unpack pass can proceed.
    dpkg --configure -a >>"$LOG" 2>&1 || true
  done
  dpkg --configure -a >>"$LOG" 2>&1 || fail "配置离线依赖失败"
else
  say "警告：$DIR/vendor 为空，跳过离线依赖（要求系统已具备 fcitx5 与 GTK/WebKit 运行库）"
fi

# --------------------------------------------------------------- strict order
step "dpkg -i $(basename "$IME_DEB")"
say "步骤 1/2：安装输入法 $(basename "$IME_DEB")"
dpkg -i "$IME_DEB" || fail "输入法安装失败（$(basename "$IME_DEB")）"
verify_installed rime-inputmethod || fail "rime-inputmethod 未正确安装"

step "dpkg -i $(basename "$SYNC_DEB")"
say "步骤 2/2：安装同步程序 $(basename "$SYNC_DEB")"
dpkg -i "$SYNC_DEB" || fail "同步程序安装失败（$(basename "$SYNC_DEB")）"
verify_installed rime-sync || fail "rime-sync 未正确安装"

say "输入法数据：/usr/share/rime-data（$(find /usr/share/rime-data -maxdepth 1 -type f 2>/dev/null | wc -l) 个文件）"
say "安装日志：$LOG"
say "安装完成，输入法设置或应用菜单里打开同步设置"
exit 0
