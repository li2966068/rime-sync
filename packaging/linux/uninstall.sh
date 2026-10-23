#!/bin/bash
# rime-sync 统一卸载入口：一次卸掉「Rime 同步」与自编译输入法，默认保留用户数据。
#
#   pkexec /usr/lib/rime-sync/uninstall.sh                # 应用菜单「卸载 Rime 同步与输入法」
#   sudo /usr/lib/rime-sync/uninstall.sh                  # 终端里交互确认
#   sudo /usr/lib/rime-sync/uninstall.sh --yes            # 不再询问（脚本/自动化）
#   sudo /usr/lib/rime-sync/uninstall.sh --keep-ime       # 只卸同步程序，保留输入法
#   sudo /usr/lib/rime-sync/uninstall.sh --purge-data     # 连本地用户数据一起删（破坏性、不可恢复）
#
# 行为约定：
#   * 顺序固定：先 rime-sync，再 rime-inputmethod；单个失败只打印 dpkg 给出的原因，
#     然后继续处理下一个（两个包可能只装了一个）。
#   * 清理我们安装的桌面项与 autostart，不动其它桌面文件。
#   * 默认不删用户数据：只有带 --purge-data，或在终端里对「是否同时删除本地用户数据」
#     回答 y 才删（默认 N）。删除前列出全部绝对路径，删除后逐条打印成功/失败；
#     失败继续处理后面的目录，最终退出码非 0。
#   * 删除范围是写死的白名单，只取调用者 home 下的这几条目录：
#       Rime 用户目录：读 ~/.config/fcitx5/conf/rime.conf 的 UserDataDirPath
#                      （去尾斜杠、展开 ~），为空则 ~/.local/share/fcitx5/rime
#       ~/.config/com.rimesync.app   同步程序的配置与备份
#       ~/.config/rime-sync          旧版目录名（存在才删）
#     绝不递归父目录，也不删 home 本身；每条删除前再校验一次「绝对路径且严格位于 home 下」。
#   * 本脚本属于 rime-sync 包，卸载时它自身的文件会被 dpkg 删除；bash 是边读边执行的，
#     源文件中途消失会出现 "unexpected EOF"，所以先把自己复制到临时目录再执行。
set -u

PKG_SYNC=rime-sync
PKG_IME=rime-inputmethod
ASSUME_YES=0
KEEP_IME=0
PURGE_DATA=0
FAILED=0
ARGV=("$@")
declare -a FINAL=()
declare -a PURGE_LIST=()
declare -a PURGED=()

usage() {
  sed -n '2,/^set -u$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
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
    --purge-data) PURGE_DATA=1 ;;
    -h|--help) usage ;;
    *) echo "未知参数：$1（--help 查看用法）" >&2; exit 64 ;;
  esac
  shift
done

say() { printf '[rime-sync] %s\n' "$*"; }

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

# 调用者的 home（root 直接跑且没有 SUDO_UID/PKEXEC_UID 时为空 → 绝不删数据）
INVOKER_HOME=""
if user=$(invoking_user) && [ -n "$user" ]; then
  home=$(getent passwd "$user" | cut -d: -f6)
  if [ -n "$home" ] && [ -d "$home" ]; then
    INVOKER_HOME=$home
  fi
fi

# --------------------------------------------------------------------- root
if [ "$(id -u)" -ne 0 ]; then
  cat >&2 <<EOF
[rime-sync] 卸载软件包需要 root 权限（会写入 /var/lib/dpkg）。

请任选一种方式重新执行：
  pkexec $0 ${ARGV[*]}        # 桌面环境会弹出认证窗口
  sudo $0 ${ARGV[*]}
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

