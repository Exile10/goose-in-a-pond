//! Context-window management services.
//!
//! These services govern how much conversation history fits into the model's
//! context window and how it is shaped before each turn:
//!
//! - [`context_governor`] — resolves how big the active model's context window
//!   actually is, and where that number came from.
//! - [`context_budget`] — computes the token budget available for history,
//!   memories, and tool declarations given the active model's context size.
//! - [`context_monitor`] — tracks live context utilisation across a session so
//!   compaction can be triggered proactively.
//! - [`turn_budget`] — the per-request reasoning-budget note shown to the model.
//! - [`image_history`] — how many historical image attachments are replayed as
//!   real pixels rather than a text placeholder.
pub mod context_budget;
pub mod context_governor;
pub mod context_monitor;
pub mod image_history;
pub mod token_counting;
pub mod turn_budget;
pub mod turn_trimmer;
