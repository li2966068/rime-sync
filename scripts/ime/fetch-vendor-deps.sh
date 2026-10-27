#!/usr/bin/env bash
# fetch-vendor-deps.sh -- download the offline runtime dependency closure for
# one or more distribution builds of the input method + sync app.
#
#   scripts/ime/fetch-vendor-deps.sh                       # trixie (if built)
#   scripts/ime/fetch-vendor-deps.sh debian-13
#   APT_MIRROR=http://mirrors.aliyun.com scripts/ime/fetch-vendor-deps.sh debian-13
#
# For each distribution this resolves the runtime dependencies of
#   build/deb/<distro>/rime-inputmethod_*.deb   (our self-built librime/addon)
#   packaging/out/rime-sync_*.deb               (or SYNC_DEB=...)
# inside the matching container, downloads every package apt would need (and
# its transitive dependencies) with "apt-get --download-only", filters out the
# packages we deliberately replace, and writes:
#
#   packaging/linux/vendor/<distro>/*.deb
#   packaging/linux/vendor/<distro>/VENDOR-INFO
#   packaging/linux/vendor/<distro>/SHA256SUMS
#   packaging/linux/vendor/<distro>/DEPS.txt
#
# Deliberately excluded (we ship our own copies / they conflict):
#   librime1, librime1t64, librime-data, fcitx5-rime
# fcitx5 (the daemon) IS included: without it the addon has nothing to load it,
# and an offline install would not actually provide a working input method.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../.." && pwd)

VENDOR_ROOT=$REPO_ROOT/packaging/linux/vendor
SYNC_DEB=${SYNC_DEB:-}
APT_MIRROR=${APT_MIRROR:-}
case "$APT_MIRROR" in
  "") : ;;
  http://*|https://*) : ;;
  *) APT_MIRROR="http://$APT_MIRROR" ;;
esac

EXCLUDE=${EXCLUDE:-"librime1 librime1t64 librime-data fcitx5-rime rime-inputmethod rime-sync"}
# The daemon itself is a root package, not a dependency of our debs.
ROOT_PKGS=${ROOT_PKGS:-"fcitx5"}

image_for() {
  case "$1" in
    debian-13) echo debian:13 ;;
    *) return 1 ;;
  esac
}

DISTROS=("$@")
if [ "${#DISTROS[@]}" -eq 0 ]; then
  DISTROS=()
  for d in debian-13; do
    compgen -G "$REPO_ROOT/build/deb/$d/rime-inputmethod_*.deb" >/dev/null && DISTROS+=("$d")
  done
  [ "${#DISTROS[@]}" -gt 0 ] || {
    echo "!! no built IME debs found under build/deb/*/; run scripts/ime/build-linux.sh ALL=1 first" >&2
    exit 1
  }
fi

if [ -z "$SYNC_DEB" ]; then
  SYNC_DEB=$(ls -1t "$REPO_ROOT"/packaging/out/rime-sync_*.deb 2>/dev/null | head -n 1 || true)
fi
[ -n "$SYNC_DEB" ] && [ -f "$SYNC_DEB" ] || {
  echo "!! rime-sync .deb not found (set SYNC_DEB=/path/to/rime-sync_*.deb)" >&2
  exit 1
}
SYNC_DEB=$(cd "$(dirname "$SYNC_DEB")" && pwd)/$(basename "$SYNC_DEB")

