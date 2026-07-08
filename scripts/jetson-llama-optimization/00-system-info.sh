#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 00-system-info.sh — read-only snapshot of the Jetson's AI-relevant state.
# Safe to run anytime. Writes a timestamped report under ./reports/.
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson

REPORT_DIR="$GIAP_OPT_ROOT/reports"
mkdir -p "$REPORT_DIR"
TS="$(date +%Y%m%d-%H%M%S)"
REPORT="$REPORT_DIR/system-info-$TS.txt"

# Tee everything to the report file as well as the console.
exec > >(tee "$REPORT") 2>&1

hdr "Board & L4T / JetPack"
tr -d '\0' < /proc/device-tree/model 2>/dev/null; echo
cat /etc/nv_tegra_release 2>/dev/null | head -1
grep PRETTY_NAME /etc/os-release 2>/dev/null
echo "Kernel: $(uname -r)"
dpkg -l nvidia-jetpack 2>/dev/null | awk '/nvidia-jetpack/{print "JetPack meta:", $3}' || true

hdr "CUDA / Driver"
ensure_cuda_on_path 2>/dev/null || warn "CUDA not on PATH"
nvcc --version 2>/dev/null | grep release || warn "nvcc unavailable"
if have nvidia-smi; then nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv 2>/dev/null || nvidia-smi 2>/dev/null | head -10; fi
echo "cuBLAS: $(ls /usr/local/cuda/lib64/libcublas.so* 2>/dev/null | head -1 || echo MISSING)"
echo "cuDNN:    $(dpkg -l 2>/dev/null | grep -ci cudnn | sed 's/^0$/not installed/;s/^[1-9].*/installed/')"
echo "TensorRT: $(dpkg -l 2>/dev/null | grep -ciE 'tensorrt|nvinfer' | sed 's/^0$/not installed/;s/^[1-9].*/installed/')"

hdr "Compute & Memory"
echo "CPU: $(nproc) x $(lscpu 2>/dev/null | awk -F: '/Model name/{gsub(/^ +/,"",$2);print $2;exit}')"
free -h | grep -E 'Mem|Swap'
echo "zram swap devices: $(ls /dev/zram* 2>/dev/null | wc -l)"

hdr "Power / Clocks"
echo -n "nvpmodel: "; nvpmodel -q 2>/dev/null | tr '\n' ' '; echo
GPU_DF="$(ls -d /sys/class/devfreq/*.gpu 2>/dev/null | head -1)"
if [ -n "$GPU_DF" ]; then
  printf 'GPU clock: cur=%s max=%s governor=%s\n' \
    "$(cat "$GPU_DF/cur_freq" 2>/dev/null)" "$(cat "$GPU_DF/max_freq" 2>/dev/null)" \
    "$(cat "$GPU_DF/governor" 2>/dev/null)"
fi
echo "CPU governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)"

hdr "Thermals & Power draw (1 sample)"
TEGRA_SAMPLE="$(timeout 2 tegrastats --interval 1000 2>/dev/null | head -1)"
[ -n "$TEGRA_SAMPLE" ] && echo "$TEGRA_SAMPLE" || warn "tegrastats unavailable"

hdr "Inference stack"
echo "llama.cpp dir: $([ -d "$LLAMA_DIR" ] && echo "$LLAMA_DIR" || echo 'not cloned')"
for b in llama-cli llama-server llama-bench; do
  p="$(command -v "$b" 2>/dev/null || echo "$BIN_DIR/$b")"
  [ -x "$p" ] && echo "  $b: $p" || echo "  $b: not installed"
done
echo "Models in $MODELS_DIR: $(ls "$MODELS_DIR"/*.gguf 2>/dev/null | wc -l) gguf file(s)"
echo -n "Ollama: "; have ollama && { ollama --version 2>/dev/null | head -1; systemctl is-active ollama 2>/dev/null | sed 's/^/  service: /'; } || echo "absent"

hdr "Build toolchain"
for t in gcc g++ cmake ninja ccache git python3; do
  if have "$t"; then printf '  %-8s %s\n' "$t" "$($t --version 2>/dev/null | head -1)"; else printf '  %-8s MISSING\n' "$t"; fi
done

echo
ok "Report saved to $REPORT"
