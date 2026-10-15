# Lensbridge Camera — DirectShow virtual camera filter for Windows 10/11.
#
# Build (Developer PowerShell for VS 2022):
#   cmake -S . -B build-x64 -A x64   && cmake --build build-x64 --config Release
#   cmake -S . -B build-x86 -A Win32 && cmake --build build-x86 --config Release
# Register (elevated):  regsvr32 build-x64\Release\lensbridge_vcam.dll
cmake_minimum_required(VERSION 3.20)
project(lensbridge_vcam LANGUAGES CXX)

if(NOT WIN32)
  message(FATAL_ERROR "The DirectShow filter can only be built for Windows")
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
# Static CRT: the DLL is loaded into arbitrary apps, avoid a VC++ redist dependency.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

set(BASECLASSES_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../third_party/baseclasses)
file(GLOB BASECLASSES_SOURCES ${BASECLASSES_DIR}/*.cpp)

add_library(strmbase STATIC ${BASECLASSES_SOURCES})
target_include_directories(strmbase PUBLIC ${BASECLASSES_DIR})
target_compile_definitions(strmbase PUBLIC UNICODE _UNICODE $<$<NOT:$<CONFIG:Debug>>:NDEBUG>)
target_compile_options(strmbase PRIVATE /W1 /wd4996)

add_library(lensbridge_vcam SHARED
  src/dll.cpp
  src/vcam.cpp
  src/frame_source.cpp
  src/lensbridge_vcam.def)
target_link_libraries(lensbridge_vcam PRIVATE strmbase strmiids winmm ole32 oleaut32 uuid gdi32 user32 advapi32)
target_compile_options(lensbridge_vcam PRIVATE /W4 /utf-8 /wd4100)
