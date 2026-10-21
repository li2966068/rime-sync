# fcitx5 module public headers（vendored）

`notifications_public.h` 与 `dbus_public.h` 来自 fcitx5 源码树
（https://github.com/fcitx/fcitx5 ，LGPL-2.1-or-later）：

- `src/modules/notifications/notifications_public.h`
- `src/modules/dbus/dbus_public.h`

librime 的 fcitx5 插件（third_party/fcitx5-rime）在 CMake 里
`find_package(Fcitx5Module REQUIRED COMPONENTS Notifications DBus)` 并直接 include 这两个
头文件，但发行版的 `libfcitx5core-dev` 并不安装它们（5.0.14 和 5.1.7 都不装）。
Dockerfile 在没有 Fcitx5Module.cmake 的发行版上把它们装到
`/usr/include/Fcitx5/Module/fcitx-module/`。

两个版本（5.0.14 与 5.1.7）的这两个头文件逐字节相同，因此只保留一份。把它们放在仓库里
而不是构建时下载，是为了避免 raw.githubusercontent 在部分网络环境下卡住导致构建挂起。
