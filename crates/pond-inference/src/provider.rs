//! [`InferenceProvider`] implementation for [`LlamaCppEngine`].
//!
//! The generation loop runs inside `tokio::task::spawn_blocking` because
//! llama-cpp-2 calls are blocking CPU/GPU operations. A `tokio::sync::mpsc`
//! channel bridges the blocking thread to the async [`ChatEventStream`].

use crate::engine::{LlamaCppEngine, ModelSlot};
use crate::memory::effective_context_size;
use crate::sampling::build_sampler;
use crate::tool_calling;

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{AddBos, ChatTemplateResult, GrammarTrigger, GrammarTriggerType};
use llama_cpp_2::openai::OpenAIChatTemplateParams;
use llama_cpp_2::sampling::LlamaSampler;
use pond_core::domain::message::{ChatMessage, Role};
use pond_core::domain::model_capabilities::ModelCapabilities;
use pond_core::ports::inference::{
    ChatEvent, ChatEventStream, InferenceOptions, InferenceProvider, ToolDefinition,
};
use pond_core::ports::provider::UsageStats;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

#[async_trait]
impl InferenceProvider for LlamaCppEngine {
    fn stream_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> ChatEventStream {
        let (tx, rx) = mpsc::channel::<Result<ChatEvent>>(64);

        // Clone everything we need to move into the blocking task.
        let system_prompt = system_prompt.to_string();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let temperature = options.temperature;
        let max_tokens = options.max_tokens;
        let enable_thinking = options.enable_thinking;
        let model_slot = self.model_slot();
        let backend = self.backend_arc();

        tokio::task::spawn_blocking(move || {
            generation_task(
                model_slot,
                backend,
                system_prompt,
                messages,
                tools,
                temperature,
                max_tokens,
                enable_thinking,
                tx,
            );
        });

        Box::pin(ReceiverStream { rx })
    }

    fn model_name(&self) -> String {
        // This is sync; use try_lock to avoid blocking.
        self.model_slot()
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.model_id.clone()))
            .unwrap_or_else(|| "none".to_string())
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.model_slot()
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.capabilities.clone()))
            .unwrap_or_default()
    }
}

