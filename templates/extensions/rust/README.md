# GIAP Extension Template: Rust

A minimal MCP server over stdio using only `serde` and `serde_json`: no async runtime, no MCP SDK.
`src/main.rs` reads one JSON-RPC message per line from stdin and writes one per line to stdout. The
empty `[workspace]` table in `Cargo.toml` keeps it out of the GIAP workspace, so it builds on its
own from this folder. For a larger server, the `rmcp` crate (what GIAP's builtins use) handles the
protocol.

## Tools

- `greet`: a greeting for `name`
- `timestamp`: the current Unix time in seconds

## Try it

```bash
cargo build --release
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | ./target/release/giap-extension-example
```

## Add it to GIAP

In the desktop app: Extensions > Add Extension, or Settings > Extensions > Add server in the hub.
Over the API (see [Building Extensions](../../../docs/developer/extensions.md#over-the-api) for the
port and authentication):

```bash
curl -X POST "$API/extensions" -H "Content-Type: application/json" \
  -d '{"name": "example-rust", "kind": "stdio", "command": "/absolute/path/to/target/release/giap-extension-example"}'
```

Use the binary's absolute path: a relative one resolves against the server's working directory,
not this folder. The tools then appear as `example-rust__greet` and `example-rust__timestamp`.

## Adding a tool

1. Add its definition to the `tools/list` arm in `src/main.rs`.
2. Add an arm for it under `tools/call`.
3. Rebuild, then restart the extension: switch it off and on in the app, or restart the server.

Log to stderr; stdout carries only protocol messages.
