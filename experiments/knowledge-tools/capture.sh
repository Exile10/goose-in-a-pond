#!/usr/bin/env bash
# Capture the payloads GIAP really sends for a knowledge question, in every
# tool_selection_mode, including the POST-TOOL inference (what the model reads
# back from a tool). Derived from scripts/jetson/bakeoff/capture-payload.sh:
# isolated scratch pond, weights HARD-LINKED (never a symlink of models/).
#
#   ./capture.sh [model] <out-dir>      then copy the chat inferences into payloads/
#
# Uses target/release/pond-server; build it from the commit you mean to measure.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REAL="$HOME/Library/Application Support/goose-in-a-pond"
MODEL="${1:-gemma-4-E4B-it-qat-UD-Q4_K_XL}"
OUT="${2:?out dir}"
BIN="$REPO/target/release/pond-server"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/kt-capture.XXXXXX")"
CAP="$SCRATCH/captures"
mkdir -p "$SCRATCH/models/gguf" "$SCRATCH/models/embedding" "$CAP" "$OUT"
PID=""
cleanup() { [ -n "$PID" ] && kill -9 "$PID" 2>/dev/null; echo "scratch kept: $SCRATCH"; }
trap cleanup EXIT

SRC="$(readlink -f "$REAL/models/gguf/$MODEL.gguf")"
[ -f "$SRC" ] || { echo "no model $MODEL"; exit 1; }
ln "$SRC" "$SCRATCH/models/gguf/$MODEL.gguf"
# The ONNX embedder lives as an HF-cache tree; hard-link the files, keep the shape.
for d in "$REAL"/models/embedding/*; do
  [ -e "$d" ] || continue
  if [ -d "$d" ]; then cp -RL "$d" "$SCRATCH/models/embedding/" ; else ln "$(readlink -f "$d")" "$SCRATCH/models/embedding/$(basename "$d")"; fi
done

start() {
  POND_DATA_DIR="$SCRATCH" POND_DEV_ALLOW_LOOPBACK=1 GIAP_CAPTURE_PAYLOAD="$CAP" \
  RUST_LOG="warn,giap::trace=info,pond_server=info" \
    "$BIN" serve --port 4983 >> "$SCRATCH/server.out" 2>&1 < /dev/zero &
  PID=$!
  rm -f "$SCRATCH/.runtime_api_port"
  for _ in $(seq 1 150); do
    [ -s "$SCRATCH/.runtime_api_port" ] && break
    kill -0 "$PID" 2>/dev/null || { tail -30 "$SCRATCH/server.out"; exit 1; }
    sleep 2
  done
  PORT="$(tr -d ' \n' < "$SCRATCH/.runtime_api_port")"
  API="http://127.0.0.1:$PORT/api/v1"
  until curl -sf -o /dev/null "$API/health"; do sleep 2; done
  echo "pond on $PORT"
}
start
sqlite3 "$SCRATCH/pond_system.db" \
  "INSERT OR REPLACE INTO onboarding_state (id, current_step) VALUES (1, 'Completed');"
curl -sf -X PUT "$API/settings" -H 'Content-Type: application/json' \
  -d "{\"chat_provider\":\"local\",\"chat_model\":\"$MODEL\",\"thinking_mode\":\"off\"}" >/dev/null || { echo "settings PUT failed"; exit 1; }
turn() { curl -sf -N -X POST "$API/chat/stream" -H 'Content-Type: application/json' \
  -d "{\"message\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$2"),\"session_id\":\"$1\"}" \
  --max-time 400 > "$OUT/sse-$1.txt" 2>&1 || true; }
turn warmup "hello"
# Restart so tuning + embedder are live at boot (see capture-payload.sh).
kill -9 "$PID"; wait "$PID" 2>/dev/null; start

for mode in all minimal relevant; do
  curl -sf -X PUT "$API/settings" -H 'Content-Type: application/json' \
    -d "{\"tool_selection_mode\":\"$mode\"}" >/dev/null; sleep 2
  [ "$mode" = relevant ] && { turn "rel-warm-1" "What is the weather right now?"; turn "rel-warm-2" "What is the weather right now?"; }
  mkdir -p "$OUT/$mode"
  before="$(ls -1 "$CAP" | sort)"
  turn "kt-$mode" "Who was Wangari Maathai?"
  comm -13 <(echo "$before") <(ls -1 "$CAP" | sort) | while read -r f; do cp "$CAP/$f" "$OUT/$mode/"; done
  echo "$mode: $(ls "$OUT/$mode" | wc -l) captures"
done
grep -a 'kind="tool_selection"' "$SCRATCH/server.out" | sed 's/\x1b\[[0-9;]*m//g' | tail -5 > "$OUT/selection.log"
cp "$SCRATCH/server.out" "$OUT/server.out"
