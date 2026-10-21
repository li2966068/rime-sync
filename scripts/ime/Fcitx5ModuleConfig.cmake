# Minimal Fcitx5Module package config, used only on distributions whose fcitx5
# predates Fcitx5Module.cmake (Ubuntu 22.04 / fcitx5 5.0.14 ships neither that
# config nor the per-module public headers).
#
# The component targets are header-only include-dir interfaces: addons are
# resolved at runtime through fcitx5's addon manager (string based
# FCITX_ADDON_DEPENDENCY_LOADER), so no library is linked here.
# scripts/ime/Dockerfile installs this file plus the matching public header
# taken from the fcitx5 source tag that matches the installed version.

get_filename_component(_fcitx5_module_prefix "${CMAKE_CURRENT_LIST_DIR}/../../../.." ABSOLUTE)
if(NOT EXISTS "${_fcitx5_module_prefix}/include/Fcitx5/Core")
  set(_fcitx5_module_prefix "/usr")
endif()

foreach(_fcitx5_comp ${Fcitx5Module_FIND_COMPONENTS})
  if(NOT TARGET Fcitx5::Module::${_fcitx5_comp})
    string(TOLOWER "${_fcitx5_comp}" _fcitx5_comp_lc)
    add_library(Fcitx5::Module::${_fcitx5_comp} INTERFACE IMPORTED)
    set_target_properties(Fcitx5::Module::${_fcitx5_comp} PROPERTIES
      INTERFACE_INCLUDE_DIRECTORIES
        "${_fcitx5_module_prefix}/include/Fcitx5/Module/fcitx-module/${_fcitx5_comp_lc}")
  endif()
  set(Fcitx5Module_${_fcitx5_comp}_FOUND TRUE)
endforeach()

set(Fcitx5Module_FOUND TRUE)
