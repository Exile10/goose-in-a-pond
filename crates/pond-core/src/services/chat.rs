use crate::ports::agent::Agent;
use crate::domain::agent::AgentRequest;
use anyhow::Result;
use std::sync::Arc;

/// Domain Service: ChatService
/// 
/// This service orchestrates business logic using injected ports.
pub struct ChatService {
    agent: Arc<dyn Agent>,
}

impl ChatService {
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self { agent }
    }

    /// Primary entry point for a chat interaction.
    pub async fn chat(&self, message: String, session_id: String) -> Result<String> {
        let request = AgentRequest {
            message,
            session_id,
        };

        let response = self.agent.chat(request).await?;
        
        // Here we could add logic to save to database via a StoragePort
        
        Ok(response.text)
    }
}
