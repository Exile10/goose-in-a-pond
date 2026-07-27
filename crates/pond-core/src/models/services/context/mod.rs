//! Context-window management services.
//!
//! These services govern how much conversation history fits into the model's
//! context window and how it is shaped before each turn:
//!
//! - [`context_budget`] — computes the token budget available for history,
//!   memories, and tool declarations given the active model's context size.
//! - [`context_compactor`] — summarises or drops older turns when the budget
//!   is exceeded, preserving the most relevant history.
//! - [`context_monitor`] — tracks live context utilisation across a session so
//!   compaction can be triggered proactively.
//! - [`compact_encoding`] — compact wire encoding for compacted history blocks.
//! - [`turn_budget`] — the per-request reasoning-budget note shown to the model.
pub mod compact_encoding;
pub mod context_budget;
pub mod context_compactor;
pub mod context_monitor;
pub mod turn_budget;
pub mod turn_trimmer;