/// Blocking generation task that runs inside `spawn_blocking`.
///
/// Acquires the model lock, builds the prompt, creates a context, and runs
/// the autoregressive generation loop, sending [`ChatEvent`]s through the
/// channel.
///
/// When `cache_path` is provided, the context state is saved after generation
/// and loaded before the next call. Prefix matching skips re-decoding tokens
/// already in the KV cache, saving 5-15s on Jetson for stable system prompts.
#[allow(clippy::too_many_arguments)]
fn generation_task(
    model_slot: ModelSlot,
    backend: Arc<LlamaBackend>,
    system_prompt: String,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    enable_thinking: bool,
    tx: mpsc::Sender<Result<ChatEvent>>,
) {
    // Acquire the model lock (blocking). Mutable for in-memory KV-cache persistence.
    let mut model_guard = model_slot.blocking_lock();
    let Some(loaded) = model_guard.as_mut() else {
        let _ = tx.blocking_send(Err(anyhow::anyhow!("no model loaded")));
        return;
    };

    // ── Build the prompt ─────────────────────────────────────────────────

    let oai_messages_json = build_openai_messages_json(&system_prompt, &messages);
    let compact_tools = tool_calling::compact_tools_json(&tools);

    // On small-context platforms (Jetson ≤4096), full tool schemas always exceed
    // the budget. Skip directly to compact to avoid wasting time on serialization,
    // Jinja rendering, and tokenization that will be discarded immediately.
    let n_ctx_train = loaded.model.n_ctx_train() as usize;
    let use_compact_directly = n_ctx_train <= 4096;

    let full_tools_json = if use_compact_directly {
        tracing::debug!(n_ctx_train, "small context — skipping full tool schema serialization");
        None
    } else {
        tool_calling::tools_to_json(&tools)
    };

    let template_result = match apply_template(
        &loaded.model,
        &loaded.chat_template,
        &oai_messages_json,
        full_tools_json.as_deref(),
        compact_tools.as_deref(),
        enable_thinking,
    ) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.blocking_send(Err(e));
            return;
        }
    };

    let prompt = &template_result.prompt;
    let additional_stops = &template_result.additional_stops;

    // Debug: log the formatted prompt and tool state so we can diagnose
    // whether the chat template is including tools properly.
    tracing::debug!(
        tools_count = tools.len(),
        has_tools_json = full_tools_json.is_some(),
        additional_stops_count = additional_stops.len(),
        has_grammar = template_result.grammar.is_some(),
        grammar_lazy = template_result.grammar_lazy,
        grammar_triggers_count = template_result.grammar_triggers.len(),
        prompt_len = prompt.len(),
        "template applied"
    );
    if !additional_stops.is_empty() {
        tracing::debug!(stops = ?additional_stops, "additional stop sequences");
    }
    if let Some(ref grammar) = template_result.grammar {
        let preview = if grammar.len() > 200 {
            &grammar[..200]
        } else {
            grammar.as_str()
        };
        tracing::debug!(grammar_preview = %preview, "tool-call grammar from template");
    }
    if !template_result.grammar_triggers.is_empty() {
        let trigger_values: Vec<&str> = template_result
            .grammar_triggers
            .iter()
            .map(|t| t.value.as_str())
            .collect();
        tracing::debug!(triggers = ?trigger_values, "grammar triggers");
    }
    // Log the last 500 chars of the prompt to see if tools are included.
    let prompt_tail = if prompt.len() > 500 {
        &prompt[prompt.len() - 500..]
    } else {
        prompt.as_str()
    };
    tracing::debug!(prompt_tail = %prompt_tail, "prompt tail (last 500 chars)");

    // ── Tokenize ─────────────────────────────────────────────────────────

    let tokens = match loaded.model.str_to_token(prompt, AddBos::Never) {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.blocking_send(Err(anyhow::anyhow!("tokenization failed: {}", e)));
            return;
        }
    };

    let prompt_token_count = tokens.len();
    let ctx_size = effective_context_size(&loaded.model, prompt_token_count);

    if prompt_token_count >= ctx_size {
        let _ = tx.blocking_send(Err(anyhow::anyhow!(
            "prompt ({} tokens) exceeds context limit ({} tokens)",
            prompt_token_count,
            ctx_size
        )));
        return;
    }

    // ── In-memory KV-cache reuse ───────────────────────────────────────────
    // Try to reuse the persistent context from the previous turn. If the prefix
    // matches, we skip re-decoding thousands of tokens (system prompt + tools).
    // If no cached context exists (first call), create a fresh one.

    let mut ctx_params =
        LlamaContextParams::default().with_n_ctx(NonZeroU32::new(ctx_size as u32));
    ctx_params = ctx_params.with_n_batch(512);
    ctx_params = ctx_params.with_flash_attention_policy(1);

    // Helper: create a fresh context sized for the current prompt.
    let create_fresh_ctx =
        |model: &llama_cpp_2::model::LlamaModel,
         backend: &LlamaBackend,
         ctx_size: usize|
         -> Result<llama_cpp_2::context::LlamaContext<'static>> {
            let mut params =
                LlamaContextParams::default().with_n_ctx(NonZeroU32::new(ctx_size as u32));
            params = params.with_n_batch(512);
            params = params.with_flash_attention_policy(1);
            let ctx = model
                .new_context(backend, params)
                .map_err(|e| anyhow::anyhow!("failed to create context: {}", e))?;
            // SAFETY: Context borrows from model which lives in the same LoadedModel struct.
            // We guarantee it's dropped before the model (see LoadedModel docs).
            Ok(unsafe { std::mem::transmute(ctx) })
        };

    // Check if we can reuse the persistent context.
    let (mut ctx, tokens_to_decode_start) = if let Some(ref cached) = loaded.cached_ctx {
        // Check if the new prompt fits in the cached context's allocation.
        // The context was sized for an earlier (smaller) prompt — if the conversation
        // grew beyond it, we must create a fresh context with the right size.
        // Reserve 512 tokens for generation headroom.
        let cached_n_ctx = cached.ctx.n_ctx() as usize;
        if tokens.len() + 512 > cached_n_ctx {
            tracing::info!(
                prompt_tokens = tokens.len(),
                cached_n_ctx,
                new_ctx_size = ctx_size,
                "KV cache too small for current prompt — creating larger context"
            );
            loaded.cached_ctx = None;
            match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
                Ok(ctx) => (ctx, 0),
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        } else {
            // Context fits — check prefix match.
            let action = crate::kv_cache::plan_cache_reuse(&cached.tokens_in_cache, &tokens);
            match action {
                crate::kv_cache::CacheAction::FullHit => {
                    tracing::info!(
                        cached = cached.tokens_in_cache.len(),
                        "KV cache full hit (in-memory) — skipping all prefill"
                    );
                    let ctx = loaded.cached_ctx.take().unwrap().ctx;
                    (ctx, tokens.len())
                }
                crate::kv_cache::CacheAction::IncrementalDecode {
                    trim_from,
                    decode_from,
                    tokens_saved,
                } => {
                    let mut ctx = loaded.cached_ctx.take().unwrap().ctx;
                    let _ = ctx.clear_kv_cache_seq(None, Some(trim_from as u32), None);
                    tracing::info!(
                        tokens_saved,
                        decode_from,
                        total = tokens.len(),
                        "KV cache partial hit (in-memory) — decoding only delta"
                    );
                    (ctx, decode_from)
                }
                crate::kv_cache::CacheAction::FullPrefill => {
                    tracing::info!("KV cache miss (in-memory, no prefix match) — full prefill");
                    loaded.cached_ctx = None;
                    match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
                        Ok(ctx) => (ctx, 0),
                        Err(e) => {
                            let _ = tx.blocking_send(Err(e));
                            return;
                        }
                    }
                }
            }
        }
    } else {
        tracing::info!("KV cache cold start (in-memory) — creating fresh context");
        match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
            Ok(ctx) => (ctx, 0),
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        }
    };

    let tokens_to_decode = &tokens[tokens_to_decode_start..];

    // Prefill tokens in batches (only the delta when cache hit).
    // If decode fails (NoKvCacheSlot), drop the cache and retry with a fresh context.
    if !tokens_to_decode.is_empty() {
        let n_batch = ctx.n_batch() as usize;
        let mut decode_failed = false;
        for chunk in tokens_to_decode.chunks(n_batch) {
            let mut batch = match LlamaBatch::get_one(chunk) {
                Ok(b) => b,
                Err(e) => {
                    let _ =
                        tx.blocking_send(Err(anyhow::anyhow!("batch creation failed: {}", e)));
                    return;
                }
            };
            if let Err(e) = ctx.decode(&mut batch) {
                tracing::warn!("decode failed ({}), retrying with fresh context", e);
                decode_failed = true;
                break;
            }
        }
        // Retry: create fresh context and full prefill if cached decode failed.
        if decode_failed {
            drop(ctx);
            ctx = match create_fresh_ctx(&loaded.model, &backend, ctx_size) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            };
            let n_batch = ctx.n_batch() as usize;
            for chunk in tokens.chunks(n_batch) {
                let mut batch = match LlamaBatch::get_one(chunk) {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx
                            .blocking_send(Err(anyhow::anyhow!("batch creation failed: {}", e)));
                        return;
                    }
                };
                if let Err(e) = ctx.decode(&mut batch) {
                    let _ =
                        tx.blocking_send(Err(anyhow::anyhow!("prefill decode failed: {}", e)));
                    return;
                }
            }
        }
    }

    // ── Generation loop ──────────────────────────────────────────────────

    let mut sampler = build_sampler(temperature);

    // NOTE: Grammar constraints from the chat template are intentionally
    // NOT applied to the sampler. The lazy grammar sampler in llama-cpp-2
    // v0.1.143 crashes with `GGML_ASSERT(!stacks.empty())` when Gemma 4's
    // complex GBNF grammar is used. Gemma 4 is fine-tuned for tool calling
    // and produces structured `<tool_call>` markup from the Jinja template
    // formatting alone. The grammar constraint can be re-enabled once the
    // llama-cpp-2 grammar sampler is stable for this model family.
    if template_result.grammar.is_some() {
        tracing::debug!(
            grammar_lazy = template_result.grammar_lazy,
            triggers = template_result.grammar_triggers.len(),
            "grammar available from template (not applied — relying on model fine-tuning)"
        );
    }

    let max_output = if let Some(max) = max_tokens {
        ctx_size
            .saturating_sub(prompt_token_count)
            .min(max as usize)
    } else {
        ctx_size.saturating_sub(prompt_token_count)
    };

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut generated_text = String::new();
    let mut streamed_len: usize = 0;
    let mut output_token_count: u32 = 0;

    for _ in 0..max_output {
        let token = sampler.sample(&ctx, -1);
        sampler.accept(token);

        if loaded.model.is_eog_token(token) {
            break;
        }

        output_token_count += 1;

        let piece = match loaded
            .model
            .token_to_piece(token, &mut decoder, true, None)
        {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.blocking_send(Err(anyhow::anyhow!("token decode failed: {}", e)));
                break;
            }
        };

        if !piece.is_empty() {
            generated_text.push_str(&piece);

            // Stream text up to the safe boundary (hold back potential tool calls).
            let stream_up_to = tool_calling::safe_stream_end(&generated_text);
            if stream_up_to > streamed_len {
                #[allow(clippy::string_slice)]
                let new_text = &generated_text[streamed_len..stream_up_to];
                if !new_text.is_empty()
                    && tx
                        .blocking_send(Ok(ChatEvent::Text(new_text.to_string())))
                        .is_err()
                {
                    break; // Receiver dropped.
                }
                streamed_len = stream_up_to;
            }

            // Check additional stop sequences from the template.
            let should_stop = additional_stops
                .iter()
                .any(|stop| generated_text.ends_with(stop));
            if should_stop {
                break;
            }
        }

        // Decode next token.
        let next_tokens = [token];
        let mut next_batch = match LlamaBatch::get_one(&next_tokens) {
            Ok(b) => b,
            Err(e) => {
                let _ = tx.blocking_send(Err(anyhow::anyhow!("batch creation failed: {}", e)));
                break;
            }
        };
        if let Err(e) = ctx.decode(&mut next_batch) {
            let _ = tx.blocking_send(Err(anyhow::anyhow!("decode failed: {}", e)));
            break;
        }
    }

    // ── Post-generation: stream remaining content and parse tool calls ───

    tracing::debug!(
        generated_len = generated_text.len(),
        output_tokens = output_token_count,
        "generation complete, parsing tool calls"
    );
    // Log the raw output for tool call debugging.
    let output_preview = if generated_text.len() > 300 {
        &generated_text[..300]
    } else {
        &generated_text
    };
    tracing::debug!(output = %output_preview, "raw model output (first 300 chars)");

    let tool_calls = tool_calling::parse_tool_calls(&generated_text);

    if !tool_calls.is_empty() {
        // Stream any remaining content before the tool calls.
        let content = tool_calling::extract_content(&generated_text);
        if content.len() > streamed_len {
            #[allow(clippy::string_slice)]
            let remaining = &content[streamed_len..];
            if !remaining.is_empty() {
                let _ = tx.blocking_send(Ok(ChatEvent::Text(remaining.to_string())));
            }
        }

        // Emit tool call events.
        for tc in tool_calls {
            let _ = tx.blocking_send(Ok(ChatEvent::ToolCall {
                id: uuid::Uuid::new_v4().to_string(),
                name: tc.name,
                arguments: tc.arguments,
            }));
        }
    } else {
        // No tool calls -- stream any remaining text.
        if generated_text.len() > streamed_len {
            #[allow(clippy::string_slice)]
            let remaining = &generated_text[streamed_len..];
            if !remaining.is_empty() {
                let _ = tx.blocking_send(Ok(ChatEvent::Text(remaining.to_string())));
            }
        }
    }

    // ── Persist context in memory for next turn ─────────────────────────────
    // Store the context + token log back into LoadedModel so the next call
    // can skip re-prefilling the stable prefix. Zero disk I/O.
    loaded.cached_ctx = Some(crate::engine::CachedInferenceContext {
        ctx,
        tokens_in_cache: tokens,
    });
    tracing::debug!(
        tokens_cached = loaded.cached_ctx.as_ref().unwrap().tokens_in_cache.len(),
        "KV cache persisted in memory for next turn"
    );

    // Emit usage stats.
    let _ = tx.blocking_send(Ok(ChatEvent::Usage(UsageStats {
        prompt_tokens: prompt_token_count as u32,
        completion_tokens: output_token_count,
    })));
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Apply the chat template using the OpenAI-compat API with jinja.
///
/// Tries full tool schema first; falls back to compact (name+description only)
/// if the full version fails or exceeds the token budget.
///
/// Returns the full [`ChatTemplateResult`] which includes the rendered prompt,
/// grammar constraints for tool calling, additional stop sequences, and trigger
/// information for lazy grammar sampling.
fn apply_template(
    model: &llama_cpp_2::model::LlamaModel,
    template: &llama_cpp_2::model::LlamaChatTemplate,
    messages_json: &str,
    full_tools_json: Option<&str>,
    compact_tools: Option<&str>,
    enable_thinking: bool,
) -> Result<ChatTemplateResult> {
    let apply = |tools: Option<&str>| {
        let params = OpenAIChatTemplateParams {
            messages_json,
            tools_json: tools,
            tool_choice: None,
            json_schema: None,
            grammar: None,
            reasoning_format: if enable_thinking { Some("auto") } else { None },
            chat_template_kwargs: None,
            add_generation_prompt: true,
            use_jinja: true,
            parallel_tool_calls: false,
            enable_thinking,
            add_bos: false,
            add_eos: false,
            parse_tool_calls: true,
        };
        model.apply_chat_template_oaicompat(template, &params)
    };

    // Try full tools first.
    match apply(full_tools_json) {
        Ok(result) => {
            // Check token count -- if too large, fall back to compact.
            let token_count = model
                .str_to_token(&result.prompt, AddBos::Never)
                .map(|t| t.len())
                .unwrap_or(0);
            let n_ctx_train = model.n_ctx_train() as usize;
            if token_count > n_ctx_train.saturating_sub(512) {
                tracing::info!(
                    token_count,
                    n_ctx_train,
                    "full tool schema exceeds budget, trying compact"
                );
                match apply(compact_tools) {
                    Ok(r) => Ok(r),
                    Err(_) => Ok(result),
                }
            } else {
                Ok(result)
            }
        }
        Err(e) => {
            tracing::warn!("full template failed: {}, trying compact", e);
            apply(compact_tools)
                .map_err(|e2| anyhow::anyhow!("chat template failed: {} (compact: {})", e, e2))
        }
    }
}

