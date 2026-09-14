#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# kiosk.sh — put the pond on the Jetson's attached panel, without a desktop.
#
# WHAT "NO BLANKET DISPLAY SERVER" MEANS HERE, PRECISELY
#
# This installs no display manager, no desktop environment and no X server. The
# default systemd target stays `multi-user.target` and gdm3 stays disabled —
# that posture exists because GNOME was holding 2,594 MiB of nvmap, and nvmap
# is exactly the contiguous pool that decides whether the model can fully
# offload (docs/developer/jetson-device-tuning.md).
#
# What it runs instead is ONE compositor process, bound to ONE DRM device,
# showing ONE fullscreen surface, started by a user systemd unit. Weston with
# `drm-backend` talks to KMS directly; there is no X, no session manager and no
# shell furniture. NVIDIA ships its own weston build for Tegra and that is the
# one used.
#
# Being honest about the wording: a compositor IS a display server in the
# strict sense. What it is not is a desktop. The cost difference between the
# two is the entire point — GNOME was gigabytes, this is a process.
#
# THE THING THAT HAS TO BE TRUE FIRST
#
# The board currently exposes NO KMS connectors: `nvidia-drm` is not loaded, so
# /dev/dri/card0 is the host1x render node and has no connector children. Until
# that changes there is nothing for any compositor to bind to, whatever it is.
# `probe` reports this; `enable-drm` is what fixes it, and needs root.
#
# AND THE GATE
#
# Anything drawn here competes with the model for one unified memory pool. The
# board had 561 MB available with the pond loaded when this was written.
# `probe` prints the headroom and `measure` captures a before/after so the
# decision is made on a number rather than a hope.
#
# Usage (run ON the Jetson):
#   bash scripts/jetson/kiosk.sh probe        # read-only readiness report
#   bash scripts/jetson/kiosk.sh enable-drm   # load nvidia-drm (needs sudo)
#   bash scripts/jetson/kiosk.sh start        # run the kiosk in the foreground
#   bash scripts/jetson/kiosk.sh measure      # memory cost, before vs after
#   bash scripts/jetson/kiosk.sh install      # user systemd unit, starts at login
#   bash scripts/jetson/kiosk.sh stop
# -----------------------------------------------------------------------------
set -uo pipefail

PORT="${GIAP_KIOSK_PORT:-8080}"
URL="${GIAP_KIOSK_URL:-http://127.0.0.1:${PORT}}"
WESTON_NV="/usr/lib/aarch64-linux-gnu/nvidia/weston-13.0"
WESTON_STD="/usr/lib/aarch64-linux-gnu/weston"
RUNTIME="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

ok()   { printf "  \033[32mok\033[0m    %s\n" "$*"; }
bad()  { printf "  \033[31mFAIL\033[0m  %s\n" "$*"; }
warn() { printf "  \033[33mwarn\033[0m  %s\n" "$*"; }
info() { printf "        %s\n" "$*"; }
head1(){ printf "\n\033[1m%s\033[0m\n" "$*"; }

# ── Facts, gathered once ─────────────────────────────────────────────────────

connectors() { find /sys/class/drm -maxdepth 1 -name 'card*-*' 2>/dev/null; }
connected()  { for c in $(connectors); do [ "$(cat "$c/status" 2>/dev/null)" = connected ] && echo "$c"; done; }
avail_mb()   { free -m | awk '/^Mem:/ {print $7}'; }
nvmap_mb()   { awk '/NvMapMemUsed/ {print int($2/1024)}' /proc/meminfo 2>/dev/null; }

weston_dir() { [ -d "$WESTON_NV" ] && echo "$WESTON_NV" || echo "$WESTON_STD"; }

renderer() {
  # What can actually draw the dashboard. Electron is preferred: it is the same
  # app the Mac runs and ships prebuilt linux-arm64 binaries. Firefox is the
  # fallback already on the box.
  if [ -x "${REPO_ROOT:-$HOME/goose-in-a-pond}/pond-desktop/node_modules/.bin/electron" ]; then
    echo electron
  elif command -v firefox >/dev/null 2>&1; then
    echo firefox
  else
    echo none
  fi
}

# ── probe ────────────────────────────────────────────────────────────────────

