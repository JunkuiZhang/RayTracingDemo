cmake_minimum_required(VERSION 3.22)

project(RayTracingDemoStreamlineBridge LANGUAGES CXX)

set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL" CACHE STRING "" FORCE)

if(NOT WIN32)
    message(FATAL_ERROR "The Streamline bridge currently supports D3D12 on Windows only")
endif()

foreach(required_var IN ITEMS STREAMLINE_SOURCE_DIR STREAMLINE_LIB_DIR)
    if(NOT DEFINED ${required_var} OR "${${required_var}}" STREQUAL "")
        message(FATAL_ERROR "${required_var} must point to a local Streamline SDK")
    endif()
endforeach()

if(NOT EXISTS "${STREAMLINE_SOURCE_DIR}/include/sl.h")
    message(FATAL_ERROR "Streamline headers are missing from ${STREAMLINE_SOURCE_DIR}")
endif()
if(NOT EXISTS "${STREAMLINE_LIB_DIR}/sl.interposer.lib")
    message(FATAL_ERROR "Streamline import library is missing from ${STREAMLINE_LIB_DIR}")
endif()

add_library(streamline_bridge STATIC src/streamline_bridge.cpp)
target_compile_features(streamline_bridge PRIVATE cxx_std_17)
target_compile_definitions(streamline_bridge PRIVATE
    WIN32_LEAN_AND_MEAN
    NOMINMAX
    _CRT_SECURE_NO_WARNINGS
)
target_include_directories(streamline_bridge PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/include"
    "${STREAMLINE_SOURCE_DIR}/include"
)
target_link_libraries(streamline_bridge PRIVATE
    "${STREAMLINE_LIB_DIR}/sl.interposer.lib"
    d3d12
    dxgi
)
set_target_properties(streamline_bridge PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
)

add_executable(streamline_bridge_smoke src/streamline_bridge_smoke.cpp)
target_compile_features(streamline_bridge_smoke PRIVATE cxx_std_17)
target_compile_definitions(streamline_bridge_smoke PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(streamline_bridge_smoke PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(streamline_bridge_smoke PRIVATE streamline_bridge)
set_target_properties(streamline_bridge_smoke PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
)
add_custom_command(TARGET streamline_bridge_smoke POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "${STREAMLINE_SOURCE_DIR}/bin/x64/development/sl.interposer.dll"
        "$<TARGET_FILE_DIR:streamline_bridge_smoke>/sl.interposer.dll"
)
