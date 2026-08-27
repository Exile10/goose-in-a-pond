//! Bridges pond-core's `LlmProvider` port to Goose's native `Provider` trait
//! (#132 Milestone 4), so `chat_provider = "mesh"` can drive real chat
//! through `GooseAdapter` — not just the `GET /api/v1/test` diagnostic probe
//! `pond-adapters-mesh-inference` was wired into first.
//!
//! Deliberately the *inverse* direction of `provider_adapter.rs`'s
//! `GooseProviderAdapter` (which wraps a Goose `Provider` as an
//! `LlmProvider`) — this wraps an `LlmProvider` as a Goose `Provider`.
//!
//! **No MCP tool-calling over mesh, by construction, not by oversight.**
//! Goose bakes `tools: &[Tool]` into every `stream()` call and expects
//! tool-call requests back in the response; `LlmProvider` has no tools
//! parameter at all (confirmed by `GooseProviderAdapter::complete`, which
//! already hardcodes `&[]` for the same reason). Extending `LlmProvider`
//! itself would ripple into every other adapter (ollama, llamafile, local) —
//! real scope, not this milestone. `tools` is silently dropped here at
//! debug level, not warned — GIAP's `giap-draft` extension is always-on, so
//! tools are present on nearly every real turn, and a per-request warning
//! for expected, by-design behavior would just be noise.
//!
//! Dropping `tools` used to be the whole story, and it left the borrowed
//! model holding a `system` prompt built as if those tools worked (Goose
//! names them there regardless of the separate structured argument). The
//! model would spend its answer reasoning about which tool to reach for, or
//! announcing that none were needed, instead of just answering — a real
//! chat turn returning "no tools or memory are needed for this question"
//! instead of an answer is this exact failure.
//!
//! [`NO_TOOLS_OVER_MESH_NOTICE`] tells the model plainly that the tools it
//! was just described no longer exist this turn — appended to the LAST
//! message, not `system`. That is not incidental: `answer_contract()`
//! (`pond-core`) is deliberately placed last inside `<system-context>`,
//! immediately before `<user-message>`, on the documented finding that an
//! instruction surviving to generation depends on how close it sits to
//! it — `system` comes first in the prompt and is exactly the position that
//! decays worst. A turn with no tools offered in the first place needs no
//! override, so `messages` reaches the peer byte-for-byte in that case.
//!
//! Measured against a real mesh peer (a slower, Jetson-class lender): even
//! trivial turns like "say hi" can still produce an empty first attempt, and
//! `system` itself is lean (~2.3K chars for 62 tools — the tool JSON schemas
//! live entirely in the dropped structured `tools` argument, never baked
//! into prompt text). The failure is behavioral, not prompt size: a bare
//! completion call has none of Goose's harness holding the model to a clean
//! final answer, so nothing here should be mistaken for a fix to that —
//! only for making the one instruction mesh depends on survive as well as
//! this codebase already knows how.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use goose::conversation::message::Message;
use goose::providers::base::{MessageStream, Provider, ProviderUsage, Usage};
use goose_providers::errors::ProviderError;
use goose_providers::model::ModelConfig;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::{LlmProvider, StreamToken};
use rmcp::model::Tool;

/// Appended to the last message whenever `tools` is non-empty and about to
/// be dropped, so the borrowed model is told plainly that the tools it was
/// just described no longer work this turn — instead of silently
/// discovering it mid-answer and narrating that discovery instead of
/// replying.
///
/// Spelled out negatively as well as positively (not just "answer plainly"
/// but "do not use tags / do not narrate") because both leaked failures
/// observed were about FORM, not just content: one echoed `<answer-contract>`
/// verbatim with the question stuffed inside, the other narrated a decision
/// about tools instead of making one.
const NO_TOOLS_OVER_MESH_NOTICE: &str = "\n\n(Tool calls and memory search are not available for \
this response — it is running on a borrowed peer over the mesh. Answer directly and briefly, in \
plain prose. Do not call a tool, do not describe deciding whether one is needed, and do not use or \
repeat any angle-bracket tags — write only the answer itself.)";

pub struct MeshProvider {
    inner: Arc<dyn LlmProvider>,
}

