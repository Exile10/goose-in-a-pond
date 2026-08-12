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
pub use context::{context_budget, context_governor, context_monitor, image_history, turn_budget};
pub use providers::fallback_provider;
// `spoken_time` is #265's; the rest of that branch's list -- `compact_encoding`,
// `context_compactor`, `fast_responder` -- names modules PAI replaced with the
// context governor, so re-exporting them would not compile.
pub use voice::{fallback_voice_output, instant_activation, spoken_time};
