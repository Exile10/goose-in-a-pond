//! Provider-composition services.
//!
//! These wrap or select among the raw [`crate::models::ports::provider`]
//! implementations to add cross-cutting behaviour:
//!
//! - [`fallback_provider`] — chains a primary provider with one or more
//!   fallbacks, retrying on failure so a single backend outage is survivable.
//!
//! `fast_responder` sat here too, keyword-matching greetings, farewells, thanks
//! and acknowledgements to answer them without the model. It had no callers, and
//! the `fast_path_enabled` switch that appeared to control it was read by
//! nothing — the pond has always paid the full round trip for "hi". It is gone
//! rather than wired, because pre-classifying a turn by keyword is the thing
//! AGENTS.md's working agreement rules out: the model decides.
//!
//! `dynamic_provider` and `model_router` sat here undeclared, so neither the
//! compiler nor the test runner ever saw them. `model_router` had six tests
//! asserting that "explain why…" reached a `think` provider and "remind me at
//! 9am" reached a `task` provider; its `complete` pinned the role to
//! `ModelRole::Chat` unconditionally, so three of those tests were false and
//! nothing could say so. Both files are gone — a design that needs per-role
//! dispatch should start from the seam in `pond-api`'s `AppState`, which is
//! where the live provider hot-swap already happens.
pub mod fallback_provider;
