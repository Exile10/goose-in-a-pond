use anyhow::Result;
use async_trait::async_trait;
use crate::ports::agent::{Agent, AgentRequest, AgentResponse};
use std::collections::HashMap;

/// Mock adapter for the Agent port.
///
/// Echoes back the user's message for testing the workflow loop
/// without a real LLM provider.
pub struct MockAgent;

impl MockAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Agent for MockAgent {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        // Simulate a tiny "thinking" delay
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        Ok(AgentResponse {
            text: format!("Echo: {}", request.message),
            metadata: HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_agent_echoes_input() {
        let agent = MockAgent::new();
        let request = AgentRequest {
            message: "Hello, Pond!".to_string(),
            session_id: "test-session".to_string(),
        };
        let response = agent.chat(request).await.unwrap();
        assert_eq!(response.text, "Echo: Hello, Pond!");
        assert!(response.metadata.is_empty());
    }
}
