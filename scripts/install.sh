#!/usr/bin/env bash
# scripts/install.sh — Unified installer for Goose In A Pond
#
# One script, any platform, any mode. Replaces the former install.sh (dev)
# and setup.sh (Linux production).
#
# Usage:
#   bash scripts/giap.sh install      <-- PREFERRED: adds host-state guardrails
#   bash scripts/install.sh [OPTIONS] <-- what giap.sh calls underneath
#
# Prefer giap.sh: it refuses to add a second systemd unit when one already
# exists at either scope (this script writes a SYSTEM unit; a hand-configured
# Jetson runs a USER unit of the same name, and two means two servers each
# loading its own model), warns when Node is too old to build the web UI, and
# stops a running service before a Jetson release link so the linker is not
# OOM-killed.
#
# Modes (auto-detected if not specified):
#   --production     Linux: systemd service + mDNS + auto-start
#   --jetson         Jetson Orin Nano (CUDA auto-detect, SQLX_OFFLINE)
#   --minimal        Fastest: server + DB only, no model downloads
#
# Build options:
#   --full           Build entire workspace including Goose (10+ min)
#   --desktop        Also build the Tauri desktop app
#   --no-verify      Skip post-install health check
#
# Model options:
#   --ollama         Use Ollama as LLM provider (auto-install on Linux)
#   --llamafile      Use llamafile as LLM provider
#   --no-models      Skip all model downloads
#   --whisper-model MODEL  Whisper size: tiny|base|small (default: base)
#
# Production options (--production / --jetson):
#   --port PORT      Override port (server default is 4000, falling back 4000..4009)
#   --dedicated      Set hostname to 'pond', serve on port 80
#   --shared         Keep hostname, use non-privileged port
#   --no-service     Skip systemd service creation
#   --data-dir DIR   Override data directory
#
# General:
#   --help           Show this message

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Source shared functions ──────────────────────────────────────────────────
for lib in common deps build models systemd desktop; do
  source "${SCRIPT_DIR}/lib/install-${lib}.sh"
done

# ── Defaults ─────────────────────────────────────────────────────────────────
MODE=""
DESKTOP=false
FULL_BUILD=false
NO_MODELS=false
NO_VERIFY=false
NO_SERVICE=false
WHISPER_SIZE="base"
LLM_PROVIDER=""        # auto-detect
PORT=""
DATA_DIR=""
DEDICATED=""
RUST_VER=""
BUILD_PROFILE=""

# ── Go ───────────────────────────────────────────────────────────────────────
parse_args "$@"
detect_os
detect_mode
resolve_data_dir

# ── Banner ───────────────────────────────────────────────────────────────────
MODE_DISPLAY=$(printf "%-12s" "$MODE")
echo ""
echo -e "${BOLD}  +-------------------------------------------+${NC}"
echo -e "${BOLD}  |   Goose In A Pond  --  Installer          |${NC}"
echo -e "${BOLD}  |   Mode: ${MODE_DISPLAY}                       |${NC}"
echo -e "${BOLD}  +-------------------------------------------+${NC}"
echo ""
echo -e "  Platform: ${BOLD}${OS}/${ARCH}${NC}"
[ "$IS_JETSON" = true ] && echo -e "  Jetson:   ${BOLD}detected${NC}"
[ "$HAS_CUDA" = true ]  && echo -e "  CUDA:     ${BOLD}available${NC}"
echo -e "  Data dir: ${DIM}${DATA_DIR}${NC}"
echo -e "  Repo:     ${DIM}${REPO_DIR}${NC}"

# ── Step 1: Pre-flight checks ───────────────────────────────────────────────
preflight_checks

# ── Step 2: Goose submodule ──────────────────────────────────────────────────
init_submodules

# ── Step 3: System dependencies ──────────────────────────────────────────────
install_system_deps

# ── Step 4: Build pond-server ────────────────────────────────────────────────
build_server

# ── Step 5: Server setup (DB + Whisper + Piper + ONNX) ──────────────────────
run_pond_setup

# ── Step 6: LLM model ───────────────────────────────────────────────────────
if [ "$MODE" != "minimal" ]; then
  download_llm

  # Piper TTS voice (fallback if pond-server setup missed it)
  if [ "$NO_MODELS" != true ] && [ "$S_PIPER" != "ok" ]; then
    download_piper_voice
  fi
else
  S_LLM="skipped"
fi

# ── Step 7-8: Systemd + mDNS (production/jetson only) ───────────────────────
if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
  setup_systemd
  setup_mdns
fi

# ── Step 9: Desktop (optional) ──────────────────────────────────────────────
if [ "$DESKTOP" = true ]; then
  build_desktop
else
  S_DESKTOP="skipped"
fi

# ── Verify ──────────────────────────────────────────────────────────────────
if [ "$NO_VERIFY" != true ] && [ "$S_BUILD" = "ok" ]; then
  # Skip verification if systemd is managing the service (it's already running)
  if [ "$S_SYSTEMD" = "ok" ]; then
    log "Service is running via systemd -- skipping standalone verification"
    S_VERIFY="ok"
  else
    verify_installation
  fi
else
  S_VERIFY="skipped"
fi

# ── Summary ─────────────────────────────────────────────────────────────────
print_summary
