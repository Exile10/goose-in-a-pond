# GIAP MCP Server Design Standard

How to build MCP tool servers for Goose in a Pond. Every tool follows this standard — no exceptions.

---

## Design Principles

1. **Small models call the right tool but send wrong params.** Design for empty `{}` params. Always provide a fallback chain.
2. **Context is scarce.** Tool schemas eat tokens. Keep descriptions lean. Keep results small. Every char costs.
3. **One domain per server.** Each MCP server owns one concern. 2-7 tools max. Only the ports it needs.
4. **Fail gracefully, never panic.** Return helpful `CallToolResult::success` messages even on failure — the LLM reads them and adapts.

---

## Tool Description Rules

Descriptions are the LLM's only guide for tool selection. They must be:

**Lean** — under 200 chars. No boilerplate. Start with the verb.
```rust
// Good: 43 chars
#[tool(description = "Get the current weather for the configured location.")]

// Bad: 180 chars of padding
#[tool(description = "This tool allows you to retrieve the current weather conditions including temperature, humidity, wind speed, and precipitation for the user's configured location.")]
```

**Decisive** — tell the model WHEN to use it, not just what it does.
```rust
// Good: tells the model when
#[tool(description = "Look up a topic on Wikipedia. Use for ANY factual question about people, places, events, science, or history.")]

// Bad: just describes the mechanism  
#[tool(description = "Searches the Wikipedia API and returns article text.")]
```

**Exclusive** — tell the model what NOT to do after calling.
```rust
#[tool(description = "Get current weather. DO NOT guess weather data or use shell commands for weather.")]
```

---

## Parameter Design

### Minimize required params
Small models struggle with JSON param formatting. Make everything optional with sensible defaults.

```rust
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecallMemoriesParams {
    /// Optional keyword filter. If omitted, returns most recent.
    pub query: Option<String>,
    /// Max results (default 10).
    pub limit: Option<u32>,
}
```

### Use `#[serde(flatten)]` to absorb extras
Models send params in unpredictable shapes. Capture everything:

```rust
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WikipediaQueryParams {
    /// The topic to look up.
    pub topic: Option<String>,
    /// Catch-all for unexpected fields (query, title, search, etc.)
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}
```

### Param Resolution: ToolCaller Primary

When a ToolCaller specialist is configured (e.g. FunctionGemma 270M), it is the **primary** param generator for all tools that take structured params. The main LLM decides WHEN to call a tool; the ToolCaller decides WHAT params to send.

```rust
async fn resolve_params(params: &MyParams, tool_name: &str) -> String {
    // 1. ToolCaller specialist — PRIMARY when configured (~80ms, 270M model)
    const SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string"}}}"#;
    if let Some(args) = crate::generate_params(tool_name, SCHEMA).await {
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            if !q.trim().is_empty() { return q.trim().to_string(); }
        }
    }

    // 2. Model's params directly (capable models like Ollama don't need ToolCaller)
    if let Some(ref q) = params.query {
        if !q.trim().is_empty() { return q.trim().to_string(); }
    }

    // 3. Scan extras (models send params in unpredictable shapes)
    for key in &["query", "q", "input", "text", "search", "topic", "name"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                if !s.trim().is_empty() { return s.trim().to_string(); }
            }
        }
    }

    // 4. Clean user message (last resort)
    let msg = crate::last_user_message();
    if !msg.is_empty() { return clean_query(&msg); }
    String::new()
}
```

The shared `crate::generate_params(tool_name, schema)` helper (in `lib.rs`) handles ToolCaller lookup, user message retrieval, and error handling. Each tool just passes its name and JSON schema.

**Latency**: ToolCaller is a 270M GGUF model in a separate model slot — ~80ms inference, no swap with the main LLM, zero overhead on non-tool turns.

---

## Result Size Budget

Tool results go back into the model's context. Keep them tight.

| Result type | Max chars | Why |
|---|---|---|
| Short answer (time, weather) | 200 | Single fact |
| List (devices, schedules) | 1000 | Enumeration |
| Article (Wikipedia) | 4000 | ~1000 tokens, leaves room for response |
| Error/guidance | 200 | Tell the model what to do next |

Truncate long results with a note:
```rust
if text.len() > MAX_CHARS {
    let mut cut = MAX_CHARS;
    while cut > 0 && !text.is_char_boundary(cut) { cut -= 1; }
    format!("{}...\n\n[Truncated — full content at source]", &text[..cut])
}
```

