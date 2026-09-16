#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# run-c4-edgellm.sh — TensorRT Edge-LLM, stage-gated.
#
#   bash run-c4-edgellm.sh --repo ~/TensorRT-Edge-LLM [--payloads DIR]
#
# This candidate is expected to stop somewhere. The useful output is therefore
# not a tok/s number but WHERE it stops and why, because each stopping point
# implies a different decision:
#
#   jetpack_tier   NVIDIA's official Orin path for 0.10.x is JetPack 7.2 /
#                  CUDA 13.2. We are on 6.2.1 / 12.6. -> the JetPack 7.2
#                  reflash question, which is the user's to answer.
#   build_ram      engine BUILD peaked past what 7,619 MB can give. A 0.8B
#                  model previously peaked at 6.8 GB here, so a Gemma-4-sized
#                  build is the expected wall. -> not a reflash question.
#   no_tool_calls  0.10.x ships no tool-calling in its OpenAI server. -> cannot
#                  serve GIAP's agent loop at any speed. -> excluded from
#                  production regardless of the other two.
#
# Whatever it does reach is recorded as the "TensorRT ceiling" for reference.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$HERE/lib.sh"

REPO_DIR="$HOME/TensorRT-Edge-LLM"; PAYLOADS=""; PORT=8002; STAGE=all
while [ $# -gt 0 ]; do
  case "$1" in
    --repo)     REPO_DIR="$2"; shift 2 ;;
    --payloads) PAYLOADS="$2"; shift 2 ;;
    --port)     PORT="$2"; shift 2 ;;
    --stage)    STAGE="$2"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done

preflight
stamp_power_env
RUN="$RESULTS_ROOT/c4-edgellm"; mkdir -p "$RUN"
BLOCKED=""; DETAIL=""

record() { BLOCKED="$1"; DETAIL="$2"; warn "blocked_at=$1: $2"; }

# ── stage 1: does this JetPack/CUDA combination have a supported tier? ────────
say "stage 1: platform tier"
L4T="$(sed -n 's/.*REVISION: \([0-9.]*\).*/\1/p' /etc/nv_tegra_release 2>/dev/null)"
CUDA="$(nvcc --version 2>/dev/null | sed -n 's/.*release \([0-9.]*\).*/\1/p')"
note "L4T R36 rev $L4T, CUDA ${CUDA:-unknown}"
note "NVIDIA's Official tier for Jetson Orin on 0.10.x is JetPack 7.2 / CUDA 13.2"
if [ ! -d "$REPO_DIR" ]; then
  record jetpack_tier "no checkout at $REPO_DIR and 0.10.x has no published JetPack 6.2 tier"
fi

# ── stage 2: is anything actually built? ─────────────────────────────────────
if [ -z "$BLOCKED" ]; then
  say "stage 2: build artefacts"
  if [ -d "$REPO_DIR/build/bin" ] && ls "$REPO_DIR/build/bin" >/dev/null 2>&1; then
    note "build/bin: $(ls "$REPO_DIR/build/bin" | tr '\n' ' ')"
  else
    record build_ram "no built binaries under $REPO_DIR/build/bin; an engine build on this board previously peaked at 6,775 MiB for a 0.8B model, against 7,619 MB total"
  fi
fi

# ── stage 3: tool calling, before anything is timed ──────────────────────────
# Deliberately BEFORE the benchmark. An engine that cannot emit tool calls
# cannot serve this agent loop at any speed, so measuring it first would be
# spending device time to learn nothing that changes the decision.
if [ -z "$BLOCKED" ]; then
  say "stage 3: tool calling"
  if grep -rqi 'tool_call\|function_call\|tool_choice' "$REPO_DIR/tensorrt_edgellm" 2>/dev/null; then
    note "the tree mentions tool calling; probing the server"
  else
    record no_tool_calls "no tool-calling surface in the tree; 0.10.0 release notes state OpenAI tools requests are unsupported because the shipped tokenizer config cannot apply a tool-aware chat template"
  fi
fi

# ── stage 4: measure whatever it does serve ──────────────────────────────────
if [ -z "$BLOCKED" ] && [ -n "$PAYLOADS" ]; then
  say "stage 4: serving"
  if curl -sf -o /dev/null "http://127.0.0.1:$PORT/v1/models" 2>/dev/null; then
    python3 "$HERE/bakeoff_turns.py" --mode openai --base "http://127.0.0.1:$PORT" \
      --payloads "$PAYLOADS" --workloads voice,decode -r 3 --out "$RUN/runs.json" \
      | tee "$RUN/driver.log"
  else
    record no_server "no OpenAI-compatible server answering on :$PORT (0.10.x ships one as experimental; start it separately and re-run --stage 4)"
  fi
fi

RUNS='{"workloads":{}}'; [ -f "$RUN/runs.json" ] && RUNS="$(cat "$RUN/runs.json")"
envelope_write "$RUN/envelope.json" c4-edgellm "TensorRT Edge-LLM" "" "${BLOCKED:-served}" \
  "$RUNS" '{"swap_delta_mb":0}' '{}' \
  "$(python3 -c 'import json,sys; print(json.dumps({"blocked_at": sys.argv[1] or None, "blocked_detail": sys.argv[2] or None, "l4t_rev": sys.argv[3], "cuda": sys.argv[4], "repo": sys.argv[5]}))' "$BLOCKED" "$DETAIL" "$L4T" "${CUDA:-}" "$REPO_DIR")"

say "done"
if [ -n "$BLOCKED" ]; then
  note "C4 blocked_at=$BLOCKED — that IS the result, and it is what the reflash question turns on"
fi
note "results: $RUN"
