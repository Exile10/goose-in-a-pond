#!/usr/bin/env bash
# scripts/lib/install-systemd.sh — Linux production: systemd, mDNS, port binding
#
# Provides: setup_systemd(), setup_mdns(), setup_port_binding(), select_port()
# Sourced by install.sh — not meant to be run directly.
# Only called when MODE is "production" or "jetson".

SERVICE_NAME="goose-in-a-pond"
ACCESS_URL=""

# ── Port selection ───────────────────────────────────────────────────────────
select_port() {
  if [ -n "$PORT" ]; then
    log "Using specified port: ${PORT}"
    return
  fi

  local preferred_ports=(80 8080 4000 5000)
  local device_mode="${DEDICATED:-shared}"

  for p in "${preferred_ports[@]}"; do
    # Skip port 80 for shared devices (requires root and might conflict)
    if [ "$device_mode" = "shared" ] && [ "$p" = "80" ]; then
      continue
    fi
    # Check if port is free
    if ! ss -tlnp 2>/dev/null | grep -q ":${p} " && \
       ! (command -v nc &>/dev/null && nc -z 127.0.0.1 "$p" 2>/dev/null); then
      PORT="$p"
      log "Selected available port: ${PORT}"
      return
    fi
  done

  # All preferred ports busy
  PORT="5001"
  log "All preferred ports busy -- using ${PORT}"
}

# ── Device mode prompt (dedicated vs shared) ─────────────────────────────────
prompt_device_mode() {
  if [ -n "$DEDICATED" ]; then return; fi

  echo ""
  log "Dedicated device  ->  hostname becomes 'pond', accessible at http://pond.local/"
  log "Shared device     ->  keeps current hostname, accessible at http://$(hostname).local:<port>/"
  echo ""
  echo -e "${BOLD}  ?  Is this device dedicated to Goose In A Pond? [y/N]${NC}"
  read -r -p "     " REPLY < /dev/tty
  if [ "${REPLY:-N}" != "${REPLY#[Yy]}" ]; then
    DEDICATED="dedicated"
  else
    DEDICATED="shared"
  fi

  success "Mode: ${DEDICATED}"
}

# ── mDNS / Avahi setup ──────────────────────────────────────────────────────
setup_mdns() {
  step "8" "Hostname and mDNS"

  local avahi_ok=true

  # Install avahi if missing
  if ! command -v avahi-daemon &>/dev/null; then
    warn "avahi-daemon not installed -- .local hostnames will not be broadcast"
    echo -e "${BOLD}  ?  Install avahi-daemon now? [Y/n]${NC}"
    read -r -p "     " REPLY < /dev/tty
    if [ "${REPLY:-Y}" != "${REPLY#[Yy]}" ]; then
      sudo apt-get install -y avahi-daemon 2>/dev/null || {
        warn "Could not install avahi-daemon -- continuing without mDNS"
        avahi_ok=false
      }
    else
      avahi_ok=false
    fi
  fi

  # Set hostname for dedicated devices
  local current_host
  current_host="$(hostname)"

  if [ "$DEDICATED" = "dedicated" ] && [ "$current_host" != "pond" ]; then
    echo -e "${BOLD}  ?  Change hostname from '${current_host}' to 'pond'? [Y/n]${NC}"
    read -r -p "     " REPLY < /dev/tty
    if [ "${REPLY:-Y}" != "${REPLY#[Yy]}" ]; then
      sudo hostnamectl set-hostname pond
      success "Hostname set to 'pond'"
      current_host="pond"
    else
      log "Keeping hostname '${current_host}'"
    fi
  fi

  # Start avahi
  if [ "$avahi_ok" = true ]; then
    sudo systemctl enable --now avahi-daemon 2>/dev/null || true
    success "avahi-daemon running -- broadcasting ${current_host}.local"
    S_MDNS="ok"
  else
    S_MDNS="skipped"
  fi

  # Derive access URL
  if [ "$PORT" = "80" ]; then
    ACCESS_URL="http://${current_host}.local/"
  else
    ACCESS_URL="http://${current_host}.local:${PORT}/"
  fi
  success "Access URL: ${ACCESS_URL}"
}

# ── Port binding capability ──────────────────────────────────────────────────
setup_port_binding() {
  local binary="${REPO_DIR}/target/release/pond-server"

  if [ "$PORT" = "80" ] && [ "$EUID" != "0" ]; then
    log "Port 80 requires CAP_NET_BIND_SERVICE -- applying to binary..."
    if sudo setcap 'cap_net_bind_service=+ep' "$binary" 2>/dev/null; then
      success "Port 80 capability set"
    else
      warn "Could not set capability -- falling back to port 8080"
      PORT="8080"
    fi
  fi
}

# ── Systemd service ─────────────────────────────────────────────────────────
setup_systemd() {
  step "7" "Systemd service"

  if [ "${NO_SERVICE:-false}" = true ]; then
    log "Skipping service creation (--no-service)"
    S_SYSTEMD="skipped"
    return
  fi

  if ! command -v systemctl &>/dev/null; then
    warn "systemctl not found -- skipping service creation"
    S_SYSTEMD="skipped"
    return
  fi

  # Prompt for device mode if not set
  prompt_device_mode

  # Select port
  select_port

  # Apply port binding if needed
  setup_port_binding

  local binary="${REPO_DIR}/target/release/pond-server"
  local service_file="/etc/systemd/system/${SERVICE_NAME}.service"

  sudo tee "$service_file" > /dev/null <<EOF
[Unit]
Description=Goose In A Pond -- Local AI Home Assistant
Documentation=https://github.com/Jarida/goose-in-a-pond
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${USER}
WorkingDirectory=${REPO_DIR}
ExecStart=${binary} serve --port ${PORT}
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=goose-in-a-pond
# Give the LLM time to load on startup
TimeoutStartSec=120

[Install]
WantedBy=multi-user.target
EOF

  sudo systemctl daemon-reload
  sudo systemctl enable "$SERVICE_NAME"
  sudo systemctl restart "$SERVICE_NAME"

  # Brief wait for startup
  sleep 3
  if systemctl is-active --quiet "$SERVICE_NAME"; then
    success "Service '${SERVICE_NAME}' is running"
    S_SYSTEMD="ok"
  else
    warn "Service did not start cleanly -- check: journalctl -u ${SERVICE_NAME} -n 50"
    S_SYSTEMD="failed"
  fi
}
