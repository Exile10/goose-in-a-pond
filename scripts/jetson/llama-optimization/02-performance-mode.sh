#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 02-performance-mode.sh — put the Jetson into maximum sustained-performance
# state for LLM inference / benchmarking.
#
#   * nvpmodel -> MAXN_SUPER  (highest power budget; must be set BEFORE clocks)
#   * jetson_clocks           (lock CPU/GPU/EMC to their max, disable DVFS)
#   * CPU scaling governor -> performance
#
# Usage:
#   ./02-performance-mode.sh            apply max-performance mode
#   ./02-performance-mode.sh --status   show current power/clock state
#   ./02-performance-mode.sh --reset    restore stored clocks + default power
#
# Trade-off: MAXN_SUPER + locked clocks raise idle power/heat and keep the fan
# busier. Great for benchmarking and serving; for battery/quiet idle use --reset
# (a reboot also fully restores defaults).
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson

CLOCK_STORE="/var/tmp/jetson_clocks.before_giap"

# Resolve the MAXN/highest-performance nvpmodel ID from the conf (fallback 2).
maxn_mode_id() {
  local id
  id="$(awk -F'[ =]' '/POWER_MODEL/ && /MAXN/ {for(i=1;i<=NF;i++) if($i=="ID"){print $(i+1); exit}}' \
        /etc/nvpmodel.conf 2>/dev/null)"
  [ -n "$id" ] && echo "$id" || echo 2
}

show_status() {
  hdr "Power mode"
  nvpmodel -q 2>/dev/null
  hdr "Clocks"
  local gpu_df cur max gov; gpu_df="$(ls -d /sys/class/devfreq/*.gpu 2>/dev/null | head -1)"
  if [ -n "$gpu_df" ]; then
    cur="$(cat "$gpu_df/cur_freq" 2>/dev/null || echo 0)"
    max="$(cat "$gpu_df/max_freq" 2>/dev/null || echo 0)"
    gov="$(cat "$gpu_df/governor" 2>/dev/null || echo '?')"
    printf 'GPU: cur=%s MHz / max=%s MHz (governor %s)\n' "$(( cur / 1000000 ))" "$(( max / 1000000 ))" "$gov"
  fi
  printf 'CPU governor: %s   |  CPU freqs:' "$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)"
  for c in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq; do
    printf ' %s' "$(( $(cat "$c") / 1000 ))"; done; echo " MHz"
  hdr "Thermals / power (1 sample)"
  timeout 2 tegrastats --interval 1000 2>/dev/null | head -1 || true
}

case "${1:-apply}" in
  --status|-s|status) show_status; exit 0 ;;
  --reset|reset)
    sudo_prime
    log "Restoring default power mode + dynamic clocks..."
    if [ -f "$CLOCK_STORE" ]; then
      as_root jetson_clocks --restore "$CLOCK_STORE" 2>/dev/null && ok "Clocks restored from $CLOCK_STORE" \
        || warn "jetson_clocks --restore failed (a reboot will fully reset clocks)"
    else
      warn "No stored clock state; a reboot will fully restore dynamic clocks."
    fi
    # Reset to the board's real factory-default mode (PM_CONFIG DEFAULT), not a
    # hardcoded guess. Overridable with RESET_MODE=. NB: after jetson_clocks has
    # run, nvpmodel may refuse to switch without a reboot.
    DEF="${RESET_MODE:-$(awk -F'[ =]' '/PM_CONFIG/{for(i=1;i<=NF;i++) if($i=="DEFAULT"){print $(i+1); exit}}' /etc/nvpmodel.conf 2>/dev/null)}"
    DEF="${DEF:-1}"
    if as_root nvpmodel -m "$DEF" >/dev/null 2>&1; then ok "nvpmodel -> mode $DEF (factory default)"
    else warn "nvpmodel reset to mode $DEF failed — a reboot will restore defaults."; fi
    for g in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
      echo schedutil | as_root tee "$g" >/dev/null 2>&1 || true; done
    ok "CPU governor -> schedutil"; exit 0 ;;
  apply|--apply|"") : ;;
  *) die "Unknown option: $1 (use --status | --reset | --apply)" ;;
esac

sudo_prime
MODE="$(maxn_mode_id)"

hdr "Applying maximum-performance mode (nvpmodel ID=$MODE = MAXN_SUPER)"
# Snapshot current clocks once so --reset can restore them without a reboot.
[ -f "$CLOCK_STORE" ] || as_root jetson_clocks --store "$CLOCK_STORE" 2>/dev/null || true

# Parse the numeric mode id by content (not a fixed line), as root for reliability.
CUR="$(as_root nvpmodel -q 2>/dev/null | grep -Eo '^[0-9]+$' | head -1)"
if [ -n "$CUR" ] && [ "$CUR" = "$MODE" ]; then
  ok "Already in power mode $MODE (MAXN_SUPER)"
else
  as_root nvpmodel -m "$MODE" && ok "nvpmodel -> mode $MODE" || die "nvpmodel failed"
fi

# jetson_clocks MUST run after nvpmodel so it locks to the new mode's ceilings.
log "Locking CPU/GPU/EMC clocks to max (jetson_clocks)..."
as_root jetson_clocks && ok "Clocks locked to maximum" || warn "jetson_clocks reported an issue"

log "Setting CPU governor to 'performance' on all cores..."
for g in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
  echo performance | as_root tee "$g" >/dev/null 2>&1 || true
done
ok "CPU governor -> performance"

echo
show_status
echo
ok "Max-performance mode active."
warn "Active cooling required: under sustained LLM load MAXN_SUPER can reach"
warn "~80-85C and SILENTLY throttle. Make sure the devkit fan is running."
warn "To change power mode again you must reboot (jetson_clocks locked it). Use --reset or reboot to revert."
