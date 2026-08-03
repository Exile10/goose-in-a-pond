pub mod context;
pub mod providers;
pub mod voice;

// Services that stay at the `models/services/` root.
pub mod history_manager;
pub mod model_service;
pub mod prompt_builder;
pub mod thought_filter;

// Compatibility re-exports so existing `models::services::<name>` paths
// keep resolving after the context/providers/voice grouping.
pub use context::{
    compact_encoding, context_budget, context_compactor, context_governor, context_monitor,
    image_history, turn_budget,
};
pub use providers::{fallback_provider, fast_responder};
pub use voice::{fallback_voice_output, instant_activation};
