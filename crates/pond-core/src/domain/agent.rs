use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub message: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub text: String,
    pub metadata: std::collections::HashMap<String, String>,
}

/// The four states of the workflow loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowState {
    /// Idle – ready for the next interaction.
    Wait,
    /// Receiving user input.
    Listen,
    /// Processing the input through the agent.
    Thinking,
    /// Delivering the agent's response.
    Speak,
}

impl fmt::Display for WorkflowState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait => write!(f, "Wait"),
            Self::Listen => write!(f, "Listen"),
            Self::Thinking => write!(f, "Thinking"),
            Self::Speak => write!(f, "Speak"),
        }
    }
}

/// Events emitted during state transitions (for UI / logging hooks).
#[derive(Debug, Clone)]
pub enum WorkflowEvent {
    StateChanged(WorkflowState),
    UserInput(String),
    AgentOutput(String),
    Exit,
}
