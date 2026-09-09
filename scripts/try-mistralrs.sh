#!/usr/bin/env bash
# Run GIAP against mistral.rs, on this Mac, in an isolated pond.
#
# Mac-only by intent. mistral.rs is not a Jetson candidate on today's numbers —
# see docs/developer/mistralrs-provider.md — so this exists to let the provider
# be driven and judged, not to prepare a deployment.
#
#   scripts/try-mistralrs.sh              # scratch pond + desktop app
#   PORT=4310 scripts/try-mistralrs.sh    # pick the port
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-4310}"
MRS="${GIAP_MISTRALRS_URL:-http://127.0.0.1:9002}"
SCRATCH="${POND_DATA_DIR:-$HOME/Library/Application Support/goose-in-a-pond-mistralrs}"

if ! curl -sf -m 5 -o /dev/null "$MRS/v1/models"; then
  echo "mistral.rs is not answering at $MRS" >&2
  echo "  start it:  ~/Documents/Jarida/mistralrs-bakeoff/run.sh" >&2
  exit 1
fi
MODEL=$(curl -s -m 5 "$MRS/v1/models" | python3 -c 'import sys,json;d=json.load(sys.stdin);print((d.get("data") or [{}])[0].get("id","default"))')
echo "mistral.rs at $MRS serving '$MODEL'"

BIN="$ROOT/target/release/pond-server"
[ -x "$BIN" ] || { echo "build first: SQLX_OFFLINE=true cargo build --release -p pond-server" >&2; exit 1; }

mkdir -p "$SCRATCH"
export POND_DATA_DIR="$SCRATCH" POND_DEV_ALLOW_LOOPBACK=1 GIAP_MISTRALRS_URL="$MRS"
export RUST_LOG="${RUST_LOG:-warn,giap::trace=info,pond_adapters_goose=info}"

P=$(lsof -ti:"$PORT" 2>/dev/null); [ -n "$P" ] && { echo "freeing :$PORT"; kill -9 $P; sleep 1; }

"$BIN" serve --port "$PORT" --native &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
for _ in $(seq 1 150); do curl -sf -m 2 -o /dev/null "http://127.0.0.1:$PORT/api/v1/health" && break; sleep 2; done

sqlite3 "$SCRATCH/pond_system.db" \
  "INSERT OR REPLACE INTO onboarding_state (id, current_step) VALUES (1,'Completed');" 2>/dev/null
curl -s -X PUT "http://127.0.0.1:$PORT/api/v1/settings" -H 'Content-Type: application/json' \
  -d "{\"chat_provider\":\"mistralrs\",\"chat_model\":\"$MODEL\"}" >/dev/null
echo
echo "  pond:     http://127.0.0.1:$PORT   (data: $SCRATCH)"
echo "  provider: mistralrs -> $MRS"
echo
echo "  The provider is read when the provider is BUILT, so it takes effect on the"
echo "  next turn. Ctrl-C to stop; your real pond is untouched."
wait $SRV
