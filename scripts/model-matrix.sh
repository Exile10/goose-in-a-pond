#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# model-matrix.sh — drive SEVERAL models through the same turns and report what
# each one actually did.
#
#   scripts/model-matrix.sh                        every GGUF that can chat
#   scripts/model-matrix.sh --models a,b,c         just these
#   scripts/model-matrix.sh --no-build             use the existing binary
#   scripts/model-matrix.sh --json OUT.json        machine-readable results
#   scripts/model-matrix.sh --thinking auto|on|off which thinking_mode to set
#   scripts/model-matrix.sh --tools all|relevant   which tool_selection_mode
#   scripts/model-matrix.sh --bin PATH             drive a specific pond-server
#
# --bin is what makes a before/after honest. Keep a copy of the OLD binary and
# point this at it, rather than rebuilding between the two runs: a rebuild in
# between means the two halves were measured minutes apart on a machine whose
# thermal and cache state moved, and it makes it impossible to re-run the
# "before" once the source has changed.
#
# WHY THIS EXISTS, SEPARATELY FROM pai-bench.sh
#
# `pai-bench.sh` answers "do the eight capabilities work on this hardware", for
# ONE model. This answers "does the pond work for a model it was not built
# around", across many — which is a different question, and the one that
# catches a layer still deciding from a filename.
#
# The columns that matter are not the speed ones:
#
#   reengage   attempts BEYOND the first. A model that reasons but was given no
#              <thinking> section ends its turn silent, gets steered back with
#              EMPTY_TURN_STEER, and runs the WHOLE turn again — prefill and
#              tool schemas included. It is the hidden cost of a capability
#              mismatch and it reads as "the pond is slow", never as a bug.
#   tool       whether the tool-using turn actually called a tool. A model that
#              answers a weather question from imagination looks identical to
#              one that answered it correctly, unless you check this.
#
# Every model gets its own scratch POND_DATA_DIR with ONE GGUF hard-linked in.
# Never symlinked: a symlinked models/ IS the real directory, and the startup
# hf_cache migration MOVES real files out of it. That cost 3.1 GB once.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DO_BUILD=1
MODELS=""
JSON_OUT=""
THINKING="auto"
TOOLS="all"
BIN_OVERRIDE=""
PORT="${PORT:-4988}"

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) DO_BUILD=0 ;;
    --models)   MODELS="${2:-}"; shift ;;
    --json)     JSON_OUT="${2:-}"; shift ;;
    --thinking) THINKING="${2:-}"; shift ;;
    --tools)    TOOLS="${2:-}"; shift ;;
    --bin)      BIN_OVERRIDE="${2:-}"; shift ;;
    --port)     PORT="${2:-}"; shift ;;
    -h|--help)  sed -n '2,34p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

case "$(uname -s)" in
  Darwin) REAL_MODELS="$HOME/Library/Application Support/goose-in-a-pond/models" ;;
  *)      REAL_MODELS="$HOME/.local/share/goose-in-a-pond/models" ;;
esac

if [ ! -d "$REAL_MODELS/gguf" ]; then
  echo "FATAL: no model directory at $REAL_MODELS/gguf" >&2; exit 1
fi

# Models that cannot serve a chat turn regardless of any fix here: the MTP
# speculative-decode drafts carry no chat template at all, and the 270m is a
# tool-calling experiment, not an assistant. Excluding them keeps the table
# about model agnosticity rather than about known non-models.
SKIP_RE='assistant|old_functiongemma'

if [ -z "$MODELS" ]; then
  MODELS="$(ls -1 "$REAL_MODELS/gguf"/*.gguf 2>/dev/null \
    | xargs -n1 basename | sed 's/\.gguf$//' \
    | grep -Ev "$SKIP_RE" | paste -sd, -)"
fi

if [ -n "$BIN_OVERRIDE" ]; then DO_BUILD=0; fi
if [ "$DO_BUILD" = "1" ]; then
  echo "building pond-server ..."
  SQLX_OFFLINE=true cargo build -p pond-server 2>&1 | tail -3
fi
BIN="${BIN_OVERRIDE:-$REPO_ROOT/target/debug/pond-server}"
[ -x "$BIN" ] || { echo "FATAL: no binary at $BIN" >&2; exit 1; }

