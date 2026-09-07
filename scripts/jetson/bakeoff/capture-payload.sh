#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# capture-payload.sh — record the prompts GIAP actually sends, for replay.
#
#   bash scripts/jetson/bakeoff/capture-payload.sh --model gemma-4-E4B-it-qat --out ~/bakeoff-payloads
#
# Why this exists: an HTTP candidate can only be compared with the in-process
# engine if it answers the SAME prompt. Goose's sessions.db holds the raw
# messages but not the shim-enforced system prompt, not the vetoed and minified
# tool array, and not the core-first ordering that decides how much KV prefix
# survives a changed tool selection. Reconstructing from it measures a prompt
# nobody sends. GIAP_CAPTURE_PAYLOAD (provider_shim.rs) writes the real one.
#
# Runs against an ISOLATED scratch data dir with ONE model HARD-LINKED in --
# never a symlink of models/, because hf_cache_migration.rs MOVES real files out
# of models/gguf at startup and a symlinked directory IS the real one. That
# mistake destroyed this device's chat model once already.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
# shellcheck disable=SC1091
. "$HERE/lib.sh"

MODEL=""; OUT="$HOME/bakeoff-payloads"; BIN=""; KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --model) MODEL="$2"; shift 2 ;;
    --out)   OUT="$2"; shift 2 ;;
    --bin)   BIN="$2"; shift 2 ;;
    --keep)  KEEP=1; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done

REAL_DATA="${POND_REAL_DATA_DIR:-$DATA_DIR_DEFAULT}"
REAL_MODELS="$REAL_DATA/models"
[ -n "$BIN" ] || BIN="$REPO/target/release/pond-server"
[ -x "$BIN" ] || BIN="$REPO/target/debug/pond-server"
[ -x "$BIN" ] || die "no pond-server binary; build it first (bash scripts/jetson.sh build --cuda)"

if [ -z "$MODEL" ]; then
  MODEL="$(sqlite3 "$REAL_DATA/pond_system.db" \
    "SELECT value FROM settings WHERE key='chat_model';" 2>/dev/null | tr -d ' \r\n')"
  [ -n "$MODEL" ] || die "--model is required (could not read chat_model from the real pond)"
  note "model defaulted to this pond's chat_model: $MODEL"
fi

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/bakeoff-capture.XXXXXX")"
CAPDIR="$SCRATCH/captures"
mkdir -p "$SCRATCH/models/gguf" "$CAPDIR" "$OUT"
SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill -9 "$SERVER_PID" 2>/dev/null
  if [ "$KEEP" = 1 ]; then note "kept scratch: $SCRATCH"
  else rm -rf "$SCRATCH"; fi
}
trap cleanup EXIT

# ── resolve ENTRY (the registry-visible name) and SRC (where the bytes are) ──
ENTRY=""
for path in "$REAL_MODELS/gguf/$MODEL.gguf" "$REAL_MODELS/gguf/$MODEL"-*.gguf; do
  [ -e "$path" ] && { ENTRY="$(basename "$path")"; break; }
done
[ -n "$ENTRY" ] || { echo "present:" >&2; ls -1 "$REAL_MODELS/gguf" | sed 's/^/  /' >&2
                     die "could not resolve '$MODEL' to a file under $REAL_MODELS/gguf"; }
SRC="$(readlink -f "$REAL_MODELS/gguf/$ENTRY")"
[ -f "$SRC" ] || die "$ENTRY points at $SRC, which is not a file (pruned blob)"
ln "$SRC" "$SCRATCH/models/gguf/$ENTRY" 2>/dev/null \
  || { note "hard link failed (different filesystem) — copying $ENTRY"; cp "$SRC" "$SCRATCH/models/gguf/$ENTRY"; }
note "model:   $MODEL  (entry $ENTRY)"
note "weights: hard-linked from $SRC"

# ── start the scratch pond with the capture hook armed ───────────────────────
PORT="${BAKEOFF_CAPTURE_PORT:-4981}"
POND_DATA_DIR="$SCRATCH" POND_DEV_ALLOW_LOOPBACK=1 GIAP_CAPTURE_PAYLOAD="$CAPDIR" \
RUST_LOG="warn,giap::trace=info,pond_server=info,pond_adapters_goose=debug" \
  "$BIN" serve --port "$PORT" > "$SCRATCH/server.out" 2>&1 < /dev/zero &
SERVER_PID=$!

