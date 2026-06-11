//! Tool-chain ports — the five seams a tool call passes through.
//!
//! GIAP's tool surface is deliberately split into five narrow ports rather than
//! one monolith. Each owns a single stage of a tool's lifecycle, and the names
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
//! - [`tool_cache`] — **memoizes.** Caches deterministic tool results so a
//!   repeated call within a session skips the round-trip. Cache identity is the
//!   tool name + canonicalised arguments.
//! - [`tool_agent`] — **the pre-LLM tool pass.** A coarse agent stage that may
//!   run tools *before* the main chat stream begins, seeding context. It sits
//!   ahead of the LLM rather than inside its agentic loop.
//!
//! Mental shorthand: registry describes, dispatcher routes, caller fills args,
//! cache memoizes, agent is the pre-LLM pass.
pub mod tool_agent;
pub mod tool_cache;
pub mod tool_caller;
pub mod tool_dispatcher;
pub mod tool_registry;
