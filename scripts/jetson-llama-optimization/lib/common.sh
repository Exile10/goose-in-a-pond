# shellcheck shell=bash
# ---------------------------------------------------------------------------
# common.sh — shared helpers & configuration for the Jetson llama.cpp toolkit
#
# Source this from every script:  source "$(dirname "$0")/lib/common.sh"
# It is intentionally side-effect free except for defining functions/vars.
# ---------------------------------------------------------------------------

# --- Resolve repo root regardless of where a script is invoked from --------
GIAP_OPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export GIAP_OPT_ROOT

# --- Tunable configuration (override via environment) ----------------------
# Where llama.cpp source + build live, and where GGUF models are stored.
: "${LLAMA_DIR:=$HOME/llama.cpp}"
: "${MODELS_DIR:=$HOME/models}"
: "${BIN_DIR:=$HOME/.local/bin}"
# CUDA compute capability for Jetson Orin (Ampere) = sm_87.
: "${CUDA_ARCH:=87}"
# Parallel build jobs. CUDA/nvcc template instantiation is RAM-hungry; on an
# 8GB unified-memory board -j6 can OOM-kill the compiler. Default to a safe 4
# (zram swap covers spikes). Override with JOBS=6 if you have headroom.
: "${JOBS:=4}"
# ccache cache size — llama.cpp rebuilds become near-instant after the first.
: "${CCACHE_SIZE:=10G}"
export LLAMA_DIR MODELS_DIR BIN_DIR CUDA_ARCH JOBS CCACHE_SIZE

# --- Pretty logging --------------------------------------------------------
if [ -t 1 ] && command -v tput >/dev/null 2>&1 && [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
  _C_RST="$(tput sgr0)"; _C_RED="$(tput setaf 1)"; _C_GRN="$(tput setaf 2)"
  _C_YLW="$(tput setaf 3)"; _C_BLU="$(tput setaf 4)"; _C_BLD="$(tput bold)"
else
  _C_RST=""; _C_RED=""; _C_GRN=""; _C_YLW=""; _C_BLU=""; _C_BLD=""
fi
log()   { printf '%s[%s]%s %s\n' "$_C_BLU" "$(date +%H:%M:%S)" "$_C_RST" "$*"; }
ok()    { printf '%s  ✓ %s%s\n' "$_C_GRN" "$*" "$_C_RST"; }
warn()  { printf '%s  ! %s%s\n' "$_C_YLW" "$*" "$_C_RST" >&2; }
err()   { printf '%s  ✗ %s%s\n' "$_C_RED" "$*" "$_C_RST" >&2; }
hdr()   { printf '\n%s%s== %s ==%s\n' "$_C_BLD" "$_C_BLU" "$*" "$_C_RST"; }
die()   { err "$*"; exit 1; }

# --- Guards ----------------------------------------------------------------
# Confirm we are actually on a Jetson (Tegra) board before touching anything.
require_jetson() {
  if [ ! -e /etc/nv_tegra_release ] && ! grep -qi tegra /proc/version 2>/dev/null; then
    die "This does not look like an NVIDIA Jetson (no /etc/nv_tegra_release). Aborting."
  fi
}

# Run a command as root. Works whether or not passwordless sudo is configured:
# when run interactively, sudo prompts the user for their password as usual.
as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo "$@"
  fi
}

# Prime the sudo timestamp up front so a long script doesn't stall on a prompt
# halfway through. No-op if already root.
sudo_prime() {
  [ "$(id -u)" -eq 0 ] && return 0
  if ! sudo -n true 2>/dev/null; then
    log "Some steps need root. Please enter your password for sudo:"
    sudo -v || die "sudo authentication failed."
  fi
}

confirm() {
  # confirm "Question?"  -> returns 0 on yes. Auto-yes if ASSUME_YES=1.
  [ "${ASSUME_YES:-0}" = "1" ] && return 0
  local reply
  printf '%s%s [y/N] %s' "$_C_YLW" "$*" "$_C_RST"
  read -r reply
  [[ "$reply" =~ ^[Yy]$ ]]
}

have() { command -v "$1" >/dev/null 2>&1; }

