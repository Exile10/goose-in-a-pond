"""
Example GIAP MCP Extension -- Python

A minimal MCP server that provides a greeting tool.
Run with: python server.py
Register in GIAP: POST /api/v1/extensions
  {"name": "example-python", "kind": "stdio", "command": "python3", "args": ["server.py"]}
"""
import json
import sys
from datetime import datetime


def handle_request(request: dict):
    """Handle a single JSON-RPC request."""
    method = request.get("method", "")
    req_id = request.get("id")

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "example-python", "version": "0.1.0"},
            },
        }
    elif method == "notifications/initialized":
        return None  # notification, no response
    elif method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "tools": [
                    {
                        "name": "greet",
                        "description": "Generate a friendly greeting for someone.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "description": "The name of the person to greet",
                                }
                            },
                            "required": ["name"],
                        },
                    },
                    {
                        "name": "timestamp",
                        "description": "Get the current server timestamp.",
                        "inputSchema": {"type": "object", "properties": {}},
                    },
                ]
            },
        }
    elif method == "tools/call":
        tool_name = request.get("params", {}).get("name", "")
        arguments = request.get("params", {}).get("arguments", {})

        if tool_name == "greet":
            person = arguments.get("name", "World")
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {"type": "text", "text": f"Hello, {person}! Welcome to GIAP."}
                    ]
                },
            }
        elif tool_name == "timestamp":
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [{"type": "text", "text": datetime.now().isoformat()}]
                },
            }
        else:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"},
            }
    else:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": -32601, "message": f"Method not found: {method}"},
        }


def main():
    """Run the MCP server over stdio (line-delimited JSON-RPC)."""
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            response = handle_request(request)
            if response is not None:
                sys.stdout.write(json.dumps(response) + "\n")
                sys.stdout.flush()
        except json.JSONDecodeError:
            continue


if __name__ == "__main__":
    main()
