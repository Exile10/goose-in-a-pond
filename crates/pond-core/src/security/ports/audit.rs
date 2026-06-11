//! Audit umbrella — the three overlapping "who-logged-what" ports.
//!
//! GIAP records audit-shaped data through three distinct ports with three
//! distinct schemas. They are NOT merged (each schema is real and load-bearing);
//! this module only re-exports them in one place and documents which to reach
//! for. Pick by *what kind of record* you are appending:
//!
//! - [`EventLogRepository`] (`event_log`) — **generic structured events.**
//!   A free-form `event_log` table in `pond_logs.db`: `level` (INFO/WARN/ERROR),
//!   `source` (subsystem), `message`, and an optional JSON `metadata` blob. Use
//!   it for cross-cutting operational events that don't fit a typed schema —
//!   startup/shutdown, provider hot-swaps, extension toggles, errors.
//!
//! - [`TelemetryPort`] (`telemetry`) — **turn-level metrics for the dashboard.**
//!   Strongly-typed per-chat-turn metrics (`TurnMetrics`: tokens, latency,
//!   model role, etc.) persisted to `pond_logs.db` and aggregated into a
//!   `TelemetrySummary`. Use it only for completed-turn measurements that feed
//!   the telemetry dashboard — never for free-form events.
//!
//! - `memory_repository::log_event` — **memory-lifecycle audit.** NOTE: this
//!   lives in [`crate::user_data::ports::memory_repository`] and STAYS there; it
//!   is intentionally not re-exported here because it is user-data scoped, not a
//!   security-boundary port. It records typed memory events (`MemoryEventKind`:
//!   save/recall/forget/recategorize) plus consolidation runs against
//!   `pond_system.db`. Use it for anything that mutates a stored memory; reach
//!   for it via the user_data repository, not this umbrella.
//!
//! Rule of thumb: typed turn metric → `TelemetryPort`; typed memory mutation →
//! `memory_repository::log_event`; everything else → `EventLogRepository`.
pub use crate::security::ports::event_log::*;
pub use crate::security::ports::telemetry::*;
