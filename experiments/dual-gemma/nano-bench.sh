#!/usr/bin/env bash
# Drive the dual-gemma bench on the Jetson's silicon, from the Mac.
#
#   ./nano-bench.sh start     nano llama-servers + ssh tunnel + console
#   ./nano-bench.sh stop      tear all three down (nano included)
#   ./nano-bench.sh status
#
# Console: http://localhost:8093  (planner/picker inference on the Orin)
# Requires the `ssh nano` alias and the models already on the board
# (E2B in the pond data dir, FunctionGemma in ~/bench-models -- see README).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLANNER_LOCAL=18091 PICKER_LOCAL=18092 CONSOLE=8093

nano_up() {
  ssh -o BatchMode=yes -o ConnectTimeout=8 nano '
    up() { curl -sf -m 2 "localhost:$1/health" >/dev/null 2>&1; }
    mkdir -p bench-logs
    if ! up 8091; then
      nohup ~/llama.cpp/build/bin/llama-server \
        -m ~/.local/share/goose-in-a-pond/models/gguf/gemma-4-E2B-it-Q4_K_M.gguf \
        --port 8091 -c 8192 -ngl 99 --jinja --host 127.0.0.1 > bench-logs/e2b.log 2>&1 &
    fi
    if ! up 8092; then
      nohup ~/llama.cpp/build/bin/llama-server \
        -m ~/bench-models/functiongemma-270m-it-Q4_K_M.gguf \
        --port 8092 -c 2048 -ngl 99 --host 127.0.0.1 > bench-logs/fg.log 2>&1 &
    fi
    for _ in $(seq 1 45); do up 8091 && up 8092 && exit 0; sleep 2; done
    echo "nano servers did not come up; see ~/bench-logs/" >&2; exit 1'
}

tunnel_up() {
  pgrep -f "ssh -fN -L $PLANNER_LOCAL" >/dev/null || \
    ssh -fN -L $PLANNER_LOCAL:localhost:8091 -L $PICKER_LOCAL:localhost:8092 nano
}

console_up() {
  pkill -f "[w]eb.py" 2>/dev/null || true
  sleep 1
  (cd "$HERE" && nohup python3 web.py --no-think \
      --planner-port $PLANNER_LOCAL --picker-port $PICKER_LOCAL \
      --backend-label "Jetson Orin Nano" > logs/web.log 2>&1 &)
}

check() {
  local what="$1" url="$2"
  if curl -sf -m 3 "$url" >/dev/null 2>&1; then echo "  $what  UP"; else echo "  $what  DOWN"; fi
}

case "${1:-start}" in
  start)
    echo "starting nano servers (first model load can take a minute)"
    nano_up
    tunnel_up
    console_up
    sleep 2
    check "nano planner (via tunnel)" "http://localhost:$PLANNER_LOCAL/health"
    check "nano picker  (via tunnel)" "http://localhost:$PICKER_LOCAL/health"
    check "console                  " "http://localhost:$CONSOLE/health"
    echo
    echo "open http://localhost:$CONSOLE -- every reply is inferred on the Orin"
    ;;
  stop)
    pkill -f "[w]eb.py" 2>/dev/null || true
    pkill -f "ssh -fN -L $PLANNER_LOCAL" 2>/dev/null || true
    ssh -o BatchMode=yes nano 'pkill -f llama-server' 2>/dev/null || true
    echo "console, tunnel and nano servers stopped"
    ;;
  status)
    check "nano planner (via tunnel)" "http://localhost:$PLANNER_LOCAL/health"
    check "nano picker  (via tunnel)" "http://localhost:$PICKER_LOCAL/health"
    check "console                  " "http://localhost:$CONSOLE/health"
    ;;
  *) echo "usage: $0 {start|stop|status}" >&2; exit 2 ;;
esac
