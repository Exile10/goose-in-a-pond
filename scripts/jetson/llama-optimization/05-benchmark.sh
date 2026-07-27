#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 05-benchmark.sh — measure real prefill (pp) + decode (tg) throughput with
# llama-bench, while logging GPU%/EMC%/temp/power via tegrastats. Writes a
# markdown report under ./reports/.
#
# Usage:
#   ./05-benchmark.sh                      # newest model in $MODELS_DIR
#   ./05-benchmark.sh /path/to/model.gguf
#   ./05-benchmark.sh --pause-ollama mymodel.gguf   # free RAM for a clean run
#
# Env: PP=512 (prompt tokens), TG=128 (gen tokens), NGL_TRY="99 32 0",
#      BATCH=512, UBATCH=512
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson
ensure_cuda_on_path

PAUSE_OLLAMA=0
ARGS=()
for a in "$@"; do
  case "$a" in
    --pause-ollama) PAUSE_OLLAMA=1 ;;
    *) ARGS+=("$a") ;;
  esac
done

BENCH="$(command -v llama-bench || echo "$LLAMA_DIR/build/bin/llama-bench")"
[ -x "$BENCH" ] || die "llama-bench not found — run ./03-build-llama-cpp.sh first."

MODEL="${ARGS[0]:-$(ls -t "$MODELS_DIR"/*.gguf 2>/dev/null | head -1)}"
[ -n "$MODEL" ] && [ -f "$MODEL" ] || die "No model. Pass a path or run ./04-download-model.sh."

PP="${PP:-512}"; TG="${TG:-128}"; BATCH="${BATCH:-512}"; UBATCH="${UBATCH:-512}"
NGL_TRY="${NGL_TRY:-99 32 0}"
REPORT_DIR="$GIAP_OPT_ROOT/reports"; mkdir -p "$REPORT_DIR"
TS="$(date +%Y%m%d-%H%M%S)"
REPORT="$REPORT_DIR/benchmark-$TS.md"
TEGRA_LOG="$REPORT_DIR/tegrastats-$TS.log"

# Scratch/telemetry handles + a single unconditional cleanup (reaps the
# background tegrastats and restores Ollama even on an early die/Ctrl-C).
RAW="$REPORT_DIR/.bench-raw-$TS.txt"
ERRF="$REPORT_DIR/.bench-err-$TS.txt"
TEGRA_PID=""
stop_tegra() { [ -n "$TEGRA_PID" ] && kill "$TEGRA_PID" 2>/dev/null; tegrastats --stop 2>/dev/null || true; TEGRA_PID=""; }
restore_ollama() { [ "$PAUSE_OLLAMA" = "1" ] && as_root systemctl start ollama 2>/dev/null || true; }
cleanup() { stop_tegra; restore_ollama; rm -f "$RAW" "$ERRF"; }
trap cleanup EXIT
if [ "$PAUSE_OLLAMA" = "1" ]; then
  sudo_prime
  log "Stopping Ollama to free unified memory for the benchmark..."
  as_root systemctl stop ollama 2>/dev/null || true
fi

hdr "Benchmark: $(basename "$MODEL")"
log "pp=$PP tg=$TG batch=$BATCH ubatch=$UBATCH | model $(du -h "$MODEL" | cut -f1)"
log "Power: $(nvpmodel -q 2>/dev/null | awk 'NR==1{print $NF}')   Free RAM: $(free -h | awk '/Mem:/{print $7}')"

# Free page cache so full GPU offload doesn't ENOMEM on the Tegra NvMap
# allocator (see free_contiguous_mem). Skip with NO_DROP_CACHES=1.
if [ "${NO_DROP_CACHES:-0}" != "1" ]; then
  sudo_prime
  free_contiguous_mem
fi

# Start telemetry capture in the background.
tegrastats --interval 500 --logfile "$TEGRA_LOG" >/dev/null 2>&1 &
TEGRA_PID=$!

# Try decreasing -ngl until one runs (works around the L4T r36.4.x allocator OOM).
USED_NGL=""
for ngl in $NGL_TRY; do
  log "Running llama-bench with -ngl $ngl ..."
  if "$BENCH" -m "$MODEL" -ngl "$ngl" -fa 1 -p "$PP" -n "$TG" -b "$BATCH" -ub "$UBATCH" -t "$(nproc)" -o md \
        > "$RAW" 2> "$ERRF"; then
    USED_NGL="$ngl"; break
  fi
  warn "-ngl $ngl failed (likely CUDA OOM); trying a lower value..."
done
stop_tegra
[ -n "$USED_NGL" ] || { cat "$ERRF" >&2; die "All -ngl attempts failed (see error log)."; }

# Confirm the GPU was actually engaged.
GPU_LINE="$(grep -iE 'CUDA[0-9]|Device 0:|compute capability' "$ERRF" | head -1)"
[ -n "$GPU_LINE" ] && ok "GPU engaged: $GPU_LINE" || warn "Could not confirm CUDA device line (check err log)."

# Telemetry peaks during the run.
PEAK_TEMP="$(grep -oE 'tj@[0-9.]+C' "$TEGRA_LOG" 2>/dev/null | grep -oE '[0-9.]+' | sort -nr | head -1)"
PEAK_GR3D="$(grep -oE 'GR3D_FREQ [0-9]+%' "$TEGRA_LOG" 2>/dev/null | grep -oE '[0-9]+' | sort -nr | head -1)"
PEAK_PWR="$(grep -oE 'VDD_IN [0-9]+mW' "$TEGRA_LOG" 2>/dev/null | grep -oE '[0-9]+' | sort -nr | head -1)"
PEAK_RAM="$(grep -oE 'RAM [0-9]+/[0-9]+MB' "$TEGRA_LOG" 2>/dev/null | grep -oE '^RAM [0-9]+' | grep -oE '[0-9]+' | sort -nr | head -1)"

# --- write the report ------------------------------------------------------
{
  echo "# llama.cpp benchmark — Jetson Orin Nano 8GB Super"
  echo
  echo "- **Date:** $(date)"
  echo "- **Model:** \`$(basename "$MODEL")\` ($(du -h "$MODEL" | cut -f1))"
  echo "- **llama.cpp:** $(git -C "$LLAMA_DIR" rev-parse --short HEAD 2>/dev/null || echo n/a)"
  echo "- **Power mode:** $(nvpmodel -q 2>/dev/null | awk 'NR==1{print $NF}') | **GPU offload (-ngl):** $USED_NGL | **flash-attn:** on"
  echo "- **Settings:** pp=$PP, tg=$TG, batch=$BATCH, ubatch=$UBATCH, threads=$(nproc)"
  echo "- **CUDA device:** ${GPU_LINE:-n/a}"
  echo
  echo "## Throughput"
  echo
  cat "$RAW"
  echo
  echo "## Telemetry peaks (during run)"
  echo
  echo "| metric | peak |"
  echo "|---|---|"
  echo "| GPU busy (GR3D_FREQ) | ${PEAK_GR3D:-?}% |"
  echo "| SoC junction temp (tj) | ${PEAK_TEMP:-?} C |"
  echo "| Board power (VDD_IN) | ${PEAK_PWR:-?} mW |"
  echo "| RAM used | ${PEAK_RAM:-?} MB / 7620 MB |"
  echo
  echo "_Raw tegrastats: $(basename "$TEGRA_LOG")_"
} > "$REPORT"

echo
hdr "Results"
sed -n '/## Throughput/,/## Telemetry/p' "$REPORT" | sed '$d'
ok "Full report: $REPORT"
# cleanup() runs on EXIT: reaps tegrastats, restores Ollama, removes scratch files.
