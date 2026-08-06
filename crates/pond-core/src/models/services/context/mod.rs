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
//! - [`model_class`] — which compaction mechanisms this model can afford, from
//!   the governor's resolved window and who is serving it. The budget modules
//!   answer "how much"; this one answers "which".
//! - [`resume_compaction`] — whether reopening a session after a gap should
//!   compact it before the first turn arrives. The time axis of PAI-4, as a pure
//!   `should_run` gate in the shape `consolidation_schedule` already proved.
//! - [`resummarisation`] — whether the large tier should rebuild its rolling
//!   summary from the source messages rather than from its own previous output,
//!   and what it may spend doing it. The model axis of PAI-4, and the one
//!   mechanism `model_class` gates rather than merely describes.
//! - [`turn_budget`] — the per-request reasoning-budget note shown to the model.
//! - [`image_history`] — how many historical image attachments are replayed as
//!   real pixels rather than a text placeholder.
pub mod context_budget;
pub mod context_governor;
pub mod context_monitor;
pub mod image_history;
pub mod model_class;
pub mod resume_compaction;
pub mod resummarisation;
pub mod token_counting;
pub mod turn_budget;
pub mod turn_trimmer;
