#!/usr/bin/env bash
# scripts/setup.sh — Deploy Goose In A Pond on Linux (Jetson Orin Nano / any ARM64 or x86_64)
#
# Usage:
#   ./scripts/setup.sh [OPTIONS]
#
# Options:
#   --dedicated        Skip prompt, configure as dedicated device (http://pond.local/)
#   --shared           Skip prompt, configure as shared device (http://<HOSTNAME>.local:<PORT>/)
#   --port PORT        Override port selection (default: auto-select 80→8080→4000→5000)
#   --data-dir DIR     Override data directory (default: ~/.local/share/goose-in-a-pond)
#   --no-models        Skip AI model downloads (faster, manual download later)
#   --no-service       Skip systemd service creation
#   --help             Show this message
#
# What this script does:
#   1. Checks prerequisites (cargo, systemd, avahi, LLVM tools — auto-installs if missing)
#   2. Builds pond-server in release mode
#   3. Detects or prompts for dedicated vs shared mode
#   4. Selects an available port
#   5. Configures mDNS hostname via Avahi
#   6. Downloads AI models (Whisper, Piper TTS, Gemma 2B LLM)
#   7. Creates a systemd service for auto-start on boot
#   8. Prints the local access URL

set -euo pipefail
IFS=$'\n\t'

# ── Colour helpers ─────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${BLUE}  ℹ${NC}  $*"; }
ok()    { echo -e "${GREEN}  ✅${NC} $*"; }
warn()  { echo -e "${YELLOW}  ⚠${NC}  $*"; }
err()   { echo -e "${RED}  ❌${NC} $*" >&2; exit 1; }
step()  { echo -e "\n${BOLD}  ── $* ──────────────────────────────────────${NC}"; }
ask()   { echo -e "${BOLD}  ?  $*${NC}"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
BINARY="${REPO_DIR}/target/release/pond-server"

# ── Default values ─────────────────────────────────────────────────────────────
MODE=""               # "dedicated" | "shared"
FORCED_PORT=""
DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}/goose-in-a-pond"
SKIP_MODELS=false
SKIP_SERVICE=false
SERVICE_NAME="goose-in-a-pond"

# ── Argument parsing ───────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dedicated)  MODE="dedicated"; shift ;;
        --shared)     MODE="shared"; shift ;;
        --port)       FORCED_PORT="$2"; shift 2 ;;
        --data-dir)   DATA_DIR="$2"; shift 2 ;;
        --no-models)  SKIP_MODELS=true; shift ;;
        --no-service) SKIP_SERVICE=true; shift ;;
        --help)
            sed -n '2,20p' "$0" | sed 's/^# //' | sed 's/^#//'
            exit 0
            ;;
        *) err "Unknown option: $1  (run with --help for usage)" ;;
    esac
done

# ── OS check ───────────────────────────────────────────────────────────────────
if [[ "$(uname)" != "Linux" ]]; then
    err "This script is for Linux only.  On Windows, run: pond-server setup"
fi

# ── Banner ─────────────────────────────────────────────────────────────────────
echo
echo -e "${BOLD}  ╔═══════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}  ║  🦆  Goose In A Pond — Linux Setup                ║${NC}"
echo -e "${BOLD}  ╚═══════════════════════════════════════════════════╝${NC}"
echo
info "Repository : ${REPO_DIR}"
info "Data dir   : ${DATA_DIR}"
echo

# ── 1. Prerequisites ───────────────────────────────────────────────────────────
step "Checking prerequisites"

# Rust / cargo
if ! command -v cargo &>/dev/null; then
    err "cargo not found.  Install Rust: https://rustup.rs"
fi
ok "cargo $(cargo --version 2>/dev/null | awk '{print $2}')"

# systemd (for service creation)
if [[ "$SKIP_SERVICE" == false ]] && ! command -v systemctl &>/dev/null; then
    warn "systemctl not found — skipping service creation (--no-service to suppress this warning)"
    SKIP_SERVICE=true
fi

# avahi (mDNS / .local hostname broadcasting)
AVAHI_OK=true
if ! command -v avahi-daemon &>/dev/null; then
    warn "avahi-daemon not installed — .local hostnames will not be broadcast"
    ask "Install avahi-daemon now? [Y/n]"
    read -r -p "     " REPLY
    if [[ "${REPLY:-Y}" =~ ^[Yy]$ ]]; then
        sudo apt-get install -y avahi-daemon || {
            warn "Could not install avahi-daemon — continuing without mDNS"
            AVAHI_OK=false
        }
    else
        AVAHI_OK=false
    fi
