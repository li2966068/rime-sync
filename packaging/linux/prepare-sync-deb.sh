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
echo "==> $(basename "$out") ($(stat -c %s "$out") bytes, scripts from $HERE)"
