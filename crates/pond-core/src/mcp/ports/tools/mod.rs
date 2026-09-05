//! Tool-chain ports — the four seams a tool call passes through, each named for what it does to a
//! call: [`tool_registry`] describes the catalogue the LLM's tool list is rendered from,
//! [`tool_dispatcher`] routes a name and arguments to the MCP server that owns it, [`tool_caller`]
//! refills malformed arguments, and [`tool_selection_control`] narrows the surface per session.
pub mod tool_caller;
pub mod tool_dispatcher;
pub mod tool_registry;
pub mod tool_selection_control;
