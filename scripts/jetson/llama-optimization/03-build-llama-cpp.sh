#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 03-build-llama-cpp.sh — clone/update and build llama.cpp with CUDA, tuned
# for the Jetson Orin Nano (Ampere, sm_87). Idempotent & ccache-accelerated.
#
# Why these flags:
#   GGML_CUDA=ON                 enable the CUDA backend (uses cuBLAS + custom
#                                kernels; needs NO cuDNN/TensorRT)
#   CMAKE_CUDA_ARCHITECTURES=87  Orin = compute capability 8.7 -> build ONLY
#                                sm_87 (smaller, faster build, optimal code)
#   GGML_NATIVE=ON               tune CPU code for this Cortex-A78AE
#   GGML_CUDA_F16=ON             FP16 path — Ampere tensor cores like it
#   LLAMA_CURL=ON                enables `-hf user/repo` model downloads
#   ccache launchers             ~instant rebuilds after the first
#
# Env overrides: LLAMA_DIR, JOBS, CUDA_ARCH, LLAMA_REPO, LLAMA_REF, BIN_DIR,
#                CUDA_F16=OFF
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson
ensure_cuda_on_path || die "nvcc not found. Is the CUDA toolkit installed? (expected /usr/local/cuda)"

LLAMA_REPO="${LLAMA_REPO:-https://github.com/ggml-org/llama.cpp.git}"
LLAMA_REF="${LLAMA_REF:-master}"
CUDA_F16="${CUDA_F16:-ON}"
REPORT_DIR="$GIAP_OPT_ROOT/reports"; mkdir -p "$REPORT_DIR"
BUILD_LOG="$REPORT_DIR/build-$(date +%Y%m%d-%H%M%S).log"

# --- preflight -------------------------------------------------------------
ensure_local_bin_on_path
for t in git nvcc; do have "$t" || die "$t missing — run ./01-install-deps.sh first."; done
# CMake MUST be >= 3.27 or the CUDA build dies on arm_neon.h. Auto-fix if we can.
if ! cmake_version_ok 3 27; then
  warn "CMake $(cmake --version 2>/dev/null | awk 'NR==1{print $3}') is too old for the CUDA build — upgrading via pip..."
  pip_upgrade_cmake || true
  cmake_version_ok 3 27 || die "CMake still <3.27 (have $(command -v cmake)). Run ./01-install-deps.sh or: python3 -m pip install --user --upgrade cmake"
fi
ok "Using cmake $(cmake --version | awk 'NR==1{print $3}') ($(command -v cmake))"
have ninja  || warn "ninja absent — using the Make generator (fine, just slower parallelism)."
have ccache || warn "ccache absent — first build only; rebuilds would be cached with it."
dpkg -s libcurl4-openssl-dev >/dev/null 2>&1 || warn "libcurl4-openssl-dev missing — '-hf' downloads need it (01-install-deps.sh)."

hdr "Fetch llama.cpp source"
if [ -d "$LLAMA_DIR/.git" ]; then
  log "Updating existing checkout in $LLAMA_DIR"
  if git -C "$LLAMA_DIR" fetch --depth=1 origin "$LLAMA_REF"; then
    git -C "$LLAMA_DIR" checkout -f "$LLAMA_REF" && git -C "$LLAMA_DIR" reset --hard "origin/$LLAMA_REF" \
      || git -C "$LLAMA_DIR" reset --hard "$LLAMA_REF"
  else
    warn "git fetch failed (offline?) — building the existing checkout as-is."
  fi
else
  log "Cloning $LLAMA_REPO (ref: $LLAMA_REF) -> $LLAMA_DIR"
  git clone --depth=1 --branch "$LLAMA_REF" "$LLAMA_REPO" "$LLAMA_DIR" 2>/dev/null \
    || git clone "$LLAMA_REPO" "$LLAMA_DIR"
fi
COMMIT="$(git -C "$LLAMA_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
ok "Source at commit $COMMIT"

