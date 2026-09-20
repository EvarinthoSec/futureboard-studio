# Futureboard's prebuilt Crashpad libraries use the dynamic MSVC CRT (/MD).
# The CEF SDK defaults its wrapper to /MT, which cannot be linked into the same
# process. This file is supplied by xtask as a target-specific CMake toolchain
# for the CEF wrapper build. cc-rs can select clang.exe for an MSVC target and
# pass MSVC plus clang-only flags; CMake must invoke that LLVM tool in clang-cl
# mode so flags such as -nologo and -Xclang are accepted.

find_program(FUTUREBOARD_CLANG_CL NAMES clang-cl.exe clang-cl)
if(FUTUREBOARD_CLANG_CL)
  get_filename_component(FUTUREBOARD_LLVM_BIN "${FUTUREBOARD_CLANG_CL}" DIRECTORY)
  find_program(FUTUREBOARD_MSVC_ARCHIVER
      NAMES llvm-lib.exe llvm-lib lib.exe lib
      HINTS "${FUTUREBOARD_LLVM_BIN}")

  set(CMAKE_C_COMPILER "${FUTUREBOARD_CLANG_CL}" CACHE FILEPATH
      "MSVC-compatible Clang driver for the CEF wrapper" FORCE)
  set(CMAKE_CXX_COMPILER "${FUTUREBOARD_CLANG_CL}" CACHE FILEPATH
      "MSVC-compatible Clang driver for the CEF wrapper" FORCE)
  if(FUTUREBOARD_MSVC_ARCHIVER)
    set(CMAKE_AR "${FUTUREBOARD_MSVC_ARCHIVER}" CACHE FILEPATH
        "MSVC-compatible archiver for the CEF wrapper" FORCE)
    set(CMAKE_C_COMPILER_AR "${FUTUREBOARD_MSVC_ARCHIVER}" CACHE FILEPATH
        "MSVC-compatible C archiver for the CEF wrapper" FORCE)
    set(CMAKE_CXX_COMPILER_AR "${FUTUREBOARD_MSVC_ARCHIVER}" CACHE FILEPATH
        "MSVC-compatible C++ archiver for the CEF wrapper" FORCE)
  endif()

  # CEF adds /MP to each compile. Ninja already parallelizes per-file builds,
  # so clang-cl warns that /MP is unused. Seed target flags because
  # cef_variables.cmake clears CMAKE_CXX_FLAGS for Ninja builds.
  list(APPEND CEF_COMPILER_FLAGS -Wno-unused-command-line-argument)
  set(CEF_COMPILER_FLAGS "${CEF_COMPILER_FLAGS}" CACHE STRING
      "clang-cl compatibility flags for the CEF wrapper" FORCE)

  # Keep CEF's /W4 /WX policy, but account for Clang-only diagnostics emitted
  # by generated CEF headers and template definitions. CEF's CXX-specific
  # flags are applied after its shared /W4 /WX flags.
  list(APPEND CEF_CXX_COMPILER_FLAGS
      -Wno-missing-field-initializers
      -Wno-error=undefined-var-template)
  set(CEF_CXX_COMPILER_FLAGS "${CEF_CXX_COMPILER_FLAGS}" CACHE STRING
      "clang-cl compatibility flags for CEF C++ wrapper sources" FORCE)
endif()

set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL" CACHE STRING
    "Use the dynamic MSVC runtime" FORCE)
set(CEF_RUNTIME_LIBRARY_FLAG "/MD" CACHE STRING
    "Use the dynamic MSVC runtime for CEF" FORCE)