fi

# ── 1b. LLVM tools ────────────────────────────────────────────────────────────
step "Checking LLVM tools"

ensure_llvm() {
    local tools=(clang llvm-ar llvm-nm llvm-objcopy llvm-objdump llvm-ranlib lld)
    local missing=()
    for t in "${tools[@]}"; do
        command -v "$t" &>/dev/null || missing+=("$t")
    done

    if [[ ${#missing[@]} -eq 0 ]]; then
        ok "LLVM tools present"
        return
    fi

    warn "Missing LLVM tools: ${missing[*]}"

    # Detect package manager and install
    if command -v apt-get &>/dev/null; then
        # Find the latest available LLVM version
        LLVM_VER=$(apt-cache search '^llvm-[0-9]+$' 2>/dev/null \
            | awk '{print $1}' | grep -oP '\d+' | sort -rn | head -1)
        LLVM_VER="${LLVM_VER:-17}"
        info "Installing LLVM ${LLVM_VER} via apt..."
        sudo apt-get install -y \
            "clang-${LLVM_VER}" \
            "llvm-${LLVM_VER}" \
            "lld-${LLVM_VER}" || err "Failed to install LLVM tools"
        # Create unversioned symlinks if missing
        for bin in clang llvm-ar llvm-nm llvm-objcopy llvm-objdump llvm-ranlib lld; do
            local versioned="/usr/bin/${bin}-${LLVM_VER}"
            local unversioned="/usr/local/bin/${bin}"
            if [[ -f "$versioned" && ! -e "$unversioned" ]]; then
                sudo ln -sf "$versioned" "$unversioned"
            fi
        done
    elif command -v dnf &>/dev/null; then
        info "Installing LLVM via dnf..."
        sudo dnf install -y clang llvm lld || err "Failed to install LLVM tools"
    elif command -v pacman &>/dev/null; then
        info "Installing LLVM via pacman..."
        sudo pacman -S --noconfirm clang llvm lld || err "Failed to install LLVM tools"
    else
        err "Cannot auto-install LLVM: no supported package manager found (apt/dnf/pacman). Install clang + llvm + lld manually."
    fi

    ok "LLVM tools installed"
}

ensure_llvm

# ── 2. Build pond-server ───────────────────────────────────────────────────────
step "Building pond-server (release)"

if [[ -f "${BINARY}" ]]; then
    ok "Binary already present: ${BINARY}"
    info "Rebuilding to pick up any changes..."
fi

cd "${REPO_DIR}"
cargo build --release -p pond-server 2>&1 | grep -E "^(   Compiling|   Finished|error)" || true

if [[ ! -f "${BINARY}" ]]; then
    err "Build failed — binary not found at ${BINARY}"
fi
ok "Built: ${BINARY}"

# ── 3. Dedicated vs shared ─────────────────────────────────────────────────────
step "Device mode"

if [[ -z "$MODE" ]]; then
    echo
    info "Dedicated device  →  hostname becomes 'pond', accessible at http://pond.local/"
    info "Shared device     →  keeps current hostname, accessible at http://$(hostname).local:<port>/"
    echo
    ask "Is this device dedicated to Goose In A Pond? [y/N]"
    read -r -p "     " REPLY
    if [[ "${REPLY:-N}" =~ ^[Yy]$ ]]; then
        MODE="dedicated"
    else
        MODE="shared"
    fi
fi

ok "Mode: ${MODE}"

# ── 4. Port selection ──────────────────────────────────────────────────────────
step "Port selection"

select_port() {
    local preferred_ports=(80 8080 4000 5000)
    for p in "${preferred_ports[@]}"; do
        # Skip port 80 for shared devices (requires root and might conflict)
        if [[ "$MODE" == "shared" && "$p" == "80" ]]; then
            continue
        fi
        # Check if port is free
        if ! ss -tlnp 2>/dev/null | grep -q ":${p} " && \
           ! (command -v nc &>/dev/null && nc -z 127.0.0.1 "$p" 2>/dev/null); then
            echo "$p"
            return
        fi
    done
    # All preferred ports busy — fall back to 5001
    echo "5001"
}

if [[ -n "$FORCED_PORT" ]]; then
    PORT="$FORCED_PORT"
    info "Using forced port: ${PORT}"
else
    PORT="$(select_port)"
    info "Selected port: ${PORT}"
fi

# Binding port 80 without root requires special capability on Linux
if [[ "$PORT" == "80" && "$EUID" != "0" ]]; then
    info "Port 80 requires CAP_NET_BIND_SERVICE — applying to binary..."
    sudo setcap 'cap_net_bind_service=+ep' "${BINARY}" || {
        warn "Could not set capability — falling back to port 8080"
        PORT=8080
    }
fi

ok "Port: ${PORT}"

# ── 5. Hostname / mDNS ────────────────────────────────────────────────────────
step "Hostname and mDNS"

CURRENT_HOST="$(hostname)"

if [[ "$MODE" == "dedicated" ]]; then
    if [[ "$CURRENT_HOST" != "pond" ]]; then
        ask "Change hostname from '${CURRENT_HOST}' to 'pond'? [Y/n]"
        read -r -p "     " REPLY
        if [[ "${REPLY:-Y}" =~ ^[Yy]$ ]]; then
            sudo hostnamectl set-hostname pond
            ok "Hostname set to 'pond'"
            CURRENT_HOST="pond"
        else
            info "Keeping hostname '${CURRENT_HOST}'"
        fi
    else
        ok "Hostname already 'pond'"
    fi
fi

if [[ "$AVAHI_OK" == true ]]; then
    sudo systemctl enable --now avahi-daemon 2>/dev/null || true
    ok "avahi-daemon running — broadcasting ${CURRENT_HOST}.local"
fi

# Derive the access URL
if [[ "$PORT" == "80" ]]; then
    ACCESS_URL="http://${CURRENT_HOST}.local/"
else
    ACCESS_URL="http://${CURRENT_HOST}.local:${PORT}/"
fi

ok "Access URL: ${ACCESS_URL}"

# ── 6. Init databases + download AI models ────────────────────────────────────
step "Initializing databases and downloading AI models"

if [[ "$SKIP_MODELS" == true ]]; then
    info "Skipping model downloads (--no-models)"
    # Still init the database
    "${BINARY}" setup --model tiny 2>&1 | head -5 || true
else
    info "This downloads Whisper (~140 MB), Piper TTS (~65 MB), and Gemma 2B LLM (~2 GB)."
    info "Total: ~2.2 GB  —  takes several minutes on first run."
    echo
    "${BINARY}" setup
fi

# ── 7. Create systemd service ─────────────────────────────────────────────────
step "Creating systemd service"

if [[ "$SKIP_SERVICE" == true ]]; then
    info "Skipping service creation (--no-service)"
else
    SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

    sudo tee "${SERVICE_FILE}" > /dev/null <<EOF
[Unit]
Description=Goose In A Pond — Local AI Home Assistant
Documentation=https://github.com/Jarida/goose-in-a-pond
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${USER}
WorkingDirectory=${REPO_DIR}
ExecStart=${BINARY} serve --port ${PORT}
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=goose-in-a-pond
# Give the LLM time to load on startup
TimeoutStartSec=120

[Install]
WantedBy=multi-user.target
EOF

    sudo systemctl daemon-reload
    sudo systemctl enable "${SERVICE_NAME}"
    sudo systemctl restart "${SERVICE_NAME}"

    # Brief wait for startup
    sleep 3
    if systemctl is-active --quiet "${SERVICE_NAME}"; then
        ok "Service '${SERVICE_NAME}' is running"
    else
        warn "Service did not start cleanly — check: journalctl -u ${SERVICE_NAME} -n 50"
    fi
fi

# ── 8. Summary ────────────────────────────────────────────────────────────────
echo
echo -e "${BOLD}  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BOLD}  ✅ Goose In A Pond is ready!${NC}"
echo
echo -e "  ${BOLD}Access URL:${NC}  ${GREEN}${ACCESS_URL}${NC}"
echo
if [[ "$SKIP_SERVICE" == false ]]; then
    echo "  Service management:"
    echo "    sudo systemctl status ${SERVICE_NAME}"
    echo "    sudo systemctl restart ${SERVICE_NAME}"
    echo "    journalctl -u ${SERVICE_NAME} -f"
    echo
fi
echo "  Data directory: ${DATA_DIR}"
echo "  Prompt files:   ${DATA_DIR}/prompts/system.md  (optional override)"
echo -e "${BOLD}  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo
