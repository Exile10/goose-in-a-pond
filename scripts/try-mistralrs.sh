#!/usr/bin/env bash
# Run GIAP against mistral.rs, on this Mac, in an isolated pond.
#
# Mac-only by intent. mistral.rs is not a Jetson candidate on today's numbers --
# see docs/developer/mistralrs-provider.md -- so this exists to let the two paths
# be driven and judged, not to prepare a deployment.
#
#   scripts/try-mistralrs.sh                # direct: no goose (default)
#   MODE=goose scripts/try-mistralrs.sh     # through the goose harness
#   PORT=4310 NATIVE=0 scripts/try-mistralrs.sh
#
# MODE is the whole experiment:
#   direct  agent_backend=mistralrs -> MistralRsAgent -> mistral.rs.
#           pond-core's system prompt, the MCP dispatcher, an HTTP client.
#   goose   agent_backend=goose, chat_provider=mistralrs -> GooseAdapter ->
#           GiapProviderShim -> goose's OpenAI provider -> mistral.rs.
# Same server, same model, same prompt. The difference is what sits in between.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-4310}"
MODE="${MODE:-direct}"
NATIVE="${NATIVE:-1}"
MRS="${GIAP_MISTRALRS_URL:-http://127.0.0.1:9002}"
# What mistral.rs was actually started with. It publishes no window anywhere --
# no `/props`, nothing on `/v1/models` -- so GIAP cannot measure it and has to
# be told. Wrong here is not a slow turn: it is the history budget, and a
# `context_pct` over 100% in `turn_stats` is what an undersized guess looks like.
CTX="${CTX:-8192}"
SCRATCH="${POND_DATA_DIR:-$HOME/Library/Application Support/goose-in-a-pond-mistralrs}"

case "$MODE" in
  direct|goose) ;;
  *) echo "MODE must be 'direct' or 'goose', not '$MODE'" >&2; exit 2 ;;
esac

if ! curl -sf -m 5 -o /dev/null "$MRS/v1/models"; then
  echo "mistral.rs is not answering at $MRS" >&2
  echo "  start it:  ~/Documents/Jarida/mistralrs-bakeoff/run.sh" >&2
  exit 1
fi
MODEL=$(curl -s -m 5 "$MRS/v1/models" | python3 -c 'import sys,json;d=json.load(sys.stdin);print((d.get("data") or [{}])[0].get("id","default"))')
echo "mistral.rs at $MRS serving '$MODEL'"

# Release by default, debug when that is all there is. A stale binary is the
# failure that costs the most here -- correct source, old binary, no error
# anywhere -- so say which one is being run and when it was built.
BIN="${BIN:-}"
if [ -z "$BIN" ]; then
  for cand in "$ROOT/target/release/pond-server" "$ROOT/target/debug/pond-server"; do
    [ -x "$cand" ] && { BIN="$cand"; break; }
  done
fi
if [ ! -x "$BIN" ]; then
  echo "build first:" >&2
  echo "  SQLX_OFFLINE=true cargo build --release -p pond-server --features mistralrs-agent" >&2
  exit 1
fi
echo "binary:  $BIN  (built $(date -r "$BIN" '+%Y-%m-%d %H:%M'))"
if [ "$MODE" = "direct" ] && [ -z "$(strings "$BIN" 2>/dev/null | grep -m1 'MistralRsAgent ready')" ]; then
  echo "this binary has no mistralrs backend compiled in -- rebuild with:" >&2
  echo "  SQLX_OFFLINE=true cargo build --release -p pond-server --features mistralrs-agent" >&2
  exit 1
fi

mkdir -p "$SCRATCH"
export POND_DATA_DIR="$SCRATCH" POND_DEV_ALLOW_LOOPBACK=1 GIAP_MISTRALRS_URL="$MRS"
export RUST_LOG="${RUST_LOG:-warn,giap::trace=info,pond_adapters_mistralrs=info,pond_adapters_goose=info}"

free_port() {
  local pids; pids=$(lsof -ti:"$1" 2>/dev/null)
  [ -n "$pids" ] && { echo "freeing :$1"; kill -9 $pids; sleep 1; }
  return 0
}

put_settings() {
  curl -s -X PUT "http://127.0.0.1:$PORT/api/v1/settings" -H 'Content-Type: application/json' \
    -d "{\"chat_provider\":\"mistralrs\",\"chat_model\":\"$MODEL\",\"context_window_override\":$CTX}" >/dev/null
}

wait_up() {
  for _ in $(seq 1 150); do
    curl -sf -m 2 -o /dev/null "http://127.0.0.1:$PORT/api/v1/health" && return 0
    sleep 2
  done
  echo "pond never came up on :$PORT" >&2
  return 1
}

# Phase 1: a throwaway boot whose only job is to create the database and take
# the settings. The agent and the provider are both built from settings read at
# STARTUP, so a PUT into a running server cannot reach the backend that is
# already wired -- and `sync_assignments_to_settings` rewrites `chat_provider`
# through a catch-all yielding "llamafile" on every boot, so the PUT has to be
# the last write before the boot that counts.
free_port "$PORT"
echo "seeding the scratch pond..."
"$BIN" serve --port "$PORT" > "$SCRATCH/seed.log" 2>&1 &
SEED=$!
wait_up || { kill $SEED 2>/dev/null; tail -20 "$SCRATCH/seed.log"; exit 1; }
sqlite3 "$SCRATCH/pond_system.db" \
  "INSERT OR REPLACE INTO onboarding_state (id, current_step) VALUES (1,'Completed');" 2>/dev/null
put_settings
kill $SEED 2>/dev/null; wait $SEED 2>/dev/null
free_port "$PORT"

# Phase 2: the boot that serves. --agent picks the path; it beats the stored
# `agent_backend` whenever it is anything other than "goose".
ARGS=(serve --port "$PORT")
[ "$MODE" = "direct" ] && ARGS+=(--agent mistralrs)
[ "$NATIVE" = "1" ] && ARGS+=(--native)

"$BIN" "${ARGS[@]}" &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
wait_up || { tail -30 "$SCRATCH/seed.log"; exit 1; }

# Twice, because the two paths read the setting at different moments and boot
# undoes it in between. `sync_assignments_to_settings` rewrites `chat_provider`
# from role assignments on EVERY boot, through a catch-all yielding "llamafile"
# -- so the seed PUT is gone by the time the server is up. MistralRsAgent is
# built from the pre-boot settings, and goose's provider is rebuilt by this PUT
# and takes effect on the next turn. Without both, MODE=goose silently talks to
# llamafile on :8080 and answers every turn with a 404.
put_settings

echo
echo "  pond:     http://127.0.0.1:$PORT   (data: $SCRATCH)"
echo "  mode:     $MODE"
echo "  context:  $CTX tokens (declared, not measured -- match run.sh's CTX)"
if [ "$MODE" = "direct" ]; then
  echo "  path:     MistralRsAgent -> $MRS   (goose not involved)"
else
  echo "  path:     GooseAdapter -> GiapProviderShim -> $MRS"
fi
echo
echo "  Ctrl-C to stop; your real pond is untouched."
wait $SRV
