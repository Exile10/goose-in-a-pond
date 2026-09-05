#!/usr/bin/env bash
# Fuse the LoRA and convert to a Q4_K_M GGUF the experiment can serve.
#   bash convert.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
PY=.venv/bin/python
CONVERT=/opt/homebrew/opt/llama.cpp/share/llama.cpp/convert_hf_to_gguf.py
[ -f "$CONVERT" ] || { echo "convert_hf_to_gguf.py not found at $CONVERT" >&2; exit 1; }

# The convert script needs torch/gguf/transformers; install once, quietly.
$PY -c "import torch, gguf, transformers" 2>/dev/null || \
  .venv/bin/pip install -q torch gguf transformers sentencepiece

$PY -m mlx_lm fuse \
  --model hf/functiongemma-270m-it \
  --adapter-path runs/adapter \
  --save-path runs/fused

$PY "$CONVERT" runs/fused --outfile runs/functiongemma-270m-tuned-f16.gguf --outtype f16
llama-quantize runs/functiongemma-270m-tuned-f16.gguf \
               runs/functiongemma-270m-tuned-Q4_K_M.gguf Q4_K_M 2>&1 | tail -2

echo
echo "serve it:   llama-server -m runs/functiongemma-270m-tuned-Q4_K_M.gguf --port 8094 -c 2048 -ngl 99"
echo "score it:   python3 picker_eval.py --port 8094 --label tuned"
echo "full loop:  PICKER_GGUF=\$PWD/runs/functiongemma-270m-tuned-Q4_K_M.gguf PICKER_PORT=8094 ../serve.sh start"
