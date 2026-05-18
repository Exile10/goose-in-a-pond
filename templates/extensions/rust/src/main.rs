//! Example GIAP MCP Extension -- Rust
//!
//! A minimal MCP server providing example tools.
//! Run with: cargo run
//! Register in GIAP: POST /api/v1/extensions
//!   {"name": "example-rust", "kind": "stdio", "command": "./target/release/giap-extension-example"}

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

fn handle_request(request: JsonRpcRequest) -> Option<JsonRpcResponse> {
    match request.method.as_str() {
        "initialize" => Some(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "example-rust", "version": "0.1.0"}
            })),
            error: None,
        }),

        "notifications/initialized" => None,

        "tools/list" => Some(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: Some(json!({
                "tools": [
                    {
                        "name": "greet",
                        "description": "Generate a friendly greeting for someone.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "description": "Name to greet"
                                }
                            },
                            "required": ["name"]
                        }
                    },
                    {
                        "name": "timestamp",
                        "description": "Get the current server timestamp.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    }
                ]
            })),
            error: None,
        }),

        "tools/call" => {
            let params = request.params.unwrap_or_default();
            let tool_name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];

            let result = match tool_name {
                "greet" => {
                    let person = args["name"].as_str().unwrap_or("World");
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Hello, {}! Welcome to GIAP.", person)
                        }]
                    })
                }
                "timestamp" => {
                    let secs = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("{}", secs)
                        }]
                    })
                }
                _ => {
                    return Some(JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: request.id,
                        result: None,
                        error: Some(json!({
                            "code": -32601,
                            "message": format!("Unknown tool: {}", tool_name)
                        })),
                    });
                }
            };

            Some(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: Some(result),
                error: None,
            })
        }

        _ => Some(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: None,
            error: Some(json!({
                "code": -32601,
                "message": format!("Method not found: {}", request.method)
            })),
        }),
    }
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Some(response) = handle_request(request) {
            let json = serde_json::to_string(&response).expect("failed to serialize response");
            writeln!(stdout, "{}", json).expect("failed to write to stdout");
            stdout.flush().expect("failed to flush stdout");
        }
    }
}
