# GIAP Extension Template -- Python

A minimal MCP (Model Context Protocol) server that provides example tools for GIAP.

## Quick Start

1. Run the server:
   ```bash
   python server.py
   ```

2. Register in GIAP:
   ```bash
   curl -X POST http://localhost:4000/api/v1/extensions \
     -H "Content-Type: application/json" \
     -d '{"name": "example-python", "kind": "stdio", "command": "python", "args": ["server.py"]}'
   ```

Or add via the GIAP desktop app: Extensions > Add Extension.

## Provided Tools

- `greet` -- Generate a friendly greeting
- `timestamp` -- Get the current server timestamp

## How It Works

This server implements the MCP protocol over stdio (stdin/stdout). GIAP starts it as a child process and communicates via JSON-RPC messages.

Key protocol methods:
- `initialize` -- Handshake, declares capabilities
- `tools/list` -- Returns available tool schemas
- `tools/call` -- Executes a tool with arguments

## Adding Your Own Tools

1. Add a tool definition to the `tools/list` response
2. Add a handler case to the `tools/call` method
3. Restart the extension in GIAP

See the [GIAP Extensions Guide](../../../docs/developer/extensions.md) for full documentation.
