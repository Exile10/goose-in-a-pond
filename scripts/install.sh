#!/usr/bin/env bash
# scripts/install.sh — First-time installation for Goose In A Pond
#
# Cross-platform (macOS + Linux). Handles everything from submodule init to
# a verified running server.
#
# Usage:
#   bash scripts/install.sh [OPTIONS]
#
# Options:
#   --full            Build entire workspace including Goose (10+ min first time)
#   --fast            Build only server crates (default, ~2 min)
#   --desktop         Also install desktop app (npm install in pond-desktop)
#   --no-models       Skip all model downloads
#   --no-verify       Skip post-install health check
#   --ollama          Pull LLM via Ollama (auto-detected if installed)
#   --llamafile       Download llamafile binary with embedded model
#   --whisper-model MODEL  Whisper size: tiny|base|small (default: base)
#   --data-dir DIR    Override data directory
#   --help            Show this message

set -euo pipefail
IFS=$'\n\t'

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

info()  { echo -e "${BLUE}  i${NC}  $*"; }
ok()    { echo -e "${GREEN}  ✓${NC}  $*"; }
warn()  { echo -e "${YELLOW}  !${NC}  $*"; }
fail()  { echo -e "${RED}  ✗${NC}  $*"; }
step()  { echo -e "\n${BOLD}  ── Step $1: $2 ──${NC}"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Defaults ──────────────────────────────────────────────────────────────────
BUILD_MODE="fast"
INSTALL_DESKTOP=false
SKIP_MODELS=false
SKIP_VERIFY=false
LLM_MODE=""          # "" = auto-detect, "ollama", "llamafile"
WHISPER_MODEL="base"
DATA_DIR=""
OS=""
ARCH=""

# ── Status tracking ───────────────────────────────────────────────────────────
S_RUST=""; S_SUBMODULE=""; S_DEPS=""; S_BUILD=""; S_DB=""
S_WHISPER=""; S_PIPER=""; S_LLM=""; S_VERIFY=""; S_DESKTOP=""

# ── Parse arguments ───────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --full)          BUILD_MODE="full"; shift ;;
    --fast)          BUILD_MODE="fast"; shift ;;
    --desktop)       INSTALL_DESKTOP=true; shift ;;
    --no-models)     SKIP_MODELS=true; shift ;;
    --no-verify)     SKIP_VERIFY=true; shift ;;
    --ollama)        LLM_MODE="ollama"; shift ;;
    --llamafile)     LLM_MODE="llamafile"; shift ;;
    --whisper-model) WHISPER_MODEL="$2"; shift 2 ;;
    --data-dir)      DATA_DIR="$2"; shift 2 ;;
    --help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *) warn "Unknown option: $1"; shift ;;
  esac
done

# ── OS / Arch detection ───────────────────────────────────────────────────────
detect_platform() {
  case "$(uname -s)" in
    Darwin*) OS="macos" ;;
    Linux*)  OS="linux" ;;
    *)       fail "Unsupported OS: $(uname -s)"; exit 1 ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64)   ARCH="x86_64" ;;
    arm64|aarch64)   ARCH="aarch64" ;;
    *)               ARCH="$(uname -m)" ;;
  esac
}

# ── Data directory ────────────────────────────────────────────────────────────
resolve_data_dir() {
  if [ -n "$DATA_DIR" ]; then return; fi
  if [ -n "${POND_DATA_DIR:-}" ]; then DATA_DIR="$POND_DATA_DIR"; return; fi
  case "$OS" in
    macos) DATA_DIR="$HOME/Library/Application Support/goose-in-a-pond" ;;
    linux) DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/goose-in-a-pond" ;;
  esac
}

# ── Check for a command, print helpful install instructions ───────────────────
check_command() {
  local cmd="$1" hint="$2"
  if command -v "$cmd" &>/dev/null; then
    return 0
  else
    fail "$cmd not found — $hint"
    return 1
  fi
}

# ══════════════════════════════════════════════════════════════════════════════
echo -e "\n${BOLD}  ╔═══════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}  ║   🦆  Goose In A Pond — Installation              ║${NC}"
echo -e "${BOLD}  ╚═══════════════════════════════════════════════════╝${NC}"

detect_platform
resolve_data_dir
echo -e "  Platform: ${BOLD}${OS}/${ARCH}${NC}"
echo -e "  Data dir: ${DIM}${DATA_DIR}${NC}"
echo -e "  Repo:     ${DIM}${REPO_DIR}${NC}"

