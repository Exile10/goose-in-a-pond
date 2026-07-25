#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# run-all.sh — end-to-end setup: deps -> performance mode -> build -> model ->
# benchmark. Each step asks first (set ASSUME_YES=1 to skip prompts).
#
#   ./run-all.sh                 # interactive full setup, default model
#   ASSUME_YES=1 ./run-all.sh    # unattended
#   MODEL_PRESET=qwen2.5-3b ./run-all.sh
# ---------------------------------------------------------------------------
set -uo pipefail
cd "$(dirname "$0")" || { echo "cannot cd to script dir" >&2; exit 1; }
source "lib/common.sh" || { echo "cannot source lib/common.sh" >&2; exit 1; }
require_jetson

# confirm() reads stdin; a non-TTY run (piped/CI) would silently skip every step.
if [ ! -t 0 ] && [ "${ASSUME_YES:-0}" != "1" ]; then
  die "Non-interactive stdin detected — set ASSUME_YES=1 to run unattended."
fi

MODEL_PRESET="${MODEL_PRESET:-llama3.2-3b}"

hdr "GIAP Jetson llama.cpp optimization — full run"
echo "  Steps: 01 deps  ->  02 performance mode  ->  03 build llama.cpp"
echo "         ->  04 download model ($MODEL_PRESET)  ->  05 benchmark"
echo "  llama.cpp: $LLAMA_DIR | models: $MODELS_DIR | jobs: $JOBS | sm_$CUDA_ARCH"
echo

step() { # step "title" script args...   (invoked via bash, robust to exec bit)
  local title="$1"; shift
  if confirm ">> Run: $title ?"; then
    bash "$@" || die "Step failed: $title"
  else
    warn "Skipped: $title"
  fi
}

step "00 system info"               ./00-system-info.sh
step "01 install dependencies"      ./01-install-deps.sh
step "02 max-performance mode"      ./02-performance-mode.sh
step "03 build llama.cpp (CUDA)"    ./03-build-llama-cpp.sh
step "04 download model ($MODEL_PRESET)" ./04-download-model.sh "$MODEL_PRESET"
step "05 benchmark"                 ./05-benchmark.sh --pause-ollama

echo
ok "All done. Optional next steps:"
echo "   ./06-install-services.sh           # run llama-server on boot + perf at boot"
echo "   ./99-install-nvidia-frameworks.sh  # ONLY if you need cuDNN/TensorRT/DeepStream"
