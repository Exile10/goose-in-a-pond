#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# run-c1-giap.sh — the incumbent: llama.cpp in-process via llama-cpp-2, driven
# through GIAP's own serving path. This is the BASELINE every other candidate is
# scored against, so it runs first and it runs last (the bookend check).
#
#   bash scripts/jetson/bakeoff/run-c1-giap.sh --model gemma-4-E4B-it-qat [--workloads all] [-r 5]
#
# Uses a scratch POND_DATA_DIR with the GGUF hard-linked in, so a probe can
# never touch the household pond's database or its weights.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
# shellcheck disable=SC1091
. "$HERE/lib.sh"

MODEL=""; WORKLOADS="voice,followup,decode,fresh_rel"; REPEATS=5; BIN=""; COLD=1; KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --model)     MODEL="$2"; shift 2 ;;
    --workloads) WORKLOADS="$2"; shift 2 ;;
    -r|--repeats) REPEATS="$2"; shift 2 ;;
    --bin)       BIN="$2"; shift 2 ;;
    --no-cold)   COLD=0; shift ;;
    --keep)      KEEP=1; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -n "$MODEL" ] || die "--model is required"

REAL_DATA="${POND_REAL_DATA_DIR:-$DATA_DIR_DEFAULT}"
[ -n "$BIN" ] || BIN="$REPO/target/release/pond-server"
[ -x "$BIN" ] || die "no release pond-server at $BIN — a debug build is CPU-only and would measure nothing"

preflight
stamp_power_env
OC0="$(oc3_count)"; T0="$(date +%s)"

RUN="$RESULTS_ROOT/c1-giap/$MODEL"
mkdir -p "$RUN"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/bakeoff-c1.XXXXXX")"
mkdir -p "$SCRATCH/models/gguf"
SERVER_PID=""; MEMWATCH_PID=""
cleanup() {
  [ -n "$MEMWATCH_PID" ] && kill "$MEMWATCH_PID" 2>/dev/null
  [ -n "$SERVER_PID" ] && kill -9 "$SERVER_PID" 2>/dev/null
  tegra_stop
  [ "$KEEP" = 1 ] && note "kept scratch: $SCRATCH" || rm -rf "$SCRATCH"
}
trap cleanup EXIT

ENTRY=""
for path in "$REAL_DATA/models/gguf/$MODEL.gguf" "$REAL_DATA/models/gguf/$MODEL"-*.gguf; do
  [ -e "$path" ] && { ENTRY="$(basename "$path")"; break; }
done
[ -n "$ENTRY" ] || { ls -1 "$REAL_DATA/models/gguf" | sed 's/^/  /' >&2; die "cannot resolve $MODEL"; }
SRC="$(readlink -f "$REAL_DATA/models/gguf/$ENTRY")"
[ -f "$SRC" ] || die "$ENTRY -> $SRC is not a file"
ln "$SRC" "$SCRATCH/models/gguf/$ENTRY" 2>/dev/null || cp "$SRC" "$SCRATCH/models/gguf/$ENTRY"
note "weights: $SRC ($(du -h "$SRC" | cut -f1))"

# Cold start is only cold if the page cache no longer holds the weights.
[ "$COLD" = 1 ] && { say "dropping page cache for an honest cold start"; drop_caches; }

MEM_BEFORE="$(mem_snapshot)"
tegra_start "$RUN/tegrastats.log"

PORT="${BAKEOFF_C1_PORT:-4982}"
# giap::trace is INFO-rooted; a warn-rooted RUST_LOG hides turn_end entirely.
# goose carves llama-cpp-2 to ERROR in tracing_setup.rs, so the KV-cache size
# lines need it raised explicitly or they never appear.
POND_DATA_DIR="$SCRATCH" POND_DEV_ALLOW_LOOPBACK=1 \
RUST_LOG="warn,giap::trace=info,pond_server=info,pond_adapters_goose=debug,goose_local_inference=debug,llama_cpp_2=info" \
  "$BIN" serve --port "$PORT" > "$RUN/server.log" 2>&1 < /dev/zero &
