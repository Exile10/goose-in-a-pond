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

# The embedder, or tool_selection_mode=relevant cannot narrow and BOTH captured
# payloads come back with all 61 tools. The first capture did exactly that, and
# a C2 replayed against it would have answered an 8.5K-token prompt while C1's
# baseline answered 3.6K -- a comparison of prompt sizes wearing an engine's
# name. It lives in models/embedding/, not models/gguf/.
mkdir -p "$SCRATCH/models/embedding"
EMB=0
for emb in "$REAL_MODELS"/embedding/*.gguf; do
  [ -e "$emb" ] || continue
  ln "$(readlink -f "$emb")" "$SCRATCH/models/embedding/$(basename "$emb")" 2>/dev/null \
    || cp "$(readlink -f "$emb")" "$SCRATCH/models/embedding/$(basename "$emb")"
  note "embedder: $(basename "$emb")"; EMB=1
done
[ "$EMB" = 1 ] || warn "no embedder under $REAL_MODELS/embedding — 'relevant' will capture all 61 tools"

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
  -d "{\"tool_selection_mode\":\"$1\"}" >/dev/null; sleep 2; }

# One warm-up turn registers the model, which is what apply_jetson_settings reads
# to size the context. Without it the capture runs against an untuned engine and
# the prompt it records is not the prompt production sends.
curl -sf -N -X POST "$API/chat/stream" -H 'Content-Type: application/json' \
  -d '{"message":"hello","session_id":"cap-warmup"}' --max-time 300 >/dev/null 2>&1 || true

# RESTART. Two things need it and both fail silently without it.
#
# The Jetson tuning is applied when the per-role provider is built, at boot.
# And the embedding provider gets 30 seconds to initialise: on a cold data dir
# that races the ONNX Runtime download and TIMES OUT, leaving embedding_provider
# as None -- at which point tool_selection_mode=relevant widens to every tool and
# says so only in a log line. A capture taken in that state carried all 61 tools
# under both modes, twice, while the C1 baseline it must be compared against ran
# at 17. On the restart the runtime is already on disk and the init succeeds.
say "restarting so the tuning and the embedder are both live at boot"
kill -9 "$SERVER_PID" 2>/dev/null; wait "$SERVER_PID" 2>/dev/null
rm -f "$SCRATCH/.runtime_api_port"
POND_DATA_DIR="$SCRATCH" POND_DEV_ALLOW_LOOPBACK=1 GIAP_CAPTURE_PAYLOAD="$CAPDIR" \
RUST_LOG="warn,giap::trace=info,pond_server=info,pond_adapters_goose=debug,pond_adapters_local_inference=info" \
  "$BIN" serve --port "$PORT" >> "$SCRATCH/server.out" 2>&1 < /dev/zero &
SERVER_PID=$!
for _ in $(seq 1 150); do
  [ -s "$SCRATCH/.runtime_api_port" ] && break
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$SCRATCH/server.out" >&2; die "server exited on restart"; }
  sleep 2
done
PORT="$(tr -d ' \n' < "$SCRATCH/.runtime_api_port")"
API="http://127.0.0.1:$PORT/api/v1"
until curl -sf -o /dev/null "$API/health"; do
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$SCRATCH/server.out" >&2; die "server exited on restart"; }
  sleep 2
done
grep -a "Jetson context sized" "$SCRATCH/server.out" | tail -1 | sed 's/\x1b\[[0-9;]*m//g' | sed 's/^/   /'
if grep -aq "embedding provider failed to init" "$SCRATCH/server.out"; then
  note "note: the embedder timed out on the FIRST boot; checking the restart took"
fi

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

# Pick the turn's CHAT inference out of the several captures one turn leaves.
#
# "Widest" was the wrong rule. A turn leaves the chat inference, a memory
# extraction, and the GOAL-COMPLETENESS CHECK -- and the goal check carries the
# same tool array, so it can be just as wide. It is also the LAST of them, so
# widest-with-ties picked it, and its final user message is
# "Finish anything still outstanding for this: ..." with the whole first
# exchange sitting above it as history.
#
# Replaying that meant every substituted question was asked as a continuation of
# a conversation that had just failed to fetch weather. The C2 canary scored
# 1/10 against C1's 10/10 and it read like an engine difference.
#
# The chat inference is the FIRST new capture that carries tools and whose last
# message is a plain user turn. Filenames are sequence-numbered, so name order
# is call order.
first_chat_new() { # first_chat_new <before-listing> <dest-name>
  local before="$1" dest="$2" pick=""
  while read -r f; do
    [ -n "$f" ] || continue
    if python3 - "$CAPDIR/$f" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
msgs = d.get("messages") or []
if not (d.get("tools") or []):
    sys.exit(1)
last = msgs[-1] if msgs else {}
if last.get("role") != "user":
    sys.exit(1)
text = last.get("content") or ""
if not isinstance(text, str):
    sys.exit(1)
# The goal check and the re-engagement steer both open with a fixed preamble.
for marker in ("Finish anything still outstanding",
               "still outstanding",
               "You did not produce"):
    if marker in text:
        sys.exit(1)
sys.exit(0)
PYEOF
    then pick="$f"; break; fi
  done < <(comm -13 <(echo "$before") <(snapshot))

  if [ -z "$pick" ]; then warn "no chat-inference capture for $dest"; return 1; fi
  cp "$CAPDIR/$pick" "$OUT/payload-$dest.json"
  local n; n="$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(len(d.get("tools") or []), len(json.dumps(d)))' "$OUT/payload-$dest.json")"
  note "payload-$dest.json   tools=${n% *}  bytes=${n#* }  (chat inference)"
}

# Wait for narrowing to be LIVE before capturing anything.
#
# The embedder finishes initialising after the port opens, so the first turn
# following a restart still widens to all 61 tools while the second narrows.
# Measured: fresh_rel captured 61 while followup, one turn later, captured 23.
# Capturing on turn one therefore recorded the widened prompt as if it were the
# narrowed one. Burn turns until the selector actually reports narrowing.
set_mode relevant
say "waiting for tool-selection narrowing to come live (the embedder finishes after the port opens)"
NARROWED=0
for i in 1 2 3 4; do
  turn "cap-warm-$i" "What is the weather right now?"
  LAST="$(grep -a 'kind="tool_selection"' "$SCRATCH/server.out" | tail -1 | sed 's/\x1b\[[0-9;]*m//g')"
  T="$(sed -n 's/.*[^_]tools=\([0-9]*\).*/\1/p' <<<"$LAST")"
  TT="$(sed -n 's/.*tools_total=\([0-9]*\).*/\1/p' <<<"$LAST")"
  note "probe $i: tools=${T:-?} of ${TT:-?}"
  if [ -n "$T" ] && [ -n "$TT" ] && [ "$T" -lt "$TT" ]; then NARROWED=1; break; fi
