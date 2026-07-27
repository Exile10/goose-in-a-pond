#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# install-desktop-deps.sh — check for (and install) the system libraries the
# GIAP Tauri desktop app needs to compile.
#
# Tauri on Linux compiles the app against the *system* WebKitGTK stack (unlike
# macOS/Windows, which use a built-in WebView), so these must be present BEFORE
# `cargo build`/`cargo tauri build` in pond-desktop/src-tauri — otherwise the
# build dies deep inside `webkit2gtk-sys` with a cryptic pkg-config error.
#
# This is the single source of truth for that dependency set. It is called by
# `scripts/jetson.sh build --desktop` and `scripts/jetson.sh deploy --desktop`
# (scripts/jetson/), and pond-desktop/src-tauri/build.rs points users here on a
# missing lib. Safe to run standalone and idempotent.
#
# Usage:
#   bash scripts/install-desktop-deps.sh          # check + install missing (apt, uses sudo)
#   bash scripts/install-desktop-deps.sh --check  # check only, non-zero exit if any missing
# -----------------------------------------------------------------------------
set -euo pipefail

CHECK_ONLY=false
[ "${1:-}" = "--check" ] && CHECK_ONLY=true

# pkg-config module -> apt package. These are the load-bearing WebKitGTK libs
# whose absence breaks the build; the apt list below adds the non-pkg-config
# extras Tauri needs at link/bundle time.
PKGCONFIG_TO_APT=(
  "webkit2gtk-4.1:libwebkit2gtk-4.1-dev"
  "gtk+-3.0:libgtk-3-dev"
  "libsoup-3.0:libsoup-3.0-dev"
  "javascriptcoregtk-4.1:libjavascriptcoregtk-4.1-dev"
)
# Extra apt packages Tauri needs that are not probed via pkg-config here.
EXTRA_APT=(librsvg2-dev patchelf libayatana-appindicator3-dev libxdo-dev build-essential)

OS="$(uname -s)"
if [ "$OS" = "Darwin" ]; then
  echo "==> macOS: Tauri uses the built-in WKWebView; no WebKitGTK deps needed. OK."
  exit 0
fi

# Which pkg-config modules are missing?
missing_pc=()
missing_apt=()
if command -v pkg-config >/dev/null 2>&1; then
  for pair in "${PKGCONFIG_TO_APT[@]}"; do
    pc="${pair%%:*}"; apt="${pair##*:}"
    if pkg-config --exists "$pc" 2>/dev/null; then
      echo "  ok      $pc ($(pkg-config --modversion "$pc" 2>/dev/null))"
    else
      echo "  MISSING $pc  -> $apt"
      missing_pc+=("$pc"); missing_apt+=("$apt")
    fi
  done
else
  echo "  pkg-config not found — treating all WebKitGTK libs as missing"
  for pair in "${PKGCONFIG_TO_APT[@]}"; do missing_apt+=("${pair##*:}"); done
  EXTRA_APT+=(pkg-config)
fi

if [ ${#missing_apt[@]} -eq 0 ]; then
  echo "==> All GIAP desktop WebKitGTK dependencies present."
  exit 0
fi

if [ "$CHECK_ONLY" = true ]; then
  echo "==> Missing: ${missing_apt[*]}"
  echo "    Run: bash scripts/install-desktop-deps.sh   (or the sudo apt-get command above)"
  exit 1
fi

if ! command -v apt-get >/dev/null 2>&1; then
  echo "ERROR: dependencies missing and this is not an apt-based system." >&2
  echo "Install the equivalents of: ${missing_apt[*]} ${EXTRA_APT[*]}" >&2
  echo "  Fedora:  sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel" >&2
  echo "  Arch:    sudo pacman -S webkit2gtk-4.1 gtk3 libsoup3 librsvg" >&2
  exit 1
fi

echo "==> Installing WebKitGTK build dependencies via apt (sudo)…"
sudo apt-get update -qq
sudo apt-get install -y "${missing_apt[@]}" "${EXTRA_APT[@]}"

# Verify the install actually satisfied pkg-config.
if command -v pkg-config >/dev/null 2>&1; then
  for pc in "${missing_pc[@]}"; do
    pkg-config --exists "$pc" 2>/dev/null \
      && echo "  installed $pc ($(pkg-config --modversion "$pc"))" \
      || { echo "ERROR: $pc still not found after install" >&2; exit 1; }
  done
fi
echo "==> GIAP desktop dependencies satisfied."
