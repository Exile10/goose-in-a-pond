#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# run-c3-vllm.sh — NVIDIA's vLLM container, the other engine NVIDIA lists for
# Gemma 4 on Jetson.
#
#   bash run-c3-vllm.sh --model google/gemma-4-E2B-it --payloads ~/bakeoff-payloads
#
# NVIDIA's own guidance is "Orin Nano: E2B only", so E4B is expected to be a
# recorded refusal rather than a run. The first question is not speed, it is
# whether the thing fits at all on 7,619 MB with a 16K window: vLLM refuses to
# start when the KV cache for --max-model-len does not fit, and that refusal is
# the honest answer, so the sweep records the first configuration that fits
# rather than forcing one.
#
# HF_TOKEN is read from the environment only. Never put it in this file or in a
# command line: gated Gemma weights need it and a token in a script is a token
# in git history.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$HERE/lib.sh"

MODEL="google/gemma-4-E2B-it"; PAYLOADS=""; ALIAS=""
IMAGE="ghcr.io/nvidia-ai-iot/vllm:gemma4-jetson-orin"
WORKLOADS="voice,followup,decode,fresh_rel"; REPEATS=5
CTX_SWEEP="16384 8192 4096"; UTIL_SWEEP="0.75 0.60 0.50"
while [ $# -gt 0 ]; do
  case "$1" in
    --model)     MODEL="$2"; shift 2 ;;
    --alias)     ALIAS="$2"; shift 2 ;;
    --payloads)  PAYLOADS="$2"; shift 2 ;;
    --image)     IMAGE="$2"; shift 2 ;;
    --workloads) WORKLOADS="$2"; shift 2 ;;
    -r|--repeats) REPEATS="$2"; shift 2 ;;
    --ctx)       CTX_SWEEP="$2"; shift 2 ;;
    --util)      UTIL_SWEEP="$2"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -n "$PAYLOADS" ] && [ -f "$PAYLOADS/payload-fresh_rel.json" ] || die "--payloads DIR is required"
[ -n "$ALIAS" ] || ALIAS="$(basename "$MODEL")"
command -v docker >/dev/null || die "docker is required for C3"

preflight
stamp_power_env
OC0="$(oc3_count)"; T0="$(date +%s)"
RUN="$RESULTS_ROOT/c3-vllm/$(basename "$MODEL")"
mkdir -p "$RUN"

CONTAINER="bakeoff-c3-$$"; MEMWATCH_PID=""
cleanup() {
  [ -n "$MEMWATCH_PID" ] && kill "$MEMWATCH_PID" 2>/dev/null
  docker rm -f "$CONTAINER" >/dev/null 2>&1
  tegra_stop
}
trap cleanup EXIT

drop_caches
MEM_BEFORE="$(mem_snapshot)"
tegra_start "$RUN/tegrastats.log"
bash "$HERE/memwatch.sh" --out "$RUN/mem.csv" --interval 0.5 & MEMWATCH_PID=$!

FIT_CTX=""; FIT_UTIL=""; ATTEMPTS="[]"
say "fit sweep (vLLM refuses to start when the KV cache does not fit — that refusal IS the answer)"
for ctx in $CTX_SWEEP; do
  for util in $UTIL_SWEEP; do
    note "trying --max-model-len $ctx --gpu-memory-utilization $util"
    docker rm -f "$CONTAINER" >/dev/null 2>&1
    docker run -d --name "$CONTAINER" --runtime nvidia --network host --ipc host \
      -v "$HOME/.cache/huggingface:/root/.cache/huggingface" \
      ${HF_TOKEN:+-e HF_TOKEN="$HF_TOKEN"} \
      "$IMAGE" vllm serve "$MODEL" \
        --host 127.0.0.1 --port 8000 \
        --served-model-name "$ALIAS" \
        --max-model-len "$ctx" --max-num-seqs 1 \
        --gpu-memory-utilization "$util" \
        --enable-prefix-caching \
        --enable-auto-tool-choice --tool-call-parser gemma4 --reasoning-parser gemma4 \
      >/dev/null 2>"$RUN/docker-run.err" || { warn "docker run failed"; continue; }

    ok=0
    for _ in $(seq 1 150); do   # vLLM start is minutes, not seconds
      if curl -sf -o /dev/null http://127.0.0.1:8000/health 2>/dev/null; then ok=1; break; fi
      docker ps -q -f name="$CONTAINER" | grep -q . || break
      sleep 4
    done
    docker logs "$CONTAINER" > "$RUN/server-${ctx}-${util}.log" 2>&1 || true
    ATTEMPTS="$(python3 -c 'import json,sys; a=json.loads(sys.argv[1]); a.append({"max_model_len":int(sys.argv[2]),"gpu_memory_utilization":float(sys.argv[3]),"started":bool(int(sys.argv[4]))}); print(json.dumps(a))' "$ATTEMPTS" "$ctx" "$util" "$ok")"
    if [ "$ok" = 1 ]; then FIT_CTX="$ctx"; FIT_UTIL="$util"; note "fits at ctx=$ctx util=$util"; break 2; fi
    note "did not start; last error:"
    grep -iE 'out of memory|no available memory|KV cache|ValueError|Error' "$RUN/server-${ctx}-${util}.log" | tail -3 | sed 's/^/     /'
  done
