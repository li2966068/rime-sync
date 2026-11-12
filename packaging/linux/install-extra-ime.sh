#!/bin/bash
# rime-sync 增强输入法「重装」脚本（Linux）
#
# 用途：在「已经装了发行版 Rime 输入法」的机器上，把旧输入法彻底删掉，
#       再全新安装 rime-sync 自建的增强输入法（数字小键盘 / 中文标点直出 / v 转数字）。
#
# 用法：
#   pkexec /usr/lib/rime-sync/install-extra-ime.sh /path/to/rime-sync-installer_<版本>.trixie.run
#   bash install-extra-ime.sh --dry-run /path/to/已解包目录
#
# 约定（与项目其它脚本一致）：
#   * 本脚本由调用方（GUI）用 pkexec 提权执行；脚本自己不再提权（不调 sudo/pkexec）。
#   * 卸载旧输入法时永远保留用户数据：词库、用户配置（~/.local/share/fcitx5/rime、
#     ~/rime、~/.config/com.rimesync.app、~/.config/rime-sync）一概不删。
#   * 装完必须重新编译一次 Rime 方案（rime_deployer --build），否则 build/ 里还是旧产物，
#     增强组件不会生效。
#   * 编译完需要重启输入法（fcitx5）才生效，脚本会给出人话提示。
#
# 依赖：只用系统自带命令（dpkg/apt-get/tar/awk/grep/sed），不引入新依赖。
set -u

# ------------------------------------------------------------------ 自搬迁
# 本脚本装在 rime-sync 包里（/usr/lib/rime-sync/install-extra-ime.sh），
# 而卸载旧版 rime-sync 包时这个文件本身会被 dpkg 删掉 —— bash 是边读边执行的，
# 文件中途消失会报 "unexpected EOF"。所以先把自己复制到临时目录再执行。
if [ "${RIME_SYNC_EXTRA_RELOCATED:-0}" != "1" ]; then
  reloc=$(mktemp "${TMPDIR:-/tmp}/rime-sync-extra-ime.XXXXXX") || {
    echo "无法在临时目录创建文件，安装中止。" >&2
    exit 1
  }
  if ! cat -- "$0" >"$reloc" 2>/dev/null; then
    rm -f -- "$reloc"
    echo "无法读取自身（$0），请用 bash $0 重试。" >&2
    exit 1
  fi
  chmod 0700 "$reloc"
  export RIME_SYNC_EXTRA_RELOCATED=1
  export RIME_SYNC_EXTRA_TMP="$reloc"
  exec bash "$reloc" "$@"
fi

# --------------------------------------------------- 临时文件清理（先注册）
# 必须在参数解析之前注册：--help / 缺参数这类早退路径也要把自搬迁副本删掉。
WORK=""
cleanup() {
  if [ -n "$WORK" ]; then
    case "$WORK" in
      /tmp/rime-sync-extra-ime.*|/var/tmp/rime-sync-extra-ime.*) rm -rf -- "$WORK" ;;
    esac
  fi
  # 删掉自搬迁用的临时副本
  if [ -n "${RIME_SYNC_EXTRA_TMP:-}" ]; then
    rm -f -- "$RIME_SYNC_EXTRA_TMP"
  fi
}
trap cleanup EXIT

# --------------------------------------------------------------- 基本设置
DRY_RUN=0
PKG_PATH=""
LOGDIR=/var/log/rime-sync
LOG="${LOGDIR:-/var/log/rime-sync}/install-extra-ime.log"

say()  { printf '[rime-sync] %s\n' "$*"; }
step() {
  local name=$1 line
  line=$(printf '%s %s' "$(date '+%F %T')" "$name")
  printf '%s\n' "$line"
  # 试运行时不写日志文件（试运行的承诺是「什么都不改动」）
  if [ "$DRY_RUN" -eq 0 ] && [ -d "${LOGDIR:-/var/log/rime-sync}" ] 2>/dev/null; then
    printf '%s\n' "$line" >>"$LOG" 2>/dev/null || true
  fi
}
fail() {
  say "!! $*"
  say "完整日志：$LOG"
  exit 3
}