/// Build a lazy grammar sampler from the template's grammar and triggers.
///
/// Lazy grammar sampling means the grammar constraints only activate when
/// specific trigger words/tokens/patterns are encountered in the output.
/// This allows the model to generate free-form text normally, and only
/// constrains output to valid tool-call JSON when a tool-call pattern starts.
///
/// The trigger classification:
/// - `GrammarTriggerType::Word` -> passed as `trigger_words` to `grammar_lazy()`
/// - `GrammarTriggerType::Token` -> passed as `trigger_tokens` to `grammar_lazy()`
/// - `GrammarTriggerType::Pattern` / `PatternFull` -> passed as patterns to
///   `grammar_lazy_patterns()`
#[allow(dead_code)]
fn build_grammar_sampler_lazy(
    model: &llama_cpp_2::model::LlamaModel,
    grammar_str: &str,
    triggers: &[GrammarTrigger],
) -> Result<LlamaSampler> {
    let mut word_triggers: Vec<Vec<u8>> = Vec::new();
    let mut token_triggers: Vec<llama_cpp_2::token::LlamaToken> = Vec::new();
    let mut pattern_triggers: Vec<String> = Vec::new();

    for trigger in triggers {
        match trigger.trigger_type {
            GrammarTriggerType::Word => {
                word_triggers.push(trigger.value.as_bytes().to_vec());
            }
            GrammarTriggerType::Token => {
                if let Some(tok) = trigger.token {
                    token_triggers.push(tok);
                }
            }
            GrammarTriggerType::Pattern | GrammarTriggerType::PatternFull => {
                pattern_triggers.push(trigger.value.clone());
            }
        }
    }

    // Use pattern-based lazy grammar if any regex patterns are present.
    if !pattern_triggers.is_empty() {
        tracing::debug!(
            patterns = ?pattern_triggers,
            token_count = token_triggers.len(),
            "building lazy grammar sampler with regex patterns"
        );
        LlamaSampler::grammar_lazy_patterns(
            model,
            grammar_str,
            "root",
            &pattern_triggers,
            &token_triggers,
        )
        .map_err(|e| anyhow::anyhow!("lazy grammar (patterns) failed: {}", e))
    } else if !word_triggers.is_empty() || !token_triggers.is_empty() {
        tracing::debug!(
            word_count = word_triggers.len(),
            token_count = token_triggers.len(),
            "building lazy grammar sampler with word/token triggers"
        );
        LlamaSampler::grammar_lazy(
            model,
            grammar_str,
            "root",
            word_triggers.iter().map(|w| w.as_slice()),
            &token_triggers,
        )
        .map_err(|e| anyhow::anyhow!("lazy grammar (words) failed: {}", e))
    } else {
        // No triggers provided -- fall back to always-on grammar.
        tracing::debug!("no grammar triggers, using strict grammar");
        LlamaSampler::grammar(model, grammar_str, "root")
            .map_err(|e| anyhow::anyhow!("strict grammar failed: {}", e))
    }
}

