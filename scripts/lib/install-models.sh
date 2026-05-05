#!/usr/bin/env bash
# scripts/lib/install-models.sh — Model downloads and pond-server setup
#
# Provides: run_pond_setup(), download_llm()
# Sourced by install.sh — not meant to be run directly.
#
# Most model downloads are delegated to `pond-server setup` which handles
# DB init, migrations, Whisper, Piper, ONNX, and prompt templates in Rust.
# This file adds LLM provider installation (Ollama/llamafile) on top.

# ── Run pond-server setup ────────────────────────────────────────────────────
# Handles: DB init, migrations, prompt template seeding, model catalog,
#          Whisper download, Piper download, ONNX runtime.
run_pond_setup() {
  step "5" "Server setup (databases, Whisper ASR, Piper TTS, ONNX)"

  cd "$REPO_DIR"

  local binary="${REPO_DIR}/target/release/pond-server"
  if [ "$MODE" = "dev" ]; then
    binary="${REPO_DIR}/target/debug/pond-server"
  fi

  if [ ! -f "$binary" ]; then
    # Fall back to cargo run
    binary=""
  fi

  local setup_cmd
  if [ -n "$binary" ]; then
    setup_cmd="$binary setup --model ${WHISPER_SIZE}"
  else
    local release_flag=""
    if [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ] || [ "$MODE" = "minimal" ]; then
      release_flag="--release"
    fi
    setup_cmd="cargo run -p pond-server ${release_flag} -- setup --model ${WHISPER_SIZE}"
  fi

  if [ "$NO_MODELS" = true ]; then
    log "Running setup with --no-models (DB + templates only)..."
    # pond-server setup always does DB + templates; model downloads are
    # conditional inside it but we still run it for the DB init.
    if eval "$setup_cmd" 2>&1 | grep -E '^\s*(v|!|>|#|\[)' || true; then
      S_SETUP="ok"
    fi
  else
    log "Running pond-server setup (DB, models, templates)..."
    if eval "$setup_cmd" 2>&1; then
      S_SETUP="ok"
    else
      warn "pond-server setup exited with errors -- check output above"
      S_SETUP="partial"
    fi
  fi

  # Verify results
  if [ -f "${DATA_DIR}/pond_system.db" ]; then
    S_SETUP="ok"
  else
    S_SETUP="${S_SETUP:-failed}"
  fi

  # Check Whisper
  local whisper_bin="${DATA_DIR}/bin/whisper-server"
  if [ -f "$whisper_bin" ]; then
    S_WHISPER="ok"
  elif [ "$NO_MODELS" = true ]; then
    S_WHISPER="skipped"
  else
    S_WHISPER="failed"
  fi

  # Check Piper
  local piper_bin="${DATA_DIR}/bin/piper"
  if [ -f "$piper_bin" ]; then
    S_PIPER="ok"
  elif [ "$NO_MODELS" = true ]; then
    S_PIPER="skipped"
  else
    S_PIPER="failed"
  fi
}

# ── LLM model download ──────────────────────────────────────────────────────
download_llm() {
  step "6" "LLM model"

  if [ "$NO_MODELS" = true ]; then
    log "Skipping LLM model (--no-models)"
    S_LLM="skipped"
    return
  fi

  # Auto-detect provider if not specified
  if [ -z "$LLM_PROVIDER" ]; then
    if command -v ollama &>/dev/null; then
      LLM_PROVIDER="ollama"
    elif [ "$MODE" = "production" ] || [ "$MODE" = "jetson" ]; then
      # On headless/production Linux, default to Ollama (will auto-install)
      LLM_PROVIDER="ollama"
    else
      LLM_PROVIDER="none"
    fi
  fi

  case "$LLM_PROVIDER" in
    ollama)    _download_llm_ollama ;;
    llamafile) _download_llm_llamafile ;;
    none|"")
      warn "No LLM model installed."
      log "Install Ollama (https://ollama.com) and run: ollama pull gemma3:4b"
      log "Or re-run: bash scripts/install.sh --ollama"
      S_LLM="not installed"
      ;;
  esac
}