# An ONNX Runtime already present on this machine, so each model does not
# re-download one into its own throwaway data dir.
if [ -z "${ORT_DYLIB_PATH:-}" ]; then
  for cand in \
    "$HOME/Library/Application Support/goose-in-a-pond/lib/libonnxruntime."*.dylib \
    "$HOME/.local/share/goose-in-a-pond/lib/libonnxruntime."*.so \
    /opt/homebrew/lib/libonnxruntime.dylib \
    /usr/local/lib/libonnxruntime.dylib \
    /usr/lib/libonnxruntime.so; do
    if [ -e "$cand" ]; then ORT_DYLIB_PATH="$cand"; export ORT_DYLIB_PATH; break; fi
  done
fi
[ -n "${ORT_DYLIB_PATH:-}" ] && echo "onnxruntime: $ORT_DYLIB_PATH (reused)"

RESULTS_DIR="$(mktemp -d)"
echo "results: $RESULTS_DIR"

# The three turns, chosen so each isolates one thing:
#   1. plain     — does it answer at all, and what does a COLD turn cost
#   2. tool      — does it call a tool, or invent the answer
#   3. followup  — does the KV prefix survive into turn 2 (reuse TTFT)
PROMPT_1='Say hello in one short sentence.'
PROMPT_2='What is the weather right now? Use your tools.'
PROMPT_3='Thanks. Now say goodbye in one short sentence.'

run_model() {
  local model="$1"
  local data_dir; data_dir="$(mktemp -d)"
  local log="$RESULTS_DIR/$model.log"
  local out="$RESULTS_DIR/$model.json"

  mkdir -p "$data_dir/models/gguf" "$data_dir/bin"

  # Pre-seed espeak-ng-data. A fresh data dir otherwise downloads ~18 MB of it
  # during startup, per model, and the health check times out waiting. Safe to
  # SYMLINK unlike the models directory: the hf_cache migration walks only
  # `models/gguf` and MOVES what it finds, which is why a symlinked models/ once
  # ate a real pond's weights. Nothing rewrites `bin/`.
  for esp in /opt/homebrew/share/espeak-ng-data /usr/share/espeak-ng-data \
             "$HOME/Library/Application Support/goose-in-a-pond/bin/espeak-ng-data" \
             "$HOME/.local/share/goose-in-a-pond/bin/espeak-ng-data"; do
    if [ -d "$esp" ]; then ln -s "$esp" "$data_dir/bin/espeak-ng-data" 2>/dev/null; break; fi
  done

  local entry=""
  for path in "$REAL_MODELS/gguf/$model.gguf" "$REAL_MODELS/gguf/$model"-*.gguf; do
    [ -e "$path" ] && { entry="$(basename "$path")"; break; }
  done
  if [ -z "$entry" ]; then
    echo "  SKIP $model — no file"; rm -rf "$data_dir"; return
  fi

  local src="$REAL_MODELS/gguf/$entry"
  src="$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "$src")"
  [ -f "$src" ] || { echo "  SKIP $model — dangling symlink"; rm -rf "$data_dir"; return; }
  ln "$src" "$data_dir/models/gguf/$entry" 2>/dev/null \
    || cp "$src" "$data_dir/models/gguf/$entry" \
    || { echo "  SKIP $model — could not stage"; rm -rf "$data_dir"; return; }

  # ORT_DYLIB_PATH is exported at the top when a runtime was found on this
  # machine, and inherited from here. Without it every model re-downloads ~30 MB
  # into its own scratch dir, because the dir is wiped between models -- minutes
  # per model of the harness measuring the network rather than the model.
  # Deliberately NOT an inline `VAR=x cmd` prefix: the macOS path contains a
  # space ("Application Support") and `${VAR:+VAR="$VAR"}` does not survive word
  # splitting, which turned the assignment into a command and failed every model
  # instantly with "No such file or directory".
  POND_DATA_DIR="$data_dir" POND_DEV_ALLOW_LOOPBACK=1 \
    RUST_LOG="warn,giap::trace=info,pond_adapters_goose=debug,pond_adapters_local_inference=debug,goose_local_inference=debug" \
    "$BIN" serve --port "$PORT" > "$log" 2>&1 &
  local pid=$!

  local ready=0
  for _ in $(seq 1 180); do
    curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health" && { ready=1; break; }
    kill -0 "$pid" 2>/dev/null || break
    sleep 2
  done
  if [ "$ready" != "1" ]; then
    echo "  FAIL $model — server never became healthy (see $log)"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; rm -rf "$data_dir"; return
  fi

  sqlite3 "$data_dir/pond_system.db" \
    "INSERT INTO onboarding_state (id, current_step) VALUES (1, 'Completed');" 2>/dev/null

  curl -s -X PUT "http://127.0.0.1:$PORT/api/v1/settings" \
    -H 'Content-Type: application/json' \
    -d "{\"chat_provider\":\"local\",\"chat_model\":\"$model\",\"thinking_mode\":\"$THINKING\",\"tool_selection_mode\":\"$TOOLS\",\"show_turn_stats\":true}" \
    > /dev/null

  local sid="matrix-$$"
  local i=0
  echo "[" > "$out"
  for prompt in "$PROMPT_1" "$PROMPT_2" "$PROMPT_3"; do
    i=$((i+1))
    local raw="$RESULTS_DIR/$model.turn$i.sse"
    curl -s -N -X POST "http://127.0.0.1:$PORT/api/v1/chat/stream" \
      -H 'Content-Type: application/json' \
      -d "$(python3 -c 'import json,sys;print(json.dumps({"message":sys.argv[1],"session_id":sys.argv[2]}))' "$prompt" "$sid")" \
      --max-time 420 > "$raw" 2>&1
    [ "$i" = "1" ] || echo "," >> "$out"
    python3 - "$raw" "$i" >> "$out" <<'PY'
import json, sys
raw, turn = sys.argv[1], int(sys.argv[2])
stats, text, tools = None, [], []
for line in open(raw, errors="replace"):
    line = line.strip()
    if not line.startswith("data:"):
        continue
    try:
        ev = json.loads(line[5:].strip())
    except Exception:
        continue
    t = ev.get("type")
    if t == "turn_stats":
        stats = ev
    elif t == "text":
        text.append(ev.get("content") or "")
    elif t == "tool_call":
        tools.append(ev.get("tool") or "?")
s = stats or {}
print(json.dumps({
    "turn": turn,
    "ttft_ms": s.get("ttft_ms"),
    "prefill_ms": s.get("prefill_ms"),
    "decode_ms": s.get("decode_ms"),
    "model_load_ms": s.get("model_load_ms"),
    "prompt_tokens": s.get("prompt_tokens"),
    "completion_tokens": s.get("completion_tokens"),
    "reasoning_tokens": s.get("reasoning_tokens"),
    "reengagements": s.get("reengagements"),
    "tools_called": tools,
    "reply_chars": len("".join(text)),
}))
PY
  done
  echo "]" >> "$out"

  # Stop the server and do not hang if it declines to go. A bare
  # `kill; wait` hung a completed run indefinitely: the child holds the model
  # and an audio device, and a TERM it does not act on leaves `wait` blocking
  # forever with every result already on disk.
  kill "$pid" 2>/dev/null
  for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
  kill -9 "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  rm -rf "$data_dir"
  echo "  done $model"
}

