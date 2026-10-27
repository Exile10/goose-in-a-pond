# pond-server

The binary and composition root. `src/main.rs` parses the CLI, opens the databases, builds every
adapter, wires them into `pond-core` services and the `pond-api` `AppState`, and serves.

## Commands

`setup`, `serve`, `chat`, `status`, `onboard`, `models`, `agent`, `prompts`, `skills`, `recipes`,
`memories`, `calibrate`, `pairing`, `setup-code`, `dashboard`. Every flag is in
[docs/cli.md](../../docs/cli.md).

`serve` opens HTTP on `127.0.0.1` (default 4000) and HTTPS on all interfaces (default 4443), each
falling back through the next nine ports. Read the bound ports from `<data_dir>/.runtime_api_port`
and `.runtime_https_port`. It stops on Ctrl-C or SIGTERM.

`chat --voice --json-events` is the process the desktop app spawns for voice mode; stdout then
carries only NDJSON events.

## Features

| Feature | Default | Adds |
|---|---|---|
| `goose-agent` | yes | The live engine, `pond-adapters-goose` (compiles the Goose submodule) |
| `local-inference` | yes | In-process GGUF via Goose's llama.cpp provider |
| `mistralrs-agent` | no | `--agent mistralrs`, a direct path to a mistral.rs server |
| `pond-agent` | no | The quarantined independent loop; cannot be activated |
| `mcp-memory`, `face-onnx`, `vision-onnx`, `mesh`, `lightning` | no | The adapters of the same names |

GPU acceleration is selected on the adapter crates, not here: `pond-adapters-local-inference/cuda`
and `pond-adapters-whisper/cuda` (or `/metal`). `giap.sh build` picks them for the host.

## Tests

`tests/` covers the CLI surface, listener wiring, the `--json-events` contract and an end-to-end
pipeline. Wiring and startup ordering are only exercised by `scripts/live-test.sh`, which runs the
real binary over HTTP; run it for any change to a migration, route, handler or startup code.

```bash
cargo test -p pond-server
scripts/live-test.sh
```
