# 编译 fcitx5 addon（rimesync.so）用的构建镜像。
#   docker build -t rime-sync-fcitx5-addon-builder:jammy \
#     --build-arg APT_MIRROR=mirrors.tuna.tsinghua.edu.cn packaging/fcitx5-addon/docker
# 由 scripts/ime/build-fcitx5-addon.sh 调用。
ARG BASE_IMAGE=ubuntu:22.04
FROM ${BASE_IMAGE}

# 空 = 用镜像自带的源；国内/慢链路可传 mirrors.tuna.tsinghua.edu.cn 之类。
ARG APT_MIRROR=

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8

# libfcitx5{core,config,utils}-dev：addon 头文件与 CMake 目标 Fcitx5::Core/Config
# qt6-base-dev + libgl-dev/libopengl-dev：Qt6 Core/Widgets（Qt6Gui 需要 OpenGL）
# fcitx5 + dbus + xvfb：容器内加载冒烟测试
RUN if [ -n "${APT_MIRROR}" ]; then \
      for f in $(grep -rl -e 'archive.ubuntu.com/ubuntu' -e 'security.ubuntu.com/ubuntu' \
                        -e 'deb.debian.org/debian' /etc/apt 2>/dev/null); do \
        sed -i -e "s|//archive.ubuntu.com/ubuntu|//${APT_MIRROR}/ubuntu|g" \
               -e "s|//security.ubuntu.com/ubuntu|//${APT_MIRROR}/ubuntu|g" \
               -e "s|//deb.debian.org/debian|//${APT_MIRROR}/debian|g" "$f"; \
      done; \
    fi \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
         build-essential cmake pkg-config gettext \
         libfcitx5core-dev libfcitx5config-dev libfcitx5utils-dev \
         qt6-base-dev libgl-dev libopengl-dev \
         fcitx5 dbus dbus-x11 xvfb libglib2.0-bin \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
