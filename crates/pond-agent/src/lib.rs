//! pond-agent — GIAP's custom agent loop with sustained tool calling.
//! Implements the [`Agent`](pond_core::models::ports::agent::Agent) port over an
//! Ollama NDJSON provider: native tool_calls looped until a final text answer, a
//! hot-swappable provider, and `PromptBuilder` prompts partitioned for KV reuse.

pub mod agent;
pub mod history;
pub mod ollama_provider;
pub mod ollama_wire;
pub mod tool_bridge;

pub use agent::PondAgent;
pub use ollama_provider::OllamaInferenceProvider;