cmd_probe() {
  head1 "Display path"
  if [ -z "$(connectors)" ]; then
    bad "no KMS connectors — nothing can draw to a panel yet"
    if [ -f "/lib/modules/$(uname -r)/updates/opensrc-disp/nvidia-drm.ko" ]; then
      info "nvidia-drm.ko is present but not loaded. Fix: sudo bash $0 enable-drm"
    else
      info "nvidia-drm.ko not found for kernel $(uname -r)"
    fi
  else
    ok "$(connectors | wc -l | tr -d ' ') connector(s) exposed"
    for c in $(connectors); do
      info "$(basename "$c"): $(cat "$c/status" 2>/dev/null)"
    done
    if [ -z "$(connected)" ]; then
      warn "none report 'connected' — is a panel actually plugged in?"
    else
      ok "panel attached: $(connected | xargs -n1 basename | tr '\n' ' ')"
    fi
  fi

  head1 "Desktop posture (must stay headless)"
  local tgt; tgt="$(systemctl get-default 2>/dev/null)"
  [ "$tgt" = "multi-user.target" ] && ok "default target is $tgt" || bad "default target is $tgt — should be multi-user.target"
  [ "$(systemctl is-active display-manager 2>/dev/null)" = active ] \
    && bad "a display manager is running — that is the 2.6 GB of nvmap this avoids" \
    || ok "no display manager running"

  head1 "Compositor"
  if command -v weston >/dev/null 2>&1; then
    ok "weston $(weston --version 2>/dev/null | awk '{print $2}') ($(weston_dir))"
    [ -f "$(weston_dir)/drm-backend.so" ] && ok "drm-backend present" || bad "no drm-backend.so — cannot drive KMS directly"
    if [ -f "$(weston_dir)/kiosk-shell.so" ]; then
      ok "kiosk-shell present"
    elif [ -f "$(weston_dir)/fullscreen-shell.so" ]; then
      ok "fullscreen-shell present (kiosk-shell absent; fullscreen-shell is the same idea)"
    else
      bad "no kiosk or fullscreen shell"
    fi
  else
    bad "weston not installed"
  fi

  head1 "Renderer"
  case "$(renderer)" in
    electron) ok "electron (the same app the Mac runs)" ;;
    firefox)  warn "firefox only — heavier than Electron and snap-confined; usable via --kiosk" ;;
    none)     bad "nothing installed that can render the dashboard" ;;
  esac

  head1 "Seat access"
  local seats; seats="$(loginctl list-seats --no-legend 2>/dev/null | wc -l | tr -d ' ')"
  [ "$seats" -gt 0 ] && ok "seat0 present, session $(loginctl list-sessions --no-legend 2>/dev/null | awk 'NR==1{print $1}')" \
                     || bad "no seat — weston's drm backend needs one"
  for g in video render input; do
    id -nG 2>/dev/null | tr ' ' '\n' | grep -qx "$g" && ok "user is in group '$g'" || warn "user is NOT in group '$g'"
  done

  head1 "Memory headroom — the gate"
  local a n; a="$(avail_mb)"; n="$(nvmap_mb)"
  info "available: ${a} MB   nvmap in use: ${n:-unknown} MB"
  if   [ "$a" -lt 400 ];  then bad  "${a} MB available — a compositor plus a browser will not fit without pushing the model into swap"
  elif [ "$a" -lt 900 ];  then warn "${a} MB available — tight. Measure before committing: bash $0 measure"
  else                         ok   "${a} MB available"
  fi
  info "the model's weights and KV live in this same pool and do not show as process RSS"

  head1 "Verdict"
  if [ -z "$(connectors)" ]; then
    echo "  Blocked on the display path. Nothing else matters until connectors exist."
  elif [ -z "$(connected)" ]; then
    echo "  Driver is fine; no panel detected. Plug one in and re-probe."
  else
    echo "  Ready to try:  bash $0 measure"
  fi
  echo
}

# ── enable-drm ───────────────────────────────────────────────────────────────

cmd_enable_drm() {
  head1 "Loading nvidia-drm with modesetting"
  info "This is what exposes KMS connectors. It adds no desktop and no X."
  info "Reversible: sudo modprobe -r nvidia-drm"
  if [ "$(id -u)" -ne 0 ]; then
    bad "needs root — re-run as: sudo bash $0 enable-drm"
    return 1
  fi
  modprobe nvidia-drm modeset=1 || { bad "modprobe failed"; return 1; }
  sleep 2
  if [ -n "$(connectors)" ]; then
    ok "connectors now exposed:"
    for c in $(connectors); do info "$(basename "$c"): $(cat "$c/status" 2>/dev/null)"; done
  else
    bad "still no connectors — the panel may be on a path this driver does not expose"
  fi
  info ""
  info "To make it survive a reboot (only once you have decided to keep it):"
  info "  echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf"
}

# ── measure ──────────────────────────────────────────────────────────────────

