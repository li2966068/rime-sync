cmake_minimum_required(VERSION 3.16)

project(rime-sync-fcitx5-addon
        VERSION 0.4.0
        DESCRIPTION "rime-sync 的 fcitx5 配置页"
        LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

include(GNUInstallDirs)

option(RIMESYNC_BUILD_FCITX5_ADDON "构建 fcitx5 addon（配置页）" ON)
option(RIMESYNC_BUILD_STANDALONE_CONFIG "构建独立 Qt 配置页（没有 fcitx5-configtool 时的退路）" ON)

# Qt 只做两件事：读写 config.json（addon，仅 Qt Core）和独立配置页（Widgets）。
find_package(QT NAMES Qt6 Qt5 REQUIRED COMPONENTS Core)
find_package(Qt${QT_VERSION_MAJOR} REQUIRED COMPONENTS Core)

# ---------------------------------------------------------------- addon ----
if(RIMESYNC_BUILD_FCITX5_ADDON)
    find_package(Fcitx5Core 5.0 REQUIRED)
    find_package(Fcitx5Config 5.0 REQUIRED)

    add_library(rimesync MODULE
        src/rimesyncaddon.cpp
        src/rimesyncstore.cpp)
    target_link_libraries(rimesync PRIVATE
        Fcitx5::Core
        Fcitx5::Config
        Qt${QT_VERSION_MAJOR}::Core)
    target_include_directories(rimesync PRIVATE src)
    set_target_properties(rimesync PROPERTIES PREFIX "" OUTPUT_NAME "rimesync")

    configure_file(addon/rimesync.conf.in
                   "${CMAKE_CURRENT_BINARY_DIR}/rimesync.conf" @ONLY)

    # fcitx5 的搜索路径：库在 <libdir>/fcitx5，元数据（.conf）在 <datadir>/fcitx5/addon。
    install(TARGETS rimesync LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}/fcitx5)
    install(FILES "${CMAKE_CURRENT_BINARY_DIR}/rimesync.conf"
            DESTINATION ${CMAKE_INSTALL_DATADIR}/fcitx5/addon)

    # 「打开 Rime 同步」按钮指向它：优先 GUI 向导，退回复 CLI。
    install(PROGRAMS scripts/rime-sync-open-setup.sh
            DESTINATION ${CMAKE_INSTALL_BINDIR}
            RENAME rime-sync-open-setup)
endif()

# ------------------------------------------------- 独立 Qt 配置页（可选） ----
if(RIMESYNC_BUILD_STANDALONE_CONFIG)
    find_package(Qt${QT_VERSION_MAJOR} REQUIRED COMPONENTS Widgets)

    add_executable(rime-sync-config
        src/main.cpp
        src/rimesyncconfig.cpp
        src/rimesyncstore.cpp)
    target_link_libraries(rime-sync-config PRIVATE Qt${QT_VERSION_MAJOR}::Widgets Qt${QT_VERSION_MAJOR}::Core)
    target_include_directories(rime-sync-config PRIVATE src)
    install(TARGETS rime-sync-config RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
    install(FILES rime-sync-config.desktop
            DESTINATION ${CMAKE_INSTALL_DATADIR}/applications)
endif()
