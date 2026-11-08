#!/usr/bin/env bash
# Repack an existing rime-sync .deb with the maintainer scripts from
# packaging/linux/. The application payload is untouched; only postinst/prerm/
# postrm are refreshed so an offline bundle does not queue a background worker
# that would apt-install the distribution fcitx5-rime over rime-inputmethod.
#
#   packaging/linux/prepare-sync-deb.sh IN.deb OUT.deb
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
src=${1:?usage: prepare-sync-deb.sh IN.deb OUT.deb}
out=${2:?usage: prepare-sync-deb.sh IN.deb OUT.deb}
test -f "$src" || { echo "!! missing $src" >&2; exit 1; }
mkdir -p "$(dirname "$out")"

# 0.6.23：/usr/bin/rime-sync-cli 的唯一来源是 tauri 的 bundle.externalBin sidecar。
# deb 里出现两份就说明 bundle.linux.deb.files 又映射了同一个路径（两条路写同一个文件，
# 谁赢取决于 tauri-bundler 内部顺序）。dpkg-deb -c 数 tar 条目，是用户实际装到什么的唯一证据。
# dpkg-deb -c 的路径列**不带** "./" 前缀（usr/bin/... 而不是 ./usr/bin/...），
# 比较前先归一化 —— 否则永远数到 0 份（0.6.24 的发布就是这么红的）。
cli_count() { dpkg-deb -c "$1" | awk '{ p = $6; sub(/^\.\//, "", p); if (p == "usr/bin/rime-sync-cli") c++ } END { print c + 0 }'; }
assert_one_cli() { # <deb>
  local n mode
  n=$(cli_count "$1")
  if [ "$n" -ne 1 ]; then
    echo "!! $1: /usr/bin/rime-sync-cli 应恰好 1 份，实际 $n 份" >&2
    dpkg-deb -c "$1" | grep rime-sync-cli >&2 || true
    exit 1
  fi
  mode=$(dpkg-deb -c "$1" | awk '{ p = $6; sub(/^\.\//, "", p); if (p == "usr/bin/rime-sync-cli") { print $1; exit } }')
  if [ "$mode" != "-rwxr-xr-x" ]; then
    echo "!! $1: /usr/bin/rime-sync-cli 权限是 $mode（期望 -rwxr-xr-x）" >&2
    exit 1
  fi
  echo "    ok /usr/bin/rime-sync-cli x1 ($mode)"
}
assert_one_cli "$src"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/rime-sync-deb.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# 0.6.24：CI runner 上 dpkg-deb --build 两次都死在「tar 往 stdout 写」这一步
# （tar: stdout: write error / dpkg-deb: error: tar subprocess returned error exit status 2），
# 同一个 deb 在本机跑得通 —— 是环境差异，不是 deb 内容问题。所以：
#   1) 先走 dpkg-deb 正常路径；
#   2) 失败就打印现场（版本/磁盘/inode/ulimit），再用 ar 直接重打包兜底：
#      只换控制归档（维护脚本），数据归档原样保留 —— 完全不走 tar 管道，不受这个坑影响。
dump_env() {
  # 这段只用于诊断：任何一条命令失败都不许把脚本带崩（set -e 会在此反噬）。
  echo "---- 重打包现场 ----"
  echo "dpkg-deb: $(dpkg-deb --version 2>&1 | head -n1 || true)"
  echo "tar: $(tar --version 2>&1 | head -n1 || true)"
  echo "xz: $(xz --version 2>&1 | head -n1 || true)"
  df -h . /tmp 2>&1 | sed 's/^/  /' || true
  df -i . /tmp 2>&1 | sed 's/^/  /' || true
  ulimit -a 2>&1 | sed 's/^/  /' || true
  echo "--------------------"
}

build_with_dpkg_deb() { # <src.deb> <out.deb>
  rm -rf "$tmp/root"; mkdir -p "$tmp/root"
  dpkg-deb -R "$src" "$tmp/root" || return 1
  for ms in postinst prerm postrm; do
    [ -f "$HERE/$ms" ] || continue
    install -m 0755 "$HERE/$ms" "$tmp/root/DEBIAN/$ms"
  done
  find "$tmp/root" -type d -exec chmod 0755 {} +
  find "$tmp/root" -type f -exec chmod 0644 {} +
  # 恢复可执行位：上面的 0644 会把 /usr/bin/rime-sync-app 变成不可执行（0.5.0 的实测 bug）。
  if [ -d "$tmp/root/usr/bin" ]; then
    find "$tmp/root/usr/bin" -type f -exec chmod 0755 {} +
  fi
  if [ -d "$tmp/root/usr/lib/rime-sync" ]; then
    find "$tmp/root/usr/lib/rime-sync" -type f -exec chmod 0755 {} +
  fi
  find "$tmp/root/DEBIAN" -type f -name 'post*' -exec chmod 0755 {} +
  find "$tmp/root/DEBIAN" -type f -name 'pre*' -exec chmod 0755 {} +
  dpkg-deb --root-owner-group -Zxz --build "$tmp/root" "$out" >/dev/null || return 1
}

repack_via_ar() { # <src.deb> <out.deb>：只换控制归档，数据归档原样搬过去
  local s=$1 o=$2 work="$tmp/ar" ctl data ms
  rm -rf "$work"; mkdir -p "$work/ctl"
  ( cd "$work" && ar x "$s" ) || return 1
  ctl=$(ls -1 "$work"/control.tar.* 2>/dev/null | head -n1)
  data=$(ls -1 "$work"/data.tar.* 2>/dev/null | head -n1)
  if [ -z "$ctl" ] || [ -z "$data" ]; then
    echo "!! ar 兜底失败：解不出 control/data 成员" >&2
    return 1
  fi
  tar -xf "$ctl" -C "$work/ctl" || return 1
  for ms in postinst prerm postrm; do
    [ -f "$HERE/$ms" ] || continue
    install -m 0755 "$HERE/$ms" "$work/ctl/$ms" || return 1
  done
  tar --owner=root --group=root -czf "$work/control.tar.gz" -C "$work/ctl" . || return 1
  rm -f "$o"
  ( cd "$work" && ar r "$o" debian-binary control.tar.gz "$(basename "$data")" ) || return 1
}

if ! build_with_dpkg_deb "$src" "$out"; then
  echo "!! dpkg-deb 路径失败，改用 ar 兜底重打包" >&2
  dump_env >&2
  repack_via_ar "$src" "$out" || { echo "!! ar 兜底也失败" >&2; exit 1; }
  echo "    (ar 兜底成功：控制归档已替换，数据归档原样保留)" >&2
fi
assert_one_cli "$out"
echo "==> $(basename "$out") ($(stat -c %s "$out") bytes, scripts from $HERE)"
