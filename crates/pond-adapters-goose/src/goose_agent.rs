use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, GoosePlatform};
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::ports::agent::{Agent as AgentPort, AgentRequest, AgentResponse};
use std::sync::Arc;

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
            )
            .await?;
        adapter
            .agent
            .update_provider(provider, &session.id)
            .await?;

        Ok(adapter)
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
