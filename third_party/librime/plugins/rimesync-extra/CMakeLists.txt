# rimesync-extra: bundled Rime plugin (see ../../PLUGINS.md).
#
# plugins/CMakeLists.txt globs every subdirectory and merges the object files
# into librime when BUILD_MERGED_PLUGINS=ON.  The PARENT_SCOPE variables at the
# bottom are the contract used by that file, mirroring librime's own sample.

option(RIMESYNC_EXTRA_BUILD_TEST
  "Build the rimesync-extra standalone unit tests" ON)

set(rimesync_extra_src
  rimesync_extra_module.cc
  vnum_convert.cc
  vnum_translator.cc
  punct_processor.cc
  vselect_processor.cc
  numpad_key.cc
  numpad_processor.cc)

add_library(rimesync-extra-objs OBJECT ${rimesync_extra_src})
if(BUILD_SHARED_LIBS)
  set_target_properties(rimesync-extra-objs
    PROPERTIES
    POSITION_INDEPENDENT_CODE ON)
endif()
if(MSVC)
  # u8"" literals are UTF-8 by definition; /utf-8 keeps the MSVC source and
  # execution charsets consistent as well.
  target_compile_options(rimesync-extra-objs PRIVATE /utf-8)
endif()

if(RIMESYNC_EXTRA_BUILD_TEST)
  add_executable(rimesync_extra_vnum_test
    test/vnum_convert_test.cc
    vnum_convert.cc)
  target_include_directories(rimesync_extra_vnum_test
    PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})
  add_executable(rimesync_extra_numpad_test
    test/numpad_key_test.cc
    numpad_key.cc)
  target_include_directories(rimesync_extra_numpad_test
    PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})
  add_executable(rimesync_extra_punct_test
    test/punct_table_test.cc)
  target_include_directories(rimesync_extra_punct_test
    PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})
  # The runtime smoke test drives the components through a real librime
  # engine, so it links against the library instead of recompiling the
  # plugin sources.  ${CMAKE_BINARY_DIR}/src carries the generated
  # rime/build_config.h.
  add_executable(rimesync_extra_engine_smoke_test
    test/engine_smoke_test.cc)
  target_include_directories(rimesync_extra_engine_smoke_test
    PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}
            ${PROJECT_SOURCE_DIR}/src
            ${CMAKE_BINARY_DIR}/src)
  target_link_libraries(rimesync_extra_engine_smoke_test PRIVATE ${rime_library})
  if(MSVC)
    target_compile_options(rimesync_extra_vnum_test PRIVATE /utf-8)
    target_compile_options(rimesync_extra_numpad_test PRIVATE /utf-8)
    target_compile_options(rimesync_extra_punct_test PRIVATE /utf-8)
    target_compile_options(rimesync_extra_engine_smoke_test PRIVATE /utf-8)
  endif()
  add_custom_target(check-rimesync-extra
    COMMAND $<TARGET_FILE:rimesync_extra_vnum_test>
    COMMAND $<TARGET_FILE:rimesync_extra_numpad_test>
    COMMAND $<TARGET_FILE:rimesync_extra_punct_test>
    COMMAND $<TARGET_FILE:rimesync_extra_engine_smoke_test>
    DEPENDS rimesync_extra_vnum_test rimesync_extra_numpad_test rimesync_extra_punct_test
            rimesync_extra_engine_smoke_test
    COMMENT "Running rimesync-extra unit tests")
  if(BUILD_TEST)
    add_test(NAME rimesync_extra_vnum_test COMMAND rimesync_extra_vnum_test)
    add_test(NAME rimesync_extra_numpad_test
      COMMAND rimesync_extra_numpad_test)
    add_test(NAME rimesync_extra_punct_test
      COMMAND rimesync_extra_punct_test)
    add_test(NAME rimesync_extra_engine_smoke_test
      COMMAND rimesync_extra_engine_smoke_test)
  endif()
endif()

set(plugin_name rime-rimesync-extra PARENT_SCOPE)
set(plugin_objs $<TARGET_OBJECTS:rimesync-extra-objs> PARENT_SCOPE)
set(plugin_deps ${rime_library} PARENT_SCOPE)
set(plugin_modules rimesync_extra PARENT_SCOPE)