# Free reclaimable page cache so the Tegra NvMap/CMA allocator can obtain the
# large CONTIGUOUS blocks that GPU offload needs. Without this, loading a model
# with -ngl>0 can fail with "NvMapMemAllocInternalTagged ... error 12" (ENOMEM)
# even when `free` shows several GB "available" — because that memory is tied up
# in (reclaimable) file cache that the contiguous allocator won't compact.
# Best-effort: needs root; warns (does not fail) if it can't.
free_contiguous_mem() {
  sync
  if echo 3 | as_root tee /proc/sys/vm/drop_caches >/dev/null 2>&1; then
    ok "Freed page cache for contiguous GPU allocation ($(free -h | awk '/Mem:/{print $4}') free)"
  else
    warn "Could not drop caches (need root). If GPU load fails with NvMap error 12, run:"
    warn "  sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'"
  fi
}

# Block until no other process holds the dpkg/apt lock (e.g. unattended-upgrades
# or a manual `apt upgrade`). Avoids the "Could not get lock" failure. Waits up
# to ${1:-600} seconds.
wait_for_apt_lock() {
  local max="${1:-600}" waited=0
  while as_root fuser /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/lib/apt/lists/lock >/dev/null 2>&1; do
    [ "$waited" -eq 0 ] && warn "Another apt/dpkg process is running — waiting for it to finish..."
    sleep 5; waited=$((waited+5))
    if [ "$waited" -ge "$max" ]; then err "apt lock still held after ${max}s — aborting."; return 1; fi
    [ $((waited % 30)) -eq 0 ] && log "  still waiting for apt lock (${waited}s)…"
  done
  return 0
}

# Prepend ~/.local/bin to PATH so a pip --user cmake (see below) wins over the
# stock apt cmake, and refresh the shell's command hash table.
ensure_local_bin_on_path() {
  case ":$PATH:" in *":$BIN_DIR:"*) ;; *) export PATH="$BIN_DIR:$PATH" ;; esac
  hash -r 2>/dev/null || true
}

# Return 0 if installed cmake >= $1.$2 (default 3.27). Stock JetPack CMake is
# 3.22.1 which, with GCC 11 + CUDA 12.6, dies on aarch64 NEON intrinsics
# (arm_neon.h: '__Int8x8_t' undefined) — confirmed on Orin Nano 8GB. We need
# a newer CMake (pip wheel) to build llama.cpp's CUDA backend.
cmake_version_ok() {
  local need_major="${1:-3}" need_minor="${2:-27}"
  have cmake || return 1
  # NB: assign on separate lines — a same-line `local a=.. b=${a}` expands all
  # RHS before binding, which trips `set -u` on the self-reference.
  local v maj rest min
  v="$(cmake --version 2>/dev/null | awk 'NR==1{print $3}')"
  [ -n "$v" ] || return 1
  maj="${v%%.*}"; rest="${v#*.}"; min="${rest%%.*}"
  [ "${maj:-0}" -gt "$need_major" ] && return 0
  [ "${maj:-0}" -eq "$need_major" ] && [ "${min:-0}" -ge "$need_minor" ] && return 0
  return 1
}

# Upgrade cmake via pip (--user). Tries plain, then PEP-668 override (Ubuntu).
pip_upgrade_cmake() {
  ensure_local_bin_on_path
  python3 -m pip install --user --upgrade cmake 2>/dev/null \
    || python3 -m pip install --user --upgrade --break-system-packages cmake
  ensure_local_bin_on_path
}

# Ensure CUDA's nvcc is reachable; export PATH/LD_LIBRARY_PATH if needed.
# Returns non-zero (does NOT die) if CUDA is absent, so it is safe to call from
# read-only/reporting scripts. Callers that require CUDA should do
#   ensure_cuda_on_path || die "..."
ensure_cuda_on_path() {
  if ! have nvcc; then
    for d in /usr/local/cuda/bin /usr/local/cuda-12/bin /usr/local/cuda-12.6/bin; do
      [ -x "$d/nvcc" ] && export PATH="$d:$PATH" && break
    done
  fi
  have nvcc || return 1
  case ":${LD_LIBRARY_PATH:-}:" in
    *:/usr/local/cuda/lib64:*) ;;
    *) export LD_LIBRARY_PATH="/usr/local/cuda/lib64:${LD_LIBRARY_PATH:-}" ;;
  esac
  return 0
}
