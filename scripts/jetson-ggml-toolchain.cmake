# CMake toolchain fragment for building llama.cpp/ggml (via llama-cpp-sys-2) for
# the Jetson Orin Nano from an off-device ARM64 Linux container.
#
# Passed through `CMAKE_TOOLCHAIN_FILE` (the `cmake` crate honours it). Its only
# job: stop ggml's GGML_NATIVE auto-detection from probing the BUILD container's
# CPU and emitting an invalid `-march=<cpu-name>`. Instead we pin the Orin's
# baseline ISA explicitly — Cortex-A78AE is armv8.2-a with fp16 NEON + dotprod,
# which is what whisper.cpp/ggml need (FP16_VECTOR_ARITHMETIC) and is portable to
# the device.
set(GGML_NATIVE OFF CACHE BOOL "" FORCE)
set(GGML_CPU_ARM_ARCH "armv8.2-a+fp16+dotprod" CACHE STRING "" FORCE)
