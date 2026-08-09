cmake_minimum_required(VERSION 3.22)

project(RayTracingDemoNrdBridge LANGUAGES CXX)

# Rust's current windows-msvc target links the dynamic release CRT for both
# debug and release profiles. Keep every C++ static library on that same CRT;
# mixing /MDd with Rust's /MD leaves __imp__CrtDbgReport unresolved.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL" CACHE STRING "" FORCE)

if(NOT WIN32)
    message(FATAL_ERROR "The NRD bridge currently supports D3D12 on Windows only")
endif()

foreach(required_var IN ITEMS NRD_SOURCE_DIR NRI_SOURCE_DIR MATHLIB_SOURCE_DIR SHADERMAKE_SOURCE_DIR D3D12MA_SOURCE_DIR DXC_PATH)
    if(NOT DEFINED ${required_var} OR "${${required_var}}" STREQUAL "")
        message(FATAL_ERROR "${required_var} must point to a local, pre-fetched dependency")
    endif()
endforeach()

# Every source directory is supplied by scripts/fetch_nrd.ps1.  These settings
# make an accidental FetchContent network access a configuration error instead
# of a hidden build side effect.
set(FETCHCONTENT_FULLY_DISCONNECTED ON CACHE BOOL "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_NRI "${NRI_SOURCE_DIR}" CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_MATHLIB "${MATHLIB_SOURCE_DIR}" CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_SHADERMAKE "${SHADERMAKE_SOURCE_DIR}" CACHE PATH "" FORCE)
set(FETCHCONTENT_SOURCE_DIR_D3D12MA "${D3D12MA_SOURCE_DIR}" CACHE PATH "" FORCE)

set(NRD_STATIC_LIBRARY ON CACHE BOOL "" FORCE)
set(NRD_NRI ON CACHE BOOL "" FORCE)
set(NRD_EMBEDS_DXIL_SHADERS ON CACHE BOOL "" FORCE)
set(NRD_EMBEDS_DXBC_SHADERS OFF CACHE BOOL "" FORCE)
set(NRD_EMBEDS_SPIRV_SHADERS OFF CACHE BOOL "" FORCE)
set(NRD_NORMAL_ENCODING "2" CACHE STRING "" FORCE)
set(NRD_ROUGHNESS_ENCODING "1" CACHE STRING "" FORCE)
set(NRD_SHADERS_PATH "${CMAKE_BINARY_DIR}/nrd_shaders" CACHE PATH "" FORCE)

set(NRI_STATIC_LIBRARY ON CACHE BOOL "" FORCE)
set(NRI_ENABLE_D3D12_SUPPORT ON CACHE BOOL "" FORCE)
set(NRI_ENABLE_D3D11_SUPPORT OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_VK_SUPPORT OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_NONE_SUPPORT OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_NVTX_SUPPORT OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_NVAPI OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_AMDAGS OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_AGILITY_SDK_SUPPORT OFF CACHE BOOL "" FORCE)
set(NRI_ENABLE_VALIDATION_SUPPORT ON CACHE BOOL "" FORCE)

set(SHADERMAKE_TOOL OFF CACHE BOOL "" FORCE)
set(SHADERMAKE_FIND_COMPILERS OFF CACHE BOOL "" FORCE)
set(SHADERMAKE_FIND_DXC OFF CACHE BOOL "" FORCE)
set(SHADERMAKE_FIND_DXC_VK OFF CACHE BOOL "" FORCE)
set(SHADERMAKE_FIND_FXC OFF CACHE BOOL "" FORCE)
set(SHADERMAKE_DXC_PATH "${DXC_PATH}" CACHE FILEPATH "" FORCE)
set(SHADERMAKE_DXC_VK_PATH "${DXC_PATH}" CACHE FILEPATH "" FORCE)

add_subdirectory("${NRD_SOURCE_DIR}" nrd EXCLUDE_FROM_ALL)

add_library(nrd_bridge STATIC src/nrd_bridge.cpp)
target_compile_features(nrd_bridge PRIVATE cxx_std_17)
target_compile_definitions(nrd_bridge PRIVATE
    WIN32_LEAN_AND_MEAN
    NOMINMAX
    _CRT_SECURE_NO_WARNINGS
    NRD_BRIDGE_NRD_COMMIT="792eff196afdd350fd9c3f862119017ccb438a0e"
)
target_include_directories(nrd_bridge PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/include"
    "${NRD_SOURCE_DIR}/Include"
    "${NRD_SOURCE_DIR}/Integration"
    "${NRI_SOURCE_DIR}/Include"
)
target_link_libraries(nrd_bridge PRIVATE NRD NRDIntegration NRI)
set_target_properties(nrd_bridge PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib"
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
)

add_executable(nrd_bridge_smoke src/nrd_bridge_smoke.cpp)
target_compile_features(nrd_bridge_smoke PRIVATE cxx_std_17)
target_compile_definitions(nrd_bridge_smoke PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(nrd_bridge_smoke PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(nrd_bridge_smoke PRIVATE nrd_bridge d3d12 dxgi)
set_target_properties(nrd_bridge_smoke PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
)
