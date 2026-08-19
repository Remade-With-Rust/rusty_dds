# Copyright (c) 2026 Remade With Rust / Mata Network
# Static C ABI shim over Microsoft DirectXTex, linked into rusty_dds_sim as the
# `dxtex` arm. Mirrors the settings of tools/dxtex_decode_bench so the peer in
# the simulator is the same peer the encode/decode boards already use.
cmake_minimum_required(VERSION 3.21)
project(dxtex_provider LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Rust's MSVC targets link the CRT dynamically; match it or the link fails.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")

set(DIRECTXTEX_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../third_party/DirectXTex"
    CACHE PATH "DirectXTex source root")

if(NOT EXISTS "${DIRECTXTEX_ROOT}/CMakeLists.txt")
  message(FATAL_ERROR
    "DirectXTex not found at ${DIRECTXTEX_ROOT}. Run:\n"
    "  git clone --depth 1 https://github.com/microsoft/DirectXTex.git third_party/DirectXTex")
endif()

set(BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(BUILD_SAMPLE OFF CACHE BOOL "" FORCE)
set(BUILD_DX11 OFF CACHE BOOL "" FORCE)
set(BUILD_DX12 OFF CACHE BOOL "" FORCE)
set(BC_USE_OPENMP OFF CACHE BOOL "" FORCE)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)

add_subdirectory("${DIRECTXTEX_ROOT}" "${CMAKE_BINARY_DIR}/DirectXTex" EXCLUDE_FROM_ALL)

add_library(dxtex_provider STATIC dxtex_provider.cpp)
target_link_libraries(dxtex_provider PRIVATE DirectXTex)
target_compile_definitions(dxtex_provider PRIVATE UNICODE _UNICODE)
set_target_properties(dxtex_provider PROPERTIES
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib")
