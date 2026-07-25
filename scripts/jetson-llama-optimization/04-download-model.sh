#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 04-download-model.sh — fetch a GGUF model sized for the 8GB unified pool.
#
# All presets are DENSE models (MoE GGUFs currently hang on decode on sm_87 in
# recent llama.cpp). Files land in $MODELS_DIR.
#
# Usage:
#   ./04-download-model.sh                 # default: llama3.2-3b (best all-round)
#   ./04-download-model.sh qwen2.5-3b      # a named preset
#   ./04-download-model.sh --list          # list presets
#   ./04-download-model.sh bartowski/Some-GGUF:Q4_K_M   # any repo:quant
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"

mkdir -p "$MODELS_DIR"

# preset -> "hf_repo|quant|note"
declare -A PRESETS=(
  [llama3.2-3b]="bartowski/Llama-3.2-3B-Instruct-GGUF|Q4_K_M|PRIMARY: ~2GB, big headroom, ~28-29 tok/s gen"
  [qwen2.5-3b]="bartowski/Qwen2.5-3B-Instruct-GGUF|Q4_K_M|Strong 3B alternative, ~28-30 tok/s gen"
  [llama3.2-1b]="bartowski/Llama-3.2-1B-Instruct-GGUF|Q8_0|Fast/low-latency, ~1.3GB, >30 tok/s"
  [qwen2.5-7b]="bartowski/Qwen2.5-7B-Instruct-GGUF|Q4_K_M|CEILING: ~4.7GB, use -c 4096 -b 256 + KV-quant, ~14-15 tok/s"
)

if [ "${1:-}" = "--list" ]; then
  hdr "Model presets (all dense, sized for 8GB unified memory)"
  for k in llama3.2-3b qwen2.5-3b llama3.2-1b qwen2.5-7b; do
    IFS='|' read -r repo quant note <<< "${PRESETS[$k]}"
    printf '  %-13s %s:%s\n                %s\n' "$k" "$repo" "$quant" "$note"
  done
  exit 0
fi

SEL="${1:-llama3.2-3b}"
if [ -n "${PRESETS[$SEL]:-}" ]; then
  IFS='|' read -r REPO QUANT NOTE <<< "${PRESETS[$SEL]}"
elif [[ "$SEL" == *:* ]]; then
  REPO="${SEL%%:*}"; QUANT="${SEL##*:}"; NOTE="custom"
else
  die "Unknown preset '$SEL'. Try --list, a preset name, or repo:QUANT."
fi

hdr "Resolving $REPO ($QUANT)"
log "$NOTE"
# Ask the HF API which .gguf file matches this quant (robust to naming).
API="https://huggingface.co/api/models/$REPO"
JSON_TMP="$(mktemp)"
trap 'rm -f "$JSON_TMP"' EXIT
curl -fsSL "$API" -o "$JSON_TMP" 2>/dev/null || true
FILE="$(QUANT="$QUANT" python3 -c '
import json, os, sys
quant = os.environ["QUANT"].lower()
try:
    data = json.load(open(sys.argv[1]))
except Exception:
    print(""); sys.exit(0)
ggufs = [s.get("rfilename","") for s in data.get("siblings", []) if s.get("rfilename","").endswith(".gguf")]
cands = [f for f in ggufs if quant in f.lower()]
single = [f for f in cands if "-of-" not in f]
shard1 = [f for f in cands if "00001-of-" in f]
pick = single or shard1 or cands
print(pick[0] if pick else "")
' "$JSON_TMP")"
rm -f "$JSON_TMP"

if [ -z "$FILE" ]; then
  warn "HF API lookup failed; falling back to a conventional filename guess."
  base="$(basename "$REPO" | sed 's/-GGUF$//')"
  FILE="${base}-${QUANT}.gguf"
fi
ok "File: $FILE"

URL="https://huggingface.co/$REPO/resolve/main/$FILE?download=true"
OUT="$MODELS_DIR/$FILE"
# Compare against the real remote size so a partially-downloaded file is
# resumed, not mistaken for complete.
REMOTE_SZ="$(curl -fsSLI "$URL" 2>/dev/null | awk 'BEGIN{IGNORECASE=1}/^content-length:/{v=$2} END{gsub(/[^0-9]/,"",v); print v+0}')"
LOCAL_SZ="$(stat -c%s "$OUT" 2>/dev/null || echo 0)"
if [ -f "$OUT" ] && [ "${REMOTE_SZ:-0}" -gt 0 ] && [ "$LOCAL_SZ" -eq "$REMOTE_SZ" ]; then
  ok "Already present and complete: $OUT ($(du -h "$OUT" | cut -f1))"
else
  hdr "Downloading -> $OUT"
  [ "$LOCAL_SZ" -gt 0 ] && [ "${REMOTE_SZ:-0}" -gt 0 ] && \
    log "Resuming from $((LOCAL_SZ/1048576)) / $((REMOTE_SZ/1048576)) MB"
  warn "This can be 1-5GB. Resumable (Ctrl-C and re-run to continue)."
  # -C - resumes; on an already-complete file the server 416s (curl rc 22/33).
  # Treat success as: curl rc 0 OR the local file now matches the remote size.
  curl -fL --retry 3 -C - -o "$OUT" "$URL"; rc=$?
  LOCAL_SZ="$(stat -c%s "$OUT" 2>/dev/null || echo 0)"
  if [ "${REMOTE_SZ:-0}" -gt 0 ] && [ "$LOCAL_SZ" -eq "$REMOTE_SZ" ]; then
    : # complete regardless of curl's range-related exit code
  elif [ "$rc" -ne 0 ]; then
    die "Download failed (curl rc=$rc). Check the repo/quant or your network."
  fi
fi

# Free-RAM sanity vs model size.
SZ_GB="$(awk -v b="$(stat -c%s "$OUT" 2>/dev/null || echo 0)" 'BEGIN{printf "%.1f", b/1073741824}')"
echo
ok "Model ready: $OUT (${SZ_GB} GB)"
log "Unified RAM available now: $(free -h | awk '/Mem:/{print $7}') (model + KV cache must fit)."
[ "$SEL" = "qwen2.5-7b" ] && warn "7B is the ceiling — stop Ollama & other models first; restart server between swaps."
echo "Next: ./05-benchmark.sh \"$OUT\""
