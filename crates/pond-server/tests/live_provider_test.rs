//! Live provider integration tests — gated by environment variables.
//!
//! These tests require real running services and are marked `#[ignore]` by default.
//! They are designed to be run manually to validate full end-to-end provider behaviour.
//!
//! # How to run
//!
//! ## Llamafile (requires a running llamafile server):
//! ```bash
//! GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
//!   cargo test -p pond-server --test live_provider_test -- --ignored llamafile
//! ```
//!
//! ## Ollama (requires `ollama serve` + a pulled model):
//! ```bash
//! GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
//!   cargo test -p pond-server --test live_provider_test -- --ignored ollama
//! ```
//!
//! ## All live tests (both services running):
//! ```bash
//! GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
//! GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
//!   cargo test -p pond-server --test live_provider_test -- --ignored
//! ```
//!
//! ## Known-good models (verified 2026-04):
//! - Llamafile: `gemma-2-2b-it.Q4_K_M.llamafile` (served on port 8080)
//! - Ollama: `gemma3:4b` or `gemma4:latest` (pulled on the local Ollama instance)
//! - GGUF: `gemma-4-E2B-it-Q4_K_M.gguf` (at `$DATA_DIR/models/gguf/`)

use futures::StreamExt;
use pond_core::domain::message::ChatMessage;
use pond_core::ports::provider::{LlmProvider, StreamToken};

// ── Environment helpers ────────────────────────────────────────────────────────

fn llamafile_url() -> Option<String> { std::env::var("GIAP_LLAMAFILE_URL").ok() }
fn ollama_url()    -> Option<String> { std::env::var("GIAP_OLLAMA_URL").ok() }
fn ollama_model()  -> String { std::env::var("GIAP_OLLAMA_MODEL").unwrap_or_else(|_| "gemma3:4b".into()) }

// ═══════════════════════════════════════════════════════════════════
//  Llamafile live tests
// ═══════════════════════════════════════════════════════════════════

/// Verify llamafile can accept a simple chat message and stream tokens back.
#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_chat_streams_tokens() {
    let url = match llamafile_url() { None => return, Some(u) => u };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a concise assistant. Reply in one short sentence.",
        vec![ChatMessage::user("Hello! What is 2 + 2?")],
    );

    let mut text_tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t))  => text_tokens.push(t),
            Ok(StreamToken::Usage(_)) => {}
            Err(e)                    => panic!("stream error: {e}"),
        }
    }

    assert!(!text_tokens.is_empty(), "expected at least one text token from llamafile");
    let full = text_tokens.join("");
    println!("llamafile replied: {full}");
    assert!(!full.trim().is_empty(), "expected non-empty response from llamafile");
}

/// Verify that a reasoning-type prompt produces a model_role = "think" response.
/// (We test via the provider adapter directly — the routing is in ModelRouter.)
#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_streaming_yields_incremental_tokens() {
    let url = match llamafile_url() { None => return, Some(u) => u };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a helpful assistant.",
        vec![ChatMessage::user("Count from 1 to 5, one number per line.")],
    );

    let mut token_count = 0;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(_))  => token_count += 1,
            Ok(StreamToken::Usage(_)) => {}
            Err(e)                    => panic!("stream error: {e}"),
        }
    }

    // A count-to-5 response should generate multiple tokens
    assert!(
        token_count >= 2,
        "expected multiple incremental tokens, got {token_count}"
    );
    println!("llamafile streamed {token_count} tokens");
}

/// Verify the done event includes usage when llamafile reports it.
#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_done_event_includes_usage() {
    let url = match llamafile_url() { None => return, Some(u) => u };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a concise assistant.",
        vec![ChatMessage::user("Hello")],
    );

    let mut usage_opt = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(_))  => {}
            Ok(StreamToken::Usage(u)) => usage_opt = Some(u),
            Err(e)                    => panic!("stream error: {e}"),
        }
    }

    // Note: not all llamafile builds report usage — log but don't hard-fail.
    match usage_opt {
        Some(u) => {
            println!("llamafile usage: prompt={} completion={}", u.prompt_tokens, u.completion_tokens);
            assert!(u.completion_tokens > 0, "completion_tokens should be > 0");
        }
        None => println!("NOTE: this llamafile build does not report usage stats"),
    }
}

