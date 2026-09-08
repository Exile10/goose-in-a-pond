#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# run-c2-llama-server.sh — a CURRENT llama.cpp, driven over HTTP with the
# captured GIAP payload.
#
#   bash run-c2-llama-server.sh --model gemma-4-E4B-it-qat --payloads ~/bakeoff-payloads \
#        [--variant baseline|iswa|kvf16|mtp] [--draft <assistant.gguf>] [--container]
#
# The `baseline` variant mirrors apply_jetson_settings flag-for-flag. That
# mirroring is the load-bearing part: it turns the C1-vs-C2 gap into a single
# question -- how stale is the vendored engine -- instead of a mixture of that
# and a dozen unmatched knobs. Every other variant changes exactly one thing.
#
# Flags are probed against `--help` before use. A build that predates a flag
# must be reported as such, not silently run without it: an `iswa` run that
# quietly kept swa_full would look like a free memory saving.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$HERE/lib.sh"

MODEL=""; PAYLOADS=""; VARIANT="baseline"; DRAFT=""; CTX=16384
SERVER_BIN="$HOME/llama.cpp/build/bin/llama-server"
IMAGE=""; WORKLOADS="voice,followup,decode,fresh_rel"; REPEATS=5; COLD=1
while [ $# -gt 0 ]; do
  case "$1" in
    --model)     MODEL="$2"; shift 2 ;;
    --payloads)  PAYLOADS="$2"; shift 2 ;;
    --variant)   VARIANT="$2"; shift 2 ;;
    --draft)     DRAFT="$2"; shift 2 ;;
    --ctx)       CTX="$2"; shift 2 ;;
    --bin)       SERVER_BIN="$2"; shift 2 ;;
    --container) IMAGE="${2:-ghcr.io/nvidia-ai-iot/llama_cpp:latest-jetson-orin}"; shift 2 ;;
    --workloads) WORKLOADS="$2"; shift 2 ;;
    -r|--repeats) REPEATS="$2"; shift 2 ;;
    --no-cold)   COLD=0; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -n "$MODEL" ] || die "--model is required"
[ -n "$PAYLOADS" ] && [ -f "$PAYLOADS/payload-fresh_rel.json" ] \
  || die "--payloads DIR must contain payload-fresh_rel.json (run capture-payload.sh first)"

REAL_DATA="${POND_REAL_DATA_DIR:-$DATA_DIR_DEFAULT}"
ENTRY=""
for path in "$REAL_DATA/models/gguf/$MODEL.gguf" "$REAL_DATA/models/gguf/$MODEL"-*.gguf; do
  [ -e "$path" ] && { ENTRY="$(basename "$path")"; break; }
done
[ -n "$ENTRY" ] || die "cannot resolve $MODEL under $REAL_DATA/models/gguf"
SRC="$(readlink -f "$REAL_DATA/models/gguf/$ENTRY")"

# A previous candidate's cleanup trap can still be tearing down when this starts,
# and then the exclusivity gate correctly refuses. Give it a bounded chance to
# finish rather than failing a chained run on a shutdown race.
for _ in $(seq 1 30); do
  pgrep -f "pond-server serve" >/dev/null 2>&1 || break
  sleep 2
done

preflight
stamp_power_env
OC0="$(oc3_count)"; T0="$(date +%s)"
RUN="$RESULTS_ROOT/c2-llama-server/$MODEL-$VARIANT"
mkdir -p "$RUN"

SERVER_PID=""; MEMWATCH_PID=""; CONTAINER=""
cleanup() {
  [ -n "$MEMWATCH_PID" ] && kill "$MEMWATCH_PID" 2>/dev/null
  [ -n "$SERVER_PID" ] && kill -9 "$SERVER_PID" 2>/dev/null
  [ -n "$CONTAINER" ] && docker rm -f "$CONTAINER" >/dev/null 2>&1
  tegra_stop
}
trap cleanup EXIT

# ── build identity, then probe the flags this build actually has ─────────────
if [ -n "$IMAGE" ]; then
  BUILD_DESC="container $IMAGE"
  HELP="$(docker run --rm --entrypoint llama-server "$IMAGE" --help 2>&1 || true)"
else
  [ -x "$SERVER_BIN" ] || die "no llama-server at $SERVER_BIN"
  BUILD_DESC="$SERVER_BIN ($("$SERVER_BIN" --version 2>&1 | head -1))"
  HELP="$("$SERVER_BIN" --help 2>&1 || true)"
fi
note "build: $BUILD_DESC"
has_flag() { grep -q -- "$1" <<<"$HELP"; }