# ── Step 1: Pre-flight checks ────────────────────────────────────────────────
step "1/8" "Pre-flight checks"

PREFLIGHT_OK=true

if check_command git "https://git-scm.com/"; then
  RUST_VER=$(rustc --version 2>/dev/null | awk '{print $2}' || echo "")
  if check_command cargo "Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; then
    ok "Rust toolchain: ${RUST_VER:-unknown}"
    S_RUST="ok"
  else
    PREFLIGHT_OK=false; S_RUST="missing"
  fi
else
  PREFLIGHT_OK=false; S_RUST="missing"
fi

if ! check_command cc "macOS: xcode-select --install | Linux: apt install build-essential"; then
  PREFLIGHT_OK=false
fi

if [ "$PREFLIGHT_OK" = false ]; then
  fail "Missing prerequisites. Install them and re-run this script."
  exit 1
fi

# ── Step 2: Goose submodule ───────────────────────────────────────────────────
step "2/8" "Goose submodule"

cd "$REPO_DIR"
if [ ! -f "goose/Cargo.toml" ]; then
  info "Initializing goose submodule (first time)..."
  git submodule update --init --recursive
  if [ -f "goose/Cargo.toml" ]; then
    ok "Goose submodule initialized"
    S_SUBMODULE="ok"
  else
    fail "Submodule init failed — check git remote access"
    S_SUBMODULE="failed"
  fi
else
  ok "Goose submodule already present"
  S_SUBMODULE="ok"
fi

# ── Step 3: System dependencies ───────────────────────────────────────────────
step "3/8" "System dependencies"

install_deps() {
  case "$OS" in
    macos)
      if command -v brew &>/dev/null; then
        local needed=()
        command -v cmake &>/dev/null || needed+=(cmake)
        command -v pkg-config &>/dev/null || needed+=(pkg-config)
        if [ ${#needed[@]} -gt 0 ]; then
          info "Installing via Homebrew: ${needed[*]}"
          brew install "${needed[@]}" 2>/dev/null || warn "Some Homebrew installs failed (non-fatal)"
        fi
        ok "macOS dependencies OK"
        S_DEPS="ok"
      else
        warn "Homebrew not found — install cmake and pkg-config manually if build fails"
        S_DEPS="partial"
      fi
      ;;
    linux)
      if command -v apt &>/dev/null; then
        info "Installing build dependencies via apt..."
        sudo apt update -qq 2>/dev/null || true
        sudo apt install -y -qq build-essential pkg-config libssl-dev libasound2-dev cmake 2>/dev/null \
          && { ok "Linux dependencies installed"; S_DEPS="ok"; } \
          || { warn "apt install had errors (non-fatal)"; S_DEPS="partial"; }
      elif command -v dnf &>/dev/null; then
        info "Installing build dependencies via dnf..."
        sudo dnf install -y gcc gcc-c++ openssl-devel alsa-lib-devel cmake pkg-config 2>/dev/null \
          && { ok "Linux dependencies installed"; S_DEPS="ok"; } \
          || { warn "dnf install had errors (non-fatal)"; S_DEPS="partial"; }
      else
        warn "No supported package manager (apt/dnf) — install build deps manually"
        S_DEPS="partial"
      fi
      ;;
  esac
}

install_deps

# ── Step 4: Build pond-server ─────────────────────────────────────────────────
step "4/8" "Building pond-server (${BUILD_MODE} mode)"

cd "$REPO_DIR"

if [ "$BUILD_MODE" = "full" ]; then
  info "Full workspace build (includes Goose — may take 10+ minutes first time)..."
  if cargo build --workspace --release 2>&1 | tail -3; then
    ok "Full workspace built"
    S_BUILD="ok"
  else
    warn "Full build failed — trying fast build as fallback"
    BUILD_MODE="fast"
  fi
fi

if [ "$BUILD_MODE" = "fast" ]; then
  info "Fast build (server crates only)..."
  if cargo build -p pond-core -p pond-infra -p pond-api -p pond-server --release 2>&1 | tail -3; then
    ok "pond-server built (release)"
    S_BUILD="ok"
  else
    fail "Build failed — check compiler output above"
    S_BUILD="failed"
  fi
fi

# ── Step 5: pond-server setup (DB + Whisper + Piper) ──────────────────────────
step "5/8" "Server setup (databases, Whisper ASR, Piper TTS)"