echo "models: $MODELS"
echo "thinking_mode: $THINKING"
echo "tool_selection_mode: $TOOLS"
echo "binary: $BIN"
echo
IFS=',' read -ra LIST <<< "$MODELS"
for m in "${LIST[@]}"; do
  [ -n "$m" ] || continue
  echo "── $m"
  run_model "$m"
done

echo
python3 - "$RESULTS_DIR" "${JSON_OUT:-}" <<'PY'
import json, os, sys, glob
d = sys.argv[1]
out = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] else None
rows, allres = [], {}
for f in sorted(glob.glob(os.path.join(d, "*.json"))):
    model = os.path.basename(f)[:-5]
    try:
        turns = json.load(open(f))
    except Exception:
        rows.append((model, "PARSE-FAIL", "", "", "", "", "", ""))
        continue
    allres[model] = turns
    t1 = turns[0] if turns else {}
    t2 = turns[1] if len(turns) > 1 else {}
    t3 = turns[2] if len(turns) > 2 else {}
    reeng = sum((t.get("reengagements") or 0) for t in turns)
    tools = ",".join(x for t in turns for x in (t.get("tools_called") or []))
    def ms(v): return f"{v/1000:.2f}s" if isinstance(v, (int, float)) else "—"
    rows.append((
        model,
        ms(t1.get("ttft_ms")),
        ms(t3.get("ttft_ms")),
        str(t1.get("prompt_tokens") or "—"),
        ms(t1.get("prefill_ms")),
        str(reeng),
        (tools or "NONE"),
        str(sum((t.get("reply_chars") or 0) for t in turns)),
    ))
hdr = ("model", "ttft cold", "ttft reuse", "prompt tok", "prefill", "reengage", "tools called", "reply chars")
w = [max(len(str(r[i])) for r in ([hdr] + rows)) for i in range(len(hdr))]
def line(r): return "  ".join(str(r[i]).ljust(w[i]) for i in range(len(hdr)))
print(line(hdr)); print("  ".join("-" * x for x in w))
for r in rows: print(line(r))
if out:
    json.dump(allres, open(out, "w"), indent=2)
    print(f"\nwrote {out}")
PY