impl MeshProvider {
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }

    /// Goose has no `System`/`Tool` message role at the type level — mirrors
    /// `GooseProviderAdapter::from_goose_message`'s existing treatment
    /// exactly (`provider_adapter.rs`), just run in the opposite direction.
    fn from_goose_message(msg: &Message) -> ChatMessage {
        let role = match msg.role {
            rmcp::model::Role::User => Role::User,
            rmcp::model::Role::Assistant => Role::Assistant,
        };
        ChatMessage {
            role,
            content: msg.as_concat_text(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[async_trait]
impl Provider for MeshProvider {
    fn get_name(&self) -> &str {
        "mesh"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        let mut chat_messages: Vec<ChatMessage> =
            messages.iter().map(Self::from_goose_message).collect();
        let system = system.to_string();
        if !tools.is_empty() {
            tracing::debug!(
                tool_count = tools.len(),
                "mesh provider: dropping tools — MCP tool-calling is not available over the mesh yet"
            );
            // `system` was built assuming the tools listed there actually work —
            // it names all `tools.len()` of them and invites the model to use
            // them. Silently dropping only the structured `tools` argument left
            // that invitation standing with nothing behind it: the borrowed
            // model would reason out loud about which tool to reach for, or
            // announce that none were needed, instead of just answering,
            // because as far as its prompt is concerned they still exist.
            //
            // Appended to the LAST message, not `system` — see the module
            // docs on why position matters this much for a small model.
            // `messages` is never empty for a real turn (it always carries at
            // least the current user turn), but an empty conversation falls
            // back to `system` rather than silently dropping the notice.
            match chat_messages.last_mut() {
                Some(last) => last.content.push_str(NO_TOOLS_OVER_MESH_NOTICE),
                None => {
                    chat_messages.push(ChatMessage::user(NO_TOOLS_OVER_MESH_NOTICE.trim_start()))
                }
            }
        }
        // Owned clones moved into the generator below so the returned stream
        // is 'static (Goose's `MessageStream` alias carries no lifetime) —
        // `self.inner.stream_complete(...)` itself returns a stream borrowing
        // `&self`, which would not outlive this method call otherwise.
        let inner = self.inner.clone();

        let stream = async_stream::stream! {
            let mut token_stream = inner.stream_complete(&system, chat_messages);
            while let Some(item) = token_stream.next().await {
                match item {
                    Ok(StreamToken::Text(text)) => {
                        yield Ok((Some(Message::assistant().with_text(&text)), None));
                    }
                    Ok(StreamToken::Usage(stats)) => {
                        let usage = Usage {
                            input_tokens: Some(stats.prompt_tokens as i32),
                            output_tokens: Some(stats.completion_tokens as i32),
                            total_tokens: Some((stats.prompt_tokens + stats.completion_tokens) as i32),
                            ..Default::default()
                        };
                        yield Ok((None, Some(ProviderUsage::new("mesh".to_string(), usage))));
                    }
                    Err(err) => {
                        yield Err(ProviderError::ExecutionError(err.to_string()));
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::mocks::mock_provider::MockProvider;

    fn user_message(text: &str) -> Message {
        Message::user().with_text(text)
    }

    #[test]
    fn from_goose_message_translates_user_and_assistant() {
        let user = Message::user().with_text("hello");
        let translated = MeshProvider::from_goose_message(&user);
        assert_eq!(translated.role, Role::User);
        assert_eq!(translated.content, "hello");

        let assistant = Message::assistant().with_text("hi there");
        let translated = MeshProvider::from_goose_message(&assistant);
        assert_eq!(translated.role, Role::Assistant);
        assert_eq!(translated.content, "hi there");
    }

    /// `MockProvider`'s `stream_complete` uses the `LlmProvider` trait's
    /// default (text-only, never yields `StreamToken::Usage`), so it can't
    /// exercise the usage-translation path — this stub emits both, mirroring
    /// what a real streaming provider (or `MeshInferenceProvider`'s own
    /// responder) actually sends.
    struct StreamingStubProvider;

    #[async_trait]
    impl LlmProvider for StreamingStubProvider {
        async fn complete(
            &self,
            _system_prompt: &str,
            _messages: Vec<ChatMessage>,
        ) -> anyhow::Result<ChatMessage> {
            Ok(ChatMessage::assistant("hi there"))
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }

        fn stream_complete<'a>(
            &'a self,
            _system_prompt: &'a str,
            _messages: Vec<ChatMessage>,
        ) -> pond_core::models::ports::provider::TokenStream<'a> {
            Box::pin(async_stream::stream! {
                yield Ok(StreamToken::Text("hi ".to_string()));
                yield Ok(StreamToken::Text("there".to_string()));
                yield Ok(StreamToken::Usage(pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    reasoning_tokens: None,
                }));
            })
        }
    }

    #[tokio::test]
    async fn stream_yields_text_deltas_then_usage() {
        let provider = MeshProvider::new(Arc::new(StreamingStubProvider));
        let cfg = ModelConfig::new("mesh");
        let mut stream = provider
            .stream(&cfg, "You are helpful.", &[user_message("hi")], &[])
            .await
            .unwrap();

        let mut texts = Vec::new();
        let mut saw_usage = false;
        while let Some(item) = stream.next().await {
            let (message, usage) = item.unwrap();
            if let Some(msg) = message {
                texts.push(msg.as_concat_text());
            }
            if let Some(usage) = usage {
                assert_eq!(usage.usage.input_tokens, Some(3));
                assert_eq!(usage.usage.output_tokens, Some(2));
                saw_usage = true;
            }
        }
        // Each StreamToken::Text is its own delta, in order — not accumulated.
        assert_eq!(texts, vec!["hi ".to_string(), "there".to_string()]);
        assert!(saw_usage, "expected a terminal usage item");
    }

    /// Records the `system_prompt` and `messages` it was called with, so
    /// tests can assert on what actually reached the "model" rather than
    /// just that the call succeeded.
    struct CapturingProvider {
        seen_system: std::sync::Mutex<Option<String>>,
        seen_messages: std::sync::Mutex<Option<Vec<ChatMessage>>>,
    }

    impl CapturingProvider {
        fn new() -> Self {
            Self {
                seen_system: std::sync::Mutex::new(None),
                seen_messages: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for CapturingProvider {
        async fn complete(
            &self,
            system_prompt: &str,
            messages: Vec<ChatMessage>,
        ) -> anyhow::Result<ChatMessage> {
            *self.seen_system.lock().unwrap() = Some(system_prompt.to_string());
            *self.seen_messages.lock().unwrap() = Some(messages);
            Ok(ChatMessage::assistant("ok"))
        }

        fn model_name(&self) -> String {
            "capturing".to_string()
        }
    }

    /// The exact failure this notice exists for: the prompt names tools that
    /// `tools: &[Tool]` is about to make non-functional. Without the notice,
    /// the borrowed model reasons about — or announces — tool use that can
    /// never happen, instead of just answering.
    ///
    /// Appended to the LAST message, not `system` — see the module docs on
    /// why: an instruction survives a small model's attention better the
    /// closer it sits to generation, and `system` is the position furthest
    /// from it.
    #[tokio::test]
    async fn a_nonempty_tools_list_gets_a_no_tools_notice_appended_to_the_last_message() {
        let provider = Arc::new(CapturingProvider::new());
        let mesh_provider = MeshProvider::new(provider.clone());
        let cfg = ModelConfig::new("mesh");
        let tool = Tool::new("device_control", "control a device", serde_json::Map::new());

        let mut stream = mesh_provider
            .stream(
                &cfg,
                "You are helpful. You have access to: device_control.",
                &[user_message("turn off the lights")],
                std::slice::from_ref(&tool),
            )
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let seen_system = provider.seen_system.lock().unwrap().clone().unwrap();
        assert_eq!(
            seen_system, "You are helpful. You have access to: device_control.",
            "system must reach the peer unmodified — the notice belongs on the last message"
        );

        let seen_messages = provider.seen_messages.lock().unwrap().clone().unwrap();
        let last = seen_messages.last().expect("at least one message");
        assert!(
            last.content.starts_with("turn off the lights"),
            "the original ask must survive: {}",
            last.content
        );
        assert!(
            last.content.contains("not available for this response"),
            "expected the no-tools-over-mesh notice on the last message, got: {}",
            last.content
        );
    }

    /// A turn with no tools offered in the first place needs no override —
    /// both `system` and the last message reach the peer byte-for-byte.
    #[tokio::test]
    async fn an_empty_tools_list_leaves_the_conversation_untouched() {
        let provider = Arc::new(CapturingProvider::new());
        let mesh_provider = MeshProvider::new(provider.clone());
        let cfg = ModelConfig::new("mesh");

        let mut stream = mesh_provider
            .stream(&cfg, "You are helpful.", &[user_message("hi")], &[])
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let seen_system = provider.seen_system.lock().unwrap().clone().unwrap();
        assert_eq!(seen_system, "You are helpful.");

        let seen_messages = provider.seen_messages.lock().unwrap().clone().unwrap();
        assert_eq!(seen_messages.last().unwrap().content, "hi");
    }

    #[tokio::test]
    async fn stream_drops_tools_without_erroring() {
        let provider = MeshProvider::new(Arc::new(MockProvider::new()));
        let cfg = ModelConfig::new("mesh");
        let tool = Tool::new("device_control", "control a device", serde_json::Map::new());
        let result = provider
            .stream(
                &cfg,
                "sys",
                &[user_message("hi")],
                std::slice::from_ref(&tool),
            )
            .await;
        assert!(result.is_ok(), "a non-empty tools list must not error");
    }

    #[tokio::test]
    async fn stream_propagates_inner_errors() {
        struct FailingProvider;
        #[async_trait]
        impl LlmProvider for FailingProvider {
            async fn complete(
                &self,
                _system_prompt: &str,
                _messages: Vec<ChatMessage>,
            ) -> anyhow::Result<ChatMessage> {
                Err(anyhow::anyhow!("no peer available"))
            }
            fn model_name(&self) -> String {
                "failing".to_string()
            }
        }

        let provider = MeshProvider::new(Arc::new(FailingProvider));
        let cfg = ModelConfig::new("mesh");
        let mut stream = provider
            .stream(&cfg, "sys", &[user_message("hi")], &[])
            .await
            .unwrap();

        let first = stream.next().await.expect("expected one item");
        assert!(matches!(first, Err(ProviderError::ExecutionError(_))));
    }
}
