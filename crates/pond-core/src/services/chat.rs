use crate::domain::agent::{AgentRequest, WorkflowEvent, WorkflowState};
use crate::domain::message::ChatMessage;
use crate::domain::session::SessionMessage;
use crate::ports::agent::Agent;
use crate::ports::session_storage::SessionStorage;
use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use uuid::Uuid;

/// Domain Service: ChatService
///
/// Orchestrates the Wait → Listen → Thinking → Speak workflow loop.
/// Also persists messages to session storage for conversation history.
pub struct ChatService {
    agent: Arc<dyn Agent>,
    session_id: String,
    session_storage: Arc<dyn SessionStorage>,
}

impl ChatService {
    pub fn new(
        agent: Arc<dyn Agent>,
        session_id: String,
        session_storage: Arc<dyn SessionStorage>,
    ) -> Self {
        Self {
            agent,
            session_id,
            session_storage,
        }
    }

    /// Single-shot chat (useful for tests and non-interactive callers).
    pub async fn chat_once(&self, message: String) -> Result<String> {
        // Persist the user message
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        let request = AgentRequest {
            message,
            session_id: self.session_id.clone(),
        };
        let response = self.agent.chat(request).await?;

        // Persist the assistant response
        let assistant_msg = ChatMessage::assistant(response.text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

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
    use crate::services::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn chat_once_returns_echo() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        let result = service.chat_once("Hello!".to_string()).await.unwrap();
        assert_eq!(result, "Echo: Hello!");
    }

    #[tokio::test]
    async fn chat_persists_messages_to_storage() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.chat_once("First message".to_string()).await.unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 2); // User message + Assistant response
        assert_eq!(messages[0].message.content, "First message");
        assert!(messages[1].message.content.contains("First message"));
    }

    #[tokio::test]
    async fn chat_messages_persist_across_iterations() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());

        // First iteration
        service.chat_once("Message 1".to_string()).await.unwrap();

        // Second iteration
        service.chat_once("Message 2".to_string()).await.unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 4); // 2 iterations × 2 messages each
        assert_eq!(messages[0].message.content, "Message 1");
        assert_eq!(messages[2].message.content, "Message 2");
    }
}
