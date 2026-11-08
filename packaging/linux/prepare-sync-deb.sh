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
dpkg-deb -R "$src" "$tmp/root"
for s in postinst prerm postrm; do
  [ -f "$HERE/$s" ] || continue
  install -m 0755 "$HERE/$s" "$tmp/root/DEBIAN/$s"
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
dpkg-deb --root-owner-group -Zxz --build "$tmp/root" "$out" >/dev/null
assert_one_cli "$out"
echo "==> $(basename "$out") ($(stat -c %s "$out") bytes, scripts from $HERE)"
