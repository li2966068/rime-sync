# Test-only image: target distro + Xvfb/dbus so the offline install can be
# exercised with --network none (Xvfb is test tooling, not a runtime dependency
# of rime-sync).
ARG BASE_IMAGE=ubuntu:22.04
FROM ${BASE_IMAGE}
ARG APT_MIRROR=
RUN if [ -n "${APT_MIRROR}" ]; then \
      for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do \
        [ -f "${f}" ] || continue; \
        sed -i -E \
          -e "s#https?://(archive\.ubuntu\.com|security\.ubuntu\.com)/ubuntu#${APT_MIRROR}/ubuntu#g" \
          -e "s#https?://deb\.debian\.org/debian-security#${APT_MIRROR}/debian-security#g" \
          -e "s#https?://security\.debian\.org/debian-security#${APT_MIRROR}/debian-security#g" \
          -e "s#https?://deb\.debian\.org/debian#${APT_MIRROR}/debian#g" \
          -e "s#https?://security\.debian\.org/debian#${APT_MIRROR}/debian#g" "${f}"; \
      done; \
    fi \
    && apt-get update \
    && apt-get install -y --no-install-recommends xvfb dbus-x11 x11-utils xkb-data \
    && rm -rf /var/lib/apt/lists/*
