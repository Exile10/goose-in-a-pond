//! Provider-composition services.
//!
//! These wrap or select among the raw [`crate::models::ports::provider`]
//! implementations to add cross-cutting behaviour:
//!
//! - [`fallback_provider`] — chains a primary provider with one or more
//!   fallbacks, retrying on failure so a single backend outage is survivable.
//! - [`fast_responder`] — short-circuits trivial turns with a low-latency
//!   canned/first-token response while the full provider warms up.
//!
//! `dynamic_provider` and `model_router` are retained here but intentionally
//! left undeclared (dead code pending removal).
pub mod fallback_provider;
pub mod fast_responder;
