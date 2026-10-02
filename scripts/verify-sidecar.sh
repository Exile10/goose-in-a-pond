#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# verify-sidecar.sh - Refuse to package a desktop app that would not work
#
# Replaces stage-server-sidecar-stub.sh, which existed because Tauri's
# `bundle.externalBin` was resolved at COMPILE time by tauri_build::build(), so
# a fresh clone could not even `cargo check` inside src-tauri without a file
# present. There is no compile step here, so that reason is gone -- but its
# risk is not, and it was always the worse half: the stub was a shell script
# that printed a marker and exited 1, and `tauri build` would happily bundle it
# into a .dmg. The result looked like a shippable app and contained no server.
#
# So this checks the opposite thing, at the moment it matters:
#
#   1. resources/pond-server exists and is executable
#   2. it is a Mach-O binary, not a shell script or a truncated copy
#   3. its architecture matches the one being packaged
#   4. it actually runs
#   5. the dist it was built against is the dist about to be shipped
#   6. the LiteRT-LM library in resources/litert-lm, when one is staged, is the
#      package its manifest describes: arm64 Mach-O, dependencies resolved
#      inside that directory or by macOS, validly signed, and loadable; and
#      every dylib in it is named under mac.binaries
#
# Usage:
#   bash scripts/verify-sidecar.sh
# -----------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DESKTOP_DIR="${REPO_ROOT}/pond-desktop"
SIDECAR="${DESKTOP_DIR}/resources/pond-server"
STAMP="${DESKTOP_DIR}/resources/.dist-stamp"

fail() {
  echo "" >&2
  echo "ERROR: $*" >&2
  echo "  Run 'npm run stage:server' from pond-desktop/ and try again." >&2
  exit 1
}

# 1. Present and executable.
[ -f "${SIDECAR}" ] || fail "no staged sidecar at ${SIDECAR}."
[ -x "${SIDECAR}" ] || fail "${SIDECAR} is not executable."

[ -x "${DESKTOP_DIR}/resources/pondnet" ] || fail "bundled networking helper is missing."
"${DESKTOP_DIR}/resources/pondnet" --help >/dev/null 2>&1 || fail "networking helper does not run."

# 2. A real binary. This is the check that would have caught the old stub: a
#    shell script starts with '#!' and passes every other test here.
if [ "$(uname -s)" = "Darwin" ]; then
  MAGIC="$(head -c 4 "${SIDECAR}" | xxd -p)"
  case "${MAGIC}" in
    cffaedfe|cefaedfe|cafebabe|bebafeca) ;;
    *) fail "${SIDECAR} is not a Mach-O binary (magic ${MAGIC}). A shell-script placeholder or a truncated copy would look exactly like this." ;;
  esac

  # 3. Right architecture for this machine.
  HOST_ARCH="$(uname -m)"
  if command -v lipo >/dev/null 2>&1; then
    ARCHS="$(lipo -archs "${SIDECAR}" 2>/dev/null || echo "")"
    case " ${ARCHS} " in
      *" ${HOST_ARCH} "*) ;;
      "") echo "  note: could not read the sidecar's architectures; skipping that check" ;;
      *) fail "sidecar is built for [${ARCHS}] but this host is ${HOST_ARCH}." ;;
    esac
  fi
fi

# 4. It runs. Catches a binary built against a newer macOS, or one missing a
#    dylib -- both of which are otherwise discovered by a user, at launch.
if ! "${SIDECAR}" --help >/dev/null 2>&1; then
  fail "${SIDECAR} did not run successfully ('--help' failed)."
fi

# 5. The renderer about to be shipped is the one compiled into that binary.
[ -f "${STAMP}" ] || fail "no .dist-stamp beside the sidecar; re-stage so the two can be compared."
EXPECTED="$(cat "${STAMP}")"
ACTUAL="$(bash "${SCRIPT_DIR}/lib/dist-stamp.sh" "${DESKTOP_DIR}/dist")"
if [ "${EXPECTED}" != "${ACTUAL}" ]; then
  fail "pond-desktop/dist has changed since the sidecar was staged.
  The app would ship one version of the UI in its window and a different one
  on the LAN dashboard that pond-server serves, because the sidecar embeds
  dist at compile time."
fi

# 6. The LiteRT-LM library. Without it the app runs and .litertlm models do not load, so its
#    absence is a warning; a staged copy that fails its checks is native code the server would
#    load into its own process, so that stops the packaging.
LITERT_STAGED="${DESKTOP_DIR}/resources/litert-lm"
LITERT_SUMMARY="no LiteRT-LM library"
if [ -d "${LITERT_STAGED}" ]; then
  # shellcheck source=lib/litert-setup.sh
  source "${SCRIPT_DIR}/lib/litert-setup.sh"
  if ! litert_verify "${LITERT_STAGED}"; then
    fail "the staged LiteRT-LM library fails verification:
${LITERT_VERIFY_PROBLEMS}"
  fi
  for LIB in "${LITERT_STAGED}"/*.dylib; do
    grep -qF "Contents/Resources/litert-lm/${LIB##*/}" "${DESKTOP_DIR}/electron-builder.yml" \
      || fail "${LIB##*/} is staged but not named under mac.binaries in electron-builder.yml."
  done
  LITERT_SUMMARY="LiteRT-LM verified (${LITERT_VERIFY_FILES} files)"
  [ -z "${LITERT_VERIFY_NOTES}" ] || echo "  note: ${LITERT_VERIFY_NOTES}"
else
  echo "  warning: no LiteRT-LM library staged; this app will not load .litertlm models"
  echo "  (bash scripts/giap.sh litert build macos-arm64, then npm run stage:server)"
fi

echo "OK: sidecar verified ($(du -h "${SIDECAR}" | cut -f1)), dist matches, ${LITERT_SUMMARY}."