hdr "Configure (CUDA sm_${CUDA_ARCH})"
# NB: NO -DGGML_CUDA_FA_ALL_QUANTS=ON — it balloons nvcc peak RAM and is the
# likeliest cause of a compile-time OOM-kill on this 8GB shared-memory board.
# Flash-attention kernels are still compiled in without it.
build_dir="$LLAMA_DIR/build"
configure() {
  local native="$1"; shift
  # Wipe the whole build dir, not just CMakeCache.txt: a prior Make-generator
  # configure would otherwise clash with -G Ninja ("generator does not match").
  rm -rf "$build_dir" 2>/dev/null || true
  local args=(
    -S "$LLAMA_DIR" -B "$build_dir"
    -DCMAKE_BUILD_TYPE=Release
    -DGGML_CUDA=ON
    -DCMAKE_CUDA_ARCHITECTURES="$CUDA_ARCH"   # bare digits 87 == cc 8.7
    -DGGML_CUDA_F16="$CUDA_F16"
    -DLLAMA_CURL=ON
    -DLLAMA_BUILD_TESTS=OFF
    -DGGML_NATIVE="$native"
  )
  [ "$native" = "OFF" ] && args+=(-DGGML_CPU_ARM_ARCH=armv8.2-a+fp16)
  have ninja  && args+=(-G Ninja)
  have ccache && args+=(-DCMAKE_C_COMPILER_LAUNCHER=ccache -DCMAKE_CXX_COMPILER_LAUNCHER=ccache -DCMAKE_CUDA_COMPILER_LAUNCHER=ccache)
  log "cmake ${args[*]}"
  cmake "${args[@]}" 2>&1 | tee "$BUILD_LOG"
  return "${PIPESTATUS[0]}"
}
configure ON || die "cmake configure failed (see $BUILD_LOG)"

hdr "Build (-j$JOBS)"
warn "First CUDA build takes a while (~10-25 min on Orin Nano). Logged to $BUILD_LOG"
warn "If the compiler gets OOM-killed (check 'dmesg'), re-run with JOBS=2."
BUILT_BIN="$build_dir/bin"
do_build() { cmake --build "$build_dir" --config Release -j"$JOBS" 2>&1 | tee -a "$BUILD_LOG"; }
do_build
# Cortex-A78AE CPU-feature probe can trip GGML_NATIVE; retry with it OFF.
if [ ! -x "$BUILT_BIN/llama-cli" ] && grep -qiE 'GGML_NATIVE|arm.*arch|unsupported|march' "$BUILD_LOG"; then
  warn "Native build failed — retrying with GGML_NATIVE=OFF (armv8.2-a+fp16)…"
  configure OFF && do_build
fi
[ -x "$BUILT_BIN/llama-cli" ] || die "Build failed: llama-cli not produced (see $BUILD_LOG)."

hdr "Install binaries -> $BIN_DIR"
mkdir -p "$BIN_DIR"
for b in llama-cli llama-server llama-bench llama-quantize; do
  [ -x "$BUILT_BIN/$b" ] && ln -sf "$BUILT_BIN/$b" "$BIN_DIR/$b" && ok "linked $b"
done
# Ensure ~/.local/bin is on PATH for future shells.
if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
  if ! grep -qs "$BIN_DIR" "$HOME/.bashrc"; then
    printf '\nexport PATH="%s:$PATH"\n' "$BIN_DIR" >> "$HOME/.bashrc"
    warn "Added $BIN_DIR to PATH in ~/.bashrc — run 'source ~/.bashrc' or re-login."
  fi
  export PATH="$BIN_DIR:$PATH"
fi

hdr "Verify CUDA linkage"
LINKS="$(ldd "$BUILT_BIN/llama-cli" 2>/dev/null)"
if echo "$LINKS" | grep -qiE 'libcublas|libcudart|libcuda'; then
  ok "llama-cli links the expected CUDA libs:"
  echo "$LINKS" | grep -iE 'cublas|cudart|libcuda' | sed 's/^/    /'
else
  warn "Could not confirm CUDA linkage — check $BUILD_LOG for 'GGML_CUDA'."
fi
# Sanity: it must NOT link cuDNN/TensorRT (llama.cpp uses neither).
if echo "$LINKS" | grep -qiE 'cudnn|nvinfer'; then
  warn "Unexpected: links cuDNN/TensorRT (not used by llama.cpp)."
else
  ok "Confirmed: no cuDNN / TensorRT in the link graph (as expected)."
fi
echo "llama-cli version: $("$BUILT_BIN/llama-cli" --version 2>&1 | head -1)"
echo
ok "Build complete (commit $COMMIT). Next: ./04-download-model.sh then ./05-benchmark.sh"
