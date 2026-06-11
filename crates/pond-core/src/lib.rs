pub mod mcp;
pub mod models;
pub mod security;
pub mod shared;
pub mod user_data;

// Re-export shims at the old paths so downstream crates compile unchanged.
// Removed in Phase 4 after all `use` paths are updated.
pub mod domain;
pub mod ports;
pub mod services;

pub mod prompts;