# ------------------------------------------------- 用户数据路径（白名单解析）
# Rime 用户目录：fcitx5 的 rime addon 把自定义目录记在 rime.conf 的 UserDataDirPath。
rime_user_dir() {
  local home=$1 conf raw=""
  conf="$home/.config/fcitx5/conf/rime.conf"
  if [ -r "$conf" ]; then
    raw=$(sed -n 's/^[[:space:]]*UserDataDirPath[[:space:]]*=[[:space:]]*//p' "$conf" 2>/dev/null | head -n 1)
    raw=$(printf '%s' "$raw" | sed 's/[[:space:]]*$//; s/^"//; s/"$//')
  fi
  case "$raw" in
    ""|"~"|"~/") printf '%s\n' "$home/.local/share/fcitx5/rime"; return 0 ;;
    "~/"*) raw=$home/${raw#\~/} ;;
  esac
  # 去尾斜杠（"/" 除外）
  while [ "${raw%/}" != "$raw" ] && [ "$raw" != "/" ]; do raw=${raw%/}; done
  printf '%s\n' "$raw"
}

# 绝对路径、严格位于 home 之下、不等于 home 本身、不是根目录
path_under_home() {
  local p=$1 real_home
  real_home=$(realpath -m -- "$2" 2>/dev/null) || real_home=$2
  case "$p" in
    /*) ;;
    *) return 1 ;;
  esac
  [ "$real_home" != "/" ] && [ -n "$real_home" ] || return 1
  [ "$p" != "/" ] || return 1
  [ "$p" != "$real_home" ] || return 1
  case "$p" in
    "$real_home"/*) return 0 ;;
  esac
  return 1
}

# 追加一条待删目录（先规范化，再校验；不合规只警告，不进列表）
add_purge_candidate() {
  local p=$1 real existing
  [ -n "$p" ] || return 0
  [ -n "$INVOKER_HOME" ] || return 0
  real=$(realpath -m -- "$p" 2>/dev/null) || real=$p
  if ! path_under_home "$real" "$INVOKER_HOME"; then
    say "!! 拒绝删除（不在调用者 home 之下，或就是 home 本身）：$p"
    return 0
  fi
  for existing in "${PURGE_LIST[@]:-}"; do
    [ "$existing" = "$real" ] && return 0
  done
  [ -d "$real" ] || [ -L "$real" ] || return 0
  PURGE_LIST+=("$real")
}

if [ -n "$INVOKER_HOME" ]; then
  add_purge_candidate "$(rime_user_dir "$INVOKER_HOME")"
  add_purge_candidate "$INVOKER_HOME/.config/com.rimesync.app"
  add_purge_candidate "$INVOKER_HOME/.config/rime-sync"
fi

# -------------------------------------------------------------------- plan
say "将要卸载：$PKG_SYNC"
[ "$KEEP_IME" -eq 1 ] || say "将要卸载：$PKG_IME"
if [ "$KEEP_IME" -eq 1 ]; then
  say "（--keep-ime：保留输入法 $PKG_IME）"
fi

# 询问之前先把「会删哪些绝对路径」摆出来
say "---- 本地用户数据 ----"
if [ -z "$INVOKER_HOME" ]; then
  say "未能确定调用者 home（没有 SUDO_UID / PKEXEC_UID），不会删除任何用户数据。"
elif [ "${#PURGE_LIST[@]}" -eq 0 ]; then
  say "没有找到可删除的用户数据目录（已被删除或从未创建）。"
else
  if [ "$PURGE_DATA" -eq 1 ]; then
    say "以下目录将被永久删除（--purge-data，不可恢复）："
  else
    say "以下目录默认保留（删除后不可恢复）："
  fi
  for p in "${PURGE_LIST[@]}"; do say "  $p"; done
fi

if [ "$PURGE_DATA" -eq 0 ] && [ -n "$INVOKER_HOME" ] && [ "${#PURGE_LIST[@]}" -gt 0 ] && [ -t 0 ]; then
  read -r -p "是否同时删除上面列出的本地用户数据？删除后不可恢复 (y/N) " purge_answer
  case "${purge_answer:-}" in
    y|Y|yes|YES) PURGE_DATA=1 ;;
    *) PURGE_DATA=0 ;;
  esac
  if [ "$PURGE_DATA" -eq 1 ]; then say "用户数据：将删除"; else say "用户数据：保留"; fi
fi

if [ "$PURGE_DATA" -eq 1 ]; then
  say "注意：卸载完成后会删除上面列出的用户数据（Rime 词库/方案自定义、同步配置与备份）。"
else
  say "用户数据不会被删除。"
fi

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
if [ -n "$INVOKER_HOME" ]; then
  cleanup_files \
    "$INVOKER_HOME/.config/autostart/rime-sync.desktop" \
    "$INVOKER_HOME/.config/autostart/rime-sync-app.desktop" \
    "$INVOKER_HOME/.config/autostart/com.rimesync.app.desktop"
fi

# 刷新菜单缓存（工具不存在就跳过）
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
fi

# ----------------------------------------------------------- purge user data
purge_dir() {
  local p=$1 real
  if [ ! -e "$p" ] && [ ! -L "$p" ]; then
    say "  跳过（已不存在）：$p"
    PURGED+=("$p|跳过")
    return 0
  fi
  # 删除前再校验一次：只删这一条明确列出的路径，不碰父目录
  real=$(realpath -m -- "$p" 2>/dev/null) || real=$p
  if ! path_under_home "$real" "$INVOKER_HOME"; then
    FAILED=1
    say "  !! 拒绝删除（校验未通过）：$p"
    PURGED+=("$p|拒绝")
    return 0
  fi
  if rm -rf -- "$real" 2>/dev/null && [ ! -e "$real" ] && [ ! -L "$real" ]; then
    say "  OK  已删除：$p"
    PURGED+=("$p|已删除")
    return 0
  fi
  FAILED=1
  say "  !!  删除失败：$p"
  PURGED+=("$p|失败")
}

if [ "$PURGE_DATA" -eq 1 ]; then
  say "---- 删除本地用户数据 ----"
  if [ -z "$INVOKER_HOME" ]; then
    FAILED=1
    say "!! 未能确定调用者 home，未删除任何用户数据（请用 sudo 或 pkexec 运行）"
  elif [ "${#PURGE_LIST[@]}" -eq 0 ]; then
    say "没有需要删除的用户数据目录。"
  else
    for p in "${PURGE_LIST[@]}"; do purge_dir "$p"; done
  fi
fi

# ----------------------------------------------------------------- summary
say "---- 结果 ----"
for entry in "${FINAL[@]}"; do
  printf '[rime-sync]   %-18s %s\n' "${entry%%|*}" "${entry##*|}"
done

if [ "$PURGE_DATA" -eq 1 ]; then
  say "---- 用户数据 ----"
  if [ "${#PURGED[@]}" -eq 0 ]; then
    say "  未删除任何目录"
  fi
  for entry in "${PURGED[@]}"; do
    printf '[rime-sync]   %-8s %s\n' "${entry##*|}" "${entry%%|*}"
  done
else
  say "---- 保留的用户数据（未删除）----"
  if [ -n "$INVOKER_HOME" ]; then
    say "  $(rime_user_dir "$INVOKER_HOME")   Rime 用户词库与方案自定义"
    say "  $INVOKER_HOME/.config/com.rimesync.app   同步配置与备份"
    say "  $INVOKER_HOME/.config/rime-sync          旧版配置目录（若存在）"
  else
    say "  ~/.local/share/fcitx5/rime               Rime 用户词库与方案自定义"
    say "  ~/.config/com.rimesync.app               同步配置与备份"
    say "  ~/.config/rime-sync                      旧版配置目录（若存在）"
  fi
  say "（确认不要时可重跑：$0 --purge-data，或自行删除）"
fi
say "  /var/log/rime-sync                       安装/卸载日志（不需要可自行删除）"

if [ "$FAILED" -ne 0 ]; then
  say "部分步骤失败（软件包未能卸载或用户数据未能删除），请按上面的报错处理后重试。"
  [ -t 0 ] && read -r -p "按回车键关闭…" _ || true
  exit 1
fi
say "卸载完成。"
[ -t 0 ] && read -r -p "按回车键关闭…" _ || true
exit 0
