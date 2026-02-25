use crate::domain::agent::{AgentRequest, WorkflowEvent, WorkflowState};
use crate::ports::agent::Agent;
use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::sync::Arc;

/// Domain Service: ChatService
///
/// Orchestrates the Wait → Listen → Thinking → Speak workflow loop.
pub struct ChatService {
    agent: Arc<dyn Agent>,
    session_id: String,
}

impl ChatService {
    pub fn new(agent: Arc<dyn Agent>, session_id: String) -> Self {
        Self { agent, session_id }
    }

    /// Single-shot chat (useful for tests and non-interactive callers).
    pub async fn chat_once(&self, message: String) -> Result<String> {
        let request = AgentRequest {
            message,
            session_id: self.session_id.clone(),
        };
        let response = self.agent.chat(request).await?;
        Ok(response.text)
    }

    /// Run the interactive workflow loop on stdin/stdout.
    ///
    /// State machine:
    ///   Wait → Listen → Thinking → Speak → (back to Wait)
    pub async fn run_loop(&self) -> Result<()> {
        let stdin = io::stdin();
        let mut reader = stdin.lock();

        loop {
            // ── Wait ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Wait));
            print!("\n  🟢 Waiting for input (type \"exit\" to quit)\n  > ");
            io::stdout().flush()?;

            // ── Listen ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Listen));
            let mut input = String::new();
            let bytes_read = reader.read_line(&mut input)?;

            // EOF (e.g. piped input ended)
            if bytes_read == 0 {
                self.emit_event(WorkflowEvent::Exit);
                println!("\n  ⏹ End of input.");
                break;
            }

            let input = input.trim().to_string();

            if input.is_empty() {
                continue;
            }

            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                self.emit_event(WorkflowEvent::Exit);
                println!("  👋 Goodbye!");
                break;
            }

            self.emit_event(WorkflowEvent::UserInput(input.clone()));

            // ── Thinking ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Thinking));
            println!("  🤔 Thinking...");

            match self.chat_once(input).await {
                Ok(response_text) => {
                    // ── Speak ──
                    self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Speak));
                    self.emit_event(WorkflowEvent::AgentOutput(response_text.clone()));
                    println!("  🗣 {}", response_text);
                }
                Err(e) => {
                    eprintln!("  ❌ Error: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Hook point for future event subscribers (logging, UI, etc.).
    fn emit_event(&self, event: WorkflowEvent) {
        // For now, just trace-log the event.
        // A channel-based approach can be added later.
        match &event {
            WorkflowEvent::StateChanged(state) => {
                tracing::debug!("Workflow state: {}", state);
            }
            WorkflowEvent::UserInput(text) => {
                tracing::debug!("User input: {}", text);
            }
            WorkflowEvent::AgentOutput(text) => {
                tracing::debug!("Agent output: {}", text);
            }
            WorkflowEvent::Exit => {
                tracing::debug!("Workflow exit requested");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mock_agent::MockAgent;

    #[tokio::test]
    async fn chat_once_returns_echo() {
        let agent = Arc::new(MockAgent::new());
        let service = ChatService::new(agent, "test-session".to_string());
        let result = service.chat_once("Hello!".to_string()).await.unwrap();
        assert_eq!(result, "Echo: Hello!");
    }
}
