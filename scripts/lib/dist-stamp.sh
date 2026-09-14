#!/usr/bin/env bash
# Print a stable content hash of a built renderer directory.
#
# Used to detect the one packaging mistake that is otherwise invisible:
# rebuilding pond-desktop/dist between staging the sidecar (which compiles dist
# into pond-server) and packaging the app (which ships dist to the renderer).
# The result is a desktop window and a LAN dashboard running different code.
set -euo pipefail
DIR="${1:?usage: dist-stamp.sh <dist-dir>}"
# Sorted paths plus per-file hashes, so the stamp is order-independent and
# notices a changed file as readily as an added or removed one.
( cd "$DIR" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256 ) | shasum -a 256 | cut -d' ' -f1
