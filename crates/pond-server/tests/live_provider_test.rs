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

fn llamafile_url()    -> Option<String>              { std::env::var("GIAP_LLAMAFILE_URL").ok() }
fn ollama_url()       -> Option<String>              { std::env::var("GIAP_OLLAMA_URL").ok() }
fn ollama_model()     -> String                      { std::env::var("GIAP_OLLAMA_MODEL").unwrap_or_else(|_| "gemma3:4b".into()) }
fn gguf_model_path()  -> Option<std::path::PathBuf>  { std::env::var("GIAP_GGUF_MODEL_PATH").ok().map(std::path::PathBuf::from) }
fn llamafile_bin()    -> Option<std::path::PathBuf>  { std::env::var("GIAP_LLAMAFILE_BIN").ok().map(std::path::PathBuf::from) }

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

// ═══════════════════════════════════════════════════════════════════
//  GGUF in-process live tests
//
//  Gate: GIAP_GGUF_MODEL_PATH=/absolute/path/to/model.gguf
//
//  Known-good model: gemma-4-E2B-it-Q4_K_M.gguf (at $DATA_DIR/models/gguf/)
//
//  Run:
//    GIAP_GGUF_MODEL_PATH=/path/to/model.gguf \
//      cargo test -p pond-server --test live_provider_test -- --ignored gguf
// ═══════════════════════════════════════════════════════════════════

/// Verify in-process GGUF inference completes a simple request.
///
/// `new_with_data_dir` with an absolute `.gguf` path registers the file
/// directly as `local_path` in Goose's model registry — `data_dir` is unused.
#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_chat_completes() {
    let path = match gguf_model_path() { None => return, Some(p) => p };
    let path_str = path.to_string_lossy().into_owned();

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter
        ::new_with_data_dir(&path_str, std::path::Path::new("/tmp"))
        .await
        .expect("GGUF adapter init failed — check that the model file exists");

    let reply = adapter
        .complete(
            "You are a concise assistant. Reply in one short sentence.",
            vec![ChatMessage::user("What is 2 + 2?")],
        )
        .await
        .expect("GGUF complete() failed");

    println!("GGUF replied: {}", reply.content);
    assert!(!reply.content.trim().is_empty(), "expected non-empty GGUF response");
}

/// Verify GGUF stream_complete yields at least one text token.
#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_stream_yields_tokens() {
    let path = match gguf_model_path() { None => return, Some(p) => p };
    let path_str = path.to_string_lossy().into_owned();

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter
        ::new_with_data_dir(&path_str, std::path::Path::new("/tmp"))
        .await
        .expect("GGUF adapter init failed");

    let mut stream = adapter.stream_complete(
        "You are a helpful assistant.",
        vec![ChatMessage::user("Say hello in one word.")],
    );

    let mut tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t))  => tokens.push(t),
            Ok(StreamToken::Usage(_)) => {}
            Err(e)                    => panic!("GGUF stream error: {e}"),
        }
    }

    assert!(!tokens.is_empty(), "expected at least one text token from GGUF");
    println!("GGUF streamed {} token(s): {}", tokens.len(), tokens.join(""));
}

/// Verify GGUF multi-turn conversation preserves context.
#[cfg(feature = "local-inference")]
#[tokio::test]
#[ignore = "requires GIAP_GGUF_MODEL_PATH pointing to a .gguf file on disk"]
async fn gguf_live_multi_turn_conversation() {
    let path = match gguf_model_path() { None => return, Some(p) => p };
    let path_str = path.to_string_lossy().into_owned();

    let adapter = pond_adapters_local_inference::LocalInferenceLlmAdapter
        ::new_with_data_dir(&path_str, std::path::Path::new("/tmp"))
        .await
        .expect("GGUF adapter init failed");

    let turn1 = adapter
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![ChatMessage::user("My favourite number is 7.")],
        )
        .await
        .expect("GGUF turn 1 failed");
    println!("GGUF turn 1: {}", turn1.content);

    let turn2 = adapter
        .complete(
            "You are a helpful assistant. Keep replies to one sentence.",
            vec![
                ChatMessage::user("My favourite number is 7."),
                ChatMessage::assistant(turn1.content.clone()),
                ChatMessage::user("What number did I just mention?"),
            ],
        )
        .await
        .expect("GGUF turn 2 failed");
    println!("GGUF turn 2: {}", turn2.content);

    assert!(
        turn2.content.contains("7"),
        "expected turn 2 to mention '7', got: {}",
        turn2.content
    );
}

