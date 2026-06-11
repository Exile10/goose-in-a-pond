pub mod extension_manager;
pub mod extension_marketplace;
pub mod mcp_knowledge;
pub mod mcp_server;
pub mod notification;
pub mod tools;

// Compatibility re-exports so `mcp::ports::tool_*` paths keep resolving after
// the tool-chain ports moved under `tools/`.
pub use tools::{tool_agent, tool_cache, tool_caller, tool_dispatcher, tool_registry};
