//! llamafile LLM provider adapter.
//!
//! llamafile runs a local server on `http://127.0.0.1:8080` that serves a
//! `POST /v1/chat/completions` endpoint compatible with the OpenAI API.
//!
//! This crate implements the GIAP `LlmProvider` port against that endpoint
//! using a plain `reqwest` HTTP client — no Goose dependency, no linker
//! conflicts with Goose's v8/llama-cpp-2 combo on Windows.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::domain::message::{ChatMessage, Role};
use pond_core::ports::provider::LlmProvider;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Default llamafile server URL.
pub const DEFAULT_HOST: &str = "http://127.0.0.1:8080";

/// Model name that llamafile reports in its responses.
pub const DEFAULT_MODEL: &str = "LLaMA_CPP";

// ── OpenAI-compatible request/response types ──────────────────────────────────

#[derive(Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: Vec<OaiMessage<'a>>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct OaiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: OaiResponseMessage,
}

#[derive(Deserialize)]
struct OaiResponseMessage {
    content: String,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct LlamafileProvider {
    client: Client,
    endpoint: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl LlamafileProvider {
    /// Create a provider targeting the given host (e.g. `http://127.0.0.1:8080`).
    /// Defaults to `DEFAULT_HOST` when `host` is `None`.
    pub fn new(host: Option<&str>) -> Self {
        let base = host.unwrap_or(DEFAULT_HOST);
        Self {
            client: Client::new(),
            endpoint: format!("{}/v1/chat/completions", base),
            model: DEFAULT_MODEL.to_string(),
            max_tokens: 1024,
            temperature: 0.7,
        }
    }

    /// Override the maximum number of tokens to generate (default: 1024).
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Override the sampling temperature (default: 0.7).
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = t;
        self
    }
}

#[async_trait]
impl LlmProvider for LlamafileProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        // Build OpenAI-format message array: system first, then conversation
        let mut oai: Vec<OaiMessage> = vec![OaiMessage {
            role: "system",
            content: system_prompt,
        }];
        for m in &messages {
            oai.push(OaiMessage {
                role: match m.role {
                    Role::User      => "user",
                    Role::Assistant => "assistant",
                    Role::System    => "system",
                },
                content: &m.content,
            });
        }

        let body = CompletionRequest {
            model: &self.model,
            messages: oai,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };

        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("llamafile request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("llamafile error {}: {}", status, text));
        }

        let parsed: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("llamafile response parse error: {}", e))?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow!("llamafile returned no choices"))?;

        Ok(ChatMessage::assistant(content))
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }
}