# ── Ollama provider ──────────────────────────────────────────────────────────
_download_llm_ollama() {
  # Install Ollama if not present
  if ! command -v ollama &>/dev/null; then
    _install_ollama
    if ! command -v ollama &>/dev/null; then
      warn "Ollama installation failed -- install manually from https://ollama.com"
      S_LLM="failed"
      return
    fi
  fi

  success "Ollama found: $(ollama --version 2>/dev/null || echo 'installed')"

  # Ensure Ollama service is running
  _ensure_ollama_running

  # Pull default model
  log "Pulling gemma3:4b via Ollama (~2.6 GB)..."
  if ollama pull gemma3:4b 2>&1; then
    success "LLM model: gemma3:4b (Ollama)"
    S_LLM="ok (ollama)"
  else
    warn "Ollama pull failed -- you can pull manually: ollama pull gemma3:4b"
    S_LLM="failed"
  fi
}

_install_ollama() {
  case "$OS" in
    linux)
      log "Installing Ollama via official installer..."
      if curl -fsSL https://ollama.com/install.sh | sh 2>&1; then
        success "Ollama installed"
      else
        warn "Ollama auto-install failed"
      fi
      ;;
    macos)
      if command -v brew &>/dev/null; then
        echo -e "${BOLD}  ?  Install Ollama via Homebrew? [Y/n]${NC}"
        read -r -p "     " REPLY < /dev/tty
        if [ "${REPLY:-Y}" != "${REPLY#[Yy]}" ]; then
          log "Installing Ollama via Homebrew..."
          if brew install ollama 2>&1; then
            success "Ollama installed via Homebrew"
          else
            warn "Homebrew install failed -- download from https://ollama.com"
          fi
        else
          log "Skipping Ollama install"
        fi
      else
        warn "Install Ollama manually from https://ollama.com"
      fi
      ;;
    *)
      warn "Install Ollama manually from https://ollama.com"
      ;;
  esac
}

_ensure_ollama_running() {
  # Check if Ollama API is responsive
  if curl -sf "http://127.0.0.1:11434/api/version" > /dev/null 2>&1; then
    return
  fi

  # Try to start it
  if [ "$OS" = "linux" ] && command -v systemctl &>/dev/null; then
    sudo systemctl start ollama 2>/dev/null || true
    sleep 2
  elif [ "$OS" = "macos" ]; then
    # On macOS, Ollama runs as a launchd agent or standalone
    if command -v ollama &>/dev/null; then
      ollama serve &>/dev/null &
      sleep 3
    fi
  fi

  if ! curl -sf "http://127.0.0.1:11434/api/version" > /dev/null 2>&1; then
    warn "Ollama is not running -- start it manually before pulling models"
  fi
}

# ── Llamafile provider ───────────────────────────────────────────────────────
_download_llm_llamafile() {
  local llamafile_dir="${DATA_DIR}/bin"
  local llamafile_path="${llamafile_dir}/gemma-2-2b-it.llamafile"
  mkdir -p "$llamafile_dir"

  if [ -f "$llamafile_path" ]; then
    success "Llamafile already present"
    S_LLM="ok (llamafile)"
    return
  fi

  log "Downloading gemma-2-2b-it llamafile (~1.5 GB)..."
  if curl -L --progress-bar \
    "https://huggingface.co/Mozilla/gemma-2-2b-it-llamafile/resolve/main/gemma-2-2b-it.Q4_K_M.llamafile" \
    -o "$llamafile_path"; then
    chmod +x "$llamafile_path"
    success "LLM model: gemma-2-2b-it (llamafile)"
    S_LLM="ok (llamafile)"
  else
    warn "Llamafile download failed"
    S_LLM="failed"
  fi
}

# ── Piper TTS voice (standalone, in case pond-server setup missed it) ────────
download_piper_voice() {
  local voice_dir="${DATA_DIR}/models/tts"
  mkdir -p "$voice_dir"

  local voice_file="${voice_dir}/en_US-lessac-medium.onnx"
  local config_file="${voice_dir}/en_US-lessac-medium.onnx.json"

  if [ -f "$voice_file" ] && [ -f "$config_file" ]; then
    success "TTS voice already present: en_US-lessac-medium"
    return
  fi

  local base_url="https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/lessac/medium"
  log "Downloading en_US-lessac-medium voice model (~63 MB)..."
  curl -L --progress-bar "${base_url}/en_US-lessac-medium.onnx" -o "$voice_file" 2>&1 || true
  curl -sL "${base_url}/en_US-lessac-medium.onnx.json" -o "$config_file" 2>&1 || true

  if [ -f "$voice_file" ] && [ -s "$voice_file" ]; then
    success "TTS voice: en_US-lessac-medium"
  else
    warn "Voice download failed -- TTS will be text-only until manually installed"
  fi
}
