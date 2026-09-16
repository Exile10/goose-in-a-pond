#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# memwatch.sh — sample memory while a candidate runs.
#
#   bash memwatch.sh --out mem.csv [--pid N] [--interval 0.5] &
#   MEMWATCH=$!;  ... run the workload ...;  kill $MEMWATCH
#   bash memwatch.sh --summary mem.csv        # -> JSON for the envelope
#
# The columns that decide a gate:
#   mem_available_mb  the minimum over the run is what G2 tests.
#   swap_free_mb      any DROP means the model was partly served from swap, and
#                     a swap-served KV cache reads as a slow model, never as an
#                     error. A run with a swap delta is not a valid measurement.
#   lfb_mb            largest free block. A long-uptime board refuses a big
#                     contiguous CUDA allocation with GBs still "free"; this is
#                     the only advance warning.
#   nvmap_pid_mb      per-process GPU memory. Needs root, and is ABSENT rather
#                     than zero without it.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$HERE/lib.sh"

OUT=""; PID=""; INTERVAL=0.5; SUMMARY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out)      OUT="$2"; shift 2 ;;
    --pid)      PID="$2"; shift 2 ;;
    --interval) INTERVAL="$2"; shift 2 ;;
    --summary)  SUMMARY="$2"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done

if [ -n "$SUMMARY" ]; then
  python3 - "$SUMMARY" <<'PY'
import csv, json, sys
rows = list(csv.DictReader(open(sys.argv[1])))
if not rows:
    print(json.dumps({"samples": 0}));  raise SystemExit
def col(name):
    return [int(r[name]) for r in rows if r.get(name) not in (None, "", "null")]
avail, swap, lfb = col("mem_available_mb"), col("swap_free_mb"), col("lfb_mb")
pidmb = col("nvmap_pid_mb")
out = {
    "samples": len(rows),
    "mem_available_start_mb": avail[0] if avail else None,
    "mem_available_min_mb": min(avail) if avail else None,
    "mem_available_end_mb": avail[-1] if avail else None,
    # Peak footprint of whatever ran, as the dip in what the OS could still hand out.
    "peak_footprint_mb": (avail[0] - min(avail)) if avail else None,
    # Must be 0. Anything else means part of the run lived in swap.
    "swap_delta_mb": (swap[0] - min(swap)) if swap else None,
    "lfb_min_mb": min(lfb) if lfb else None,
    "nvmap_pid_peak_mb": max(pidmb) if pidmb else None,
}
print(json.dumps(out))
PY
  exit 0
fi

[ -n "$OUT" ] || die "--out CSV is required (or --summary CSV)"
echo "ts,mem_available_mb,mem_free_mb,swap_free_mb,cached_mb,nvmap_total_mb,nvmap_pid_mb,lfb_mb,gpu_mhz" > "$OUT"
trap 'exit 0' TERM INT
while :; do
  nvt="$(nvmap_total_mb || echo '')"
  nvp=""; [ -n "$PID" ] && nvp="$(nvmap_pid_mb "$PID")"
  printf '%s,%d,%d,%d,%d,%s,%s,%s,%s\n' \
    "$(date +%s)" \
    "$(( $(meminfo_kb MemAvailable) / 1024 ))" \
    "$(( $(meminfo_kb MemFree) / 1024 ))" \
    "$(( $(meminfo_kb SwapFree) / 1024 ))" \
    "$(( $(meminfo_kb Cached) / 1024 ))" \
    "${nvt:-}" "${nvp:-}" "$(tegra_lfb_mb)" "$(gpu_cur_mhz)" >> "$OUT"
  sleep "$INTERVAL"
done
