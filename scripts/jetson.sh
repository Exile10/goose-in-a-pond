#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# jetson.sh — single entry point for everything Jetson (Orin Nano).
#
# Usage:
#   bash scripts/jetson.sh deploy [--branch BR] [--desktop]   # dev machine: one-command deploy to the nano
#   bash scripts/jetson.sh build [--cuda] [--desktop]         # ON the Jetson: native build (GPU builds MUST be on-device)
#   bash scripts/jetson.sh docker-build [--full]              # off-device: aarch64 CPU binary via linux/arm64 container
#   bash scripts/jetson.sh optimize                           # ON the Jetson: llama.cpp perf suite (deps, perf mode, bench)
#   bash scripts/jetson.sh probe-mlc [--keep]                 # ON the Jetson: MLC-LLM tool-call quality probe
#
# The implementations live in scripts/jetson/ — see scripts/jetson/README.md
# for the full workflow (daily deploy loop, first-time device setup, gotchas).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CMD="${1:-help}"
shift || true

case "$CMD" in
  deploy)       exec bash "$HERE/jetson/deploy.sh" "$@" ;;
  build)        exec bash "$HERE/jetson/build-native.sh" "$@" ;;
  docker-build) exec bash "$HERE/jetson/build-docker.sh" "$@" ;;
  optimize)     exec bash "$HERE/jetson/llama-optimization/run-all.sh" "$@" ;;
  probe-mlc)    exec bash "$HERE/jetson/mlc-toolcall-probe.sh" "$@" ;;
  help|--help|-h)
    sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "Unknown command: $CMD (try: deploy | build | docker-build | optimize | probe-mlc)" >&2
    exit 1
    ;;
esac