done
[ "$NARROWED" = 1 ] || warn "selector never reported narrowing after 4 turns — the arms check below will catch it"

say "capturing"
B="$(snapshot)"; SID="cap-rel-$$"
turn "$SID" "What is the weather right now?";            first_chat_new "$B" fresh_rel
B="$(snapshot)"; turn "$SID" "And tomorrow?";            first_chat_new "$B" followup
# Snapshot AFTER the opening turn, not before it: first_chat_new takes the
# earliest NEW capture, so a snapshot taken before turn one hands back turn
# one's payload -- two messages, no tool history -- which is the opposite of
# what this arm exists to test. The multiturn workload uses it to check that a
# streaming tool-call parser survives history carrying prior tool_calls, the
# case that broke a --jinja parser before.
SID2="cap-hist-$$"
turn "$SID2" "What is on my schedule?"
B="$(snapshot)"
turn "$SID2" "Thanks. Now what is the weather?";         first_chat_new "$B" toolhistory
B="$(snapshot)"; set_mode all; SID3="cap-all-$$"
turn "$SID3" "What is the weather right now?";           first_chat_new "$B" fresh_all

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
# The two arms must actually differ, or every C2 comparison built on them is a
# comparison of one prompt against itself. Two captures have already come back
# with 61 tools under both modes.
REL_T="$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1])).get("tools") or []))' "$OUT/payload-fresh_rel.json" 2>/dev/null || echo 0)"
ALL_T="$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1])).get("tools") or []))' "$OUT/payload-fresh_all.json" 2>/dev/null || echo 0)"
if [ "$REL_T" -ge "$ALL_T" ]; then
  grep -a "tool_selection_widened\|kind=\"tool_selection\"" "$SCRATCH/server.out" | tail -2 \
    | sed 's/\x1b\[[0-9;]*m//g' | sed 's/^/   /' >&2
  die "relevant captured $REL_T tools and all captured $ALL_T — narrowing did not happen, so
   the two payloads are the same prompt. Replaying C2 against them would compare it with a
   C1 baseline that ran at a different prompt size and call the difference an engine result."
fi
note "arms differ: relevant=$REL_T tools, all=$ALL_T tools"
note "payloads -> $OUT"
