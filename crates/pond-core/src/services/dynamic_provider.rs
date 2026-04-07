//! DynamicProvider — reads `settings.llm_provider` on every completion call
//! and delegates to the matching named provider so that web-UI provider changes
//! take effect immediately without a server restart.
//!
//! Token budget and temperature are baked into each provider at server startup.
//! Changing those settings still requires a restart (architectural limitation:
//! `LlmProvider::complete` does not accept per-call overrides).

use crate::domain::message::ChatMessage;
use crate::ports::provider::LlmProvider;
use crate::ports::settings::SettingsRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Reads `Settings.llm_provider` on every call and delegates to the matching
/// provider from an internal registry.  If the configured name is not found,
/// falls back to `fallback_name`.
pub struct DynamicProvider {
    providers:     HashMap<String, Arc<dyn LlmProvider>>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    fallback_name: String,
}

impl DynamicProvider {
    /// `providers` — a map from provider name (e.g. `"llamafile"`, `"ollama"`) to
    /// a concrete `LlmProvider` implementation.  At least one entry matching
    /// `fallback_name` must be present.
    pub fn new(
        providers:     HashMap<String, Arc<dyn LlmProvider>>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
        fallback_name: impl Into<String>,
    ) -> Self {
        Self { providers, settings_repo, fallback_name: fallback_name.into() }
    }

    fn pick(&self, name: &str) -> Arc<dyn LlmProvider> {
        self.providers
            .get(name)
            .or_else(|| self.providers.get(&self.fallback_name))
            .cloned()
            .unwrap_or_else(|| {
                // Should never happen when callers register at least the fallback.
                panic!("DynamicProvider: no provider registered for '{}' and fallback '{}' is also missing", name, self.fallback_name)
            })
    }
}

#[async_trait]
impl LlmProvider for DynamicProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let provider = self.pick(&settings.llm_provider);
        provider.complete(system_prompt, messages).await
    }

    fn model_name(&self) -> String {
        "dynamic".to_string()
    }
}