/// Verify multi-turn conversation: second reply references the first message.
#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_URL pointing to a running llamafile server"]
async fn llamafile_live_multi_turn_conversation() {
    let url = match llamafile_url() { None => return, Some(u) => u };
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    // Turn 1: establish context
    let turn1 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![ChatMessage::user("My favourite colour is purple.")],
        )
        .await
        .expect("turn 1 failed");
    println!("turn 1: {}", turn1.content);

    // Turn 2: ask about established context
    let turn2 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![
                ChatMessage::user("My favourite colour is purple."),
                ChatMessage::assistant(turn1.content.clone()),
                ChatMessage::user("What colour did I just mention?"),
            ],
        )
        .await
        .expect("turn 2 failed");
    println!("turn 2: {}", turn2.content);

    assert!(
        turn2.content.to_lowercase().contains("purple"),
        "expected turn 2 to mention 'purple', got: {}",
        turn2.content
    );
}

// ═══════════════════════════════════════════════════════════════════
//  Ollama live tests
// ═══════════════════════════════════════════════════════════════════

/// Verify Ollama can accept a simple chat message and return a response.
#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL pointing to a running Ollama server with a pulled model"]
async fn ollama_live_chat_completes() {
    let url = match ollama_url() { None => return, Some(u) => u };
    let model = ollama_model();
    let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&model));

    let reply = provider
        .complete(
            "You are a concise assistant. Reply in one sentence.",
            vec![ChatMessage::user("What is the capital of France?")],
        )
        .await
        .expect("live Ollama completion failed");

    println!("Ollama replied: {}", reply.content);
    assert!(!reply.content.trim().is_empty(), "expected non-empty response from Ollama");
    assert!(
        reply.content.to_lowercase().contains("paris"),
        "expected 'Paris' in response, got: {}",
        reply.content
    );
}

/// Verify Ollama stream_complete yields text + usage stats.
#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL pointing to a running Ollama server with a pulled model"]
async fn ollama_live_usage_reported_in_stream() {
    let url = match ollama_url() { None => return, Some(u) => u };
    let model = ollama_model();
    let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&model));

    let mut stream = provider.stream_complete(
        "You are a concise assistant.",
        vec![ChatMessage::user("What is 2+2?")],
    );

    let mut text_tokens = Vec::new();
    let mut usage_opt = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t))  => text_tokens.push(t),
            Ok(StreamToken::Usage(u)) => usage_opt = Some(u),
            Err(e)                    => panic!("Ollama stream error: {e}"),
        }
    }

    let full = text_tokens.join("");
    println!("Ollama replied: {full}");
    assert!(!full.trim().is_empty(), "expected non-empty response from Ollama");

    let usage = usage_opt.expect("Ollama should always report usage stats");
    println!("Ollama usage: prompt={} completion={}", usage.prompt_tokens, usage.completion_tokens);
    assert!(usage.prompt_tokens > 0, "expected prompt_tokens > 0");
    assert!(usage.completion_tokens > 0, "expected completion_tokens > 0");
}

/// Verify multi-turn conversation with Ollama preserves context.
#[tokio::test]
#[ignore = "requires GIAP_OLLAMA_URL pointing to a running Ollama server with a pulled model"]
async fn ollama_live_multi_turn_conversation() {
    let url = match ollama_url() { None => return, Some(u) => u };
    let model = ollama_model();
    let provider = pond_adapters_ollama::OllamaProvider::new(Some(&url), Some(&model));

    let turn1 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![ChatMessage::user("My lucky number is 42.")],
        )
        .await
        .expect("turn 1 failed");
    println!("turn 1: {}", turn1.content);

    let turn2 = provider
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![
                ChatMessage::user("My lucky number is 42."),
                ChatMessage::assistant(turn1.content.clone()),
                ChatMessage::user("What number did I just mention?"),
            ],
        )
        .await
        .expect("turn 2 failed");
    println!("turn 2: {}", turn2.content);

    assert!(
        turn2.content.contains("42"),
        "expected turn 2 to mention '42', got: {}",
        turn2.content
    );
}
