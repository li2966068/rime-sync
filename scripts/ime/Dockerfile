# Builder image for the vendored Rime input method.
# Used by scripts/ime/build-linux.sh; the base image is selectable:
#   docker build -t rime-ime-builder --build-arg BASE_IMAGE=ubuntu:22.04 scripts/ime
ARG BASE_IMAGE=ubuntu:22.04
FROM ${BASE_IMAGE}

# Optional apt mirror base URL, e.g. http://mirrors.aliyun.com. Rewrites both
# the classic sources.list and the deb822 .sources files used by Ubuntu >= 24.04
# and Debian >= 12. Empty = keep the image's official sources.
ARG APT_MIRROR=

# Some build hosts (and buildkit's host-network namespace) resolve AAAA but have
# no IPv6 route, so apt fails with "Network is unreachable". Non-empty forces
# apt to IPv4; scripts/ime/build-linux.sh enables it when the host has no IPv6
# default route.
ARG APT_FORCE_IPV4=

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8

# The Rime Sync config page is a fcitx5 SharedLibrary addon (rimesync.so) that
# uses Qt Core for its JSON access and Fcitx5Config for its option tree. It is
# built by scripts/ime/container-build.sh and shipped in the IME deb.
#
# Qt major: qtbase5-dev is preferred because it is what every supported
# distribution ships for fcitx5-config-qt and it pulls a far smaller runtime
# closure than Qt6; qt6-base-dev is the fallback for images without Qt5.
# libglib2.0-bin provides gdbus, used by the in-container load smoke test.
RUN set -eux; \
    if [ -n "$APT_MIRROR" ]; then \
      for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do \
        [ -f "$f" ] || continue; \
        sed -i -E \
          -e "s#https?://(archive\.ubuntu\.com|security\.ubuntu\.com)/ubuntu#$APT_MIRROR/ubuntu#g" \
          -e "s#https?://deb\.debian\.org/debian-security#$APT_MIRROR/debian-security#g" \
          -e "s#https?://security\.debian\.org/debian-security#$APT_MIRROR/debian-security#g" \
          -e "s#https?://deb\.debian\.org/debian#$APT_MIRROR/debian#g" \
          -e "s#https?://security\.debian\.org/debian#$APT_MIRROR/debian#g" \
          "$f"; \
      done; \
    fi; \
    if [ -n "$APT_FORCE_IPV4" ]; then \
      echo 'Acquire::ForceIPv4 "true";' >/etc/apt/apt.conf.d/99force-ipv4; \
    fi; \
    apt-get update; \
    QT_DEV=qtbase5-dev; \
    QT_CAND=$(apt-cache policy qtbase5-dev | sed -n 's/^ *Candidate: *//p'); \
    case "$QT_CAND" in ""|"(none)") QT_DEV=qt6-base-dev ;; esac; \
    echo "Qt development package: $QT_DEV"; \
    apt-get install -y --no-install-recommends \
      build-essential cmake pkg-config gettext extra-cmake-modules \
      libboost-all-dev \
      libopencc-dev libyaml-cpp-dev libleveldb-dev libgoogle-glog-dev libmarisa-dev \
      libfcitx5core-dev libfcitx5config-dev libfcitx5utils-dev \
      fcitx5 fcitx5-modules \
      xvfb dbus-x11 libglib2.0-bin ca-certificates \
      "$QT_DEV"; \
    rm -rf /var/lib/apt/lists/*; \
    dpkg-query -W -f='  ${Package} ${Version}\n' "$QT_DEV" libfcitx5config-dev

# fcitx5 5.0.14 (Ubuntu 22.04) installs neither Fcitx5Module.cmake nor the
# per-module public headers that fcitx5-rime includes (notifications_public.h,
# dbus_public.h). Newer fcitx5 releases still do not install those headers, so
# drop in a header-only cmake shim plus the two public headers. The headers are
# byte-identical between fcitx5 5.0.14 and 5.1.7 and are vendored under
# scripts/ime/include/ (LGPL-2.1-or-later, copied from the fcitx5 source tree);
# keeping them local makes the build reproducible and avoids raw.githubusercontent
# hangs. Distributions that already ship Fcitx5Module are left untouched.
COPY Fcitx5ModuleConfig.cmake /tmp/Fcitx5ModuleConfig.cmake
COPY include/ /tmp/fcitx5-module-include/
RUN set -eux; \
    if ls /usr/lib/*/cmake/Fcitx5Module/Fcitx5ModuleConfig.cmake >/dev/null 2>&1; then \
      echo "system fcitx5 provides Fcitx5Module; no shim needed"; \
    else \
      MULTIARCH=$(dpkg-architecture -qDEB_HOST_MULTIARCH); \
      install -d /usr/include/Fcitx5/Module/fcitx-module/notifications \
                 /usr/include/Fcitx5/Module/fcitx-module/dbus; \
      cp /tmp/fcitx5-module-include/notifications_public.h \
         /usr/include/Fcitx5/Module/fcitx-module/notifications/; \
      cp /tmp/fcitx5-module-include/dbus_public.h \
         /usr/include/Fcitx5/Module/fcitx-module/dbus/; \
      install -d "/usr/lib/$MULTIARCH/cmake/Fcitx5Module"; \
      cp /tmp/Fcitx5ModuleConfig.cmake "/usr/lib/$MULTIARCH/cmake/Fcitx5Module/"; \
    fi

WORKDIR /work
