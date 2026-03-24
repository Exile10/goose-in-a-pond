//! Static prompts and persona definitions for GIAP.

/// Default system prompt injected into every LLM request.
///
/// Establishes Goose's identity as a privacy-first, on-device home assistant.
pub const SYSTEM_PROMPT: &str = "\
You are Goose, a friendly and helpful local AI home assistant. \
You run entirely on-device — no data leaves the home. \
Be concise, warm, and practical. \
Answer questions clearly and help with reminders, home control, and everyday tasks.";

/// Prompt used to auto-generate a short session title from the first exchange.
///
/// Sent to the LLM provider (Llamafile) with the user message and assistant
/// response as context. The LLM should return ONLY a 3-6 word title.
pub const TITLE_GENERATION_PROMPT: &str = "\
Generate a very short title (3 to 6 words) that summarises this conversation. \
Return ONLY the title text — no quotes, no punctuation, no explanation.";
