#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# jetson-deploy.sh — one-command deploy of GIAP to the Jetson Orin Nano
#
# Run from the repo root ON THE DEV MACHINE (not the Jetson):
#   bash scripts/jetson-deploy.sh                # deploy origin/main
#   bash scripts/jetson-deploy.sh --branch mybr  # deploy another pushed branch
#   JETSON_HOST=nano-ip bash scripts/jetson-deploy.sh   # alternate ssh host
#
# What it does:
#   1. Jetson: fetch + hard-reset the checkout to origin/<branch>, sync the
#      goose submodule to the pinned SHA.
#   2. Dev machine: build the web UI (Vite needs Node >= 18, which the Jetson
#      does not have) and rsync pond-desktop/dist to the Jetson — pond-server
#      embeds it at compile time (crates/pond-api/build.rs).
#   3. Jetson: release build with CUDA (sm_87; .cargo/config.toml's
#      target-cpu=native is correct for an on-device build).
#   4. Jetson: restart the user-level systemd service and health-check the API.
#
# Prereqs (already true on nano.local):
#   - ssh alias in ~/.ssh/config (Host nano → nano.local, key nano_jetson)
#   - loginctl enable-linger nano   (user service survives logout / starts at boot)
#   - The branch you deploy must be pushed to the Jetson's `origin` remote.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${JETSON_HOST:-nano}"
BRANCH="main"
DESKTOP=false
while [ $# -gt 0 ]; do
  case "$1" in
    --branch) BRANCH="$2"; shift 2 ;;
    --desktop) DESKTOP=true; shift ;;   # also build the native Tauri app (needs a display)
    *) echo "Unknown argument: $1"; exit 1 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_REPO="goose-in-a-pond"

echo "==> [1/4] Updating Jetson checkout to origin/${BRANCH}"
ssh "$HOST" "cd ~/${REMOTE_REPO} \
  && git fetch origin \
  && git checkout -q ${BRANCH} \
  && git reset --hard -q origin/${BRANCH} \
  && git submodule sync -q --recursive \
  && git submodule update --init --recursive \
  && git log --oneline -1"

echo "==> [2/4] Building web UI locally and syncing dist"
( cd "${REPO_ROOT}/pond-desktop" && npm run build )
rsync -az --delete "${REPO_ROOT}/pond-desktop/dist/" "${HOST}:${REMOTE_REPO}/pond-desktop/dist/"

echo "==> [3/4] Release build on the Jetson (CUDA sm_87) — this is the slow step"
# CMAKE_CUDA_ARCHITECTURES=87 makes ggml emit a real sm_87 cubin; without it the
# build ships compute_80 PTX that the driver JIT-compiles at first model load.
# whisper/cuda moves ASR decode onto the GPU (research R7: ~20s of audio in
# ~1-2.6s instead of a multi-second all-6-core CPU burst).
ssh "$HOST" "cd ~/${REMOTE_REPO} \
  && PATH=\$HOME/.cargo/bin:/usr/local/cuda/bin:\$PATH SQLX_OFFLINE=true \
     CMAKE_CUDA_ARCHITECTURES=87 \
     cargo build -p pond-server \
       --features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda \
       --release"

echo "==> [4/4] Restarting service + health check"
ssh "$HOST" "systemctl --user restart goose-in-a-pond.service && sleep 4 \
  && systemctl --user --no-pager status goose-in-a-pond.service | head -5 \
  && curl -sf -o /dev/null -w 'API: HTTP %{http_code}\n' http://127.0.0.1:8080/api/v1/health \
     || curl -sf -o /dev/null -w 'API(root): HTTP %{http_code}\n' http://127.0.0.1:8080/"

if [ "$DESKTOP" = true ]; then
  echo "==> [desktop] Ensuring WebKitGTK deps, then building the native Tauri app"
  # install-desktop-deps.sh checks + installs the WebKitGTK stack (also enforced
  # at compile time by pond-desktop/src-tauri/build.rs). A plain cargo build of
  # the src-tauri crate embeds the dist synced above — no Node/cargo-tauri needed
  # on the device (the Jetson's Node is too old for Vite).
  ssh "$HOST" "cd ~/${REMOTE_REPO} \
    && bash scripts/install-desktop-deps.sh \
    && PATH=\$HOME/.cargo/bin:\$PATH SQLX_OFFLINE=true \
       cargo build --release --manifest-path pond-desktop/src-tauri/Cargo.toml \
    && echo 'Desktop app: ~/'${REMOTE_REPO}'/pond-desktop/src-tauri/target/release/pond-desktop'"
  echo "    Launch on the attached display:  ssh $HOST 'DISPLAY=:0 ~/${REMOTE_REPO}/pond-desktop/src-tauri/target/release/pond-desktop'"
fi

echo ""
echo "Deployed. Dashboard: http://nano.local:8080"