done

if [ -z "$FIT_CTX" ]; then
  add_warning "vLLM did not start in ANY swept configuration on this board — recorded as blocked_at=memory_fit"
  kill "$MEMWATCH_PID" 2>/dev/null; MEMWATCH_PID=""; tegra_stop
  export BAKEOFF_OC3_DELTA=$(( $(oc3_count) - OC0 )) BAKEOFF_OC3_SECS=$(( $(date +%s) - T0 ))
  envelope_write "$RUN/envelope.json" c3-vllm "$IMAGE" "" blocked \
    '{"workloads":{}}' \
    "$(bash "$HERE/memwatch.sh" --summary "$RUN/mem.csv")" \
    "$(tegra_summary "$RUN/tegrastats.log")" \
    "$(python3 -c 'import json,sys; print(json.dumps({"blocked_at":"memory_fit","attempts":json.loads(sys.argv[1]),"hf_model":sys.argv[2]}))' "$ATTEMPTS" "$MODEL")"
  say "C3 blocked at memory fit — envelope written, this is a result"
  exit 0
fi

note "served: $(curl -sf http://127.0.0.1:8000/v1/models | python3 -c 'import json,sys;print(",".join(m["id"] for m in json.load(sys.stdin).get("data",[])))' 2>/dev/null)"
say "workloads: $WORKLOADS (x$REPEATS)"
python3 "$HERE/bakeoff_turns.py" --mode openai --base http://127.0.0.1:8000 \
  --model "$ALIAS" --payloads "$PAYLOADS" --workloads "$WORKLOADS" -r "$REPEATS" \
  --out "$RUN/runs.json" | tee "$RUN/driver.log"

kill "$MEMWATCH_PID" 2>/dev/null; MEMWATCH_PID=""
tegra_stop
docker logs "$CONTAINER" > "$RUN/server.log" 2>&1 || true
export BAKEOFF_OC3_DELTA=$(( $(oc3_count) - OC0 )) BAKEOFF_OC3_SECS=$(( $(date +%s) - T0 ))
export BAKEOFF_CLOCKS="$( [ "$(gpu_cur_mhz)" = "$(gpu_max_mhz)" ] && echo pinned || echo dynamic )"
envelope_write "$RUN/envelope.json" c3-vllm "$IMAGE" "" "ctx${FIT_CTX}-util${FIT_UTIL}" \
  "$(cat "$RUN/runs.json")" \
  "$(python3 -c 'import json,sys; a=json.loads(sys.argv[1]); c=json.loads(sys.argv[2]); c["before"]=a; print(json.dumps(c))' "$MEM_BEFORE" "$(bash "$HERE/memwatch.sh" --summary "$RUN/mem.csv")")" \
  "$(tegra_summary "$RUN/tegrastats.log")" \
  "$(python3 -c 'import json,sys; print(json.dumps({"hf_model":sys.argv[1],"max_model_len":int(sys.argv[2]),"gpu_memory_utilization":float(sys.argv[3]),"attempts":json.loads(sys.argv[4])}))' "$MODEL" "$FIT_CTX" "$FIT_UTIL" "$ATTEMPTS")"
say "done"; note "results: $RUN"
