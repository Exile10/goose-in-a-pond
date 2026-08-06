//! Tool-chain ports — the four seams a tool call passes through.
//!
//! GIAP's tool surface is deliberately split into narrow ports rather than one
//! monolith. Each owns a single stage of a tool's lifecycle, and the names
//! describe *what they do to a call*, not *what they call*:
//!
//! - [`tool_registry`] — **describes.** The catalogue of available tools and
//!   their JSON-schema declarations. It answers "what tools exist and what
//!   shape are their arguments?" — the source the LLM's tool list is rendered
//!   from.
//! - [`tool_dispatcher`] — **routes.** Given a tool name + arguments, finds the
//!   MCP server (builtin, stdio, or streamable_http) that owns it and forwards
//!   the invocation. It does not interpret arguments, only delivers them.
//! - [`tool_caller`] — **fills args.** The fallback specialist (FunctionGemma)
//!   that re-generates structured arguments when the main LLM emits an empty or
//!   malformed `{}`. It is step 3 of the parameter fallback chain.
//! - [`tool_selection_control`] — **narrows.** Which extension groups have
//!   their schemas in the prompt for a session, and the escape hatch that lets
//!   the model pull a dormant group in. It shapes the tool SURFACE, never
//!   whether a tool is used.
//!
//! Mental shorthand: registry describes, dispatcher routes, caller fills args,
//! selection control narrows the surface.
//!
//! Two further seams were declared here and never given an implementor:
//! `tool_cache` was to memoize deterministic results, and `tool_agent` was to
//! run a coarse tool pass ahead of the main chat stream. Both are gone, and so
//! is the `tool_cache_enabled` switch that shipped in Settings to toggle a cache
//! that did not exist — every tool call paid the full round-trip regardless of
//! where it was set.
pub mod tool_caller;
pub mod tool_dispatcher;
pub mod tool_registry;
pub mod tool_selection_control;
