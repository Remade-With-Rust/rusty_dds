# Copyright (c) 2026 Remade With Rust / Mata Network
# Benchmark harness: Microsoft DirectXTex Decompress vs rusty_dds cases.
cmake_minimum_required(VERSION 3.21)
project(dxtex_decode_bench LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(DIRECTXTEX_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../third_party/DirectXTex" CACHE PATH "DirectXTex source root")

if(NOT EXISTS "${DIRECTXTEX_ROOT}/CMakeLists.txt")
  message(FATAL_ERROR "DirectXTex not found at ${DIRECTXTEX_ROOT}. Run: git clone --depth 1 https://github.com/microsoft/DirectXTex.git third_party/DirectXTex")
endif()

set(BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(BUILD_SAMPLE OFF CACHE BOOL "" FORCE)
set(BUILD_DX11 OFF CACHE BOOL "" FORCE)
set(BUILD_DX12 OFF CACHE BOOL "" FORCE)
set(BC_USE_OPENMP OFF CACHE BOOL "" FORCE)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)

add_subdirectory("${DIRECTXTEX_ROOT}" "${CMAKE_BINARY_DIR}/DirectXTex" EXCLUDE_FROM_ALL)

add_executable(dxtex_decode_bench main.cpp)
target_link_libraries(dxtex_decode_bench PRIVATE DirectXTex)
target_compile_definitions(dxtex_decode_bench PRIVATE UNICODE _UNICODE)

add_executable(dxtex_encode_bench encode_main.cpp)
target_link_libraries(dxtex_encode_bench PRIVATE DirectXTex)
target_compile_definitions(dxtex_encode_bench PRIVATE UNICODE _UNICODE)

add_executable(dxtex_roundtrip roundtrip_main.cpp)
target_link_libraries(dxtex_roundtrip PRIVATE DirectXTex)
