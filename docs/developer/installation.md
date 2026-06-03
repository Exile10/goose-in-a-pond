# Installation Guide

## Quick Start

```bash
git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
cd goose-in-a-pond
bash scripts/install.sh
```

The install script handles everything: submodule init, dependencies, build, database setup, model downloads, and verification.

## Install Script Options

```
bash scripts/install.sh [OPTIONS]

  --full            Build entire workspace including Goose (10+ min first time)
  --fast            Build only server crates (default, ~2 min)
  --desktop         Also install desktop app (npm install in pond-desktop)
  --no-models       Skip all model downloads
  --no-verify       Skip post-install health check
  --ollama          Pull LLM via Ollama (auto-detected if installed)
  --llamafile       Download llamafile binary with embedded model
  --whisper-model MODEL  Whisper size: tiny|base|small (default: base)
  --data-dir DIR    Override data directory
```

## Prerequisites

| Requirement | macOS | Linux |
|------------|-------|-------|
| Git | Xcode CLI tools | `apt install git` |
| Rust | [rustup.rs](https://rustup.rs) | [rustup.rs](https://rustup.rs) |
| C compiler | `xcode-select --install` | `apt install build-essential` |
| cmake | `brew install cmake` | `apt install cmake` |
| pkg-config | `brew install pkg-config` | `apt install pkg-config` |
| Node.js (desktop only) | [nodejs.org](https://nodejs.org) | `apt install nodejs npm` |

The install script auto-installs cmake and pkg-config via Homebrew (macOS) or apt (Linux).

## What Gets Installed

### Data Directory

| Platform | Default Path |
|----------|-------------|
| macOS | `~/Library/Application Support/goose-in-a-pond/` |
| Linux | `~/.local/share/goose-in-a-pond/` |
| Custom | Set `POND_DATA_DIR=/path` or `--data-dir` |

### Directory Structure

```
$DATA_DIR/
├── pond_system.db              SQLite (sessions, settings, memory, skills)
├── pond_logs.db                SQLite (telemetry, sensor readings)
├── schedules.json              Scheduled task definitions
├── schedule_runs.json          Execution history
├── bin/
│   ├── whisper-server          Whisper ASR binary
│   ├── piper                   Piper TTS binary
│   └── espeak-ng-data/         TTS voice data
├── models/
│   ├── ggml-base.en.bin        Whisper model (~74 MB)
│   ├── tts/
│   │   ├── en_US-lessac-medium.onnx       TTS voice (~63 MB)
│   │   └── en_US-lessac-medium.onnx.json  TTS config
│   ├── gguf/                   GGUF models (local inference)
│   └── face/                   Face recognition ONNX models
└── prompts/                    User prompt overrides
```

## LLM Provider Options

GIAP supports three LLM backends. You need at least one:

### Ollama (recommended for development)

```bash
# Install: https://ollama.com
ollama pull gemma3:4b    # 2.6 GB — works great with GIAP
# Or for Gemma 4:
ollama pull gemma4:latest
```

Configure in GIAP: Settings → Chat Provider = "ollama", Model = "gemma3:4b"

### Llamafile (self-contained, no dependencies)

```bash
# The install script downloads this with --llamafile:
bash scripts/install.sh --llamafile
```

### Local GGUF (in-process, no subprocess)

```bash
# Requires --features local-inference (default)
# Place .gguf file in $DATA_DIR/models/gguf/
# Configure: Settings → Chat Provider = "local", Model = "filename.gguf"
```

## Running

```bash
# Start server + web dashboard
cargo run -p pond-server --release -- serve --open

# Voice mode (requires whisper-server running on port 9000)
cargo run -p pond-server --release -- chat --input whisper

# Desktop app
cd pond-desktop && npm run tauri dev
```

## Production Deployment (Linux)

For headless deployment on Jetson or Linux servers:

```bash
# All-in-one: build, configure systemd, setup mDNS
bash scripts/setup.sh --dedicated --port 80
```

See `scripts/setup.sh --help` for options.

## Cross-Compilation (Jetson Orin Nano)

```bash
# Requires: cargo install cross, Docker running
SQLX_OFFLINE=true make server

# Deploy to Jetson
make deploy JETSON_HOST=jetson@192.168.1.100
```

## Troubleshooting

### "goose/Cargo.toml not found"
```bash
git submodule update --init --recursive
```

### "Unable to find libclang" (Jetson)
```bash
sudo apt install libclang-dev clang
```

### "instruction requires: fullfp16" (Jetson ARM)
```bash
# Add to .cargo/config.toml:
[target.aarch64-unknown-linux-gnu]
rustflags = ["-C", "target-cpu=native"]
```

### Whisper build fails (macOS)
```bash
brew install cmake
# Re-run setup:
cargo run -p pond-server -- setup
```

### Port 4000 already in use
```bash
cargo run -p pond-server --release -- serve --port 8080
```
