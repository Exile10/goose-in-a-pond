# MCP Specification Compliance

This document tracks GIAP's compliance with the [Model Context Protocol (MCP)](https://spec.modelcontextprotocol.io/) specification, including protocol versions, transport support, primitive coverage, and authorization model.

---

## Protocol Version

| Component | Version | Notes |
|-----------|---------|-------|
| **rmcp crate** | `1.2.0` | Pinned in both GIAP workspace `Cargo.toml` (`=1.2.0`) and Goose workspace `Cargo.toml` (`1.2.0`). |
| **MCP client (Goose)** | `2025-03-26` | Goose's `McpClient` advertises `ProtocolVersion::V_2025_03_26` during `initialize`. See `goose/crates/goose/src/agents/mcp_client.rs:372`. |
| **MCP server (GIAP)** | `2024-11-05` | `GiapMcpServer` advertises `ProtocolVersion::V_2024_11_05` in its `get_info()`. See `crates/pond-mcp-server/src/giap_server.rs:1724`. |
| **MCP spec latest** | `2025-03-26` | [spec.modelcontextprotocol.io](https://spec.modelcontextprotocol.io/) |

The GIAP MCP server advertises `2024-11-05` because it only exposes tools (no resources or prompts directly) and does not require features introduced in the `2025-03-26` revision. The Goose MCP client, which connects to external MCP servers, uses the latest `2025-03-26` protocol version.

---

## Transport Support

| Transport | Status | Implementation | Notes |
|-----------|--------|----------------|-------|
| **stdio** | Supported | `ExtensionConfig::Stdio` | Primary transport for local extensions. Subprocess spawned with stdin/stdout. Env var sandboxing applied. |
| **Streamable HTTP** | Supported | `ExtensionConfig::StreamableHttp` | Used for remote MCP servers. Custom headers supported. OAuth PKCE flow available for servers that require authentication. |
| **SSE (deprecated)** | Config-only | `ExtensionConfig::Sse` | Kept in `ExtensionConfig` enum for config file backward compatibility. Not actively used for new connections. Comment in source: "SSE transport is no longer supported - kept only for config file compatibility." |

Additional extension types defined in Goose (not standard MCP transports):
- **builtin** -- in-process MCP server (GIAP's native tools run this way)
- **platform** -- extensions with direct access to the agent process
- **frontend** -- tools provided by and executed through the frontend UI
- **inline_python** -- Python code executed via `uvx`

---

## MCP Primitives

| Primitive | Client Support | Server Support | Details |
|-----------|---------------|----------------|---------|
| **Tools** | Full | Full | Goose client supports `list_tools` and `call_tool` with pagination (`next_cursor`). GIAP MCP server exposes 24 built-in tools via `rmcp` `#[tool]` macro. |
| **Resources** | Passthrough | Not exposed | Goose client implements `list_resources` and `read_resource` for external MCP servers. GIAP's own MCP server does not expose resources (`ServerCapabilities::builder().enable_tools().build()` -- tools only). |
| **Prompts** | Passthrough | Not exposed | Goose client implements `list_prompts` and `get_prompt` for external MCP servers. GIAP's own MCP server does not expose prompts. |
| **Sampling** | Supported | N/A | Goose handles `CreateMessageRequest` from MCP servers via its `ClientHandler` implementation, allowing servers to request LLM completions through the client. |

### Built-in Tools (24)

Defined in `crates/pond-mcp-server/src/giap_server.rs`:

| Category | Count | Tools |
|----------|-------|-------|
| Schedule | 7 | `list_schedules`, `create_schedule`, `delete_schedule`, `pause_schedule`, `resume_schedule`, `run_schedule_now`, `get_schedule_runs` |
| Memory | 3 | `save_memory`, `recall_memories`, `forget_memory` |
| Knowledge | 2 | `search_wikipedia`, `get_wikipedia_article` |
| System | 6 | `get_current_time`, `get_system_info`, `send_notification`, `run_shell_command`, `read_file`, `write_file` |
| Profile | 2 | `get_user_profile`, `get_model_assignments` |
| Other | 4 | `get_current_weather`, `list_registered_devices`, `list_skills`, `get_recipe` |

---

## Authorization Model

### stdio Transport

Environment variables are passed to the subprocess via the `envs` field in `ExtensionConfig::Stdio`. This is compliant with the MCP specification's recommendation for stdio-based credential passing.

Env var keys can also be referenced by name via `env_keys`, which resolves values from the host environment at connection time.

### Streamable HTTP Transport

Two authorization mechanisms are available:

1. **Custom headers** -- Static headers passed in the `headers` field of `ExtensionConfig::StreamableHttp`. Typically used for API keys (e.g., `Authorization: Bearer <token>`).

2. **OAuth 2.0 with PKCE** -- Implemented in Goose (`goose/crates/goose/src/oauth/`). When an MCP server returns an auth error, the `create_streamable_http_client` function in `extension_manager.rs` automatically triggers the OAuth flow:
   - Uses rmcp's `OAuthState` and `AuthorizationManager` from `rmcp::transport::auth`
   - Spins up a local callback server on a random port (`/oauth_callback`)
   - Opens the browser for user authorization
   - Exchanges the authorization code for tokens
   - Credentials stored via `GooseCredentialStore` which uses Goose's config system (backed by the OS keychain via `keyring` crate v3.6.2)
   - `AuthClient` wraps reqwest with automatic token refresh

### Credential Storage

OAuth credentials are persisted through `GooseCredentialStore` (implements rmcp's `CredentialStore` trait) which delegates to Goose's `Config::get_secret`/`set_secret` system. On macOS this uses the native Keychain; on Linux, the Secret Service API; on Windows, the Windows Credential Manager. All via the `keyring` crate (v3.6.2 with platform-specific feature flags).

---

## Environment Variable Sandboxing

Goose blocks 31 sensitive environment variables from being overridden by MCP extension configurations. Defined in `Envs::DISALLOWED_KEYS` in `goose/crates/goose/src/agents/extension.rs`.

Blocked categories:
- **Binary path manipulation** (4): `PATH`, `PATHEXT`, `SystemRoot`, `windir`
- **Dynamic linker hijacking -- Linux** (6): `LD_LIBRARY_PATH`, `LD_PRELOAD`, `LD_AUDIT`, `LD_DEBUG`, `LD_BIND_NOW`, `LD_ASSUME_KERNEL`
- **Dynamic linker hijacking -- macOS** (3): `DYLD_LIBRARY_PATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_FRAMEWORK_PATH`
- **Language runtime hijacking** (8): `PYTHONPATH`, `PYTHONHOME`, `NODE_OPTIONS`, `RUBYOPT`, `GEM_PATH`, `GEM_HOME`, `CLASSPATH`, `GO111MODULE`, `GOROOT`
- **Windows process/DLL hijacking** (7): `APPINIT_DLLS`, `SESSIONNAME`, `ComSpec`, `TEMP`, `TMP`, `LOCALAPPDATA`, `USERPROFILE`
- **Windows home directory** (2): `HOMEDRIVE`, `HOMEPATH`

If an extension config includes any of these keys, they are silently skipped with a warning log.

---

## Extension Registration

Extensions can be registered through two paths:

1. **REST API** -- `POST /api/v1/extensions` accepts an `ExtensionConfig` payload (stdio or streamable_http). The adapter validates the command/URI before connecting, persists the config to SQLite (`mcp_servers` table), and syncs the tool registry. Enabled extensions auto-reconnect on server restart.

2. **Marketplace registry** -- `BundledMarketplace` service in `crates/pond-core/src/services/marketplace.rs` parses `marketplace_registry.json` (embedded at compile time via `include_str!`). Contains 6 curated extensions: Filesystem, GitHub, Brave Search, Memory, Puppeteer, Slack. Installed via `POST /api/v1/marketplace/{id}/install`.

---

## Forward/Backward Compatibility

- **Registry versioning**: `marketplace_registry.json` has a `version` field (currently `1`) to support schema evolution.
- **Serde defaults**: Extension configs use `#[serde(default)]` extensively, ensuring older configs deserialize without errors when new fields are added.
- **SSE compat**: The deprecated SSE transport variant is preserved in the `ExtensionConfig` enum solely for backward compatibility with existing config files.
- **Protocol negotiation**: The MCP `initialize` handshake includes protocol version negotiation, allowing the client and server to agree on a compatible version.

---

## Key Dependencies

| Crate | Version | Role |
|-------|---------|------|
| `rmcp` | `1.2.0` | MCP protocol types, client/server traits, transport implementations, OAuth auth |
| `goose` | `1.29.0` | Agent framework; MCP client, extension manager, OAuth flow |
| `keyring` | `3.6.2` | OS-native credential storage for OAuth tokens |
| `oauth2` | `5.0` | OAuth 2.0 protocol primitives (used by Goose's OAuth flow) |
| `schemars` | `1.0` | JSON Schema generation for tool parameter types (MCP tool schemas) |

---

## References

- [MCP Specification](https://spec.modelcontextprotocol.io/) -- canonical protocol specification
- [MCP Authorization Specification](https://spec.modelcontextprotocol.io/specification/2025-03-26/basic/authorization/) -- OAuth 2.0 with PKCE for HTTP transports
- [rmcp crate](https://crates.io/crates/rmcp) -- Rust MCP implementation
- [Goose repository](https://github.com/block/goose) -- Block's agent framework (submodule at `goose/`)
- [GIAP Extension Developer Guide](../developer/extensions.md) -- building custom MCP servers for GIAP
