use crate::models::domain::model_capabilities::ModelCapabilities;
pub use crate::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
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

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>>;

    /// Runtime capabilities of the model backing this agent.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    /// Call a tool directly by its fully-qualified name (e.g. "giap-weather__get_current_weather").
    ///
    /// Used as a fallback when the model emits tool calls as text markup
    /// (`<|tool_call>...<tool_call|>`) instead of through the structured protocol.
    /// Returns the tool result text, or an error if the tool is not found.
    async fn call_tool(
        &self,
        _session_id: &str,
        _tool_name: &str,
        _args_json: &str,
    ) -> Result<String> {
        Err(anyhow::anyhow!("call_tool not supported by this agent"))
    }

    /// Release any engine-side session state paired with a GIAP session id.
    ///
    /// Deleting a GIAP session removes its `pond_system.db` row and the
    /// `engine_session_map` pairing that named this agent's own session — but
    /// nothing tells the agent itself. An agent that keeps a session store of
    /// its own (messages, usage ledgers, inline image attachments) must
    /// release that state here, or it becomes permanently unreachable garbage
    /// in a store the REST API never reads. Call this BEFORE the pairing is
    /// dropped: once it is gone, the engine-side id cannot be recovered.
    ///
    /// This sits on the delete-session request path, so it must never fail
    /// that request: implementations should log a warning and return rather
    /// than propagate an error. Deleting the pond row is the user's intent;
    /// leaving engine-side garbage behind is strictly better than a 500.
    ///
    /// The default no-op is the CORRECT implementation for an agent with no
    /// session store of its own (every mock, and any future agent that is
    /// genuinely stateless) — there is nothing to release. Only an agent that
    /// owns persistent per-session state needs to override this.
    async fn forget_session(&self, _session_id: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Implements only the two required methods — proving `forget_session`
    /// compiles, is reachable through `dyn Agent`, and needs no session store
    /// (or session ID validity) to run: the whole point of the default.
    struct NoSessionStoreAgent;

    #[async_trait]
    impl Agent for NoSessionStoreAgent {
        async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
            unimplemented!("not exercised by this test")
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            unimplemented!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn forget_session_default_is_a_harmless_no_op_for_a_stateless_agent() {
        let agent: Box<dyn Agent> = Box::new(NoSessionStoreAgent);
        // Neither a real session store nor an existing session id is needed —
        // this must simply return, not panic and not err.
        agent.forget_session("session-that-was-never-real").await;
    }
}
