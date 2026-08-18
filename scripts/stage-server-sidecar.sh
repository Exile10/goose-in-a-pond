#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# stage-server-sidecar.sh - Build pond-server and stage it as a Tauri 2 sidecar
#
# Produces the file the Tauri bundler expects for `bundle.externalBin`:
#
#     pond-desktop/src-tauri/binaries/pond-server-<HOST_TARGET_TRIPLE>
#
# The macOS bundler copies this into <App>.app/Contents/MacOS/, strips the
# target-triple suffix, and the runtime resolver in process.rs finds it as a
# sibling of the main executable. See docs: https://tauri.app/develop/sidecar/
#
# Steps (each fails loudly if it fails):
#   1. Build the web UI (pond-desktop/dist). The pond-server *release* build
#      embeds pond-desktop/dist at COMPILE TIME (crates/pond-api/build.rs +
#      include_dir). Skipping this yields a binary that serves the build.rs
#      placeholder page instead of the real dashboard.
#   2. Build pond-server in release with RUSTFLAGS explicitly EMPTIED, with
#      the `mesh` feature always on — the native desktop app's own copy of
#      pond-server (this sidecar) needs it compiled in for the Mesh screen
#      to be anything but a permanent no-op; there is no non-mesh sidecar
#      variant, so nobody has to remember a flag to get it.
#      .cargo/config.toml sets `-C target-cpu=native`, which bakes host-CPU
#      instructions into the binary. A distributable binary built that way can
#      SIGILL on a different CPU (per AGENTS.md, the same landmine CI overrides).
#      For a shippable sidecar we must NOT specialise to this build host's CPU.
#      SQLX_OFFLINE=true keeps the build hermetic (no DB connection needed;
#      session/settings storage uses runtime sqlx::query, so there is no .sqlx/).
#   3. Copy target/release/pond-server to the triple-suffixed sidecar path.
#
# Idempotent: safe to re-run; it overwrites the staged sidecar in place.
#
# Usage:
#   bash scripts/stage-server-sidecar.sh
# -----------------------------------------------------------------------------
set -euo pipefail

# Resolve the repo root from this script's location so the script works no
# matter the caller's cwd (npm runs it from pond-desktop/, humans from root).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

DESKTOP_DIR="${REPO_ROOT}/pond-desktop"
BINARIES_DIR="${DESKTOP_DIR}/src-tauri/binaries"
RELEASE_BIN="${REPO_ROOT}/target/release/pond-server"

fail() {
  echo "" >&2
  echo "ERROR: $*" >&2
  echo "  stage-server-sidecar.sh aborted." >&2
  exit 1
}

# --- 0. Determine the host target triple -------------------------------------
# Tauri requires the sidecar file to carry a `-<TARGET_TRIPLE>` suffix, e.g.
# pond-server-aarch64-apple-darwin. Derive it from rustc so it always matches
# the toolchain that produced the binary.
TARGET_TRIPLE="$(rustc -vV | sed -n 's/host: //p')"
[ -n "${TARGET_TRIPLE}" ] || fail "could not determine host target triple from 'rustc -vV'"

SIDECAR_PATH="${BINARIES_DIR}/pond-server-${TARGET_TRIPLE}"

echo "==> Staging pond-server sidecar for target: ${TARGET_TRIPLE}"

# --- 1. Build the web UI (embedded into the release binary) -------------------
echo "==> [1/3] Building web UI (pond-desktop/dist) ..."
( cd "${DESKTOP_DIR}" && npm run build ) \
  || fail "web UI build failed (cd pond-desktop && npm run build). Run 'npm ci' first if deps are missing."
[ -d "${DESKTOP_DIR}/dist" ] || fail "web UI build reported success but pond-desktop/dist is missing."

# --- 2. Build pond-server (release, no host-CPU specialisation) ---------------
echo "==> [2/3] Building pond-server (release, RUSTFLAGS emptied, mesh feature on) ..."
# RUSTFLAGS="" overrides the repo's .cargo/config.toml target-cpu=native so the
# sidecar is portable. SQLX_OFFLINE=true keeps the build offline-safe.
# --features mesh: always on, so the staged sidecar is never the reason the
# Mesh screen doesn't work — see this script's own header comment.
( cd "${REPO_ROOT}" && SQLX_OFFLINE=true RUSTFLAGS="" cargo build --release -p pond-server --features mesh ) \
  || fail "cargo build of pond-server failed."
[ -f "${RELEASE_BIN}" ] || fail "cargo reported success but ${RELEASE_BIN} is missing."

# --- 3. Stage the sidecar with the triple suffix ------------------------------
echo "==> [3/3] Staging sidecar -> ${SIDECAR_PATH}"
mkdir -p "${BINARIES_DIR}"
cp "${RELEASE_BIN}" "${SIDECAR_PATH}" || fail "failed to copy release binary into binaries/."
chmod +x "${SIDECAR_PATH}"

echo ""
echo "OK: staged $(du -h "${SIDECAR_PATH}" | cut -f1) sidecar at:"
echo "    ${SIDECAR_PATH}"
echo ""
echo "Next: cd pond-desktop && npm run tauri build"
