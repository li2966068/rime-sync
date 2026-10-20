cmake_minimum_required(VERSION 3.16)

project(rime-sync-fcitx5-addon
        VERSION 0.2.0
        DESCRIPTION "rime-sync 的 fcitx5 配置页与 addon"
        LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

include(GNUInstallDirs)

option(RIMESYNC_BUILD_FCITX5_ADDON "构建 fcitx5 addon 共享库" ON)

find_package(QT NAMES Qt6 Qt5 REQUIRED COMPONENTS Widgets)
find_package(Qt${QT_VERSION_MAJOR} REQUIRED COMPONENTS Widgets)

# 配置页：独立 Qt Widgets 程序（fcitx5 的通用配置渲染器只认识选项树，
# 画不了「打开 Rime 同步」这种动作按钮，所以配置页做成独立窗口）。
add_executable(rime-sync-config
    src/main.cpp
    src/rimesyncconfig.cpp)
target_link_libraries(rime-sync-config PRIVATE Qt${QT_VERSION_MAJOR}::Widgets)
target_include_directories(rime-sync-config PRIVATE src)
install(TARGETS rime-sync-config RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(FILES rime-sync-config.desktop
        DESTINATION ${CMAKE_INSTALL_DATADIR}/applications)

if(RIMESYNC_BUILD_FCITX5_ADDON)
    find_package(Fcitx5Core 5.1 REQUIRED)
    add_library(rimesync MODULE src/rimesyncaddon.cpp)
    target_link_libraries(rimesync PRIVATE Fcitx5::Core)
    target_include_directories(rimesync PRIVATE src)
    set_target_properties(rimesync PROPERTIES PREFIX "" OUTPUT_NAME "rimesync")
    configure_file(addon/rimesync.conf.in
                   "${CMAKE_CURRENT_BINARY_DIR}/rimesync.conf" @ONLY)
    install(TARGETS rimesync LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}/fcitx5)
    install(FILES "${CMAKE_CURRENT_BINARY_DIR}/rimesync.conf"
            DESTINATION ${CMAKE_INSTALL_LIBDIR}/fcitx5)
endif()
