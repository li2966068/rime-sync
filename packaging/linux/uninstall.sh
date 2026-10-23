#!/bin/bash
# rime-sync 统一卸载入口：一次卸掉「Rime 同步」与自编译输入法，保留用户数据。
#
#   pkexec /usr/lib/rime-sync/uninstall.sh              # 应用菜单「卸载 Rime 同步与输入法」
#   sudo /usr/lib/rime-sync/uninstall.sh                # 终端里交互确认
#   sudo /usr/lib/rime-sync/uninstall.sh --yes          # 不再询问（脚本/自动化）
#   sudo /usr/lib/rime-sync/uninstall.sh --keep-ime     # 只卸同步程序，保留输入法
#
# 行为约定：
#   * 顺序固定：先 rime-sync，再 rime-inputmethod；单个失败只打印 dpkg 给出的原因，
#     然后继续处理下一个（两个包可能只装了一个）。
#   * 清理我们安装的桌面项与 autostart，不动其它桌面文件。
#   * 绝不删除 ~/.local/share/fcitx5/rime、~/.config/rime-sync 等用户数据，最后打印保留路径。
#   * 本脚本属于 rime-sync 包，卸载时它自身的文件会被 dpkg 删除；bash 是边读边执行的，
#     源文件中途消失会出现 "unexpected EOF"，所以先把自己复制到临时目录再执行。
set -u

PKG_SYNC=rime-sync
PKG_IME=rime-inputmethod
ASSUME_YES=0
KEEP_IME=0
FAILED=0
declare -a FINAL=()

usage() {
  sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

# ---------------------------------------------------------------- relocation
if [ "${RIME_SYNC_UNINSTALL_RELOCATED:-0}" != "1" ]; then
  reloc=$(mktemp "${TMPDIR:-/tmp}/rime-sync-uninstall.XXXXXX") || {
    echo "无法在临时目录创建文件，卸载中止。" >&2
    exit 1
  }
  if ! cat -- "$0" >"$reloc" 2>/dev/null; then
    rm -f -- "$reloc"
    echo "无法读取自身（$0），请用 bash $0 重试。" >&2
    exit 1
  fi
  chmod 0700 "$reloc"
  export RIME_SYNC_UNINSTALL_RELOCATED=1 RIME_SYNC_UNINSTALL_TMP="$reloc"
  exec bash "$reloc" "$@"
fi
trap 'rm -f -- "${RIME_SYNC_UNINSTALL_TMP:-}"' EXIT

# --------------------------------------------------------------------- args
while [ $# -gt 0 ]; do
  case "$1" in
    --yes|-y) ASSUME_YES=1 ;;
    --keep-ime) KEEP_IME=1 ;;
    -h|--help) usage ;;
    *) echo "未知参数：$1（--help 查看用法）" >&2; exit 64 ;;
  esac
  shift
done

say() { printf '[rime-sync] %s\n' "$*"; }

# --------------------------------------------------------------------- root
if [ "$(id -u)" -ne 0 ]; then
  cat >&2 <<EOF
[rime-sync] 卸载软件包需要 root 权限（会写入 /var/lib/dpkg）。

请任选一种方式重新执行：
  pkexec $0 $*        # 桌面环境会弹出认证窗口
  sudo $0 $*
EOF
  [ -t 0 ] && read -r -p "按回车键关闭…" _ || true
  exit 1
fi

# ---------------------------------------------------------------- dpkg state
pkg_status() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null || true; }

pkg_present() {
  case "$(pkg_status "$1")" in
    ""|"unknown ok not-installed"|"deinstall ok config-files") return 1 ;;
    *) return 0 ;;
  esac
}

remove_pkg() {
  local pkg=$1 out rc status
  if ! pkg_present "$pkg"; then
    say "$pkg：未安装，跳过"
    FINAL+=("$pkg|未安装")
    return 0
  fi
  say "卸载 $pkg（当前状态：$(pkg_status "$pkg")）"
  out=$(dpkg -r "$pkg" 2>&1)
  rc=$?
  if [ -n "$out" ]; then printf '%s\n' "$out" | sed '/^$/d; s/^/    /'; fi
  status=$(pkg_status "$pkg")
  if [ "$rc" -ne 0 ] || pkg_present "$pkg"; then
    FAILED=1
    say "!! $pkg 卸载失败（dpkg 退出码 $rc，残留状态：${status:-无记录}）——继续处理下一个"
    FINAL+=("$pkg|失败")
    return 0
  fi
  # dpkg -r 只删文件，包仍以 "deinstall ok config-files"（rc）状态留在 dpkg 数据库里；
  # 我们的包没有需要保留的 /etc 配置，补一次 purge 让 dpkg -l 彻底看不到它。
  if [ "$status" = "deinstall ok config-files" ]; then
    out=$(dpkg -P "$pkg" 2>&1)
    rc=$?
    if [ -n "$out" ]; then printf '%s\n' "$out" | sed '/^$/d; s/^/    /'; fi
    status=$(pkg_status "$pkg")
    if [ "$rc" -ne 0 ] || pkg_present "$pkg"; then
      FAILED=1
      say "!! $pkg 文件已删除，但清理 dpkg 记录失败（退出码 $rc，状态：${status:-无记录}）"
      FINAL+=("$pkg|已卸载(记录残留)")
      return 0
    fi
  fi
  say "OK: $pkg 已卸载"
  FINAL+=("$pkg|已卸载")
}

