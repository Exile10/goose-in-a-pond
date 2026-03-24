pub mod goose_agent;
pub mod provider_adapter;
pub mod session_adapter;
pub mod logging {
    //! Logging utilities for Goose-based adapters.
    //!
    //! This module is currently minimal and serves as a placeholder for
    //! shared logging helpers and configuration related to Goose workloads.
    //!
    //! As logging needs evolve, common patterns and helpers should be added
    //! here so they can be reused across adapters.

    // Intentionally left minimal for now.
}

pub use goose_agent::GooseAdapter;
pub use provider_adapter::GooseProviderAdapter;
pub use session_adapter::GooseSessionAdapter;
