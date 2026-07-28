# Installation Guide

## Quick Start

```bash
git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
cd goose-in-a-pond
bash scripts/giap.sh          # menu: install, build, service, logs, doctor
```

`scripts/giap.sh` is the primary interface. It detects the host (Jetson /
generic Linux / macOS), whether CUDA is usable, and the known bad states, then
offers only the actions that apply. Non-interactive equivalents:

```bash
bash scripts/giap.sh install     # first-time install (-y to skip confirmations)
bash scripts/giap.sh build       # web UI + pond-server with this host's features
bash scripts/giap.sh doctor      # health report; exits 1 on any FAIL
```

Always run `doctor` after installing.

### What `giap.sh install` does

It delegates the work to `scripts/install.sh` — which remains the only place
that knows the full sequence — and adds the guardrails install.sh cannot have,
because they depend on host state it does not inspect:

- **Refuses to add a second service unit.** `install.sh` writes a root-owned
  unit to `/etc/systemd/system/`; a Jetson set up by hand runs a *user* unit of
  the same name. Both installed means two servers, each loading its own ~3 GB
  model into one memory pool. `giap.sh` passes `--no-service` when a unit exists
  at either scope.
- **Warns when Node is too old to build the web UI**, since the server would
  then embed the `build.rs` placeholder and nothing at runtime can tell you.
- **Stops a running service before a Jetson release build**, because the linker
  is OOM-killed while a model is resident.

## Install Script Options

`scripts/install.sh` is what `giap.sh install` calls underneath. Its real flags
(from `scripts/lib/install-common.sh`):

```
bash scripts/install.sh [OPTIONS]

Modes (auto-detected if omitted):
  --production      Linux: systemd service + mDNS + auto-start
  --jetson          Jetson Orin Nano (CUDA auto-detect, SQLX_OFFLINE)
  --minimal         Fastest: server + DB only, no model downloads

Build:
  --full            Build the entire workspace including Goose (10+ min)
  --desktop         Also build the Tauri desktop app
  --no-verify       Skip the post-install health check

Models:
  --ollama          Use Ollama as the LLM provider
  --llamafile       Use llamafile as the LLM provider
  --no-models       Skip all model downloads
  --whisper-model MODEL   tiny|base|small (default: base)

Production:
  --port PORT       Override the port
  --dedicated       Set hostname to 'pond', serve on port 80
  --shared          Keep hostname, use a non-privileged port
  --no-service      Skip systemd service creation
  --data-dir DIR    Override the data directory
```

There is no `--fast` flag; it was documented here but never parsed.

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

For headless deployment on a Linux server:

```bash
bash scripts/giap.sh install          # auto-detects production mode
# or directly:
bash scripts/install.sh --production --dedicated --port 80
```

`scripts/setup.sh` no longer exists — `install.sh` replaced both it and the old
dev installer.

Service control afterwards is `bash scripts/giap.sh` menu 20-25 (install, start,
stop, restart, status, logs, uninstall).

## Jetson Orin Nano

Cross-compilation is **not** viable for the local-inference path: nvcc must
target the device architecture, so the CUDA build has to happen on the Jetson.
Deploy from your dev machine instead — it builds the web UI locally (the
device's Node is too old for Vite), syncs it, then builds on-device with the
correct feature flags:

```bash
bash scripts/giap.sh deploy           # or: bash scripts/jetson.sh deploy
```

For a CPU-only aarch64 binary without CUDA, `bash scripts/jetson.sh docker-build`
builds in a linux/arm64 container.

See [scripts/jetson/README.md](../../scripts/jetson/README.md) for the full
workflow and [docs/jetson-build-and-run.txt](../jetson-build-and-run.txt) for
the operational traps.

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
