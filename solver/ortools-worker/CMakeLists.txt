cmake_minimum_required(VERSION 3.24)

if(POLICY CMP0144)
  cmake_policy(SET CMP0144 NEW)
endif()

# The checked-in dependency path below is the pinned Apple Silicon archive.
# Explicitly select its architecture even when an x86_64 Homebrew CMake is
# running under Rosetta on an Apple Silicon host.
if(CMAKE_HOST_APPLE AND NOT CMAKE_OSX_ARCHITECTURES)
  set(CMAKE_OSX_ARCHITECTURES arm64 CACHE STRING "Worker target architecture" FORCE)
endif()

project(class_schedule_ortools_worker VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

option(ORTOOLS_WORKER_BUILD_TESTS "Build the worker's CTest suite" ON)

get_filename_component(REPOSITORY_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(
  ORTOOLS_ROOT
  "${REPOSITORY_ROOT}/.cache/ortools/or-tools_arm64_macOS-26.2_cpp_v9.15.6755"
  CACHE PATH
  "Root of the pinned OR-Tools 9.15.6755 C++ distribution"
)

if(NOT EXISTS "${ORTOOLS_ROOT}/lib/cmake/ortools/ortoolsConfig.cmake")
  message(FATAL_ERROR "ORTOOLS_ROOT does not contain the pinned C++ distribution: ${ORTOOLS_ROOT}")
endif()

list(PREPEND CMAKE_PREFIX_PATH "${ORTOOLS_ROOT}")
find_package(ortools 9.15.6755 CONFIG REQUIRED)
find_package(Protobuf CONFIG REQUIRED)

find_program(
  PINNED_PROTOC
  NAMES protoc-33.1.0 protoc
  HINTS "${ORTOOLS_ROOT}/bin"
  NO_DEFAULT_PATH
  REQUIRED
)

set(PROTO_ROOT "${REPOSITORY_ROOT}/crates/solver-contract/proto")
set(PROTO_FILE "${PROTO_ROOT}/scheduler/v1/solver.proto")
set(GENERATED_ROOT "${CMAKE_CURRENT_BINARY_DIR}/generated")
set(GENERATED_HEADER "${GENERATED_ROOT}/scheduler/v1/solver.pb.h")
set(GENERATED_SOURCE "${GENERATED_ROOT}/scheduler/v1/solver.pb.cc")

add_custom_command(
  OUTPUT "${GENERATED_HEADER}" "${GENERATED_SOURCE}"
  COMMAND "${CMAKE_COMMAND}" -E make_directory "${GENERATED_ROOT}/scheduler/v1"
  COMMAND
    "${PINNED_PROTOC}"
    "--proto_path=${PROTO_ROOT}"
    "--cpp_out=${GENERATED_ROOT}"
    "${PROTO_FILE}"
  DEPENDS "${PROTO_FILE}"
  COMMENT "Generating scheduler.v1 C++ protocol with pinned protoc"
  VERBATIM
)

add_library(scheduler_solver_proto STATIC "${GENERATED_SOURCE}")
target_include_directories(scheduler_solver_proto PUBLIC "${GENERATED_ROOT}")
target_link_libraries(scheduler_solver_proto PUBLIC protobuf::libprotobuf)

add_library(
  ortools_worker_core STATIC
  src/frame_io.cpp
  src/sha256.cpp
  src/solver_worker.cpp
)
target_include_directories(
  ortools_worker_core
  PUBLIC
    "${CMAKE_CURRENT_SOURCE_DIR}/include"
)
target_link_libraries(
  ortools_worker_core
  PUBLIC
    scheduler_solver_proto
    ortools::ortools
)
target_compile_definitions(
  ortools_worker_core
  PRIVATE ORTOOLS_WORKER_BUILD_REVISION="${PROJECT_VERSION}"
)
target_compile_options(
  ortools_worker_core
  PRIVATE
    $<$<CXX_COMPILER_ID:AppleClang,Clang,GNU>:-Wall;-Wextra;-Wpedantic;-Wconversion;-Wshadow>
)

add_executable(ortools-scheduler-worker src/main.cpp)
target_link_libraries(ortools-scheduler-worker PRIVATE ortools_worker_core)

# The developer/test binary must find the exact dylibs from the pinned archive.
# Production packaging will copy and sign the same locked dependency set.
if(APPLE)
  set_target_properties(
    ortools-scheduler-worker
    PROPERTIES BUILD_RPATH "${ORTOOLS_ROOT}/lib"
  )
endif()

if(ORTOOLS_WORKER_BUILD_TESTS)
  enable_testing()

  add_executable(ortools-worker-tests tests/worker_tests.cpp)
  target_link_libraries(ortools-worker-tests PRIVATE ortools_worker_core)
  if(APPLE)
    set_target_properties(ortools-worker-tests PROPERTIES BUILD_RPATH "${ORTOOLS_ROOT}/lib")
  endif()
  add_test(NAME ortools-worker-unit-and-model COMMAND ortools-worker-tests)

  add_executable(ortools-worker-protocol-test tests/protocol_integration_test.cpp)
  target_link_libraries(ortools-worker-protocol-test PRIVATE ortools_worker_core)
  if(APPLE)
    set_target_properties(
      ortools-worker-protocol-test
      PROPERTIES BUILD_RPATH "${ORTOOLS_ROOT}/lib"
    )
  endif()
  add_test(
    NAME ortools-worker-one-shot-protocol
    COMMAND ortools-worker-protocol-test $<TARGET_FILE:ortools-scheduler-worker>
  )
endif()