SERVER_PID=$!
bash "$HERE/memwatch.sh" --out "$RUN/mem.csv" --pid "$SERVER_PID" --interval 0.5 &
MEMWATCH_PID=$!

note "waiting for the scratch pond"
for _ in $(seq 1 150); do
  [ -s "$SCRATCH/.runtime_api_port" ] && break
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -40 "$RUN/server.log" >&2; die "server exited"; }
  sleep 2
done
PORT="$(tr -d ' \n' < "$SCRATCH/.runtime_api_port" 2>/dev/null || echo "$PORT")"
until curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health"; do
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -40 "$RUN/server.log" >&2; die "server exited"; }
  sleep 2
done
API="http://127.0.0.1:$PORT/api/v1"
sqlite3 "$SCRATCH/pond_system.db" \
  "INSERT OR REPLACE INTO onboarding_state (id, current_step) VALUES (1, 'Completed');"
curl -sf -X PUT "$API/settings" -H 'Content-Type: application/json' \
  -d "{\"chat_provider\":\"local\",\"chat_model\":\"$MODEL\",\"tool_selection_mode\":\"relevant\"}" >/dev/null \
  || die "settings PUT failed"
note "pond ready on $PORT"

say "workloads: $WORKLOADS  (x$REPEATS)"
python3 "$HERE/bakeoff_turns.py" --mode giap --base "http://127.0.0.1:$PORT" \
  --workloads "$WORKLOADS" -r "$REPEATS" --out "$RUN/runs.json" | tee "$RUN/driver.log"

kill "$MEMWATCH_PID" 2>/dev/null; MEMWATCH_PID=""
tegra_stop

# ── harvest what only the log knows ──────────────────────────────────────────
# The prefill plan and the KV-cache allocation are the two facts that decide
# whether a follow-up turn really reused its prefix and what the cache costs
# per token. Neither reaches turn_stats.
{
  echo "=== prefill plans (expect CreateContext, then ReusePrefix on turn 2+) ==="
  grep -o 'prompt prefill plan.*' "$RUN/server.log" | tail -40
  echo; echo "=== KV cache allocations (size = N MiB (C cells, L layers)) ==="
  grep -o 'llama_kv_cache.*size *= *[0-9.]* MiB.*' "$RUN/server.log" | sort -u
  echo; echo "=== Jetson settings actually applied ==="
  grep -E 'Applied Jetson|Jetson context sized|effective_ctx' "$RUN/server.log" | tail -10
  echo; echo "=== model slot identity (differing pointers = a slot bug) ==="
  grep -o 'generate: model slot identity.*' "$RUN/server.log" | tail -10
} > "$RUN/engine-facts.txt"
sed -n '1,12p' "$RUN/engine-facts.txt" | sed 's/^/   /'

MEM_AFTER="$(mem_snapshot)"
MEM_SUM="$(bash "$HERE/memwatch.sh" --summary "$RUN/mem.csv")"
THERMAL="$(tegra_summary "$RUN/tegrastats.log")"
export BAKEOFF_OC3_DELTA=$(( $(oc3_count) - OC0 )) BAKEOFF_OC3_SECS=$(( $(date +%s) - T0 ))
export BAKEOFF_CLOCKS="$( [ "$(gpu_cur_mhz)" = "$(gpu_max_mhz)" ] && echo pinned || echo dynamic )"

envelope_write "$RUN/envelope.json" c1-giap "llama-cpp-2 0.1.146 in-process" \
  "$REAL_DATA/models/gguf/$ENTRY" baseline \
  "$(cat "$RUN/runs.json")" \
  "$(python3 -c 'import json,sys; a=json.loads(sys.argv[1]); b=json.loads(sys.argv[2]); c=json.loads(sys.argv[3]); c.update({"before":a,"after":b}); print(json.dumps(c))' "$MEM_BEFORE" "$MEM_AFTER" "$MEM_SUM")" \
  "$THERMAL" \
  "$(python3 -c 'import json,sys; print(json.dumps({"engine_facts_file": sys.argv[1], "server_log": sys.argv[2]}))' "$RUN/engine-facts.txt" "$RUN/server.log")"

say "done"
note "results: $RUN"
