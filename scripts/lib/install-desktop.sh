#!/usr/bin/env bash
# scripts/lib/install-desktop.sh — Optional desktop app build
#
# Provides: build_desktop()
# Sourced by install.sh — not meant to be run directly.

build_desktop() {
  step "9" "Desktop app (pond-desktop)"

  # Check Node.js
  if ! command -v npm &>/dev/null; then
    warn "npm not found -- install Node.js to use the desktop app"
    S_DESKTOP="npm missing"
    return
  fi

  cd "${REPO_DIR}/pond-desktop"

  log "Running npm install..."
  if npm install 2>&1 | tail -5; then
    success "Desktop app dependencies installed"
  else
    warn "npm install had issues"
    S_DESKTOP="failed"
    cd "$REPO_DIR"
    return
  fi

  # Install Tauri CLI if not present and user wants to build
  if ! command -v cargo-tauri &>/dev/null 2>&1; then
    log "Tauri CLI not found -- it will be installed on first 'npm run tauri dev'"
  fi

  S_DESKTOP="ok"
  cd "$REPO_DIR"
}
