pub use crate::domain::agent::{AgentRequest, AgentResponse};
use anyhow::Result;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("General error: {0}")]
    General(String),
}

/// Driven Port: Agent
///
/// This trait defines the interface for interacting with an AI agent.
#[async_trait]
pub trait Agent: Send + Sync {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse>;
}
