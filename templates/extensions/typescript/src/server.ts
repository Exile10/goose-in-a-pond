#!/usr/bin/env node
/**
 * Example GIAP MCP Extension -- TypeScript
 *
 * A minimal MCP server providing example tools.
 * Run with: npx tsx src/server.ts
 * Register in GIAP: POST /api/v1/extensions
 *   {"name": "example-ts", "kind": "stdio", "command": "npx", "args": ["tsx", "src/server.ts"]}
 */
import * as readline from "readline";

interface JsonRpcRequest {
  jsonrpc: string;
  id?: number | string;
  method: string;
  params?: Record<string, unknown>;
}

function handleRequest(
  request: JsonRpcRequest,
): Record<string, unknown> | null {
  const { method, id, params } = request;

  switch (method) {
    case "initialize":
      return {
        jsonrpc: "2.0",
        id,
        result: {
          protocolVersion: "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "example-ts", version: "0.1.0" },
        },
      };

    case "notifications/initialized":
      return null;

    case "tools/list":
      return {
        jsonrpc: "2.0",
        id,
        result: {
          tools: [
            {
              name: "greet",
              description: "Generate a friendly greeting for someone.",
              inputSchema: {
                type: "object",
                properties: {
                  name: { type: "string", description: "Name to greet" },
                },
                required: ["name"],
              },
            },
            {
              name: "timestamp",
              description: "Get the current server timestamp.",
              inputSchema: { type: "object", properties: {} },
            },
          ],
        },
      };

    case "tools/call": {
      const toolName = (params as Record<string, unknown>)?.name as string;
      const args =
        ((params as Record<string, unknown>)?.arguments as Record<
          string,
          unknown
        >) ?? {};

      if (toolName === "greet") {
        const person = (args.name as string) || "World";
        return {
          jsonrpc: "2.0",
          id,
          result: {
            content: [
              {
                type: "text",
                text: `Hello, ${person}! Welcome to GIAP.`,
              },
            ],
          },
        };
      } else if (toolName === "timestamp") {
        return {
          jsonrpc: "2.0",
          id,
          result: {
            content: [{ type: "text", text: new Date().toISOString() }],
          },
        };
      }
      return {
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: `Unknown tool: ${toolName}` },
      };
    }

    default:
      return {
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: `Method not found: ${method}` },
      };
  }
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line: string) => {
  try {
    const request = JSON.parse(line) as JsonRpcRequest;
    const response = handleRequest(request);
    if (response) {
      process.stdout.write(JSON.stringify(response) + "\n");
    }
  } catch {
    // Ignore malformed JSON
  }
});