cmd_measure() {
  head1 "Measuring what the kiosk costs"
  info "Everything drawn here comes out of the pool the model offloads into."
  local a0 n0 a1 n1
  a0="$(avail_mb)"; n0="$(nvmap_mb)"
  info "before:  available ${a0} MB   nvmap ${n0:-?} MB"
  info "starting the kiosk for 45s ..."
  cmd_start >/dev/null 2>&1 &
  local pid=$!
  sleep 45
  a1="$(avail_mb)"; n1="$(nvmap_mb)"
  info "during:  available ${a1} MB   nvmap ${n1:-?} MB"
  kill "$pid" 2>/dev/null; cmd_stop >/dev/null 2>&1
  sleep 3
  head1 "Cost"
  info "available: ${a0} -> ${a1} MB   (delta $((a0 - a1)) MB)"
  [ -n "$n0" ] && [ -n "$n1" ] && info "nvmap:     ${n0} -> ${n1} MB   (delta $((n1 - n0)) MB)"
  echo
  info "Decide against docs/developer/jetson-device-tuning.md: if this pushes"
  info "the model out of full offload, the panel is not worth it on this board."
  info "Check with: dmesg | grep -i 'nvmap.*error'  and  swapon --show"
}

# ── start / stop ─────────────────────────────────────────────────────────────

cmd_start() {
  if [ -z "$(connectors)" ]; then
    bad "no KMS connectors; run 'sudo bash $0 enable-drm' first"
    return 1
  fi
  local shell_mod="fullscreen-shell.so"
  [ -f "$(weston_dir)/kiosk-shell.so" ] && shell_mod="kiosk-shell.so"

  head1 "Starting the kiosk"
  info "compositor: weston ($(weston_dir)) drm-backend + ${shell_mod}"
  info "target:     ${URL}"
  export XDG_RUNTIME_DIR="$RUNTIME"
  mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null
  chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null

  weston \
    --backend=drm-backend.so \
    --shell="${shell_mod}" \
    --modules="" \
    --idle-time=0 \
    -- "$(kiosk_client_cmd)"
}

kiosk_client_cmd() {
  case "$(renderer)" in
    electron)
      # The same app, told to attach to the running server rather than spawn one.
      echo "env GIAP_SERVER_PORT=${PORT} ${REPO_ROOT:-$HOME/goose-in-a-pond}/pond-desktop/node_modules/.bin/electron ${REPO_ROOT:-$HOME/goose-in-a-pond}/pond-desktop --ozone-platform=wayland --enable-features=UseOzonePlatform --touch-events=enabled"
      ;;
    firefox)
      echo "firefox --kiosk ${URL}"
      ;;
    *)
      echo "weston-terminal"
      ;;
  esac
}

cmd_stop() {
  pkill -u "$(id -u)" -f "weston .*drm-backend" 2>/dev/null
  pkill -u "$(id -u)" -f "electron .*pond-desktop" 2>/dev/null
  pkill -u "$(id -u)" -f "firefox --kiosk" 2>/dev/null
  ok "kiosk stopped"
}

cmd_status() {
  pgrep -u "$(id -u)" -f "weston .*drm-backend" >/dev/null && ok "compositor running" || info "compositor not running"
  pgrep -u "$(id -u)" -f "electron .*pond-desktop|firefox --kiosk" >/dev/null && ok "renderer running" || info "renderer not running"
}

# ── install (user unit, not system) ──────────────────────────────────────────

cmd_install() {
  head1 "Installing a USER systemd unit"
  info "User, not system: it starts with this user's session and needs no root,"
  info "and it cannot pull in graphical.target or a display manager."
  local unit="$HOME/.config/systemd/user/giap-kiosk.service"
  mkdir -p "$(dirname "$unit")"
  cat > "$unit" <<UNIT
[Unit]
Description=GIAP kiosk panel (weston fullscreen, no desktop)
After=goose-in-a-pond.service
Wants=goose-in-a-pond.service

[Service]
Type=simple
ExecStart=/bin/bash %h/goose-in-a-pond/scripts/jetson/kiosk.sh start
ExecStop=/bin/bash %h/goose-in-a-pond/scripts/jetson/kiosk.sh stop
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
UNIT
  systemctl --user daemon-reload
  ok "wrote $unit"
  info "enable with: systemctl --user enable --now giap-kiosk"
  info "and:         loginctl enable-linger $(id -un)   # so it survives logout"
}

case "${1:-probe}" in
  probe)      cmd_probe ;;
  enable-drm) cmd_enable_drm ;;
  measure)    cmd_measure ;;
  start)      cmd_start ;;
  stop)       cmd_stop ;;
  status)     cmd_status ;;
  install)    cmd_install ;;
  *) echo "usage: $0 {probe|enable-drm|measure|start|stop|status|install}" >&2; exit 2 ;;
esac