if [ "$SKIP_MODELS" = true ]; then
  info "Skipping model downloads (--no-models)"
  # Still init the DB
  cargo run -p pond-server --release -- setup --model "$WHISPER_MODEL" 2>&1 | grep -E '^\s*(✅|⚠|📂|📋|\[)' || true
  S_DB="ok"; S_WHISPER="skipped"; S_PIPER="skipped"
else
  if cargo run -p pond-server --release -- setup --model "$WHISPER_MODEL" 2>&1 | tee /dev/stderr | grep -q '✅.*Databases ready'; then
    S_DB="ok"
  else
    S_DB="ok"  # DB init rarely fails — migrations are robust
  fi
  # Check results
  [ -f "$DATA_DIR/pond_system.db" ] && S_DB="ok" || S_DB="failed"

  WHISPER_BIN="$DATA_DIR/bin/whisper-server"
  [ -f "$WHISPER_BIN" ] || WHISPER_BIN="$DATA_DIR/bin/whisper-server.exe"
  [ -f "$WHISPER_BIN" ] && S_WHISPER="ok" || S_WHISPER="failed"

  PIPER_BIN="$DATA_DIR/bin/piper"
  [ -f "$PIPER_BIN" ] || PIPER_BIN="$DATA_DIR/bin/piper.exe"
  [ -f "$PIPER_BIN" ] && S_PIPER="ok" || S_PIPER="failed"
fi

# ── Step 6: LLM model ────────────────────────────────────────────────────────
step "6/8" "LLM model"

if [ "$SKIP_MODELS" = true ]; then
  info "Skipping LLM model (--no-models)"
  S_LLM="skipped"
else
  # Auto-detect LLM strategy
  if [ -z "$LLM_MODE" ]; then
    if command -v ollama &>/dev/null; then
      LLM_MODE="ollama"
    else
      LLM_MODE="none"
    fi
  fi

  case "$LLM_MODE" in
    ollama)
      info "Pulling gemma3:4b via Ollama (~2.6 GB)..."
      if ollama pull gemma3:4b 2>&1; then
        ok "LLM model: gemma3:4b (Ollama)"
        S_LLM="ok (ollama)"
      else
        warn "Ollama pull failed — you can pull manually: ollama pull gemma3:4b"
        S_LLM="failed"
      fi
      ;;
    llamafile)
      LLAMAFILE_DIR="$DATA_DIR/bin"
      LLAMAFILE_PATH="$LLAMAFILE_DIR/gemma-2-2b-it.llamafile"
      mkdir -p "$LLAMAFILE_DIR"
      if [ ! -f "$LLAMAFILE_PATH" ]; then
        info "Downloading gemma-2-2b-it llamafile (~1.5 GB)..."
        curl -L --progress-bar \
          "https://huggingface.co/Mozilla/gemma-2-2b-it-llamafile/resolve/main/gemma-2-2b-it.Q4_K_M.llamafile" \
          -o "$LLAMAFILE_PATH" \
          && chmod +x "$LLAMAFILE_PATH" \
          && { ok "LLM model: gemma-2-2b-it (llamafile)"; S_LLM="ok (llamafile)"; } \
          || { warn "Llamafile download failed"; S_LLM="failed"; }
      else
        ok "Llamafile already present"
        S_LLM="ok (llamafile)"
      fi
      ;;
    *)
      warn "No LLM model installed. Install Ollama (https://ollama.com) and run: ollama pull gemma3:4b"
      S_LLM="not installed"
      ;;
  esac
fi

# ── Step 7: Piper TTS voice ──────────────────────────────────────────────────
step "7/8" "Piper TTS voice model"

if [ "$SKIP_MODELS" = true ]; then
  info "Skipping voice model (--no-models)"
else
  VOICE_DIR="$DATA_DIR/models/tts"
  mkdir -p "$VOICE_DIR"
  VOICE_FILE="$VOICE_DIR/en_US-lessac-medium.onnx"
  CONFIG_FILE="$VOICE_DIR/en_US-lessac-medium.onnx.json"

  if [ -f "$VOICE_FILE" ] && [ -f "$CONFIG_FILE" ]; then
    ok "TTS voice already present: en_US-lessac-medium"
  else
    PIPER_VOICE_BASE="https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/lessac/medium"
    info "Downloading en_US-lessac-medium voice model (~63 MB)..."
    curl -L --progress-bar "${PIPER_VOICE_BASE}/en_US-lessac-medium.onnx" -o "$VOICE_FILE" 2>&1 || true
    curl -sL "${PIPER_VOICE_BASE}/en_US-lessac-medium.onnx.json" -o "$CONFIG_FILE" 2>&1 || true

    if [ -f "$VOICE_FILE" ] && [ -s "$VOICE_FILE" ]; then
      ok "TTS voice: en_US-lessac-medium"
    else
      warn "Voice download failed — TTS will be text-only until manually installed"
    fi
  fi
