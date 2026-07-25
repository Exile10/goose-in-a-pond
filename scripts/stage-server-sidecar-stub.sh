#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# stage-server-sidecar-stub.sh - Stage a PLACEHOLDER pond-server sidecar
#
# tauri.conf.json declares `bundle.externalBin: ["binaries/pond-server"]`, which
# makes tauri_build::build() (src-tauri/build.rs) resolve and copy the sidecar
# at COMPILE TIME. That file — binaries/pond-server-<HOST_TARGET_TRIPLE> — is
# produced only by stage-server-sidecar.sh and is gitignored, so on a fresh
# clone a plain `cargo build`/`cargo check`/`cargo test` inside src-tauri (all
# documented dev commands) fails before it can even compile, with a confusing
# "external binary not found" error.
#
# This script stages a tiny, clearly-marked STUB executable at that path so
# compile-only workflows (bare `cargo check`/`cargo test`, `tauri dev` smoke
# builds) succeed for teammates who have not run a full release build. The stub
# is NOT a working server: it just prints a message and exits 1, so it can never
# be mistaken for a shippable binary at runtime.
#
# For a real, distributable bundle use `npm run stage:server` (which builds the
# actual pond-server release binary) — this script does NOT replace it.
#
# Idempotent: safe to re-run; overwrites the stub in place. Refuses to clobber a
# real (non-stub) staged sidecar so it never downgrades a proper build.
#
# Usage:
#   npm run stage:server:stub        # from pond-desktop/
#   bash scripts/stage-server-sidecar-stub.sh
# -----------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BINARIES_DIR="${REPO_ROOT}/pond-desktop/src-tauri/binaries"

# Marker line the stub prints so we can recognise (and safely overwrite) it.
STUB_MARKER="GIAP_POND_SERVER_SIDECAR_STUB"

TARGET_TRIPLE="$(rustc -vV | sed -n 's/host: //p')"
if [ -z "${TARGET_TRIPLE}" ]; then
  echo "ERROR: could not determine host target triple from 'rustc -vV'" >&2
  exit 1
fi

SIDECAR_PATH="${BINARIES_DIR}/pond-server-${TARGET_TRIPLE}"

# Never clobber a real staged sidecar (from stage-server-sidecar.sh). Only skip
# if an existing file is NOT our stub.
if [ -f "${SIDECAR_PATH}" ] && ! grep -q "${STUB_MARKER}" "${SIDECAR_PATH}" 2>/dev/null; then
  echo "==> A non-stub pond-server sidecar already exists at:"
  echo "    ${SIDECAR_PATH}"
  echo "    Leaving it in place (run 'npm run stage:server' to refresh the real one)."
  exit 0
fi

mkdir -p "${BINARIES_DIR}"

cat > "${SIDECAR_PATH}" <<EOF
#!/usr/bin/env sh
# ${STUB_MARKER}
# This is a COMPILE-ONLY placeholder so 'cargo check'/'cargo test' in src-tauri
# resolve the tauri externalBin sidecar without a full release build.
# It is NOT a working pond-server. Run 'npm run stage:server' for the real one.
echo "pond-server sidecar stub (${STUB_MARKER}) — not a real server. Run 'npm run stage:server'." >&2
exit 1
EOF
chmod +x "${SIDECAR_PATH}"

echo "OK: staged STUB pond-server sidecar (compile-only) at:"
echo "    ${SIDECAR_PATH}"