// ═══════════════════════════════════════════════════════════════════
//  Llamafile auto-start live tests
//
//  Gate: GIAP_LLAMAFILE_BIN=/absolute/path/to/model.llamafile
//
//  These tests self-contain the spawn logic so they do NOT depend on
//  pond-server internals (`llamafile_process` is declared in main.rs).
//
//  Run:
//    GIAP_LLAMAFILE_BIN=/path/to/gemma-2-2b-it.Q4_K_M.llamafile \
//      cargo test -p pond-server --test live_provider_test -- --ignored llamafile_autostart
// ═══════════════════════════════════════════════════════════════════

/// RAII guard — kills the llamafile child process on drop.
struct LlamafileGuard(tokio::process::Child);

impl Drop for LlamafileGuard {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// Spawn the given llamafile binary on `port` and poll until ready (60 s max).
/// Uses a port other than 8080 so it does not collide with a running prod instance.
async fn spawn_llamafile_for_test(bin: &std::path::Path, port: u16) -> Option<LlamafileGuard> {
    let child = tokio::process::Command::new(bin)
        .arg("--server")
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .arg("--nobrowser")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let guard = LlamafileGuard(child);
    let client = reqwest::Client::new();

    for _ in 0..60u32 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if client
            .get(format!("http://127.0.0.1:{}", port))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .is_ok()
        {
            return Some(guard);
        }
    }
    None // did not become ready within 60 s
}

/// Verify that auto-starting a llamafile binary and running a streaming chat works.
#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_BIN pointing to a .llamafile binary"]
async fn llamafile_autostart_chat_streams_tokens() {
    let bin = match llamafile_bin() { None => return, Some(p) => p };
    let test_port: u16 = 8081;

    let _guard = spawn_llamafile_for_test(&bin, test_port).await
        .expect("llamafile did not start within 60 s — check GIAP_LLAMAFILE_BIN");

    let url = format!("http://127.0.0.1:{}", test_port);
    let provider = pond_adapters_llamafile::LlamafileProvider::new(Some(&url));

    let mut stream = provider.stream_complete(
        "You are a concise assistant. Reply in one short sentence.",
        vec![ChatMessage::user("What is 3 + 3?")],
    );

    let mut tokens = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(StreamToken::Text(t))  => tokens.push(t),
            Ok(StreamToken::Usage(_)) => {}
            Err(e)                    => panic!("auto-start stream error: {e}"),
        }
    }

    assert!(!tokens.is_empty(), "expected tokens from auto-started llamafile");
    println!("auto-started llamafile replied: {}", tokens.join(""));
    // _guard drops here → process killed
}

/// Verify the guard kills the llamafile process on drop (port stops responding).
#[tokio::test]
#[ignore = "requires GIAP_LLAMAFILE_BIN pointing to a .llamafile binary"]
async fn llamafile_autostart_guard_kills_on_drop() {
    let bin = match llamafile_bin() { None => return, Some(p) => p };
    let test_port: u16 = 8082;
    let client = reqwest::Client::new();

    {
        let _guard = spawn_llamafile_for_test(&bin, test_port).await
            .expect("llamafile did not start");

        // Confirm it is up while the guard is alive
        assert!(
            client
                .get(format!("http://127.0.0.1:{}", test_port))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .is_ok(),
            "expected llamafile to be reachable while guard is alive"
        );
    } // guard dropped here — process killed

    // Give the OS a moment to reap the process
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let still_up = client
        .get(format!("http://127.0.0.1:{}", test_port))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    assert!(!still_up, "expected llamafile to be down after guard drop");
}
