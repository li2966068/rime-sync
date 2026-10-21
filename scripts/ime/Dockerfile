# Builder image for the vendored Rime input method.
# Used by scripts/ime/build-linux.sh; the base image is selectable:
#   docker build -t rime-ime-builder --build-arg BASE_IMAGE=ubuntu:22.04 scripts/ime
ARG BASE_IMAGE=ubuntu:22.04
FROM ${BASE_IMAGE}

# Optional apt mirror base URL, e.g. http://mirrors.aliyun.com. Rewrites both
# the classic sources.list and the deb822 .sources files used by Ubuntu >= 24.04
# and Debian >= 12. Empty = keep the image's official sources.
ARG APT_MIRROR=

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8

RUN if [ -n "$APT_MIRROR" ]; then \
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
    fi \
    && apt-get update && apt-get install -y --no-install-recommends \
      build-essential cmake pkg-config gettext extra-cmake-modules \
      libboost-all-dev \
      libopencc-dev libyaml-cpp-dev libleveldb-dev libgoogle-glog-dev libmarisa-dev \
      libfcitx5core-dev \
      fcitx5 fcitx5-modules \
      xvfb dbus-x11 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

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
