# GIAP Extension Template: TypeScript

A minimal MCP server over stdio, run by `tsx` with no build step. `src/server.ts` reads one JSON-RPC
message per line from stdin (`readline`) and writes one per line to stdout.

## Tools

- `greet`: a greeting for `name`
- `timestamp`: the current time, ISO 8601 in UTC

## Try it

```bash
npm install
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | npx tsx src/server.ts
```

## Add it to GIAP

In the desktop app: Extensions > Add Extension, or Settings > Extensions > Add server in the hub.
Over the API (see [Building Extensions](../../../docs/developer/extensions.md#over-the-api) for the
port and authentication):

```bash
curl -X POST "$API/extensions" -H "Content-Type: application/json" \
  -d '{"name": "example-ts", "kind": "stdio", "command": "npx", "args": ["tsx", "/absolute/path/to/src/server.ts"]}'
```

Use an absolute path: a relative one resolves against the server's working directory, not this
folder. `npx` must be on the server's `PATH`. The tools then appear as `example-ts__greet` and
`example-ts__timestamp`.

## Adding a tool

1. Add its definition to the `tools/list` case in `src/server.ts`.
2. Add a branch for it in the `tools/call` case.
3. Restart the extension: switch it off and on in the app, or restart the server.

Log to stderr; stdout carries only protocol messages.
