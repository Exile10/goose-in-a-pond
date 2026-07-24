//! Audit umbrella — which store to append a record to.
//!
//! This module re-exports the audit-shaped ports in one place and documents how
//! to choose between them. Pick by *what kind of record* you are writing:
//!
//! - [`EventLog`] (`events`) — **the default.** The unified, append-only,
//!   correlatable event log (#108). One typed `Event` per thing that happened,
//!   carrying a category, session/trace id, structured attributes and a privacy
//!   sensitivity. Anything a user could reasonably ask the assistant about —
//!   what it did, what it touched, what it sent to the internet, who paired —
//!   goes here. It backs `GET /api/v1/activity`, sensitivity-aware retention
//!   (#117) and the audit MCP tools (#115).
//!
//! - [`OperationalLogRepository`] (`event_log` table) — **drained tracing
//!   output.** The backing store for the operational Logs viewer
//!   (`GET /api/v1/logs`), written only by `pond-server`'s tracing drain. Do not
//!   write domain events here. It is deliberately kept *out* of [`EventLog`]:
//!   the drain mirrors every INFO+ line, and merging it would bury the activity
//!   feed in log noise.
//!
//! - [`TelemetryPort`] (`turn_metrics`) — **typed per-turn metrics.**
//!   `TurnMetrics` (tokens, latency, model role) aggregated into a
//!   `TelemetrySummary` for the telemetry dashboard. Completed-turn
//!   measurements only, never free-form events.
//!
//! - `memory_repository::log_event` — **memory-lifecycle audit.** NOTE: this
//!   lives in [`crate::user_data::ports::memory_repository`] and STAYS there; it
//!   is intentionally not re-exported here because it is user-data scoped, not a
//!   security-boundary port. It records typed memory events (`MemoryEventKind`:
//!   save/recall/forget/recategorize) plus consolidation runs against
//!   `pond_system.db`. Use it for anything that mutates a stored memory; reach
//!   for it via the user_data repository, not this umbrella.
//!
//! Rule of thumb: **semantic record → [`EventLog`]**; typed turn metric →
//! [`TelemetryPort`]; typed memory mutation → `memory_repository::log_event`;
//! tracing output → just use `tracing::` and let the drain handle it.
//!
//! ## On the "four silos"
//!
//! #108 described the unified event as replacing four disjoint silos
//! (`event_log`, `TurnMetrics`, sensors, camera). Three of those are settled and
//! are **not** pending migrations:
//!
//! - **Sensors and cameras already emit into [`EventLog`]** — see
//!   `BusEvent::to_event` in [`crate::shared::ports::event_bus`]. Ingestion
//!   persists, then publishes, and a bridge appends the event. Their typed
//!   tables remain on purpose: an append-only attribute bag cannot serve numeric
//!   range queries, and camera `acknowledged` is *mutable* state an append-only
//!   log cannot hold at all. They are projections beside the log, not silos.
//! - **The `event_log` table is not an audit silo** — it holds drained tracing
//!   output, a different concern, as described above.
//!
//! Only `TurnMetrics` is genuinely still separate, and deliberately so: its
//! aggregation feeds a dashboard, and reimplementing that over the event store
//! risks silently wrong numbers.
pub use crate::security::ports::event_log::*;
pub use crate::security::ports::telemetry::*;