/// Build OpenAI-compatible messages JSON array.
fn build_openai_messages_json(system_prompt: &str, messages: &[ChatMessage]) -> String {
    let mut arr: Vec<serde_json::Value> = vec![serde_json::json!({
        "role": "system",
        "content": system_prompt
    })];

    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            // Tool results use "tool" role so the model recognizes them as
            // tool responses (not user input). This prevents the model from
            // asking follow-up questions about tool results.
            Role::Tool => "tool",
        };
        arr.push(serde_json::json!({
            "role": role,
            "content": msg.content
        }));
    }

    serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string())
}

// ── Stream adapter ───────────────────────────────────────────────────────────

/// Adapts a `tokio::sync::mpsc::Receiver` into a `futures::Stream`.
struct ReceiverStream {
    rx: mpsc::Receiver<Result<ChatEvent>>,
}

impl Stream for ReceiverStream {
    type Item = Result<ChatEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_messages_json_basic() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];
        let json = build_openai_messages_json("You are helpful.", &messages);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 3); // system + user + assistant
        assert_eq!(parsed[0]["role"], "system");
        assert_eq!(parsed[1]["role"], "user");
        assert_eq!(parsed[1]["content"], "Hello");
        assert_eq!(parsed[2]["role"], "assistant");
    }

    #[test]
    fn build_messages_json_empty() {
        let json = build_openai_messages_json("sys", &[]);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["content"], "sys");
    }
}
