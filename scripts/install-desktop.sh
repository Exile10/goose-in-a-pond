#!/usr/bin/env bash
# scripts/install-desktop.sh — install the freshly-built pond-desktop bundle
# into /Applications and force macOS to register it.
#
# Why a script: Tauri builds an unsigned `.app`. When you drag an unsigned
# bundle from the build directory into /Applications, macOS:
#   1. Stamps it with `com.apple.quarantine` (Gatekeeper),
#   2. Doesn't always tell Launch Services to refresh, so the app never
#      appears in Launchpad / the Applications grid until you reboot or
#      `killall Finder`.
# This script does both, plus prints the path so you can confirm.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Goose In A Pond.app"
SRC="$REPO_ROOT/pond-desktop/src-tauri/target/release/bundle/macos/$APP_NAME"
DEST="/Applications/$APP_NAME"

if [[ ! -d "$SRC" ]]; then
  echo "✗ Built bundle not found at:" >&2
  echo "    $SRC" >&2
  echo "Build it first:  cd pond-desktop && npm run tauri build" >&2
  exit 1
fi

if [[ -d "$DEST" ]]; then
  echo "⚠  Removing existing $DEST"
  rm -rf "$DEST"
fi

echo "→ Copying bundle into /Applications…"
cp -R "$SRC" "$DEST"

echo "→ Stripping com.apple.quarantine attribute (recursive)…"
xattr -cr "$DEST" || true

echo "→ Re-registering with Launch Services so Finder/Launchpad pick it up…"
/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister \
  -f "$DEST" || true

# Bouncing Finder is the most reliable way to refresh the grid view in
# /Applications (Tauri doesn't sign the app, so Finder caches the broken
# one until either reboot or this nudge).
echo "→ Restarting Finder…"
killall Finder 2>/dev/null || true

echo
echo "✅ Installed: $DEST"
echo "    Open it from /Applications, or: open \"$DEST\""