usage() {
  sed -n '2,/^set -u$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage ;;
    --) shift; break ;;
    -*) echo "未知参数: $1（--help 查看用法）" >&2; exit 64 ;;
    *)  if [ -n "$PKG_PATH" ]; then
          echo "只能指定一个安装包路径（收到两个：$PKG_PATH 与 $1）" >&2
          exit 64
        fi
        PKG_PATH=$1 ;;
  esac
  shift
done
if [ $# -gt 0 ] && [ -z "$PKG_PATH" ]; then PKG_PATH=$1; fi

if [ -z "$PKG_PATH" ]; then
  say "缺少参数：没有给出本地完整安装包的路径。"
  say "用法：install-extra-ime.sh [--dry-run] <安装包路径>"
  say "  <安装包路径> 可以是 .run 自解压包，也可以是已解包目录（里面有 install.sh 与 debian-13/*.deb）。"
  exit 64
fi
if [ ! -e "$PKG_PATH" ]; then
  say "找不到安装包：$PKG_PATH"
  say "请确认路径拼写正确（这个路径必须是本机已经下载好的完整安装包）。"
  exit 66
fi

# 需要 root：脚本本身不负责提权，由调用方用 pkexec 执行。
if [ "$(id -u)" -ne 0 ] && [ "$DRY_RUN" -eq 0 ]; then
  say "需要管理员权限：请让调用方用 pkexec 执行本脚本。"
  say "（本脚本不会自己调用 sudo/pkexec 提权。）"
  exit 77
fi

if [ "$DRY_RUN" -eq 1 ]; then
  say "试运行模式（--dry-run）：只打印将要做什么，不会改动系统。"
fi

# --------------------------------------------------------- 定位安装包内容
# 目标：拿到一个目录，里面有 install.sh 与 debian-13/（含 rime-inputmethod_*.deb）。
BUNDLE=""
if [ -d "$PKG_PATH" ]; then
  BUNDLE=$(cd "$PKG_PATH" && pwd)
  say "安装包：已解包目录 $BUNDLE"
else
  say "安装包：$PKG_PATH（自解压包，正在解包）"
  if [ "$DRY_RUN" -eq 0 ]; then
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-sync-extra-ime.XXXXXX") ||
      fail "无法创建临时目录"
    MARKER=__RIME_SYNC_PAYLOAD_BELOW__
    LINE=$(awk -v m="$MARKER" '$0 == m { print NR + 1; exit }' "$PKG_PATH" 2>/dev/null || true)
    if [ -z "$LINE" ]; then
      fail "这个文件不是 rime-sync 的 .run 安装包（找不到 __RIME_SYNC_PAYLOAD_BELOW__ 标记）"
    fi
    tail -n +"$LINE" "$PKG_PATH" | tar -xzf - -C "$WORK" ||
      fail "解包失败：$PKG_PATH"
    BUNDLE="$WORK"
    say "已解包到临时目录"
  else
    say "（试运行：这里会解包 .run 到临时目录，检查里面的 install.sh 与 debian-13/*.deb）"
    BUNDLE="$PKG_PATH"
  fi
fi

# 用户可能直接指向「装着 deb 的那一层」（如 out/offline/debian-13/），
# 那一层自己没有 install.sh，但它的上一层有 —— 这种情况下上溯一层再试。
if [ ! -f "$BUNDLE/install.sh" ] && [ -f "$BUNDLE/../install.sh" ] &&
   ls -1 "$BUNDLE"/rime-inputmethod_*.deb >/dev/null 2>&1; then
  BUNDLE=$(cd "$BUNDLE/.." && pwd)
  say "（安装包的那一层里没有 install.sh，已上溯到上一层：$BUNDLE）"
fi

INSTALL_SH="$BUNDLE/install.sh"
if [ ! -f "$INSTALL_SH" ]; then
  say "安装包里没有 install.sh：$INSTALL_SH"
  say "这个目录不是 rime-sync 的完整安装包（应当包含 install.sh 与 debian-13/*.deb）。"
  exit 66
fi

# 找到 debian-13 里的输入法 deb（完整包必须带 rime-inputmethod_*.deb）
DISTRO_DIR=""
for d in "$BUNDLE"/debian-13 "$BUNDLE"/*/debian-13; do
  [ -d "$d" ] || continue
  if ls -1 "$d"/rime-inputmethod_*.deb >/dev/null 2>&1; then
    DISTRO_DIR="$d"
    break
  fi
done
if [ -z "$DISTRO_DIR" ]; then
  say "安装包里找不到 rime-inputmethod_*.deb（完整安装包必须自带输入法本体）。"
  say "请改用「完整包」（full），而不是只带同步程序的精简包（thin）。"
  exit 66
fi
say "安装集目录：$DISTRO_DIR"
# 安装器要的是「装着 debian-13/ 的那一层」，我们显式告诉它，避免它猜错。
PAYLOAD_ROOT=$(cd "$(dirname "$DISTRO_DIR")" && pwd)
if [ ! -d "$PAYLOAD_ROOT/debian-13" ]; then
  say "!! 安装包结构异常：找不到 $PAYLOAD_ROOT/debian-13（install.sh 需要它）。"
  exit 66
fi

# ------------------------------------------------------- 查询当前输入法状况
# 说明：我们的包名是 rime-inputmethod，并且 Provides: fcitx5-rime。
# dpkg 不解析虚包，所以「真实包名叫 fcitx5-rime 且已安装」= 机器上装的是发行版输入法。
real_pkg_installed() { # <真实包名>：只看 dpkg 里的真实包名，忽略 Provides
  dpkg-query -W -f='${Package}\t${Status}\n' 2>/dev/null |
    awk -v p="$1" '$1 == p && $2 == "install" && $3 == "ok" && $4 == "installed" { found = 1 } END { exit(found ? 0 : 1) }'
}
is_our_package() { # <包名>：靠我们自己的标志文件判断是不是 rime-sync 的包
  dpkg -L "$1" 2>/dev/null | grep -q '^/usr/share/rime-sync/'
}

DISTRO_IME_PKG=fcitx5-rime
DISTRO_IME_INSTALLED=0
if real_pkg_installed "$DISTRO_IME_PKG"; then
  DISTRO_IME_INSTALLED=1
fi

OLD_IME_INSTALLED=0
OLD_IME_IS_OURS=0
if real_pkg_installed rime-inputmethod; then
  OLD_IME_INSTALLED=1
  if is_our_package rime-inputmethod; then OLD_IME_IS_OURS=1; fi
fi

OLD_SYNC_INSTALLED=0
if real_pkg_installed rime-sync; then OLD_SYNC_INSTALLED=1; fi

# 发行版 librime-bin 会带 /usr/bin/rime_deployer，和我们自建包里的同名文件冲突
DISTRO_DEPLOYER_INSTALLED=0
if real_pkg_installed librime-bin; then DISTRO_DEPLOYER_INSTALLED=1; fi

say "---- 当前输入法状况 ----"
if [ "$DISTRO_IME_INSTALLED" -eq 1 ]; then
  say "  发行版输入法 $DISTRO_IME_PKG：已安装（需要卸掉，否则会和我们的输入法抢 fcitx5 的 rime 插件）"
else
  say "  发行版输入法 $DISTRO_IME_PKG：未安装"
fi
if [ "$OLD_IME_INSTALLED" -eq 1 ]; then
  if [ "$OLD_IME_IS_OURS" -eq 1 ]; then
    say "  rime-inputmethod：已安装（是 rime-sync 自己的旧版，将按旧版卸载流程处理）"
  else
    say "  rime-inputmethod：已安装（不是 rime-sync 的包，是发行版的同名包，将直接卸载）"
  fi
else
  say "  rime-inputmethod：未安装"
fi
if [ "$OLD_SYNC_INSTALLED" -eq 1 ]; then
  say "  rime-sync（旧版同步程序）：已安装"
else
  say "  rime-sync：未安装"
fi
if [ "$DISTRO_DEPLOYER_INSTALLED" -eq 1 ]; then
  say "  librime-bin：已安装（它带的 rime_deployer 会和我们自建的文件冲突，需要卸掉）"
fi

# ------------------------------------------------------------ 目标桌面用户
pick_target_user() {
  local user="${RIMESYNC_TARGET_USER:-}" cand
  if [ -n "$user" ]; then printf '%s' "$user"; return 0; fi
  if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then printf '%s' "$SUDO_USER"; return 0; fi
  if [ -n "${PKEXEC_UID:-}" ]; then
    user=$(id -nu "$PKEXEC_UID" 2>/dev/null || true)
    if [ -n "$user" ] && [ "$user" != root ]; then printf '%s' "$user"; return 0; fi
  fi
  user=$(logname 2>/dev/null || true)
  if [ -n "$user" ] && [ "$user" != root ]; then printf '%s' "$user"; return 0; fi
  for cand in /home/*; do
    [ -d "$cand" ] || continue
    user=$(basename "$cand")
    if id -u "$user" >/dev/null 2>&1; then printf '%s' "$user"; return 0; fi
  done
  printf ''
}
TARGET_USER=$(pick_target_user)
TARGET_HOME=""
if [ -n "$TARGET_USER" ]; then
  TARGET_HOME=$(getent passwd "$TARGET_USER" 2>/dev/null | cut -d: -f6)
  if [ -z "$TARGET_HOME" ] || [ ! -d "$TARGET_HOME" ]; then TARGET_HOME=""; fi
fi
say "目标桌面用户：${TARGET_USER:-（未确定）}"

# Rime 用户目录：规则与 packaging/linux/postinst 的 resolve_rime_user_dir 一致
# （key=value 或 key: value、引号、~ 展开、空值视为未设置、末尾斜杠归一）。
resolve_rime_user_dir() { # <home>
  local home=$1 conf raw="" out=""
  [ -n "$home" ] || { printf ''; return 0; }
  conf="$home/.config/fcitx5/conf/rime.conf"
  if [ -r "$conf" ]; then
    raw=$(sed -n -E 's/^[[:space:]]*UserDataDirPath[[:space:]]*([=:][[:space:]]*)?(.*)$/\2/p' "$conf" 2>/dev/null | head -n 1)
    raw=$(printf '%s' "$raw" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
  fi
  if [ -n "$raw" ]; then
    if [ "$raw" = "~" ]; then
      out=$home
    elif [ "${raw:0:2}" = "~/" ]; then
      out=$home/${raw:2}
    else
      out=$raw
    fi
  else
    out=$home/.local/share/fcitx5/rime
  fi
  while [ "${out%/}" != "$out" ] && [ "$out" != "/" ]; do out=${out%/}; done
  printf '%s' "$out"
}

# 先看 rime-sync 自己的解析（权威值：crates/core/src/rime_dir.rs），拿不到再退回 shell 规则
rime_user_dir=""
if command -v rime-sync-cli >/dev/null 2>&1; then
  json=$(rime-sync-cli check-ime --json 2>/dev/null || true)
  rime_user_dir=$(printf '%s' "$json" | sed -n 's/.*"rime_dir"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
fi
if [ -z "$rime_user_dir" ] && [ -n "$TARGET_HOME" ]; then
  rime_user_dir=$(resolve_rime_user_dir "$TARGET_HOME")
fi
say "Rime 用户目录：${rime_user_dir:-（未确定）}"

# --------------------------------------------------------------- 卸载旧输入法
say "---- 第 1 步：卸载旧输入法（用户数据全部保留）----"
say "保留不动的用户数据："
if [ -n "$TARGET_HOME" ]; then
  say "  ${rime_user_dir:-$TARGET_HOME/.local/share/fcitx5/rime}   Rime 词库与方案自定义"
  say "  $TARGET_HOME/.config/com.rimesync.app   同步配置与备份"
  say "  $TARGET_HOME/.config/rime-sync          旧版配置目录"
fi

# 1a) 发行版输入法 fcitx5-rime：必须卸掉（它带的发行版 librime 没有增强组件，
#     与我们的输入法共存时 fcitx5 仍会加载发行版的 rime 插件，增强功能用不了）
if [ "$DISTRO_IME_INSTALLED" -eq 1 ]; then
  say "卸载发行版输入法包：$DISTRO_IME_PKG"
  step "purge distro ime $DISTRO_IME_PKG"
  if [ "$DRY_RUN" -eq 0 ]; then
    if command -v apt-get >/dev/null 2>&1; then
      DEBIAN_FRONTEND=noninteractive apt-get purge -y --no-install-recommends "$DISTRO_IME_PKG" ||
        dpkg --purge --force-depends "$DISTRO_IME_PKG" ||
        fail "无法卸载发行版输入法 $DISTRO_IME_PKG"
    else
      dpkg --purge --force-depends "$DISTRO_IME_PKG" ||
        fail "无法卸载发行版输入法 $DISTRO_IME_PKG"
    fi
  else
    say "  [试运行] 将执行：apt-get purge -y --no-install-recommends $DISTRO_IME_PKG"
  fi
  say "  已卸载（发行版包只删它自己的系统文件，不会碰你的词库）"
else
  say "发行版输入法 $DISTRO_IME_PKG 未安装，跳过"
fi

# 1b) 旧的 rime-inputmethod：
#     - 是我们自己的旧版 → 交给统一卸载脚本 uninstall.sh（默认语义：保留用户数据）
#     - 是发行版同名包 → 直接卸载
UNINSTALLER=""
for c in /usr/lib/rime-sync/uninstall.sh "$BUNDLE/uninstall.sh"; do
  if [ -f "$c" ]; then UNINSTALLER=$c; break; fi
done
if [ "$OLD_IME_INSTALLED" -eq 1 ] && [ "$OLD_IME_IS_OURS" -eq 1 ]; then
  if [ -n "$UNINSTALLER" ]; then
    say "卸载 rime-sync 的旧版输入法与同步程序（用统一卸载脚本 uninstall.sh，保留用户数据）"
    step "uninstall old via uninstall.sh"
    if [ "$DRY_RUN" -eq 0 ]; then
      # stdin 接 /dev/null：uninstall.sh 只在「有终端」时才问是否删用户数据，
      # 这里让它拿不到终端 ⇒ 直接走默认的「保留用户数据」分支。
      bash "$UNINSTALLER" --yes </dev/null || say "!! 旧版卸载脚本返回非零，继续安装新版"
    else
      say "  [试运行] 将执行：bash $UNINSTALLER --yes"
    fi
  else
    say "卸载 rime-sync 的旧版输入法与同步程序（找不到 uninstall.sh，直接卸载包，不删用户数据）"
    step "purge old rime-inputmethod rime-sync"
    if [ "$DRY_RUN" -eq 0 ]; then
      dpkg --purge --force-depends rime-inputmethod >/dev/null 2>&1 || true
      dpkg --purge --force-depends rime-sync >/dev/null 2>&1 || true
    else
      say "  [试运行] 将执行：dpkg --purge --force-depends rime-inputmethod rime-sync"
    fi
  fi
elif [ "$OLD_IME_INSTALLED" -eq 1 ]; then
  say "卸载发行版同名的 rime-inputmethod 包"
  step "purge foreign rime-inputmethod"
  if [ "$DRY_RUN" -eq 0 ]; then
    dpkg --purge --force-depends rime-inputmethod || say "!! rime-inputmethod 卸载不干净，继续"
  else
    say "  [试运行] 将执行：dpkg --purge --force-depends rime-inputmethod"
  fi
else
  say "没有旧的 rime-inputmethod，跳过"
fi

# 1c) 发行版 librime-bin：它带的 /usr/bin/rime_deployer 会和我们自建的文件同名冲突
if [ "$DISTRO_DEPLOYER_INSTALLED" -eq 1 ]; then
  say "卸载发行版 librime-bin（避免 rime_deployer 文件冲突；它只是命令行工具，不含用户数据）"
  step "purge distro librime-bin"
  if [ "$DRY_RUN" -eq 0 ]; then
    if command -v apt-get >/dev/null 2>&1; then
      DEBIAN_FRONTEND=noninteractive apt-get purge -y --no-install-recommends librime-bin ||
        dpkg --purge --force-depends librime-bin || say "!! librime-bin 卸载失败，继续"
    else
      dpkg --purge --force-depends librime-bin || say "!! librime-bin 卸载失败，继续"
    fi
  else
    say "  [试运行] 将执行：apt-get purge -y --no-install-recommends librime-bin"
  fi
fi

# --------------------------------------------------------------- 全新安装
say "---- 第 2 步：全新安装 rime-sync 的输入法与同步程序 ----"
say "安装器：$INSTALL_SH"
step "install fresh"
if [ "$DRY_RUN" -eq 0 ]; then
  # stdin 接 /dev/null：install.sh 在缺少离线安装集时会问「是否改用联网 apt」，
  # 这里拿不到终端 ⇒ 它会自己放弃联网（我们绝不能让它把发行版 fcitx5-rime 装回来）。
  bash "$INSTALL_SH" --payload "$PAYLOAD_ROOT" </dev/null ||
    fail "安装器执行失败（详见 /var/log/rime-sync/install.log）。若日志里写的是「没有找到本机对应的安装集」，说明当前系统不是 Debian 13（trixie）。"
else
  say "  [试运行] 将执行：bash $INSTALL_SH --payload $PAYLOAD_ROOT"
  say "  （它内部会先卸载旧版，再全新安装，并播种方案增强模板）"
fi

# --------------------------------------------------- 重新编译一次 Rime 方案
# 为什么必须做：Linux 上让 librime 重新编译的唯一可靠手段是 rime_deployer --build
# （fcitx5-remote -r 与 DBus ReloadConfig 都不会触发重编译）。
# 不重编译的话 build/ 里还是旧产物，数字小键盘 / 标点直出 / v 转数字都不会生效。
say "---- 第 3 步：重新编译 Rime 方案（让增强组件真正生效）----"
DEPLOYER=/usr/bin/rime_deployer
if [ ! -x "$DEPLOYER" ]; then
  say "!! 找不到 $DEPLOYER，无法自动重新编译。"
  say "   请手动执行：rime_deployer --build \"<Rime 用户目录>\" /usr/share/rime-data"
else
  if [ -z "$rime_user_dir" ]; then
    say "!! 没有确定 Rime 用户目录，跳过重新编译。"
    say "   请手动执行：rime_deployer --build \"<Rime 用户目录>\" /usr/share/rime-data"
  else
    say "编译命令：$DEPLOYER --build \"$rime_user_dir\" /usr/share/rime-data"
    step "rebuild rime schemas"
    if [ "$DRY_RUN" -eq 0 ]; then
      mkdir -p "$rime_user_dir/build"
      if ! "$DEPLOYER" --build "$rime_user_dir" /usr/share/rime-data; then
        say "!! 重新编译失败，请检查 /usr/share/rime-data 是否存在，或手动重跑上面这条命令。"
      else
        say "重新编译完成"
        # 编译产物属主改回桌面用户（编译是 root 跑的）
        if [ -n "$TARGET_USER" ]; then
          chown -R "$TARGET_USER":"$(id -gn "$TARGET_USER" 2>/dev/null || echo "$TARGET_USER")" \
            "$rime_user_dir/build" 2>/dev/null || true
        fi
        # 校验增强组件是否真的进了编译产物
        if grep -rqs 'rimesync_vnum\|rimesync_punct' "$rime_user_dir/build" 2>/dev/null; then
          say "校验通过：编译产物里含有 rimesync 增强组件标记"
        else
          say "!! 警告：编译产物里没找到 rimesync 增强组件标记，输入法增强功能可能不生效。"
        fi
      fi
    else
      say "  [试运行] 将执行：$DEPLOYER --build \"$rime_user_dir\" /usr/share/rime-data"
    fi
  fi
fi

# ---------------------------------------------------------------- 收尾提示
say ""
say "---- 完成 ----"
say "1) 需要重启输入法才能生效："
if [ -n "$TARGET_USER" ]; then
  say "   在桌面会话里执行：systemctl --user restart fcitx5"
fi
say "   或者直接注销后重新登录一次（最稳妥）。"
say "2) 重启后按 Ctrl+~ 或点击候选栏里的「齿轮」图标，确认增强功能（数字小键盘 / 中文标点直出 / v 转数字）在列表里。"
say "3) 如果增强功能还是没出现，麻烦把这两处日志发给我们："
say "   /var/log/rime-sync/install.log 与 /var/log/rime-sync/install-extra-ime.log"
say "   另外提醒：你的词库与个人配置全程没有被删除（重装只换输入法本体与同步程序）。"
exit 0
