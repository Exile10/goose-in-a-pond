use pond_core::ports::agent::{Agent as AgentPort, AgentRequest, AgentResponse};
use goose::agents::agent::{Agent as GooseAgent, AgentConfig, GoosePlatform};
use goose::conversation::message::Message;
use goose::session::SessionManager;
use goose::config::Config;
use goose::providers::base::Provider;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use futures::StreamExt;

/// Adapter: GooseAdapter
/// 
/// Implementation of the Agent port using the Goose framework.
pub struct GooseAdapter {
    agent: Arc<GooseAgent>,
}

impl GooseAdapter {
    pub async fn new(provider: Arc<dyn Provider>) -> Result<Self> {
        let session_manager = Arc::new(SessionManager::instance());
        let permission_manager = Arc::new(goose::config::permission::PermissionManager::instance());
        
        // Use default config from Goose
        let config = AgentConfig::new(
            session_manager,
            permission_manager,
            None,
            goose::config::GooseMode::Auto,
            false,
            GoosePlatform::GooseCli,
        );

        let mut agent = GooseAgent::with_config(config);
        
        // Set the provider
        *agent.provider.lock().await = Some(provider);

        Ok(Self {
            agent: Arc::new(agent),
        })
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let user_message = Message::user().with_text(&request.message);
        
        // In this simplified adapter, we create a session config per request
        // or we could manage persistent sessions. For now, one-off.
        let session_config = goose::agents::types::SessionConfig {
            id: request.session_id,
            working_dir: std::env::current_dir()?,
        };

        let mut stream = self.agent.reply(user_message, session_config, None).await?;
        
        let mut full_text = String::new();
        
        while let Some(event_result) = stream.next().await {
            let event = event_result?;
            match event {
                goose::agents::agent::AgentEvent::Message(msg) => {
                    if let Some(text) = msg.text() {
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