for distro in "${DISTROS[@]}"; do
  image=$(image_for "$distro") || { echo "!! unknown distro key '$distro'" >&2; exit 2; }
  ime_deb=$(ls -1t "$REPO_ROOT/build/deb/$distro"/rime-inputmethod_*.deb 2>/dev/null | head -n 1 || true)
  [ -n "$ime_deb" ] && [ -f "$ime_deb" ] || {
    echo "!! $distro: no rime-inputmethod deb under build/deb/$distro/" >&2
    exit 1
  }
  vendor=$VENDOR_ROOT/$distro
  mkdir -p "$vendor"
  rm -f "$vendor"/*.deb "$vendor/VENDOR-INFO" "$vendor/SHA256SUMS" "$vendor/DEPS.txt"

  echo "==> $distro ($image)"
  echo "    ime : $(basename "$ime_deb")"
  echo "    sync: $(basename "$SYNC_DEB")"

  docker run --rm --network host \
    -v "$vendor:/out" \
    -v "$(dirname "$ime_deb"):/in-ime:ro" \
    -v "$(dirname "$SYNC_DEB"):/in-sync:ro" \
    -e IME_DEB=/in-ime/$(basename "$ime_deb") \
    -e SYNC_DEB=/in-sync/$(basename "$SYNC_DEB") \
    -e EXPECTED_DISTRO="$distro" \
    -e APT_MIRROR="$APT_MIRROR" \
    -e EXCLUDE="$EXCLUDE" \
    -e ROOT_PKGS="$ROOT_PKGS" \
    "$image" bash -euxo pipefail -c '
      export DEBIAN_FRONTEND=noninteractive
      . /etc/os-release
      case "$EXPECTED_DISTRO" in
        ubuntu-*) [ "$ID" = ubuntu ] || { echo "wrong image: $ID" >&2; exit 2; } ;;
        debian-*) [ "$ID" = debian ] || { echo "wrong image: $ID" >&2; exit 2; } ;;
      esac

      if [ -n "$APT_MIRROR" ]; then
        for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
          [ -f "$f" ] || continue
          sed -i -E \
            -e "s#https?://(archive\.ubuntu\.com|security\.ubuntu\.com)/ubuntu#$APT_MIRROR/ubuntu#g" \
            -e "s#https?://deb\.debian\.org/debian-security#$APT_MIRROR/debian-security#g" \
            -e "s#https?://security\.debian\.org/debian-security#$APT_MIRROR/debian-security#g" \
            -e "s#https?://deb\.debian\.org/debian#$APT_MIRROR/debian#g" \
            -e "s#https?://security\.debian\.org/debian#$APT_MIRROR/debian#g" "$f"
        done
      fi
      apt-get update -qq

      # Runtime deps of both local debs; strip version constraints and keep the
      # first alternative of "a | b".
      {
        dpkg-deb -f "$IME_DEB" Depends
        dpkg-deb -f "$SYNC_DEB" Depends
      } | tr "," "\n" | sed -E "s/\(.*//; s/\|.*//; s/[[:space:]]+//g" \
        | grep -v "^$" | sort -u > /tmp/deps.txt

      req=/tmp/req.txt
      cp /tmp/deps.txt "$req"
      for p in $ROOT_PKGS; do echo "$p" >> "$req"; done
      sort -u -o "$req" "$req"
      for p in $EXCLUDE; do grep -vx "$p" "$req" > "$req.tmp" || true; mv "$req.tmp" "$req"; done

      echo "--- requested packages ---"; cat "$req"
      mkdir -p /tmp/dl/partial
      apt-get install -y --no-install-recommends --download-only \
        -o Dir::Cache::archives=/tmp/dl -o Debug::NoLocking=1 $(cat "$req")

      mkdir -p /out
      exclude_re=" $(printf "%s " $EXCLUDE) "
      n=0
      for d in /tmp/dl/*.deb; do
        [ -f "$d" ] || continue
        pkg=$(dpkg-deb -f "$d" Package)
        case "$exclude_re" in *" $pkg "*) echo "skip $pkg"; continue ;; esac
        cp -f "$d" /out/
        n=$((n + 1))
      done
      echo "==> copied $n debs, $(du -sh /out | cut -f1)"

      {
        echo "# rime-sync offline dependency set, generated by scripts/ime/fetch-vendor-deps.sh"
        echo "distro=$ID"
        echo "version_id=$VERSION_ID"
        echo "codename=${VERSION_CODENAME:-unknown}"
        echo "distro_key=$EXPECTED_DISTRO"
        echo "generated=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "ime_deb=$(basename "$IME_DEB")"
        echo "sync_deb=$(basename "$SYNC_DEB")"
        echo "root_packages=$ROOT_PKGS"
        echo "excluded=$EXCLUDE"
        echo "package_count=$n"
        echo "---requested---"
        cat /tmp/deps.txt
        echo "---packages---"
        for d in /out/*.deb; do
          printf "%s %s\n" "$(dpkg-deb -f "$d" Package)" "$(dpkg-deb -f "$d" Version)"
        done | sort
      } > /out/VENDOR-INFO
      ( cd /out && sha256sum *.deb > SHA256SUMS )
      cp /tmp/deps.txt /out/DEPS.txt
    '

  echo "==> $distro done: $(find "$vendor" -name '*.deb' | wc -l) debs, $(du -sh "$vendor" | cut -f1)"
done

echo "==> all requested distributions done"
