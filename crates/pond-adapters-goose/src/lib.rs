pub mod extension_manager;
pub mod giap_registration;
pub mod goose_agent;
/// Test-only: pins the wording of the invisible messages goose appends to a
/// turn. See the module docs — it lives here rather than in `pond-core` because
/// it `include_str!`s the submodule, which `pond-core` must never need.
#[cfg(test)]
mod goose_nudges;
pub mod logging;
pub mod mesh_provider;
pub mod orchestrator;
pub mod provider_adapter;
pub mod provider_shim;
pub mod token_counter;
pub mod vision_encoder;

pub use extension_manager::GiapGooseExtensionManager;
pub use giap_registration::{register_giap_extensions, registered_extensions};
pub use goose_agent::GooseAdapter;
pub use orchestrator::{GooseChildRunner, GooseOrchestrator};
pub use provider_adapter::GooseProviderAdapter;
