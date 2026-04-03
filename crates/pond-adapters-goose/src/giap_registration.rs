//! Registers the GIAP builtin MCP server into Goose's global extension registry.
//!
//! Call `register_giap_extension(services)` once at startup, before creating
//! any `GooseAdapter`.  Afterwards, the "giap" extension can be added to any
//! Goose agent with:
//!
//! ```ignore
//! agent.add_extension(
//!     ExtensionConfig::Builtin { name: "giap".to_string(), ... },
//!     session_id,
//! ).await?;
//! ```
use anyhow::Result;
use goose::builtin_extension::register_builtin_extension;
use pond_mcp_server::registry::{init_giap_services, spawn_giap_server};
use std::sync::Arc;

/// Register the GIAP MCP server as a Goose builtin extension named `"giap"`.
///
/// This must be called **once** at process startup before any `GooseAdapter`
/// tries to add the `"giap"` builtin.  It is idempotent — subsequent calls are
/// no-ops because `OnceLock` is used internally.
pub fn register_giap_extension(services: Arc<GiapServiceHandles>) -> Result<()> {
    init_giap_services(services);
    register_builtin_extension("giap", spawn_giap_server);
    Ok(())
}

pub use pond_mcp_server::registry::GiapServiceHandles;
