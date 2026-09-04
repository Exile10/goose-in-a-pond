#!/usr/bin/env bash
# Fine-tune FunctionGemma 270M with an MLX LoRA on the generated data.
#
# One-time prerequisite (the weights are licence-gated on Hugging Face):
#   1. accept the licence at https://huggingface.co/google/functiongemma-270m-it
#   2. hf auth login          (from this venv: .venv/bin/hf auth login)
# Then:
#   bash train.sh             ~2000 examples, minutes on Apple silicon
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
PY=.venv/bin/python

[ -f data/train.jsonl ] || $PY gen_data.py

if [ ! -f hf/functiongemma-270m-it/config.json ]; then
  echo "fetching base weights (needs licence acceptance + hf auth login)"
  $PY - <<'FETCH'
import os
from huggingface_hub import snapshot_download
snapshot_download("google/functiongemma-270m-it",
                  local_dir=os.path.abspath("hf/functiongemma-270m-it"))
FETCH
fi

$PY -m mlx_lm lora \
  --model hf/functiongemma-270m-it \
  --train \
  --data data \
  --fine-tune-type lora \
  --mask-prompt \
  --num-layers 16 \
  --batch-size 4 \
  --iters 800 \
  --learning-rate 1e-4 \
  --adapter-path runs/adapter \
  --save-every 200

echo
echo "adapter in runs/adapter -- next: bash convert.sh"
