//! A direct path from GIAP to a mistral.rs server.
//!
//! # Why this exists
//!
//! GIAP already had a `"mistralrs"` *provider*: goose's OpenAI provider pointed
//! at a mistral.rs port. That measured mistral.rs through the whole goose
//! harness — `GooseAdapter`, `Agent::reply`, the extension manager, and
//! `GiapProviderShim`, which exists mainly to undo goose's own prompt handling.
//! This crate is the other end of the experiment: the smallest thing that can
//! serve a turn, so that what is being measured is the engine.
//!
//! # The boundary
//!
//! `pond-core` is the only GIAP dependency. Tools arrive through the
//! [`ToolDispatcher`](pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher)
//! port, history through
//! [`SessionStorage`](pond_core::user_data::ports::session_storage::SessionStorage),
//! and the system prompt is built by `pond_core::prompts` +
//! `models::services::prompt_builder`. Nothing here can reach the goose
//! submodule, and a `goose` line in this crate's `Cargo.toml` would end the
//! experiment without failing a test — so treat one as a review failure.
//!
//! # Two entry points
//!
//! - [`MistralRsProvider`] implements `pond-core`'s `InferenceProvider`, so any
//!   loop that takes that port can drive mistral.rs.
//! - [`MistralRsAgent`] implements the `Agent` port and is what `pond-server`
//!   binds when `agent_backend = "mistralrs"`. It is a turn loop and nothing
//!   more: no review, no delegation, no memory extraction.
//!
//! # Status
//!
//! A checkpoint. Mac-only by policy — mistral.rs was measured over budget on
//! the Orin (see `docs/developer/mistralrs-provider.md`) — and kept behind an
//! off-by-default cargo feature so a production build cannot select it.

pub mod agent;
pub mod provider;
pub mod wire;

pub use agent::MistralRsAgent;
pub use provider::{MistralRsProvider, MrEvent, MrEventStream};

/// The `agent_backend` value that selects this path.
pub const BACKEND_NAME: &str = "mistralrs";

/// Where a mistral.rs server is expected when nothing says otherwise. Matches
/// the port `scripts/try-mistralrs.sh` and the bake-off lab both use.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:9002";

/// The server URL, from `GIAP_MISTRALRS_URL` or the default.
pub fn base_url_from_env() -> String {
    std::env::var("GIAP_MISTRALRS_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_url_is_used_when_the_env_var_is_unset() {
        // Not asserting on the env var itself: the test process is shared and
        // another test setting it would make this flaky. The default is what
        // matters, and it must agree with the launcher script.
        assert_eq!(DEFAULT_BASE_URL, "http://127.0.0.1:9002");
        assert!(base_url_from_env().starts_with("http"));
    }
}
