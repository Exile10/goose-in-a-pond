#!/usr/bin/env bash
# Start (or stop) the two llama-server instances this experiment talks to.
#
#   ./serve.sh start    two servers, backgrounded, logs in ./logs
#   ./serve.sh stop
#   ./serve.sh status
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODELS="${GIAP_MODELS:-$HOME/Library/Application Support/goose-in-a-pond/models/gguf}"
PLANNER_GGUF="${PLANNER_GGUF:-$MODELS/gemma-4-E2B-it-Q4_K_M.gguf}"
PICKER_GGUF="${PICKER_GGUF:-$MODELS/old_functiongemma-270m-it-Q4_K_M.gguf}"
PLANNER_PORT="${PLANNER_PORT:-8091}"
PICKER_PORT="${PICKER_PORT:-8092}"
# Bounded thinking for the planner: -1 unrestricted (default), 0 off at the
# server, N>0 a hard token budget. Decide per SESSION -- the think token sits
# at the top of the prompt, so changing this invalidates the whole KV cache.
THINK_BUDGET="${THINK_BUDGET:--1}"
LOGS="$HERE/logs"

start_one() {
  local gguf="$1" port="$2" name="$3" ctx="$4" extra="${5:-}"
  if curl -sf -m 2 "http://localhost:$port/health" >/dev/null 2>&1; then
    echo "  $name already up on :$port"
    return
  fi
  [ -f "$gguf" ] || { echo "  MISSING: $gguf" >&2; exit 1; }
  mkdir -p "$LOGS"
  # shellcheck disable=SC2086
  nohup llama-server -m "$gguf" --port "$port" -c "$ctx" -ngl 99 $extra \
    > "$LOGS/$name.log" 2>&1 &
  echo "  $name starting on :$port (log: logs/$name.log)"
}

wait_up() {
  local port="$1" name="$2"
  for _ in $(seq 1 90); do
    curl -sf -m 2 "http://localhost:$port/health" >/dev/null 2>&1 && { echo "  $name ready"; return; }
    sleep 1
  done
  echo "  $name did not come up; see logs/$name.log" >&2
  exit 1
}

case "${1:-start}" in
  start)
    echo "starting servers"
    start_one "$PLANNER_GGUF" "$PLANNER_PORT" planner 8192 "--jinja --reasoning-budget $THINK_BUDGET"
    start_one "$PICKER_GGUF"  "$PICKER_PORT"  picker  2048
    wait_up "$PLANNER_PORT" planner
    wait_up "$PICKER_PORT"  picker
    echo "both up. run: python3 chat.py"
    ;;
  stop)
    pkill -f "llama-server.*--port $PLANNER_PORT" 2>/dev/null || true
    pkill -f "llama-server.*--port $PICKER_PORT"  2>/dev/null || true
    echo "stopped"
    ;;
  status)
    for pair in "planner:$PLANNER_PORT" "picker:$PICKER_PORT"; do
      name="${pair%%:*}"; port="${pair##*:}"
      if curl -sf -m 2 "http://localhost:$port/health" >/dev/null 2>&1; then
        echo "  $name  UP    :$port"
      else
        echo "  $name  DOWN  :$port"
      fi
    done
    ;;
  *) echo "usage: $0 {start|stop|status}" >&2; exit 2 ;;
esac