# apply_jetson_settings, mirrored: n_gpu_layers 99, flash_attn, n_batch 512,
# n_ubatch 128, n_threads 4, type_k/type_v q8_0, and the derived context.
ARGS=(-m /models/"$(basename "$SRC")" --host 127.0.0.1 --port 8001
      -a "$MODEL" -ngl 99 -b 512 -ub 128 -t 4 -c "$CTX" -np 1 --jinja)
if has_flag "--flash-attn"; then ARGS+=(-fa on); else add_warning "build has no --flash-attn"; fi
if has_flag "--cache-type-k"; then ARGS+=(-ctk q8_0 -ctv q8_0); else add_warning "build has no --cache-type-k; KV runs at f16"; fi
if has_flag "--cache-reuse"; then ARGS+=(--cache-reuse 256); fi

MISSING=()
case "$VARIANT" in
  baseline)
    # swa_full=true is hardcoded in the vendored engine because ReusePrefix
    # does a partial seq_rm behind the sliding window. Mirror it, or C2 is
    # measuring a different cache policy and calling it the same baseline.
    if has_flag -- "--swa-full"; then ARGS+=(--swa-full)
    else MISSING+=("--swa-full"); add_warning "no --swa-full: this build cannot mirror C1's cache policy"; fi ;;
  iswa)
    # The saving under test: interleaved-SWA shrinks the sliding-window cache.
    # Deliberately WITHOUT --swa-full, with checkpoints standing in for the
    # partial-removal reuse that swa_full made sound.
    if has_flag -- "--ctx-checkpoints"; then ARGS+=(--ctx-checkpoints 8)
    elif has_flag -- "--swa-checkpoints"; then ARGS+=(--swa-checkpoints 8)
    else MISSING+=("--ctx-checkpoints"); add_warning "no SWA checkpoints in this build; reuse may be lost"; fi ;;
  kvf16)
    ARGS=("${ARGS[@]/-ctk q8_0/}"); ARGS=("${ARGS[@]/-ctv q8_0/}") ;;
  mtp|draft)
    [ -n "$DRAFT" ] || die "--variant $VARIANT needs --draft <drafter.gguf>"
    [ -f "$DRAFT" ] || die "draft model not found: $DRAFT"
    # The drafter must carry arch `gemma4-assistant` (HYPHEN). The centroid-format
    # drafters this project has from the ik_llama era are `gemma4_assistant` and
    # `gemma4_mtp`, and they are not a rename away: upstream wants
    # nextn.{eh_proj,enorm,hnorm,shared_head_*} where those carry
    # mtp.{pre_projection,centroids,token_ordering}. Different layout entirely.
    DRAFT_ARCH="$(strings -a "$DRAFT" 2>/dev/null | grep -m1 -x "gemma4-assistant\|gemma4_assistant\|gemma4_mtp" || echo unknown)"
    note "drafter arch: $DRAFT_ARCH"
    [ "$DRAFT_ARCH" = "gemma4-assistant" ] || add_warning \
      "drafter arch is '$DRAFT_ARCH'; upstream llama.cpp registers 'gemma4-assistant' and will refuse this file"
    ARGS+=(-md /models/"$(basename "$DRAFT")")
    if has_flag -- "--spec-type"; then
      # unsloth documents `draft-mtp` for this drafter; this build advertises
      # none,draft-simple,draft-eagle3,draft-mtp,draft-dflash,draft-dspark,ngram-*.
      ARGS+=(--spec-type "${BAKEOFF_SPEC_TYPE:-draft-mtp}")
      has_flag -- "--spec-draft-n-max" && ARGS+=(--spec-draft-n-max "${BAKEOFF_DRAFT_N_MAX:-4}")
    else MISSING+=("--spec-type"); add_warning "no --spec-type; speculative decoding cannot be tested on this build"; fi ;;
  *) die "unknown variant: $VARIANT" ;;
esac
[ "${#MISSING[@]}" -gt 0 ] && note "flags this build lacks: ${MISSING[*]}"

[ "$COLD" = 1 ] && { say "dropping page cache"; drop_caches; }
MEM_BEFORE="$(mem_snapshot)"
tegra_start "$RUN/tegrastats.log"

say "starting llama-server ($VARIANT)"
MODELS_DIR="$(dirname "$SRC")"
if [ -n "$IMAGE" ]; then
  CONTAINER="bakeoff-c2-$$"
  docker run -d --name "$CONTAINER" --runtime nvidia --network host \
    -v "$MODELS_DIR:/models:ro" --entrypoint llama-server "$IMAGE" "${ARGS[@]}" \
    > "$RUN/container.id" 2>"$RUN/server.log" || die "docker run failed"
  ( docker logs -f "$CONTAINER" >> "$RUN/server.log" 2>&1 ) &
  SERVER_PID=""
