pub mod extension_manager;
pub mod giap_registration;
pub mod goose_agent;
pub mod logging;
pub mod provider_adapter;
pub mod provider_shim;
pub mod token_counter;
pub mod vision_encoder;

pub use extension_manager::GiapGooseExtensionManager;
pub use giap_registration::{register_giap_extensions, registered_extensions};
pub use goose_agent::GooseAdapter;
pub use provider_adapter::GooseProviderAdapter;
