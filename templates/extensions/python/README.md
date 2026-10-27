# GIAP Extension Template: Python

A minimal MCP server over stdio, standard library only (Python 3.10+). `server.py` reads one
JSON-RPC message per line from stdin and writes one per line to stdout.

## Tools

- `greet`: a greeting for `name`
- `timestamp`: the current local time, ISO 8601

## Try it

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"greet","arguments":{"name":"Jerry"}}}' \
  | python3 server.py
```

## Add it to GIAP

In the desktop app: Extensions > Add Extension, or Settings > Extensions > Add server in the hub.
Over the API (see [Building Extensions](../../../docs/developer/extensions.md#over-the-api) for the
port and authentication):

```bash
curl -X POST "$API/extensions" -H "Content-Type: application/json" \
  -d '{"name": "example-python", "kind": "stdio", "command": "python3", "args": ["/absolute/path/to/server.py"]}'
```

Use an absolute path: a relative one resolves against the server's working directory, not this
folder. The tools then appear as `example-python__greet` and `example-python__timestamp`.

## Adding a tool

1. Add its definition to the `tools/list` branch of `handle_request`.
2. Add a branch for it under `tools/call`.
3. Restart the extension: switch it off and on in the app, or restart the server.

Log to stderr; stdout carries only protocol messages.