else
  # A bare binary reads the real path, not a bind mount.
  ARGS=("${ARGS[@]/\/models\/$(basename "$SRC")/$SRC}")
  [ -n "$DRAFT" ] && ARGS=("${ARGS[@]/\/models\/$(basename "$DRAFT")/$DRAFT}")
  "$SERVER_BIN" "${ARGS[@]}" > "$RUN/server.log" 2>&1 &
  SERVER_PID=$!
fi
printf '   flags: %s\n' "${ARGS[*]}"
printf '%s\n' "${ARGS[*]}" > "$RUN/flags.txt"

[ -n "$SERVER_PID" ] && { bash "$HERE/memwatch.sh" --out "$RUN/mem.csv" --pid "$SERVER_PID" --interval 0.5 & MEMWATCH_PID=$!; } \
                     || { bash "$HERE/memwatch.sh" --out "$RUN/mem.csv" --interval 0.5 & MEMWATCH_PID=$!; }

note "waiting for /health (a cold load of $(du -h "$SRC" | cut -f1) takes a while)"
READY=0
for _ in $(seq 1 180); do
  if curl -sf -o /dev/null "http://127.0.0.1:8001/health" 2>/dev/null; then READY=1; break; fi
  [ -n "$SERVER_PID" ] && { kill -0 "$SERVER_PID" 2>/dev/null || { tail -40 "$RUN/server.log" >&2; die "llama-server exited"; }; }
  sleep 2
done
[ "$READY" = 1 ] || { tail -40 "$RUN/server.log" >&2; die "llama-server never became healthy"; }
note "served models: $(curl -sf http://127.0.0.1:8001/v1/models | python3 -c 'import json,sys;print(",".join(m["id"] for m in json.load(sys.stdin).get("data",[])))' 2>/dev/null)"
# The window the server actually built, not the one we asked for.
REPORTED_CTX="$(curl -sf http://127.0.0.1:8001/props 2>/dev/null | python3 -c 'import json,sys;print((json.load(sys.stdin).get("default_generation_settings") or {}).get("n_ctx",""))' 2>/dev/null)"
note "server n_ctx: ${REPORTED_CTX:-unknown} (asked for $CTX)"

say "workloads: $WORKLOADS (x$REPEATS)"
python3 "$HERE/bakeoff_turns.py" --mode openai --base http://127.0.0.1:8001 \
  --model "$MODEL" --payloads "$PAYLOADS" --workloads "$WORKLOADS" -r "$REPEATS" \
  --out "$RUN/runs.json" | tee "$RUN/driver.log"

kill "$MEMWATCH_PID" 2>/dev/null; MEMWATCH_PID=""
tegra_stop
{
  echo "=== KV cache allocations ==="
  grep -o 'llama_kv_cache.*size *= *[0-9.]* MiB.*' "$RUN/server.log" | sort -u
  echo; echo "=== context / SWA ==="
  grep -iE 'n_ctx|swa|sliding|flash_attn|type_k|type_v' "$RUN/server.log" | head -20
  echo; echo "=== speculative ==="
  grep -iE 'draft|spec|accept' "$RUN/server.log" | head -20
} > "$RUN/engine-facts.txt"
sed -n '1,10p' "$RUN/engine-facts.txt" | sed 's/^/   /'

MEM_SUM="$(bash "$HERE/memwatch.sh" --summary "$RUN/mem.csv")"
export BAKEOFF_OC3_DELTA=$(( $(oc3_count) - OC0 )) BAKEOFF_OC3_SECS=$(( $(date +%s) - T0 ))
# BAKEOFF_CLOCKS was stamped by stamp_power_env at run START. Reading it here
# sampled mid-load and called a schedutil run "pinned".
: "${BAKEOFF_CLOCKS:=unknown}"
envelope_write "$RUN/envelope.json" c2-llama-server "$BUILD_DESC" \
  "$REAL_DATA/models/gguf/$ENTRY" "$VARIANT" \
  "$(cat "$RUN/runs.json")" \
  "$(python3 -c 'import json,sys; a=json.loads(sys.argv[1]); c=json.loads(sys.argv[2]); c["before"]=a; print(json.dumps(c))' "$MEM_BEFORE" "$MEM_SUM")" \
  "$(tegra_summary "$RUN/tegrastats.log")" \
  "$(python3 -c 'import json,sys; print(json.dumps({"flags": sys.argv[1], "reported_n_ctx": sys.argv[2] or None, "requested_n_ctx": int(sys.argv[3]), "flags_missing": sys.argv[4].split() if sys.argv[4] else []}))' "$(cat "$RUN/flags.txt")" "$REPORTED_CTX" "$CTX" "${MISSING[*]:-}")"

say "done"
note "results: $RUN"
