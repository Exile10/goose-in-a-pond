#!/usr/bin/env bash
# scripts/deploy-jetson.sh — Cross-compile pond-server for Jetson Orin Nano and deploy.
#
# Prerequisites:
#   cargo install cross --locked
#   SSH access to the Jetson at $JETSON_HOST (default: jetson.local)
#
# Usage:
#   ./scripts/deploy-jetson.sh
#   JETSON_HOST=192.168.1.50 ./scripts/deploy-jetson.sh
#   ./scripts/deploy-jetson.sh --features goose-agent   # include Goose agent backend
#
# The script does NOT include --features local-inference because llama-cpp-2 requires
# native compilation with CUDA headers. Compile that directly on the Jetson:
#   cargo build -p pond-server --features local-inference --release
#   # or with CUDA: cargo build -p pond-server --features "local-inference,pond-adapters-local-inference/cuda" --release

set -euo pipefail

JETSON_HOST="${JETSON_HOST:-jetson.local}"
JETSON_USER="${JETSON_USER:-jetson}"
JETSON_DEPLOY_DIR="${JETSON_DEPLOY_DIR:-/opt/giap}"
TARGET="aarch64-unknown-linux-gnu"
FEATURES="${@}"

if ! command -v clang &>/dev/null; then
  echo "==> clang not found — installing..."
  if command -v apt-get &>/dev/null; then
    LLVM_VER=$(apt-cache search '^clang-[0-9]+$' 2>/dev/null \
      | awk '{print $1}' | grep -oP '\d+' | sort -rn | head -1)
    LLVM_VER="${LLVM_VER:-17}"
    sudo apt-get install -y "clang-${LLVM_VER}"
    sudo ln -sf "/usr/bin/clang-${LLVM_VER}" /usr/local/bin/clang
  elif command -v dnf &>/dev/null; then
    sudo dnf install -y clang
  elif command -v pacman &>/dev/null; then
    sudo pacman -S --noconfirm clang
  else
    echo "ERROR: Cannot auto-install clang — no supported package manager (apt/dnf/pacman). Install it manually."
    exit 1
  fi
  echo "==> clang installed: $(clang --version | head -1)"
fi

if ! command -v cross &>/dev/null; then
  echo "ERROR: 'cross' not found. Install it with:"
  echo " cargo install cross "
  exit 1
fi

echo "==> Building pond-server for ${TARGET}..."
cross build -p pond-server --target "${TARGET}" --release ${FEATURES}

BINARY="target/${TARGET}/release/pond-server"

if [ ! -f "${BINARY}" ]; then
  echo "ERROR: Build succeeded but binary not found at ${BINARY}"
  exit 1
fi

echo "==> Binary size: $(du -sh "${BINARY}" | cut -f1)"

echo "==> Deploying to ${JETSON_USER}@${JETSON_HOST}:${JETSON_DEPLOY_DIR}..."
ssh "${JETSON_USER}@${JETSON_HOST}" "mkdir -p ${JETSON_DEPLOY_DIR}/bin"
scp "${BINARY}" "${JETSON_USER}@${JETSON_HOST}:${JETSON_DEPLOY_DIR}/bin/pond-server"

echo "==> Restarting GIAP service on Jetson (if systemd service exists)..."
ssh "${JETSON_USER}@${JETSON_HOST}" \
  "if systemctl is-active --quiet giap.service; then sudo systemctl restart giap.service && echo 'Service restarted.'; else echo 'giap.service not running — start manually: ${JETSON_DEPLOY_DIR}/bin/pond-server serve'; fi"

echo "==> Deploy complete."
echo "    pond-server is at ${JETSON_DEPLOY_DIR}/bin/pond-server on ${JETSON_HOST}"