# ------------------------------------------------------------ invoking user
invoking_user() {
  local uid=${PKEXEC_UID:-}
  [ -n "$uid" ] || uid=${SUDO_UID:-}
  if [ -n "$uid" ]; then
    getent passwd "$uid" | cut -d: -f1
    return 0
  fi
  if [ -n "${SUDO_USER:-}" ]; then
    printf '%s\n' "$SUDO_USER"
    return 0
  fi
  return 1
}

cleanup_files() {
  local p
  for p in "$@"; do
    [ -e "$p" ] || continue
    if rm -f -- "$p" 2>/dev/null; then
      say "已清理：$p"
    else
      say "!! 无法删除：$p"
    fi
  done
}

# -------------------------------------------------------------------- plan
say "将要卸载：$PKG_SYNC"
[ "$KEEP_IME" -eq 1 ] || say "将要卸载：$PKG_IME"
if [ "$KEEP_IME" -eq 1 ]; then
  say "（--keep-ime：保留输入法 $PKG_IME）"
fi
say "用户数据不会被删除。"

if [ "$ASSUME_YES" -eq 0 ] && [ -t 0 ]; then
  read -r -p "继续？[y/N] " answer
  case "${answer:-}" in
    y|Y|yes|YES) : ;;
    *) say "已取消，未做任何修改。"; exit 130 ;;
  esac
fi

# --------------------------------------------------------------- remove pkgs
remove_pkg "$PKG_SYNC"
if [ "$KEEP_IME" -eq 0 ]; then
  remove_pkg "$PKG_IME"
fi

# ------------------------------------------------- desktop entries / autostart
say "清理桌面项与开机自启项（若有）"
cleanup_files \
  /usr/share/applications/rime-sync-uninstall.desktop \
  /usr/share/applications/rime-sync-setup.desktop \
  /usr/share/applications/rime-sync.desktop \
  /etc/xdg/autostart/rime-sync.desktop \
  /etc/xdg/autostart/rime-sync-app.desktop

# 开机自启由应用自身以用户身份写入（tauri-plugin-autostart），root 脚本只能按
# 已知文件名清理调用者的那一份，绝不遍历其他用户的目录。
INVOKER_HOME=""
if user=$(invoking_user) && [ -n "$user" ]; then
  home=$(getent passwd "$user" | cut -d: -f6)
  if [ -n "$home" ] && [ -d "$home" ]; then
    INVOKER_HOME=$home
    cleanup_files \
      "$home/.config/autostart/rime-sync.desktop" \
      "$home/.config/autostart/rime-sync-app.desktop" \
      "$home/.config/autostart/com.rimesync.app.desktop"
  fi
fi

# 刷新菜单缓存（工具不存在就跳过）
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
fi

# ----------------------------------------------------------------- summary
say "---- 结果 ----"
for entry in "${FINAL[@]}"; do
  printf '[rime-sync]   %-18s %s\n' "${entry%%|*}" "${entry##*|}"
done

say "---- 保留的用户数据（未删除）----"
if [ -n "$INVOKER_HOME" ]; then
  say "  $INVOKER_HOME/.local/share/fcitx5/rime   Rime 用户词库与方案自定义"
  say "  $INVOKER_HOME/.config/rime-sync          同步配置与远程仓库信息"
else
  say "  ~/.local/share/fcitx5/rime               Rime 用户词库与方案自定义"
  say "  ~/.config/rime-sync                      同步配置与远程仓库信息"
fi
say "  /var/log/rime-sync                       安装/卸载日志（不需要可自行删除）"

if [ "$FAILED" -ne 0 ]; then
  say "部分软件包未能卸载，请按上面的 dpkg 报错处理后重试。"
  [ -t 0 ] && read -r -p "按回车键关闭…" _ || true
  exit 1
fi
say "卸载完成。"
[ -t 0 ] && read -r -p "按回车键关闭…" _ || true
exit 0
