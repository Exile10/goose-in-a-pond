#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-jetson-native.sh — Build GIAP natively on a Jetson Orin Nano
#
# Run this script ON the Jetson after cloning the repo.
#
# Usage:
#   bash scripts/build-jetson-native.sh           # CPU-only server
#   bash scripts/build-jetson-native.sh --cuda    # server with CUDA GPU accel
#   bash scripts/build-jetson-native.sh --desktop # server + Tauri desktop app
#   bash scripts/build-jetson-native.sh --cuda --desktop
#
# Requirements:
#   - JetPack 5.x (CUDA 11.4) or JetPack 6.x (CUDA 12.2) — pre-installed
#   - Rust toolchain (installed by this script if missing)
#   - Internet access for apt / cargo downloads
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

CUDA=false
DESKTOP=false

for arg in "$@"; do
  case "$arg" in
    --cuda)    CUDA=true ;;
    --desktop) DESKTOP=true ;;
    *) echo "Unknown argument: $arg"; exit 1 ;;
  esac
done

echo "═══════════════════════════════════════════════════"
echo "  GIAP — Jetson Orin Nano native build"
echo "  CUDA: $CUDA  |  Desktop: $DESKTOP"
echo "═══════════════════════════════════════════════════"

# ── 1. Rust toolchain ────────────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
  echo "Installing Rust..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi
echo "Rust: $(rustc --version)"

# ── 2. System dependencies ───────────────────────────────────────────────────
echo "Installing system packages..."
sudo apt-get update -qq
sudo apt-get install -y \
  build-essential \
  pkg-config \
  cmake \
  libssl-dev \
  libasound2-dev \
  libdbus-1-dev \
  libsqlite3-dev

if [ "$DESKTOP" = true ]; then
  echo "Installing Tauri/GTK dependencies..."
  sudo apt-get install -y \
    libwebkit2gtk-4.1-dev \
    libgtk-3-dev \
    librsvg2-dev \
    patchelf \
    libayatana-appindicator3-dev
fi

# ── 3. Build pond-server ─────────────────────────────────────────────────────
echo ""
echo "Building pond-server..."

SERVER_FEATURES=""
if [ "$CUDA" = true ]; then
  echo "  CUDA enabled — using GPU acceleration (sm_87 / Ampere)"
  SERVER_FEATURES="--features pond-adapters-local-inference/cuda"
fi

SQLX_OFFLINE=true cargo build -p pond-server $SERVER_FEATURES --release

echo ""
echo "Server binary: $(pwd)/target/release/pond-server"

# ── 4. Build Tauri desktop (optional) ────────────────────────────────────────
if [ "$DESKTOP" = true ]; then
  echo ""
  echo "Building Tauri desktop app..."

  if ! command -v node &>/dev/null; then
    echo "Node.js not found. Install via nvm or apt:"
    echo "  curl -fsSL https://deb.nodesource.com/setup_20.x | sudo bash -"
    echo "  sudo apt-get install -y nodejs"
    exit 1
  fi

  if ! command -v cargo-tauri &>/dev/null; then
    echo "Installing tauri-cli..."
    cargo install tauri-cli --locked
  fi

  cd pond-desktop
  npm install
  cargo tauri build
  cd ..

  echo ""
  echo "Desktop bundle: $(pwd)/pond-desktop/src-tauri/target/release/bundle/"
fi

# ── Done ─────────────────────────────────────────────────────────────────────
echo ""
echo "═══════════════════════════════════════════════════"
echo "  Build complete!"
echo ""
echo "  Start the server:"
echo "    ./target/release/pond-server"
echo ""
if [ "$CUDA" = true ]; then
  echo "  Verify CUDA is active:"
  echo "    RUST_LOG=debug ./target/release/pond-server 2>&1 | grep -i cuda"
fi
echo "═══════════════════════════════════════════════════"
