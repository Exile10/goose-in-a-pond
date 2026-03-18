# Goose in a Pond 🦢🏡

**Goose in a Pond** is a privacy-first smart home assistant powered by the [Goose](https://github.com/block/goose) agent framework. It runs entirely locally, ensuring your data never leaves your environment.

---

## 🏛️ Architecture

We use a **Hexagonal (Ports & Adapters)** architecture to keep our core logic ("The Brain") independent from external frameworks and technologies.

- **[Architecture Overview](./docs/architecture/overview.md)**: Deep dive into our design philosophy.
- **[Visual Architecture & Example Flow](./docs/architecture/visual_workflow.md)**: Flowcharts and Sequence diagrams (Weather Example).
- **[Component Breakdown](./docs/architecture/components.md)**: Purpose of each crate in the `crates/` directory.
- **[Data Flow & Lifecycle](./docs/architecture/data_flow.md)**: How requests travel through the system.

---

## 📚 Development & Documentation

### Guides
- **[Developer Tutorial: Clean Code & Dependencies](./docs/developer/clean_code_and_dependencies.md)**: Maintaining modularity.
- **[End-to-End Workflow Example: Weather Feature](./docs/developer/example_workflow_weather.md)**: A complete implementation walkthrough.
- **[Pond Builder Manual](./docs/developer/pond_builder.md)**: Using our custom `pond-builder` CLI.
- **[TDD Guide](./docs/testing/tdd_guide.md)**: Writing unit and integration tests.
- **[Contributing Guide](./docs/CONTRIBUTING.md)**: Standard workflow for contributions.

---

## 🛠️ Tech Stack
- **Core**: Rust
- **Agent Framework**: [Goose](https://github.com/block/goose)
- **Database**: SQLite (via SQLx)
- **API**: Axum

---

## 🚀 Getting Started

1. **Clone & Submodules**:
   ```bash
   git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
   ```

2. **Build** (fast — skips Goose compilation):
   ```bash
   cargo build -p pond-core -p pond-infra -p pond-api -p pond-server
   ```

3. **First-time setup** — initializes the database and downloads the Whisper ASR model (~141 MB):
   ```bash
   cargo run -p pond-server -- setup
   # Optional: choose model size
   cargo run -p pond-server -- setup --model tiny    # ~39 MB
   cargo run -p pond-server -- setup --model small   # ~244 MB
   ```

4. **Run** the HTTP server (dashboard + REST API):
   ```bash
   cargo run -p pond-server -- serve [--port 4000] [--open]
   ```

5. **Chat** — interactive voice/text loop:
   ```bash
   # Text mode (default)
   cargo run -p pond-server -- chat

   # With a local LLM (llamafile must be running on port 8080)
   cargo run -p pond-server -- chat --provider llamafile

   # Microphone mode (whisper.cpp server must be running on port 9000)
   cargo run -p pond-server -- chat --input whisper
   cargo run -p pond-server -- chat --input whisper --provider llamafile
   ```

---

## 🎤 Voice Input (Whisper ASR)

Goose in a Pond uses [whisper.cpp](https://github.com/ggerganov/whisper.cpp) for speech-to-text.

1. **Download a model** (done automatically by `pond-server setup`).

2. **Build and run the whisper.cpp server**:

   **Linux / macOS**
   ```bash
   git clone https://github.com/ggerganov/whisper.cpp
   cd whisper.cpp && make server
   ./server -m /path/to/ggml-base.en.bin --port 9000
   ```

   **Windows** — build with CMake (requires Visual Studio or MinGW) or use [WSL](https://learn.microsoft.com/en-us/windows/wsl/):
   ```powershell
   git clone https://github.com/ggerganov/whisper.cpp
   cd whisper.cpp
   cmake -B build
   cmake --build build --config Release --target server
   .\build\bin\Release\server.exe -m C:\path\to\ggml-base.en.bin --port 9000
   ```

   > The model path for `pond-server setup` defaults to:
   > - **Linux/macOS**: `~/.local/share/goose-in-a-pond/models/ggml-base.en.bin`
   > - **Windows**: `%APPDATA%\goose-in-a-pond\models\ggml-base.en.bin`

3. **Start the assistant in voice mode**:
   ```bash
   cargo run -p pond-server -- chat --input whisper
   ```

The adapter posts audio to `POST /inference` (whisper.cpp's native HTTP API) — no native C++ bindings, no long compile times.

---

## 🧪 Testing

```bash
# All tests
cargo test

# Single crate (fast iteration)
cargo test -p pond-core
cargo test -p pond-adapters-whisper

# Run the live whisper integration test (requires whisper.cpp server on port 9000)
cargo test -p pond-adapters-whisper -- --ignored live_transcription_of_jfk_wav
```

Test fixtures live in `tests/blobs/` — `jfk.wav` is the canonical whisper.cpp sample (JFK's 1961 inaugural address, public domain, 16-bit mono 16 kHz).

