# GIAP Extension Template -- Rust

A minimal MCP (Model Context Protocol) server that provides example tools for GIAP.

## Quick Start

1. Build the extension:
   ```bash
   cargo build --release
   ```

2. Run the server:
   ```bash
   cargo run
   ```

3. Register in GIAP:
   ```bash
   curl -X POST http://localhost:4000/api/v1/extensions \
     -H "Content-Type: application/json" \
     -d '{"name": "example-rust", "kind": "stdio", "command": "./target/release/giap-extension-example"}'
   ```

Or add via the GIAP desktop app: Extensions > Add Extension.

## Provided Tools

- `greet` -- Generate a friendly greeting
- `timestamp` -- Get the current server timestamp (Unix epoch seconds)

## How It Works

This server implements the MCP protocol over stdio (stdin/stdout). GIAP starts it as a child process and communicates via JSON-RPC messages.

Key protocol methods:
- `initialize` -- Handshake, declares capabilities
- `tools/list` -- Returns available tool schemas
- `tools/call` -- Executes a tool with arguments

## Adding Your Own Tools

1. Add a tool definition to the `tools/list` match arm in `src/main.rs`
2. Add a handler case in the `tools/call` match arm
3. Rebuild and restart the extension in GIAP

## Dependencies

This template uses only `serde` and `serde_json` -- no async runtime, no framework. For extensions that need HTTP clients or async I/O, add `tokio` and `reqwest` as needed.

See the [GIAP Extensions Guide](../../../docs/developer/extensions.md) for full documentation.
