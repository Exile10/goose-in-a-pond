#!/usr/bin/env bash
# build-jetson-cuda.sh — Native CUDA build for Jetson Orin Nano
#
# Run this script ON THE JETSON (not cross-compiled from x86).
# nvcc must be on PATH and CMAKE_CUDA_ARCHITECTURES must target sm_87
# (Ampere — the GA10B GPU in the Jetson Orin Nano).
#
# JetPack 6.2 ships CUDA 12.6 at /usr/local/cuda.
# Install required packages first:
#   sudo apt-get install -y pkg-config libssl-dev libasound2-dev libdbus-1-dev
#
# Why CUDA cannot be cross-compiled:
#   nvcc compiles device code for the target GPU. It must run on the same
#   architecture and needs the CUDA runtime libraries for that architecture.
#   Cross-compiling CUDA kernels (x86 → aarch64) is not supported by
#   llama-cpp-2's CMake build system. Use `scripts/deploy-jetson.sh` to
#   deploy the non-CUDA binary via cross-compilation, then build the
#   CUDA variant natively on the Jetson for the `local-inference` feature.

set -euo pipefail

CUDA_PATH="${CUDA_PATH:-/usr/local/cuda}"
CUDACXX="${CUDACXX:-${CUDA_PATH}/bin/nvcc}"

if ! command -v "${CUDACXX}" &>/dev/null && ! [ -x "${CUDACXX}" ]; then
    echo "ERROR: nvcc not found at ${CUDACXX}" >&2
    echo "       Install CUDA toolkit: sudo apt-get install cuda-toolkit-12-6" >&2
    exit 1
fi

CUDA_VERSION=$("${CUDACXX}" --version 2>&1 | grep -oP 'release \K[0-9]+\.[0-9]+' | head -1)
echo "Building with CUDA ${CUDA_VERSION} (nvcc: ${CUDACXX})"
echo "Target: aarch64 / sm_87 (Jetson Orin Nano — Ampere GA10B)"

export CUDA_PATH
export CUDACXX
# Ampere architecture — Jetson Orin Nano compute capability 8.7
export CMAKE_CUDA_ARCHITECTURES="87"
export LD_LIBRARY_PATH="${CUDA_PATH}/lib64${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"

cargo build \
    -p pond-server \
    --features "local-inference" \
    --release \
    "$@"

echo ""
echo "Build complete: target/release/pond-server"
echo ""
echo "Run with:"
echo "  ./target/release/pond-server chat --provider local"
