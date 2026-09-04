//! Provider-composition services: [`fallback_provider`] chains a primary with
//! fallbacks so one backend outage is survivable. Pre-classifying a turn by keyword
//! is ruled out by AGENTS.md's working agreement; per-role dispatch belongs at the
//! `pond-api` `AppState` seam, where the live provider hot-swap already happens.
pub mod fallback_provider;
