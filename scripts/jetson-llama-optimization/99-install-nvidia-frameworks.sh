#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 99-install-nvidia-frameworks.sh  —  OPTIONAL full NVIDIA AI stack.
#
#   ⚠  NONE OF THIS IS REQUIRED FOR llama.cpp.
#   llama.cpp's CUDA backend uses only the CUDA toolkit + cuBLAS (already
#   present). cuDNN / TensorRT / DeepStream / VPI are for OTHER workloads:
#     - cuDNN     : deep-learning primitives for PyTorch/TensorFlow & TensorRT
#     - TensorRT  : compile ONNX/TF graphs to optimized engines (incl. TRT-LLM)
#     - DeepStream: GStreamer-based video-analytics / multi-camera pipelines
#     - VPI       : vision programming (image proc) accelerated on GPU/PVA/VIC
#
# Install these only if you plan to run those frameworks. They add several GB.
#
# Usage:
#   ./99-install-nvidia-frameworks.sh --all          # full nvidia-jetpack meta
#   ./99-install-nvidia-frameworks.sh cudnn tensorrt # pick components
#   ./99-install-nvidia-frameworks.sh --list         # show what's available
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson

usage() { sed -n '2,19p' "$0"; exit "${1:-0}"; }
[ $# -eq 0 ] && usage 0

hdr "Disclaimer"
warn "These frameworks do NOT accelerate llama.cpp. Continue only if you need"
warn "them for PyTorch/TensorRT-LLM/DeepStream/VPI workloads."
df -h / | awk 'NR==2{print "  Free disk on /: "$4" (full JetPack needs several GB)"}'
confirm "Proceed with NVIDIA framework install?" || { log "Aborted."; exit 0; }

sudo_prime
wait_for_apt_lock 900 || die "Could not acquire apt lock."
as_root apt-get update -qq || warn "apt update issues"

# JetPack 6.2 (L4T r36.4) names first, with graceful fallbacks for older naming.
declare -A PKG=(
  [cudnn]="nvidia-cudnn cudnn libcudnn9"
  [tensorrt]="nvidia-tensorrt tensorrt"
  [deepstream]="deepstream-7.1 deepstream-7.0 deepstream"
  [vpi]="libnvvpi3 vpi3-dev"
)

# Returns: 0 installed, 1 install failed, 2 no candidate found.
install_pkg() {
  local key="$1"; shift
  for cand in $1; do
    if apt-cache show "$cand" >/dev/null 2>&1; then
      log "Installing $key ($cand)..."
      if as_root apt-get install -y "$cand"; then ok "$key installed"; return 0
      else err "$key: install of $cand failed"; return 1; fi
    fi
  done
  warn "$key: no installable candidate found (check apt sources / JetPack repo)."
  return 2
}

case "${1:-}" in
  --list)
    for k in "${!PKG[@]}"; do printf '  %-10s candidates: %s\n' "$k" "${PKG[$k]}"; done
    echo "  nvidia-jetpack candidate: $(apt-cache policy nvidia-jetpack 2>/dev/null | awk '/Candidate/{print $2}')"
    exit 0 ;;
  --all)
    log "Installing the full nvidia-jetpack meta-package (everything)..."
    as_root apt-get install -y nvidia-jetpack && ok "nvidia-jetpack installed" \
      || die "nvidia-jetpack install failed (verify JetPack apt repo is configured)"
    ;;
  *)
    fail=0
    for comp in "$@"; do
      if [ -n "${PKG[$comp]:-}" ]; then
        install_pkg "$comp" "${PKG[$comp]}" || fail=1
      else
        warn "Unknown component: $comp"; fail=1
      fi
    done ;;
esac
echo
if [ "${fail:-0}" -eq 0 ]; then
  ok "Done. (Reminder: llama.cpp itself needs none of this.)"
else
  die "Completed with errors — see messages above."
fi
