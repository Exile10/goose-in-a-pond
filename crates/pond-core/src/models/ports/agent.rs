use crate::models::domain::model_capabilities::ModelCapabilities;
use crate::models::services::context::prefix_cache::PrefixCacheState;
pub use crate::shared::domain::agent::{
    AgentRequest, AgentResponse, AgentStreamEvent, WarmupPhase,
};
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

    /// The state of this agent's static prompt prefix in the engine's KV
    /// cache, when the agent tracks one (PAI-4 P5).
    ///
    /// Compaction is arithmetic on the window, but moving the prefix costs
    /// seconds of re-prefill — 3.7 s on the Orin, measured — and until this
    /// existed the compaction path had no way to ask whether it was about to
    /// spend that. `PrefixCacheState::posture_of` turns the answer into the
    /// one decision anybody makes with it.
    ///
    /// `None` is the CORRECT implementation for an agent whose engine keeps no
    /// prefix cache, or whose cache it cannot see — every mock, and every HTTP
    /// provider path. It is not "unknown, therefore assume the cheap thing":
    /// `posture_of(None)` is `Warm`, which is the narrowing answer, so an
    /// agent that says nothing gets exactly the behaviour this repository
    /// shipped before P5. Returning `Some` of a fabricated cold state would
    /// spend re-prefills on a conversation that was mid-flight.
    ///
    /// Synchronous and by value: the state is four small `Copy` fields behind
    /// the adapter's own lock, and a snapshot is all any caller can act on —
    /// by the time an `async` read returned, the turn it described would have
    /// moved on.
    fn prefix_cache_state(&self) -> Option<PrefixCacheState> {
        None
    }

    /// Precompile the static prompt prefix into the engine's KV cache, before
    /// the user's first message.
    ///
    /// A cold first turn pays model load plus a multi-thousand-token preamble
    /// prefill (measured: ~12 s on the Mac at 61 tools, ~5 s on the Orin) while
    /// the user watches. The prefix is knowable the moment settings are — so an
    /// implementation runs one throwaway generation at startup or on a model
    /// change, and the first real turn hits the engine's `ReusePrefix` path
    /// instead.
    ///
    /// `voice_mode` must match the surface being warmed: the voice prompt
    /// renders its own section, so a prefix warmed for chat does not serve a
    /// voice session, and vice versa.
    ///
    /// `progress` is called on phase transitions (`Warming`, then exactly one
    /// of `Ready`/`Skipped`/`Failed`). It must be cheap and must not block.
    ///
    /// The default reports `Skipped` and does nothing — the CORRECT
    /// implementation for every agent without a prefix-caching engine (mocks,
    /// HTTP providers). Failure is never propagated: a pond that could not
    /// warm is a pond that behaves exactly as it did before this existed.
    async fn prewarm(
        &self,
        _voice_mode: bool,
        progress: std::sync::Arc<dyn Fn(WarmupPhase) + Send + Sync>,
    ) {
        progress(WarmupPhase::Skipped {
            reason: "this agent keeps no prefix cache".to_string(),
        });
    }
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

    /// An agent that reports no prefix cache must resolve to the WARM posture,
    /// not the cold one. This is the direction that matters: cold is the
    /// permission to recompact, and handing it to every mock and every HTTP
    /// path by default would widen P5 from "recompact when the prefix is
    /// already lost" to "recompact whenever nobody said otherwise".
    #[test]
    fn an_agent_with_no_prefix_cache_resolves_to_the_warm_posture() {
        use crate::models::services::context::prefix_cache::{CachePosture, PrefixCacheState};

        let agent: Box<dyn Agent> = Box::new(NoSessionStoreAgent);
        let state = agent.prefix_cache_state();
        assert!(state.is_none());
        assert_eq!(
            PrefixCacheState::posture_of(state.as_ref()),
            CachePosture::Warm
        );
    }
}
