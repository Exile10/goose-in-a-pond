use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::ports::agent::{Agent as AgentPort, AgentRequest, AgentResponse};
use std::sync::Arc;

use crate::extension_manager::GiapGooseExtensionManager;

/// Adapter: GooseAdapter
///
/// Implementation of the Agent port using the Goose framework.
pub struct GooseAdapter {
    agent: Arc<GooseAgent>,
}

impl GooseAdapter {
    pub async fn new(provider: Arc<dyn Provider>) -> Result<Self> {
        let session_manager = Arc::new(SessionManager::instance());
        let permission_manager = goose::config::permission::PermissionManager::instance();

        // Use default config from Goose
        let config = AgentConfig::new(
            session_manager,
            permission_manager,
            None,
            goose::config::GooseMode::Auto,
            false,
            GoosePlatform::GooseCli,
        );

        let agent = GooseAgent::with_config(config);

        // Create the adapter first, then set the provider via a session
        let adapter = Self {
            agent: Arc::new(agent),
        };

        // Create a temporary session to set the provider
        let session = adapter
            .agent
            .config
            .session_manager
            .create_session(
                std::env::current_dir()?,
                "init".to_string(),
                goose::session::session_manager::SessionType::User,
                goose::config::GooseMode::default(),
            )
            .await?;
        adapter
            .agent
            .update_provider(provider, &session.id)
            .await?;

        Ok(adapter)
    }

    /// Build a GooseAdapter backed by an OllamaProvider pointing at llamafile on port 8080.
    ///
    /// This is the default factory used by `pond-server --features goose-agent`.
    /// llamafile speaks the same `POST /v1/chat/completions` format as Ollama.
    pub async fn with_llamafile(host: Option<&str>) -> Result<Self> {
        let host = host.unwrap_or("http://127.0.0.1:8080");
        // OllamaProvider reads OLLAMA_HOST from env.
        std::env::set_var("OLLAMA_HOST", host);
        let model_cfg = goose::model::ModelConfig::new_or_fail("llamafile");
        let provider = goose::providers::ollama::OllamaProvider::from_env(model_cfg).await
            .map_err(|e| anyhow!("OllamaProvider init failed: {e}"))?;
        Self::new(Arc::new(provider)).await
    }

    /// Returns an `GiapGooseExtensionManager` that can add/remove/list Goose extensions.
    pub fn extension_manager(&self, session_id: String) -> GiapGooseExtensionManager {
        GiapGooseExtensionManager::new(self.agent.clone(), session_id)
    }

    /// Convenience: add the named builtin extension to the given session.
    pub async fn add_builtin_extension(&self, name: &str, session_id: &str) -> Result<()> {
        let config = ExtensionConfig::Builtin {
            name: name.to_string(),
            description: String::new(),
            display_name: None,
            timeout: None,
            bundled: Some(false),
            available_tools: vec![],
        };
        self.agent
            .add_extension(config, session_id)
            .await
            .map_err(|e| anyhow!("Failed to add builtin extension '{}': {}", name, e))
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let user_message = Message::user().with_text(&request.message);

        let session_config = goose::agents::types::SessionConfig {
            id: request.session_id,
            schedule_id: None,
            max_turns: None,
            retry_config: None,
        };

        let mut stream = self.agent.reply(user_message, session_config, None).await?;

        let mut full_text = String::new();

        while let Some(event_result) = stream.next().await {
            let event = event_result?;
            match event {
                goose::agents::AgentEvent::Message(msg) => {
                    let text = msg.as_concat_text();
                    if !text.is_empty() {
                        full_text.push_str(&text);
                    }
                }
                _ => {} // Handle other events if needed (tool calls, etc)
            }
        }

        if full_text.is_empty() {
            return Err(anyhow!("Received empty response from Goose agent"));
        }

        Ok(AgentResponse {
            text: full_text,
            metadata: std::collections::HashMap::new(),
        })
    }
}