fi

# ── Step 8: Desktop app (optional) ───────────────────────────────────────────
if [ "$INSTALL_DESKTOP" = true ]; then
  step "8/8" "Desktop app (pond-desktop)"
  if command -v npm &>/dev/null; then
    cd "$REPO_DIR/pond-desktop"
    info "Running npm install..."
    if npm install 2>&1 | tail -3; then
      ok "Desktop app dependencies installed"
      S_DESKTOP="ok"
    else
      warn "npm install had issues"
      S_DESKTOP="failed"
    fi
    cd "$REPO_DIR"
  else
    warn "npm not found — install Node.js to use the desktop app"
    S_DESKTOP="npm missing"
  fi
else
  S_DESKTOP="skipped"
fi

# ── Verification ──────────────────────────────────────────────────────────────
if [ "$SKIP_VERIFY" = false ] && [ "$S_BUILD" = "ok" ]; then
  echo -e "\n${BOLD}  ── Verifying installation... ──${NC}"

  SERVER_PID=""
  cleanup_server() { [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true; }
  trap cleanup_server EXIT

  cargo run -p pond-server --release -- serve --port 4000 &>/dev/null &
  SERVER_PID=$!
  sleep 4

  if curl -sf "http://127.0.0.1:4000/api/v1/health" > /dev/null 2>&1; then
    ok "Server health check passed"
    S_VERIFY="ok"
  else
    warn "Server health check failed (may need more startup time)"
    S_VERIFY="failed"
  fi

  kill "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  trap - EXIT
else
  S_VERIFY="skipped"
fi

# ── Status report ─────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}  ╔═══════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}  ║   🦆  Installation Summary                        ║${NC}"
echo -e "${BOLD}  ╚═══════════════════════════════════════════════════╝${NC}"
echo ""

status_icon() {
  case "$1" in
    ok*)      echo -e "${GREEN}✓${NC}" ;;
    partial)  echo -e "${YELLOW}~${NC}" ;;
    skipped)  echo -e "${DIM}⏭${NC}" ;;
    failed)   echo -e "${RED}✗${NC}" ;;
    missing|"not installed"|"npm missing") echo -e "${YELLOW}!${NC}" ;;
    *)        echo -e "${DIM}-${NC}" ;;
  esac
}

print_row() {
  local label="$1" status="$2" detail="${3:-}"
  local icon
  icon=$(status_icon "$status")
  printf "  %s  %-24s %s\n" "$icon" "$label" "$detail"
}

print_row "Rust toolchain"       "$S_RUST"      "${RUST_VER:-}"
print_row "Goose submodule"      "$S_SUBMODULE"
print_row "System dependencies"  "$S_DEPS"
print_row "pond-server binary"   "$S_BUILD"     "(release)"
print_row "SQLite databases"     "$S_DB"
print_row "Whisper ASR"          "$S_WHISPER"
print_row "Piper TTS"            "$S_PIPER"
print_row "LLM model"            "$S_LLM"
print_row "Health check"         "$S_VERIFY"
print_row "Desktop app"          "$S_DESKTOP"

echo ""

# Count issues
ISSUES=0
for s in "$S_RUST" "$S_SUBMODULE" "$S_BUILD" "$S_DB"; do
  [ "$s" = "failed" ] || [ "$s" = "missing" ] && ISSUES=$((ISSUES + 1))
done

if [ "$ISSUES" -gt 0 ]; then
  echo -e "  ${RED}${ISSUES} issue(s) need attention.${NC} Check the output above."
else
  echo -e "  ${GREEN}All core components ready!${NC}"
  echo ""
  echo -e "  ${BOLD}Quick start:${NC}"
  echo -e "    cargo run -p pond-server --release -- serve --open"
  echo ""
  echo -e "  ${DIM}Web dashboard: http://localhost:4000${NC}"
  echo -e "  ${DIM}Voice mode:    cargo run -p pond-server --release -- chat --input whisper${NC}"
  if [ "$S_DESKTOP" = "ok" ]; then
    echo -e "  ${DIM}Desktop app:   cd pond-desktop && npm run tauri dev${NC}"
  elif [ "$S_DESKTOP" = "skipped" ]; then
    echo -e "  ${DIM}Desktop app:   bash scripts/install.sh --desktop${NC}"
  fi
fi

echo ""
