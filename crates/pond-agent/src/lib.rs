//! pond-agent — GIAP's custom agent loop with sustained tool calling.
//!
//! Implements the [`Agent`](pond_core::ports::agent::Agent) port with an
//! Ollama-native inference provider and a multi-turn tool-calling loop.
//! Replaces Goose's `GooseAdapter` with a purpose-built agent that:
//!
//! - Streams chat completions via Ollama's NDJSON `/api/chat` endpoint
//! - Supports native tool calling (Ollama tool_calls in responses)
//! - Loops on tool results until the model produces a final text answer
//! - Hot-swaps the inference provider at runtime when settings change
//! - Builds system prompts via `PromptBuilder` for KV-cache-friendly partitioning

pub mod agent;
pub mod history;
pub mod ollama_provider;
pub mod ollama_wire;
pub mod tool_bridge;

pub use agent::PondAgent;
pub use ollama_provider::OllamaInferenceProvider;