---

## Server Structure

Every MCP server follows this exact pattern:

```rust
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};

#[derive(Clone)]
pub struct MyMcpServer {
    // Only the ports this server needs — no god-struct
    my_repo: Arc<dyn MyPort + Send + Sync>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MyMcpServer {
    pub fn new(my_repo: Arc<dyn MyPort + Send + Sync>) -> Self {
        Self { my_repo, tool_router: Self::tool_router() }
    }

    #[tool(description = "Lean, decisive description. Under 200 chars.")]
    async fn my_tool(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<MyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Extract with fallback chain
        let query = extract_query(&params.0);
        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a query to search. Please ask your question directly."
            )]));
        }
        // Execute
        match self.my_repo.do_thing(&query).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(result)])),
            Err(e) => {
                // Return error as content, not ErrorData — the LLM can adapt
                Ok(CallToolResult::success(vec![Content::text(
                    format!("Could not complete the request: {e}. Try rephrasing.")
                )]))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for MyMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("giap-my-domain", env!("CARGO_PKG_VERSION")))
            .with_instructions("One paragraph. What this server does and how to use it.")
    }
}
```

---

## Registration (OnceLock + Spawn)

Goose's `SpawnServerFn` is `fn(DuplexStream, DuplexStream)` — a plain function pointer. Dependencies go in a static `OnceLock`:

```rust
use std::sync::OnceLock;
use rmcp::ServiceExt;
use tokio::io::DuplexStream;

struct MyDeps {
    my_repo: Arc<dyn MyPort + Send + Sync>,
}
static MY_DEPS: OnceLock<MyDeps> = OnceLock::new();

pub fn init_my_deps(my_repo: Arc<dyn MyPort + Send + Sync>) {
    let _ = MY_DEPS.set(MyDeps { my_repo });
}

pub fn spawn_my_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = MY_DEPS.get().expect("init_my_deps() not called");
    let server = MyMcpServer::new(deps.my_repo.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => { let _ = running.waiting().await; }
            Err(e) => tracing::error!("giap-my-domain MCP server failed: {e}"),
        }
    });
}
```

Register in `giap_registration.rs`:
```rust
pond_mcp_server::init_my_deps(my_repo);
register_builtin_extension("giap-my-domain", pond_mcp_server::spawn_my_server);
```

---

## Error Handling

**Never return `ErrorData` for user-facing failures.** The LLM sees `ErrorData` as a system error and tells the user "a technical error occurred." Instead, return guidance as content:

```rust
// Bad: LLM says "I encountered a technical error"
Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, "API timeout", None))

// Good: LLM says "the weather service isn't configured, let me check settings"
Ok(CallToolResult::success(vec![Content::text(
    "Weather service is not configured. The user needs to set a location in Settings."
)]))
```

Reserve `ErrorData` for true protocol errors (invalid params, missing required fields).

---

## Testing

Every module gets `#[cfg(test)] mod tests` with stub implementations:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct StubRepo;
    #[async_trait::async_trait]
    impl MyPort for StubRepo {
        async fn do_thing(&self, _q: &str) -> anyhow::Result<String> {
            Ok("stub result".into())
        }
    }

    #[test]
    fn server_constructs() {
        let server = MyMcpServer::new(Arc::new(StubRepo));
        // Verifies the tool_router macro expansion works
        assert!(true);
    }
}
```

Live tests (requiring network/hardware) use `#[ignore]`:
```rust
#[tokio::test]
#[ignore] // requires internet
async fn live_wikipedia_lookup() { ... }
```

---

## Checklist for New MCP Servers

- [ ] Single domain, 2-7 tools
- [ ] Tool descriptions: lean (<200 chars), decisive (when to use), exclusive (what not to do)
- [ ] All text params: Optional + extras scan + user message fallback
- [ ] Results capped to budget (see table above)
- [ ] Errors returned as content, not ErrorData
- [ ] OnceLock deps + spawn function
- [ ] Registered in `giap_registration.rs`
- [ ] Added to `GIAP_EXTENSIONS` constant
- [ ] Unit tests with stubs
- [ ] `cargo build -p pond-mcp-server` passes
- [ ] `cargo test -p pond-mcp-server` passes
