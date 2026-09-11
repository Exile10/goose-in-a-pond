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
#   bash scripts/jetson.sh bakeoff <sub> [args]                # ON the Jetson: inference-engine bake-off
#       sub = prep | capture | c1 | c2 | c3 | c4 | report
#
# The implementations live in scripts/jetson/ — see scripts/jetson/README.md
# for the full workflow (daily deploy loop, first-time device setup, gotchas).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Every subcommand below either compiles here or ships what was compiled here,
# and a cold CUDA build on the nano is 40-90 minutes. Catching an SDK mismatch
# now costs nothing; catching it after the cross-compile costs the build.
# shellcheck source=lib/macos-sdk.sh
source "$HERE/lib/macos-sdk.sh"
giap_pin_macos_sdk

CMD="${1:-help}"
shift || true

case "$CMD" in
  deploy)       exec bash "$HERE/jetson/deploy.sh" "$@" ;;
  build)        exec bash "$HERE/jetson/build-native.sh" "$@" ;;
  docker-build) exec bash "$HERE/jetson/build-docker.sh" "$@" ;;
  optimize)     exec bash "$HERE/jetson/llama-optimization/run-all.sh" "$@" ;;
  probe-mlc)    exec bash "$HERE/jetson/mlc-toolcall-probe.sh" "$@" ;;
  bakeoff)
    SUB="${1:-help}"; shift || true
    case "$SUB" in
      prep)    exec bash   "$HERE/jetson/bakeoff/00-prep-sudo.sh" "$@" ;;
      capture) exec bash   "$HERE/jetson/bakeoff/capture-payload.sh" "$@" ;;
      c1)      exec bash   "$HERE/jetson/bakeoff/run-c1-giap.sh" "$@" ;;
      c2)      exec bash   "$HERE/jetson/bakeoff/run-c2-llama-server.sh" "$@" ;;
      c3)      exec bash   "$HERE/jetson/bakeoff/run-c3-vllm.sh" "$@" ;;
      c4)      exec bash   "$HERE/jetson/bakeoff/run-c4-edgellm.sh" "$@" ;;
      report)  exec python3 "$HERE/jetson/bakeoff/report.py" "$@" ;;
      *) echo "bakeoff sub-commands: prep | capture | c1 | c2 | c3 | c4 | report" >&2; exit 1 ;;
    esac ;;
  help|--help|-h)
    sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    ;;
  *)
    echo "Unknown command: $CMD (try: deploy | build | docker-build | optimize | probe-mlc | bakeoff)" >&2
    exit 1
    ;;
esac