note "waiting for the scratch pond (a fresh dir downloads ONNX Runtime first — not a hang)"
for _ in $(seq 1 120); do
  [ -s "$SCRATCH/.runtime_api_port" ] && break
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$SCRATCH/server.out" >&2; die "server exited"; }
  sleep 2
done
[ -s "$SCRATCH/.runtime_api_port" ] || { tail -30 "$SCRATCH/server.out" >&2; die "no .runtime_api_port"; }
PORT="$(tr -d ' \n' < "$SCRATCH/.runtime_api_port")"
until curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health"; do
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$SCRATCH/server.out" >&2; die "server exited"; }
  sleep 2
done
note "scratch pond on port $PORT"

API="http://127.0.0.1:$PORT/api/v1"
# The API is gated behind onboarding, and a fresh dir has an EMPTY table -- an
# INSERT, not an UPDATE, and 'Completed' is PascalCase.
sqlite3 "$SCRATCH/pond_system.db" \
  "INSERT OR REPLACE INTO onboarding_state (id, current_step) VALUES (1, 'Completed');"
curl -sf -X PUT "$API/settings" -H 'Content-Type: application/json' \
  -d "{\"chat_provider\":\"local\",\"chat_model\":\"$MODEL\"}" >/dev/null || die "settings PUT failed"

set_mode() { curl -sf -X PUT "$API/settings" -H 'Content-Type: application/json' \
  -d "{\"tool_selection_mode\":\"$1\"}" >/dev/null; sleep 1; }

# ── drive turns, taking the widest NEW capture after each ────────────────────
# The shim captures every provider call, so one turn leaves several files: the
# chat inference plus the goal check and the memory-extraction side call. The
# chat inference is the one carrying the tool array; the side calls carry none.
# Widest-new is therefore the turn's own payload, without having to guess order.
turn() { # turn <session> <message>
  curl -sf -N -X POST "$API/chat/stream" -H 'Content-Type: application/json' \
    -d "{\"message\":$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1]))' "$2"),\"session_id\":\"$1\"}" \
    --max-time 300 >/dev/null 2>&1 || true
}
snapshot() { ls -1 "$CAPDIR" 2>/dev/null | sort; }
widest_new() { # widest_new <before-listing> <dest-name>
  local before="$1" dest="$2" pick
  pick="$(comm -13 <(echo "$before") <(snapshot) | while read -r f; do
            [ -n "$f" ] && printf '%s %s\n' "$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1])).get("tools") or []))' "$CAPDIR/$f" 2>/dev/null || echo 0)" "$f"
          done | sort -rn | head -1 | awk '{print $2}')"
  if [ -z "$pick" ]; then warn "no new capture for $dest"; return 1; fi
  cp "$CAPDIR/$pick" "$OUT/payload-$dest.json"
  local n; n="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(len(d.get("tools") or []), len(json.dumps(d)))' "$OUT/payload-$dest.json")"
  note "payload-$dest.json   tools=${n% *}  bytes=${n#* }"
}

say "capturing"
B="$(snapshot)"; set_mode relevant; SID="cap-rel-$$"
turn "$SID" "What is the weather right now?";            widest_new "$B" fresh_rel
B="$(snapshot)"; turn "$SID" "And tomorrow?";            widest_new "$B" followup
B="$(snapshot)"; SID2="cap-hist-$$"
turn "$SID2" "What is on my schedule?"
turn "$SID2" "Thanks. Now what is the weather?";         widest_new "$B" toolhistory
B="$(snapshot)"; set_mode all; SID3="cap-all-$$"
turn "$SID3" "What is the weather right now?";           widest_new "$B" fresh_all

# What the engines must agree with, within 3%, or they are not answering the
# same prompt and no comparison between them means anything.
python3 - "$OUT" <<'PY'
import glob, json, os, sys
out = sys.argv[1]
seen = {}
for p in sorted(glob.glob(os.path.join(out, "payload-*.json"))):
    d = json.load(open(p))
    name = os.path.basename(p)[len("payload-"):-len(".json")]
    seen[name] = {"tools": len(d.get("tools") or []),
                  "messages": len(d.get("messages") or []),
                  "body_bytes": len(json.dumps(d)),
                  "approx_prompt_tokens": len(json.dumps(d)) // 4}
json.dump(seen, open(os.path.join(out, "prompt_tokens_seen.json"), "w"), indent=2)
print("\n   captured payloads:")
for k, v in seen.items():
    print(f"     {k:<14} tools={v['tools']:<3} msgs={v['messages']:<3} ~{v['approx_prompt_tokens']} tok")
PY
note "payloads -> $OUT"
