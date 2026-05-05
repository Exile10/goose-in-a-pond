#!/usr/bin/env bash
# scripts/setup.sh — Wrapper for the unified installer
#
# DEPRECATED: Use install.sh --production instead.
#
# This script now delegates to install.sh with --production mode.
# All original setup.sh flags are still supported:
#   --dedicated, --shared, --port, --data-dir, --no-models, --no-service

echo -e "\033[1;33m  Note:\033[0m setup.sh is now a wrapper for install.sh --production"
echo ""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/install.sh" --production "$@"
