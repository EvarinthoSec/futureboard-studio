# Futureboard's prebuilt Crashpad libraries use the dynamic MSVC CRT (/MD).
# The CEF SDK defaults its wrapper to /MT, which cannot be linked into the same
# process. This file is supplied by xtask as a target-specific CMake toolchain
# for the CEF wrapper build.

set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL" CACHE STRING
    "Use the dynamic MSVC runtime" FORCE)
set(CEF_RUNTIME_LIBRARY_FLAG "/MD" CACHE STRING
    "Use the dynamic MSVC runtime for CEF" FORCE)
