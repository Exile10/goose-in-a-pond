use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort;
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::ports::agent::{
    Agent as AgentPort, AgentRequest, AgentResponse, AgentStreamEvent,
};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::token_counter::TokenCounter as PondTokenCounter;
use pond_core::models::services::context::context_budget::CompactionProfile;
use pond_core::models::services::context::context_governor::{
    ContextGovernor, ContextInputs, WindowResolution,
};
use pond_core::models::services::context::prefix_cache::{InvalidationReason, PrefixCacheState};
use pond_core::models::services::context::token_counting::HeuristicTokenCounter;
use pond_core::models::services::prompt_builder::build_prompt_partition;
use pond_core::prompts::PromptState;
use pond_core::user_data::domain::memory::{cosine_similarity, MemoryFragment};
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::extension_manager::GiapGooseExtensionManager;
use crate::giap_registration::registered_extensions;
use pond_core::user_data::domain::profile::ProfileScope;

/// The template used when the DB has no row for the current `prompt_style`.
///
/// This is the shipped `balanced` template, not a separate string. It used to be
/// a hand-written Markdown-headed prompt that predated the tag skeleton, was
/// covered by none of the guards in `pond_core::prompts` — no covertness rule,
/// no tool licence, no `<tool-failure>` — and drifted further every time the
/// real templates were edited, because nothing tied them together.
///
/// It is NOT a rare path. Both `chat_stream` and `child_environment` fall back
/// here, so a `prompt_style` naming an absent template renders a delegated
/// subagent's entire prefix from it.
fn fallback_prompt() -> &'static str {
    // `balanced` is the documented default and the fallback for an unknown
    // style in `resolve_builtin_template`, so the two agree by construction.
    pond_core::prompts::builtin_template_content("balanced")
        .map(|(content, _)| content)
        .unwrap_or(pond_core::prompts::PROMPT_BALANCED)
}

/// Verbatim copy of Goose's PRIVATE `MAX_TURNS_MESSAGE`
/// (`goose/crates/goose/src/agents/agent.rs:72`) — the plain assistant text
/// Goose streams when `turns_taken > max_turns`.
///
/// Matching it exactly is the only signal GIAP gets that the loop stopped on its
/// budget rather than on the task being done, and it is what turns that dead
/// sentence into an [`AgentStreamEvent::TurnLimitReached`] with a real continue
/// affordance. The constant is not `pub` upstream, so this copy is the coupling
/// — `goose_cap_message_is_still_verbatim` reads the fork source and fails if a
/// Goose sync rewords it.
/// How many memory candidates to retrieve per injection slot.
///
/// Retrieval breadth and injection width are separate concerns: ranking can only
/// choose well from a pool it can see. Cheap because the semantic search already
/// scores every embedded row server-side and truncates afterwards.
const MEMORY_CANDIDATE_FANOUT: usize = 8;

/// Minimum candidate pool, so a small `agent_memory_limit` still ranks over a
/// meaningful slice of the store rather than the handful written most recently.
const MEMORY_CANDIDATE_FLOOR: usize = 40;

const GOOSE_MAX_TURNS_MESSAGE: &str = "I've reached the maximum number of actions I can do without user input. Would you like me to continue?";

/// Goose's own text when a turn produced nothing at all. Matched verbatim, like
/// [`GOOSE_MAX_TURNS_MESSAGE`], so GIAP can re-engage instead of handing the
/// user a message that only tells them to try again.
const GOOSE_EMPTY_TURN_MESSAGE: &str =
    "The model returned an empty response. Please resend your message to continue.";

/// How many times GIAP re-engages the model after a turn that produced no text
/// and no tool call.
///
/// Deliberately small: each attempt is a full turn, and on-device that is a real
/// wait. Two buys the recovery without turning a bad turn into a minute of
/// silence.
const MAX_EMPTY_TURN_REENGAGEMENTS: usize = 2;

/// Appended to the user message when re-engaging after an empty turn.
///
/// The prompt MUST change between attempts. A local model sampling
/// deterministically answers an unchanged conversation identically, so a retry
/// that alters nothing is a wasted prefill — which is exactly what Goose's own
/// retry did before `GOOSE_MAX_EMPTY_TURN_RETRIES=0` handed this over.
const EMPTY_TURN_STEER: &str = "(Your previous attempt produced only internal reasoning and no reply. \
Do not reason further — either call the tool you already decided on, or write the answer directly.)";

/// Shown once the re-engagement budget is spent, in place of silence.
///
/// Says what the user can actually do about it. The failure is usually specific
/// to how this turn's prompt lands, so rewording or a fresh session both clear
/// it, while resending the same words often will not.
const EMPTY_TURN_EXHAUSTED_MESSAGE: &str =
    "I could not produce a response to that, even after retrying. \
This usually clears if you reword the question — or start a new chat if it keeps happening.";

/// Goose environment knobs GIAP owns, as `(key, Some(value) | None)` where
/// `None` means "unset this key".
///
/// Split out as a pure function for two reasons: it is the decision table worth
/// unit-testing, and its result doubles as the change signature that stops
/// `set_var` from firing on every turn (`set_var` is documented-unsound in a
/// multi-threaded process, so it runs only when something actually changed).
fn goose_env_knobs(
    provider: &str,
    effective_ctx: usize,
    hybrid_compaction: bool,
) -> [(&'static str, Option<String>); 4] {
    let local = matches!(provider, "local" | "gguf");
    [
        // Without this Goose's ModelConfig defaults context_limit to 128K and
        // its compaction targets ~102K, while the real KV cache is 8K (macOS) or
        // 4K (Jetson) — the model hits ContextLengthExceeded long before the
        // threshold and falls into the expensive emergency-compaction path.
        ("GOOSE_CONTEXT_LIMIT", Some(effective_ctx.to_string())),
        // Hybrid compaction: GIAP owns trimming (deterministic, in-turn) and
        // summarization (idle). A threshold >= 1.0 disables Goose's own
        // auto-compaction, which would stall the turn with an LLM summarization
        // pass mid-conversation on-device.
        (
            "GOOSE_AUTO_COMPACT_THRESHOLD",
            hybrid_compaction.then(|| "1.0".to_string()),
        ),
        // Ownership rule: tool-result pruning has exactly one owner. In hybrid
        // mode that owner is GIAP's deterministic trimmer, which truncates tool
        // results head+tail with no model call at all. Goose's tool-pair
        // summarization (default ON) spawns background LLM calls to summarize
        // old tool pairs — on-device that spends the tok/s budget the user is
        // waiting on, to redo work the trimmer already did. Only disabled for
        // the local engine: an HTTP provider's spare capacity is not ours to
        // save, and there the summaries are close to free.
        (
            "GOOSE_TOOL_PAIR_SUMMARIZATION",
            (local && hybrid_compaction).then(|| "false".to_string()),
        ),
        // Empty-turn recovery has exactly one owner, and it is GIAP. Goose's own
        // retry re-sends an unchanged conversation, which a deterministic local
        // model answers identically — three full prefills before the user sees
        // anything. GIAP varies the prompt between attempts instead
        // (EMPTY_TURN_STEER), so Goose should detect the empty turn and hand
        // straight back.
        ("GOOSE_MAX_EMPTY_TURN_RETRIES", Some("0".to_string())),
    ]
}

/// What a session's tool groups are, and what they are allowed to become.
///
/// Two lists rather than one because the difference is the PAI-1 boundary, and
/// collapsing them is how a guest came to be shown a menu of the groups that had
/// just been withheld from it. `loaded` is what is in the prompt now; `permitted`
/// is the ceiling `enable_tool_group` may raise it to and the only list
/// `dormant_groups_note` may advertise from.
struct SessionGroups {
    /// In the prompt for this turn.
    loaded: Vec<String>,
    /// The ceiling. Never widened by anything the model can say.
    permitted: Vec<String>,
}

/// Adapter: GooseAdapter
///
/// Full-capability implementation of the `Agent` port using the Goose framework.
///
/// On every `chat()` call the adapter:
/// 1. Loads `Settings` from DB and fetches the active prompt template.
/// 2. Calls `agent.override_system_prompt()` with the rendered GIAP prompt.
/// 3. Injects active `PromptExtra` records and user `Skill` content as keyed extras.
/// 4. Optionally injects recent memory fragments when `agent_memory_inject = true`.
/// 5. Hot-swaps the Goose provider when `chat_provider` / `chat_model` changes.
/// 6. Auto-loads the `"giap-*"` builtin MCP extensions (once per session).
/// 7. Runs Goose's full agentic loop and returns aggregated text + tool-call metadata.
pub struct GooseAdapter {
    agent: Arc<GooseAgent>,
    session_manager: Arc<SessionManager>,
    settings_repo: Arc<dyn SettingsRepository>,
    template_repo: Arc<dyn PromptTemplateRepository>,
    extras_repo: Arc<dyn PromptExtraRepository>,
    skill_repo: Arc<dyn UserSkillRepository>,
    memory_repo: Arc<dyn MemoryRepository>,
    /// Embedder for the per-turn memory search. When present the injection path
    /// embeds the user message and ranks by cosine similarity; when absent it
    /// falls back to the keyword LIKE search. Optional because the fastembed
    /// adapter can fail to initialise (ONNX Runtime mismatch) or be disabled.
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    /// Device registry — queried per turn to populate PromptState for Jinja2 rendering.
    device_repo: Arc<dyn DeviceRegistry>,
    /// Model catalog — the ONLY way this adapter can reach
    /// `ModelRecord.context_length`, which is rung 3 of the context governor.
    ///
    /// Optional because the CLI one-shot paths build an adapter without one and
    /// a missing catalog row must degrade to the heuristic rather than fail a
    /// turn. But when it is `None` on the serving path, rung 3 is unreachable
    /// and every Ollama model falls back to a substring match on its name —
    /// which is the bug PAI-3 exists to remove, so `main.rs` supplies it.
    model_repo: Option<Arc<dyn ModelRepository>>,
    llamafile_url: String,
    /// GIAP data directory — used to resolve GGUF model paths under
    /// `$data_dir/models/gguf/` for the in-process LocalInferenceProvider.
    data_dir: Option<PathBuf>,
    /// Shared manager for extensions.
    extension_manager: Arc<GiapGooseExtensionManager>,
    /// Tracks the last "chat_provider:chat_model" key we wired into Goose.
    last_provider_key: Mutex<String>,
    /// The provider + model config last wired into Goose, retained so NEW
    /// Goose sessions can be configured without rebuilding the provider.
    /// Goose resolves the model PER-SESSION: `update_provider` persists the
    /// model_config onto exactly one session row, and a session created
    /// afterwards has none — its reply path then falls back to the GLOBAL
    /// goose config (`~/.config/goose/config.yaml` / GOOSE_MODEL), which on a
    /// dev machine can name a long-gone model (seen live: "gemma4:latest").
    current_provider: Mutex<Option<(Arc<dyn Provider>, goose_providers::model::ModelConfig)>>,
    /// Goose sessions already configured (via `update_provider`) with the
    /// `last_provider_key` pair. Cleared on every provider/model change.
    provider_configured_sessions: Mutex<HashSet<String>>,
    /// The `enable_thinking` request-param last stamped onto the ModelConfig.
    /// Tracked separately from `last_provider_key` so a thinking-mode change
    /// re-stamps the config on the RETAINED provider instead of rebuilding it.
    last_thinking_param: Mutex<Option<bool>>,
    /// Signature of the Goose env knobs currently exported, so `set_var` runs
    /// only when a setting actually changed rather than on every turn.
    last_env_signature: Mutex<String>,
    /// Token counter for the trim/replay budget paths, built on first use.
    ///
    /// `None` inside the cell means construction failed and the caller falls
    /// back to the chars/4 heuristic — a worse estimate is not a reason to fail
    /// a turn, and the overshoot-feedback correction still bounds the error.
    token_counter: tokio::sync::OnceCell<Option<Arc<crate::token_counter::TiktokenCounter>>>,
    /// The context window last resolved for the active provider/model, with its
    /// provenance.
    ///
    /// The budget paths (`trim_goose_history`, `hydrate_goose_session`) used to
    /// read `GOOSE_CONTEXT_LIMIT` from the process environment and fall back to
    /// a hardcoded 8192 — a value nobody guaranteed, since it is only exported
    /// as a side effect of `apply_goose_env_knobs` and only when its signature
    /// changes. This field is the same number, owned deliberately: written on
    /// the settings path, read by the budget paths, never round-tripped through
    /// the environment. See `docs/architecture/pai/03-context-governor.md`.
    ///
    /// Cached WITH the provider it was resolved for, since PAI-3 P5: the
    /// prompt-side clamp is a function of the provider class, so a budget path
    /// that has the window but not the provider cannot build the asymmetric
    /// profile and would silently fall back to the symmetric one.
    last_window: Mutex<Option<(String, WindowResolution)>>,
    /// `Settings::compaction_verbatim_days`, cached on the settings path for
    /// the same reason `last_window` is: `trim_goose_history` runs on every
    /// turn, and a settings load per turn is a cost the budget paths
    /// deliberately do not pay. PAI-4 P3.
    last_verbatim_days: Mutex<Option<u32>>,
    /// Per-turn controls for the [`GiapProviderShim`] wrapped around every
    /// provider handed to Goose — GIAP's last-mile veto over the system
    /// prompt, Goose's `<turn-context>` message injection, and the tools list.
    shim_controls: Arc<crate::provider_shim::ShimControls>,
    /// Each live turn's [`DelegationAuthority`], keyed by the ENGINE session id
    /// its tool calls carry. PAI-6 P3.
    ///
    /// Published at the point the turn's allow-set is published to
    /// `shim_controls` and revoked when the turn's stream is dropped, so a
    /// `delegate` tool call can only ever be authorised by a turn that is still
    /// running. Handed to `GooseOrchestrator` so the two read the same map.
    turn_authorities: Arc<pond_core::shared::services::turn_authority::TurnAuthorityRegistry>,
    /// Maps GIAP session IDs → Goose session IDs (Goose auto-generates its own IDs).
    goose_session_map: Mutex<HashMap<String, String>>,
    /// Goose sessions that have already had GIAP builtin extensions loaded.
    /// Extensions are loaded once per session on first use.
    loaded_sessions: Mutex<HashSet<String>>,
    /// Dynamic tool registry — provides tool descriptions for the system prompt.
    /// When `None`, no prose tool list is rendered at all -- builtins reach
    /// the model as native tool schemas regardless.
    tool_registry: Option<Arc<dyn ToolRegistryPort>>,
    /// Tracks which extensions the user explicitly added via the REST API.
    /// These are preserved across turns (not stripped in the extension cleanup loop).
    user_extensions: Arc<tokio::sync::RwLock<HashSet<String>>>,
    /// When true, prompt templates include voice-mode instructions (keep responses
    /// short, conversational, no formatting). Set by the CLI when `--input whisper`.
    voice_mode: std::sync::atomic::AtomicBool,
    /// Runtime capabilities of the currently loaded model.
    model_capabilities: Mutex<pond_core::models::domain::model_capabilities::ModelCapabilities>,
    /// Hash of the last static prefix sent via `override_system_prompt()`.
    /// When the current partition's `prefix_hash` matches this value, the static
    /// prefix has not changed and we skip `override_system_prompt()` — allowing
    /// local inference providers to reuse their KV-cache for the stable portion.
    last_prefix_hash: Mutex<u64>,
    /// PAI-4 P5's cache-age axis: the same prefix `last_prefix_hash` tracks,
    /// plus what it has served and what most recently destroyed it.
    ///
    /// Deliberately alongside rather than folded into `last_prefix_hash`. That
    /// field is read to decide whether to call `override_system_prompt`, on the
    /// hot path, under a lock held for two lines; this one is written from six
    /// places that have nothing else in common and read by the trimmer. Merging
    /// them would put the compaction decision inside the prompt-assembly lock.
    prefix_cache: Mutex<PrefixCacheState>,
    /// GIAP session storage — read-only source of the rolling conversation
    /// summary for the deterministic turn trimmer. Optional: without it the
    /// trimmer still runs, just without a summary splice.
    giap_session_storage:
        Option<Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>>,
    /// Last engine-reported prompt token count per GIAP session — feedback
    /// for the trimmer's chars/4 estimate. OnceLock<Arc<..>> so the 'static
    /// stream closure can hold a handle.
    last_prompt_tokens_arc: std::sync::OnceLock<Arc<Mutex<HashMap<String, u32>>>>,
    /// Cached tool set from the last list_tools() call. Invalidated when
    /// extensions are added/removed. Avoids re-querying all MCP servers every turn.
    cached_tools: tokio::sync::RwLock<Option<std::collections::HashSet<String>>>,
    /// Whether the Goose default extensions have been stripped for this session.
    /// Only needs to happen once, not every turn.
    defaults_stripped: Mutex<HashSet<String>>,
    /// Phase D2 tool selection: GIAP session id -> chosen extension groups.
    ///
    /// Resolved ONCE per session (from its opening message + injected memories)
    /// and then held stable, so the tools JSON — and therefore the local engine's
    /// KV prompt prefix — does not churn between turns. Backed by
    /// `session_tool_groups` in `pond_system.db` so a restart mid-conversation
    /// does not silently drop a group the model enabled for itself.
    session_tool_groups: tokio::sync::RwLock<HashMap<String, Vec<String>>>,
    /// PAI-1: the groups a session may EVER hold, as distinct from the ones it
    /// holds now. GIAP session id -> permitted extension groups.
    ///
    /// The boundary has to exist as its own value because two things read it and
    /// both used to read the wrong list:
    ///
    /// * `dormant_groups_note` was built from `registered_extensions()`, so an
    ///   unidentified speaker was shown `giap-memory`, `giap-vision`,
    ///   `giap-audit` and `giap-context` on a menu that says "call
    ///   enable_tool_group with its name and its tools become available
    ///   immediately". The groups had been withheld from the selection and then
    ///   advertised anyway.
    /// * `enable_group` checked catalog membership and registration only — no
    ///   scope, no denylist. `giap-toolkit` is deliberately NOT on the guest
    ///   denylist (`a_guest_keeps_the_neutral_groups`), so the guest could read
    ///   the menu and take the item.
    ///
    /// Deriving it once, at selection, and letting the hatch widen only *within*
    /// it makes the boundary structural rather than a second check somebody has
    /// to remember. Not persisted: it is a pure function of the registered
    /// extensions and the turn's scope, so it is recomputed on a restore rather
    /// than trusted from disk — a stored boundary is a boundary that can go
    /// stale against a scope that changed.
    session_permitted_groups: tokio::sync::RwLock<HashMap<String, Vec<String>>>,
    /// Embeddings of the scorable group descriptions, computed on first use.
    /// The descriptions are `&'static str` constants, so one pass is enough for
    /// the process lifetime.
    /// Deliberately NOT `OnceCell<Option<_>>`. See
    /// [`GooseAdapter::group_description_embeddings`] — an `Option` inside the
    /// cell means a failed first attempt is a value the cell keeps forever, which
    /// on the Jetson silently disabled narrowing for the whole process.
    group_embeddings: tokio::sync::OnceCell<Vec<(String, Vec<f32>)>>,
}

/// Hard ceiling on a single buffered reasoning passage, in bytes.
///
/// The coalescer holds at most one passage, and every real block ends the
/// moment the model says anything the user can see. A provider that never
/// produces visible output — broken, hostile, or simply looping — would
/// otherwise grow the buffer for the whole turn. 64 KiB is far past any
/// reasoning block a shipped model emits (a 16k-token context cannot hold one)
/// and is bounded memory rather than a correctness rule, so crossing it splits
/// the passage and logs, instead of dropping it.
const REASONING_BUFFER_LIMIT: usize = 64 * 1024;

/// PAI-5 P1 (granularity). One reasoning passage, assembled from however many
/// pieces the provider chose to send it in.
///
/// The providers disagree about what an `AgentEvent::Message` carrying
/// `Thinking` *means*, and P1 originally assumed they agreed:
///
/// | provider family | shape | source |
/// |---|---|---|
/// | local / gguf (the Jetson headline config) | one message **per token piece** | `goose-local-inference/src/llamacpp/inference_native_tools.rs` calls `push_structured_reasoning` from inside the per-token `\|piece\|` callback |
/// | openai-format HTTP (Ollama, DeepSeek, OpenRouter, vLLM) | one message per streamed delta | `goose-provider-types/src/formats/openai.rs` pushes each chunk's newly-arrived `reasoning_text()` |
/// | google | one message per part | delta-shaped |
/// | anthropic | one message per **complete block** | `formats/anthropic.rs` accumulates `ThinkingDelta` internally and emits once at `content_block_stop` |
/// | any non-streaming response | one message per complete block | `response_to_message` |
///
/// Emitting one `AgentStreamEvent::Thinking` per message therefore rendered a
/// single passage as hundreds of one-fragment `<p>`s in `sections/Chat.tsx`
/// (which appends thinking frames while it concatenates text deltas), with the
/// inter-fragment spacing destroyed by a per-fragment `.trim()`. Buffering
/// fixes that without costing anything on the streaming surface: `Chat.tsx`
/// gates the whole thinking panel on `!msg.streaming`, so nothing was rendered
/// mid-turn anyway.
///
/// The buffer is written only through [`ReasoningCoalescer::push`], which
/// carries the display gate. With `emit` false nothing is ever stored, so a
/// voice turn or a `show_thinking = false` turn holds no reasoning text in
/// memory at all — the gate narrows both the surface and the residency.
#[derive(Default)]
struct ReasoningCoalescer {
    buf: String,
}

impl ReasoningCoalescer {
    /// Append this message's reasoning fragments, **raw**.
    ///
    /// No trimming and no blank-dropping happen here: a fragment that is a lone
    /// `" "` is the space between two words, and dropping it is precisely how
    /// `"the user asked about the light"` became `"the userasked aboutthe
    /// light"`. Normalisation happens once, in [`Self::flush`], at the surface
    /// that actually produces a frame.
    fn push(&mut self, msg: &Message, emit: bool) {
        for fragment in GooseAdapter::reasoning_frames(msg, emit) {
            self.buf.push_str(&fragment);
        }
    }

    /// Whether the buffered passage has outgrown [`REASONING_BUFFER_LIMIT`].
    fn over_cap(&self) -> bool {
        self.buf.len() >= REASONING_BUFFER_LIMIT
    }

    /// Bytes currently buffered — for the over-cap log line only.
    fn len(&self) -> usize {
        self.buf.len()
    }

    /// Take the passage, trimmed once, or `None` if there is nothing readable.
    ///
    /// This is the surface that keeps P1's user-visible claim: no blank frame,
    /// no ciphertext. `RedactedThinking` never enters the buffer (it is dropped
    /// in the lift), and a buffer holding only whitespace trims to empty and
    /// yields nothing rather than a flickering empty paragraph.
    fn flush(&mut self) -> Option<String> {
        let passage = std::mem::take(&mut self.buf);
        let trimmed = passage.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(trimmed.to_string())
    }
}

impl GooseAdapter {
    /// Primary factory — all repos are injected by `pond-server/main.rs`.
    pub async fn new(
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Arc<dyn PromptTemplateRepository>,
        extras_repo: Arc<dyn PromptExtraRepository>,
        skill_repo: Arc<dyn UserSkillRepository>,
        memory_repo: Arc<dyn MemoryRepository>,
        device_repo: Arc<dyn DeviceRegistry>,
        llamafile_url: String,
        data_dir: Option<PathBuf>,
        tool_registry: Option<Arc<dyn ToolRegistryPort>>,
    ) -> Result<Self> {
        let session_manager = Arc::new(SessionManager::instance());
        let permission_manager = goose::config::permission::PermissionManager::instance();

        let config = AgentConfig::new(
            session_manager.clone(),
            permission_manager,
            None,
            GooseMode::Auto,
            // Goose's background session-naming is a full LLM call per session;
            // GIAP derives titles itself (ChatService::ensure_session_title),
            // so that call is pure wasted compute on-device.
            true,
            GoosePlatform::GooseCli,
        );

        let agent = Arc::new(GooseAgent::with_config(config));

        // Ensure a Goose session exists for extension management.
        // Extensions are added/removed on this session; chat sessions inherit them.
        // Try to reuse an existing session, or create a new one.
        let current_dir = std::env::current_dir().unwrap_or_default();
        let ext_session_id = {
            let existing = session_manager.list_sessions().await.unwrap_or_default();
            if let Some(session) = existing.first() {
                // A reused session's `working_dir` is frozen at whatever it was
                // when first created, potentially days/restarts ago from a
                // different cwd. Extension subprocesses spawn with THIS
                // directory, so keep it pinned to the current process's cwd
                // on every startup rather than letting it go stale.
                if session.working_dir != current_dir {
                    if let Err(e) = session_manager
                        .update(&session.id)
                        .working_dir(current_dir.clone())
                        .apply()
                        .await
                    {
                        tracing::warn!("Failed to refresh extension session working_dir: {e}");
                    }
                }
                session.id.clone()
            } else {
                match session_manager
                    .create_session(
                        current_dir.clone(),
                        "giap-extensions".to_string(),
                        goose::session::session_manager::SessionType::User,
                        GooseMode::Auto,
                    )
                    .await
                {
                    Ok(session) => session.id,
                    Err(e) => {
                        tracing::warn!("Failed to create extension session: {e}");
                        "giap-extensions".to_string()
                    }
                }
            }
        };
        tracing::info!("Extension manager bound to session: {ext_session_id}");

        let extension_manager = Arc::new(GiapGooseExtensionManager::new(
            agent.clone(),
            ext_session_id,
        ));

        Ok(Self {
            agent,
            session_manager,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            memory_repo,
            embedding_provider: None,
            device_repo,
            model_repo: None,
            llamafile_url,
            data_dir,
            extension_manager,
            last_provider_key: Mutex::new(String::new()),
            current_provider: Mutex::new(None),
            provider_configured_sessions: Mutex::new(HashSet::new()),
            last_thinking_param: Mutex::new(None),
            last_env_signature: Mutex::new(String::new()),
            token_counter: tokio::sync::OnceCell::new(),
            last_window: Mutex::new(None),
            last_verbatim_days: Mutex::new(None),
            shim_controls: Arc::new(crate::provider_shim::ShimControls::default()),
            turn_authorities: Arc::new(
                pond_core::shared::services::turn_authority::TurnAuthorityRegistry::new(),
            ),
            goose_session_map: Mutex::new(HashMap::new()),
            loaded_sessions: Mutex::new(HashSet::new()),
            tool_registry,
            user_extensions: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            voice_mode: std::sync::atomic::AtomicBool::new(false),
            model_capabilities: Mutex::new(
                pond_core::models::domain::model_capabilities::ModelCapabilities::default(),
            ),
            last_prefix_hash: Mutex::new(0),
            prefix_cache: Mutex::new(PrefixCacheState::new(0, std::time::Instant::now())),
            giap_session_storage: None,
            last_prompt_tokens_arc: std::sync::OnceLock::new(),
            cached_tools: tokio::sync::RwLock::new(None),
            defaults_stripped: Mutex::new(HashSet::new()),
            session_tool_groups: tokio::sync::RwLock::new(HashMap::new()),
            session_permitted_groups: tokio::sync::RwLock::new(HashMap::new()),
            group_embeddings: tokio::sync::OnceCell::new(),
        })
    }

    /// Enable voice mode — prompt templates will include instructions for
    /// short, conversational, TTS-friendly responses.
    pub fn set_voice_mode(&self, enabled: bool) {
        self.voice_mode
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Convenience factory for non-server use (tests, CLI one-shots).
    /// Uses mock repos and connects to llamafile at `host`.
    pub async fn with_llamafile(host: Option<&str>) -> Result<Self> {
        use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
        use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
        use pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository;
        use pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository;
        use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
        use pond_core::user_data::mocks::mock_skill::MockSkillRepository;

        let url = host.unwrap_or("http://127.0.0.1:8080").to_string();
        Self::new(
            Arc::new(MockSettingsRepository::default()),
            Arc::new(MockPromptTemplateRepository::default()),
            Arc::new(MockPromptExtraRepository::default()),
            Arc::new(MockSkillRepository::default()),
            Arc::new(MockMemoryRepository::default()),
            Arc::new(MockDeviceRegistry),
            url,
            None,
            None, // tool_registry — no prose tool list; native schemas still apply
        )
        .await
    }

    // ── PAI-4 P5: prefix-cache bookkeeping ────────────────────────────────
    //
    // Six places in this file already destroy the engine's KV prefix, and
    // until P5 every one of them did so silently. These three helpers are
    // what turns that into a signal the compaction path can read. All are
    // synchronous and drop the guard before returning, so they are safe to
    // call from `async fn`s that later `.await` — the same discipline
    // `last_prefix_hash` follows two fields up.

    /// Record that the prefix was destroyed, and by what.
    fn note_prefix_invalidated(&self, reason: InvalidationReason) {
        let mut state = self.prefix_cache.lock().unwrap_or_else(|e| e.into_inner());
        let served = state.turns_served;
        state.invalidate(reason);
        tracing::debug!(
            target: "giap::trace",
            kind = "prefix_cache_invalidated",
            reason = reason.as_str(),
            turns_served = served,
            "KV prefix invalidated"
        );
    }

    /// Which of the two provider-side reasons a completed swap was (PAI-4 P5).
    ///
    /// `previous_key` is `last_provider_key` as it stood BEFORE the swap, in
    /// the `"provider:model"` form `ensure_provider_current` builds. Reaching
    /// the swap means provider or model differs; only the model half decides
    /// between the two reasons, because only a different model explains a
    /// change in the answers as well as in the prefill.
    ///
    /// A model name may itself contain a colon (`gemma4:e2b`), so the split is
    /// from the LEFT — the provider is the part before the first colon and the
    /// model is everything after. Splitting from the right would compare
    /// `"e2b"` against `"gemma4:e2b"` and call every swap a model swap.
    fn provider_change_reason(previous_key: &str, new_model: &str) -> InvalidationReason {
        match previous_key.split_once(':') {
            Some((_, previous_model)) if previous_model == new_model => {
                InvalidationReason::ProviderRebuilt
            }
            // No previous key at all is first use: nothing was swapped away
            // from, and calling that a model swap is the honest reading.
            _ => InvalidationReason::ModelSwapped,
        }
    }

    /// Record that a new prefix was installed. Does not make it warm — only a
    /// served turn does that; see `PrefixCacheState::rebuilt`.
    fn note_prefix_rebuilt(&self, hash: u64) {
        self.prefix_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rebuilt(hash, std::time::Instant::now());
    }

    /// Record that this turn is being served off the existing prefix.
    fn note_prefix_served(&self) {
        self.prefix_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .serve_turn();
    }

    /// Returns an `GiapGooseExtensionManager` for managing Goose extensions on a session.
    pub fn extension_manager(&self) -> Arc<GiapGooseExtensionManager> {
        self.extension_manager.clone()
    }

    /// Returns the dynamic tool registry, if one was injected.
    pub fn tool_registry(&self) -> Option<Arc<dyn ToolRegistryPort>> {
        self.tool_registry.clone()
    }

    /// Mark an extension name as user-added so it survives the per-turn extension strip.
    pub async fn track_user_extension(&self, name: &str) {
        self.user_extensions.write().await.insert(name.to_string());
        // Invalidate tool cache — new extension means new tools available.
        *self.cached_tools.write().await = None;
        // ...and new tools mean a different tools block inside the static
        // prefix. PAI-4 P5: this is 3.3's named example — two sessions
        // alternating on one model diverge here and re-prefill on every
        // switch, which used to be an invisible tax.
        self.note_prefix_invalidated(InvalidationReason::ToolSetChanged);
    }

    /// Remove an extension from the user-tracking set.
    pub async fn untrack_user_extension(&self, name: &str) {
        self.user_extensions.write().await.remove(name);
        // Invalidate tool cache — removed extension means tools changed.
        *self.cached_tools.write().await = None;
        self.note_prefix_invalidated(InvalidationReason::ToolSetChanged);
    }

    /// The `ExtensionConfig` GIAP registers its builtins under.
    fn builtin_extension_config(name: &str) -> ExtensionConfig {
        ExtensionConfig::Builtin {
            name: name.to_string(),
            description: String::new(),
            display_name: None,
            timeout: Some(600),
            bundled: Some(false),
            available_tools: vec![],
        }
    }

    /// Add a named builtin extension to a Goose session (idempotent).
    pub async fn add_builtin_extension(&self, name: &str, session_id: &str) -> Result<()> {
        let config = Self::builtin_extension_config(name);

        // Also register it with the extension manager so it can be re-enabled if disabled
        self.extension_manager
            .register_config(name.to_string(), config.clone())
            .await;

        self.agent
            .add_extension(config, session_id)
            .await
            .map_err(|e| anyhow!("Failed to add builtin extension '{}': {}", name, e))
    }

    /// Add every GIAP builtin to a session in one pass.
    ///
    /// `Agent::add_extension` is `add_extension_inner` (a `get_session` read for
    /// the working dir) plus `persist_extension_state` (another `get_session`
    /// and a `sessions` UPDATE carrying the whole serialized extension blob) —
    /// about three SQLite round trips each. Fifteen of those, awaited one after
    /// another, sat between a new conversation's first message and its first
    /// token. `add_extensions_bulk` loads them concurrently and persists once.
    ///
    /// Returns the number that loaded, and logs each failure: an extension that
    /// does not load is a real problem, not a degraded mode.
    async fn add_builtin_extensions(&self, names: &[String], session_id: &str) -> usize {
        for name in names {
            self.extension_manager
                .register_config(name.clone(), Self::builtin_extension_config(name))
                .await;
        }

        let configs: Vec<ExtensionConfig> = names
            .iter()
            .map(|n| Self::builtin_extension_config(n))
            .collect();

        match self.agent.add_extensions_bulk(configs, session_id).await {
            Ok(results) => {
                // Report against each result's own `name`. Nothing documents
                // that bulk loading preserves input order, and pairing a failure
                // with the wrong extension is worse than not reporting it.
                let mut loaded = 0usize;
                for result in &results {
                    if result.success {
                        loaded += 1;
                        tracing::debug!("giap extension loaded: {}", result.name);
                    } else {
                        tracing::warn!(
                            "giap extension failed to load: {}: {}",
                            result.name,
                            result.error.as_deref().unwrap_or("unknown error")
                        );
                    }
                }
                loaded
            }
            Err(e) => {
                tracing::warn!("giap extensions failed to load in bulk: {e}");
                0
            }
        }
    }

    /// The registry each live turn's delegation authority is published into.
    ///
    /// PAI-6 P3. `GooseOrchestrator` needs the SAME registry this adapter writes
    /// to — a second one would answer `None` for every spawn and refuse every
    /// delegation, which is the safe direction but is also a feature that does
    /// nothing. Wiring is `GooseOrchestrator::new(runner, adapter.turn_authorities())`.
    pub fn turn_authorities(
        &self,
    ) -> Arc<pond_core::shared::services::turn_authority::TurnAuthorityRegistry> {
        self.turn_authorities.clone()
    }

    /// Resolve (and create if needed) the Goose-internal session for a given GIAP session ID.
    ///
    /// Goose uses its own SQLite sessions.db with auto-generated IDs (`YYYYMMDD_N`).
    /// A GIAP session ID (UUID or arbitrary string) won't exist there unless we create it.
    /// Returns the Goose session ID to use for all subsequent `agent.*` calls.
    ///
    /// Resolution order — process cache, then the PERSISTED pairing, then the
    /// GIAP id used verbatim, then a fresh session. The persisted step is what
    /// stops a pond-server restart from amnesia-wiping a live chat: without it
    /// the map was process-local, every restart landed on the fresh-session
    /// branch, and the model lost the conversation that `pond_system.db` still
    /// held in full. A fresh session created for a GIAP session that HAS pond
    /// history is hydrated from that history before it is used.
    async fn resolve_goose_session(&self, giap_sid: &str) -> String {
        // Fast path: already mapped this session in this process.
        if let Some(gid) = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(giap_sid)
            .cloned()
        {
            return gid;
        }
        // Persisted pairing from a previous run. Re-validated against Goose:
        // its store can be wiped independently of ours, and a dangling id would
        // fail every agent call for the session. The `name` check on top of
        // `is_ok()` guards against a persisted pairing that resolves to a
        // REAL but foreign session (a recycled id, or a row corrupted by a
        // bug elsewhere) — every session this method creates is named after
        // the GIAP id that owns it (see the create-session branch below), so
        // a mismatch means the pairing is pointing at someone else's
        // conversation. Falling through here still resolves correctly for a
        // pairing that predates this check (the id-as-is branch a few lines
        // down re-resolves the very same session id, name unchecked); it only
        // changes behaviour for a pairing that was actually wrong.
        if let Some(storage) = &self.giap_session_storage {
            if let Ok(Some(gid)) = storage.get_engine_session_id(giap_sid).await {
                match self.session_manager.get_session(&gid, false).await {
                    Ok(session) if session.name == giap_sid => {
                        self.goose_session_map
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(giap_sid.to_string(), gid.clone());
                        tracing::debug!(
                            "Restored persisted goose session pairing {giap_sid} -> {gid}"
                        );
                        return gid;
                    }
                    Ok(_) => {
                        tracing::warn!(
                            "Persisted goose session '{gid}' for '{giap_sid}' is named for a \
                             different session — treating the pairing as stale and re-resolving"
                        );
                    }
                    Err(_) => {
                        tracing::info!(
                            "Persisted goose session '{gid}' for '{giap_sid}' is gone — re-creating"
                        );
                    }
                }
            }
        }
        // Try using the GIAP session_id as-is (e.g. if Goose already stored it).
        if self
            .session_manager
            .get_session(giap_sid, false)
            .await
            .is_ok()
        {
            self.remember_goose_session(giap_sid, giap_sid).await;
            return giap_sid.to_string();
        }
        // Create a brand-new Goose session; use the GIAP id as the human name.
        match self
            .session_manager
            .create_session(
                std::env::current_dir().unwrap_or_default(),
                giap_sid.to_string(),
                goose::session::session_manager::SessionType::User,
                GooseMode::Auto,
            )
            .await
        {
            Ok(session) => {
                let gid = session.id.clone();
                self.remember_goose_session(giap_sid, &gid).await;
                // A brand-new engine session for an EXISTING conversation must
                // not start empty. The trimmer cannot cover this: it returns
                // early on an empty conversation, so nothing would ever replay.
                self.hydrate_goose_session(&gid, giap_sid).await;
                gid
            }
            Err(e) => {
                tracing::warn!("Failed to create Goose session for '{}': {e}", giap_sid);
                giap_sid.to_string()
            }
        }
    }

    /// Record a GIAP -> Goose session pairing in the process cache and, when
    /// session storage is wired, durably.
    async fn remember_goose_session(&self, giap_sid: &str, goose_sid: &str) {
        self.goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(giap_sid.to_string(), goose_sid.to_string());
        if let Some(storage) = &self.giap_session_storage {
            if let Err(e) = storage.set_engine_session_id(giap_sid, goose_sid).await {
                // Non-fatal: the process cache still works for this run, the
                // next restart just repeats the hydration path.
                tracing::warn!("Failed to persist goose session pairing: {e}");
            }
        }
    }

    /// Replay a GIAP conversation into a freshly created Goose session.
    ///
    /// Tool rows are dropped on purpose: `pond_system.db` stores tool calls and
    /// results as separate rows and cannot reconstruct a provider-valid
    /// request/response pair, and an orphaned tool response breaks the provider
    /// outright. The user/assistant turns plus the rolling summary are what the
    /// model needs to stay coherent about the conversation.
    ///
    /// Image attachments (phase F2) ARE replayed, but only up to
    /// `MAX_HISTORY_REPLAY_IMAGES`, newest-first; anything beyond that — or
    /// anything whose bytes could not be loaded — becomes a text placeholder,
    /// whose wording is derived from what was actually attached rather than from
    /// what the budget planned. See
    /// `pond_core::models::services::context::image_history` for why the budget
    /// is small (every replayed image makes every subsequent turn in the session
    /// a multimodal turn, and multimodal turns forfeit the engine's KV prefix
    /// cache).
    ///
    /// Budgeting reuses the same `trim_history` the in-turn trimmer uses, so a
    /// long history is cut to the profile's budget exactly the way a live
    /// conversation would have been. Any failure is logged and skipped — losing
    /// the replay degrades the turn, it must never fail it.
    async fn hydrate_goose_session(&self, goose_sid: &str, giap_session_id: &str) {
        use pond_core::models::domain::message::Role as GiapRole;
        use pond_core::models::services::context::turn_trimmer::{plan_replay, TrimRole};

        // PAI-4 P5. Reaching here at all means a FRESH engine session was just
        // created for a conversation that already has history — the definition
        // of a resume, and there is no cache to inherit. Recorded before the
        // storage read so it holds even when the read finds nothing: the engine
        // session is new either way.
        self.note_prefix_invalidated(InvalidationReason::SessionResumed);

        let Some(storage) = &self.giap_session_storage else {
            return;
        };
        // Cap the read before budgeting: a 500-message session would otherwise
        // be loaded in full only to have most of it dropped.
        let history = match storage.get_recent_messages(giap_session_id, 200).await {
            Ok(rows) => rows,
            // A brand-new conversation has no row yet — the common case.
            Err(e) => {
                tracing::debug!("hydrate: no pond history for {giap_session_id}: {e}");
                return;
            }
        };

        // Rows and their message ids, kept in lockstep. The ids are what lets a
        // planned message be joined back to its stored attachments.
        //
        // The two filters below duplicate what `plan_replay` does internally
        // (blank drop, trailing-user pop) ON PURPOSE: `plan_replay` assigns each
        // TrimMessage an `index` by enumerating AFTER those filters, so applying
        // them here first is what makes that index a valid subscript into `ids`.
        // Having already applied them, the copies inside `plan_replay` are
        // no-ops.
        let mut rows: Vec<(TrimRole, String)> = Vec::with_capacity(history.len());
        let mut ids: Vec<String> = Vec::with_capacity(history.len());
        for m in history {
            let role = match m.message.role {
                GiapRole::User => TrimRole::User,
                GiapRole::Assistant => TrimRole::Assistant,
                // Tool rows are dropped (see above); System never reaches history.
                GiapRole::Tool | GiapRole::System => continue,
            };
            if m.message.content.trim().is_empty() {
                continue;
            }
            rows.push((role, m.message.content));
            ids.push(m.id);
        }
        while matches!(rows.last(), Some((TrimRole::User, _))) {
            rows.pop();
            ids.pop();
        }
        if rows.is_empty() {
            return;
        }

        let rolling_summary = storage
            .get_rolling_summary(giap_session_id)
            .await
            .ok()
            .and_then(|(s, _)| s);

        let profile = self.turn_profile(giap_session_id).await;

        // Trailing-user drop, blank filtering, budget cut and summary splice all
        // live in pond-core's `plan_replay` so they are unit-tested there.
        let planned = plan_replay(
            rows,
            &profile,
            rolling_summary.as_deref(),
            self.token_counter().await,
        );
        if planned.is_empty() {
            return;
        }

        // ── Phase F2: decide which historical images get real pixels ──────────
        //
        // Counted only over messages that SURVIVED the budget cut: loading an
        // image for a turn that was trimmed away is pure waste.
        let attachment_counts: std::collections::HashMap<String, usize> = storage
            .list_session_attachments(giap_session_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .fold(std::collections::HashMap::new(), |mut acc, a| {
                *acc.entry(a.message_id).or_insert(0) += 1;
                acc
            });

        let mut replay_images: Vec<usize> = vec![0; planned.len()];
        let mut had_images: Vec<usize> = vec![0; planned.len()];
        if !attachment_counts.is_empty() {
            for (slot, tm) in had_images.iter_mut().zip(planned.iter()) {
                // A spliced summary has no source row (`index == usize::MAX`).
                if let Some(id) = ids.get(tm.index) {
                    *slot = attachment_counts.get(id).copied().unwrap_or(0);
                }
            }
            // Only the user arm of the rebuild below attaches pixels, so an
            // assistant row must not spend a budget it could never use. Its own
            // `had_images` entry is left alone: the index row is still evidence
            // an image was there, and it still earns a placeholder.
            let plannable: Vec<usize> = had_images
                .iter()
                .zip(planned.iter())
                .map(|(n, tm)| match tm.role {
                    TrimRole::Assistant => 0,
                    _ => *n,
                })
                .collect();
            // Shared with the live trimmer's cap so a session that survives a
            // restart neither gains nor loses images.
            replay_images =
                pond_core::models::services::context::image_history::plan_history_images(
                    &plannable,
                );
        }

        let wanted_ids: Vec<String> = planned
            .iter()
            .zip(replay_images.iter())
            .filter(|(_, n)| **n > 0)
            .filter_map(|(tm, _)| ids.get(tm.index).cloned())
            .collect();
        let loaded_images = if wanted_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            storage
                .load_message_images(&wanted_ids)
                .await
                .unwrap_or_default()
        };

        let mut replayed_images_total = 0usize;
        let mut placeholders_total = 0usize;
        let mut replayed: Vec<Message> = Vec::with_capacity(planned.len());
        for (i, tm) in planned.iter().enumerate() {
            // ATTACHMENT REALITY — not the plan — decides the wording and both
            // counters. `replay_images[i]` is only a request, and three things
            // routinely make it larger than what this message can actually
            // carry: `load_message_images` deliberately skips an attachment
            // whose file has gone missing, a storage error collapses the whole
            // load to an empty map, and only the user arm below attaches
            // anything at all. Reading the plan instead let a message announce
            // "the image still shown in this message" while carrying zero
            // images — the exact text-contradicts-reality failure the
            // placeholder exists to prevent.
            let images = match tm.role {
                TrimRole::Assistant => None,
                _ => ids.get(tm.index).and_then(|id| loaded_images.get(id)),
            };
            let attached = replay_images[i].min(images.map_or(0, Vec::len));
            let dropped = had_images[i].saturating_sub(attached);

            // The model must know an image WAS there. Without the placeholder, a
            // turn reading "what colour is this?" with nothing attached invites a
            // confident invention.
            //
            // Which wording depends on whether an image SURVIVED this message:
            // the replay writes the text before the images, so a
            // partially-replayed turn would otherwise read "an image is no
            // longer available" immediately above the image that still is.
            // (`tm.text` comes from durable GIAP history, which stores real
            // attachments and never a placeholder, so there is none to strip
            // here — unlike the live cap, which re-reads its own output.)
            let text = if dropped > 0 {
                placeholders_total += dropped;
                format!(
                    "{}\n{}",
                    tm.text,
                    pond_core::models::services::context::image_history::history_image_placeholder(
                        attached
                    )
                )
            } else {
                tm.text.clone()
            };

            replayed.push(match tm.role {
                TrimRole::Assistant => Message::assistant().with_text(&text),
                // The spliced summary rides a user message, like the trimmer's.
                _ => {
                    let mut msg = Message::user().with_text(&text);
                    // `attached` is already clamped to `images.len()`, so this
                    // yields exactly `attached` images and the counter cannot
                    // drift from what the message holds.
                    for img in images.into_iter().flatten().take(attached) {
                        msg = msg.with_image(&img.data, &img.mime_type);
                        replayed_images_total += 1;
                    }
                    msg
                }
            });
        }
        let replayed_len = replayed.len();

        let conversation = goose::conversation::Conversation::new_unvalidated(replayed);
        match self
            .session_manager
            .replace_conversation(goose_sid, &conversation)
            .await
        {
            Ok(()) => tracing::info!(
                target: "giap::trace",
                kind = "history_hydrate",
                session_id = %giap_session_id,
                goose_session_id = %goose_sid,
                messages = replayed_len,
                summary_spliced = rolling_summary.is_some(),
                images_replayed = replayed_images_total,
                images_placeheld = placeholders_total,
            ),
            Err(e) => tracing::warn!("hydrate: replace_conversation failed: {e}"),
        }
    }

    /// Export the Goose env knobs GIAP owns, but only when they changed.
    ///
    /// These used to be written inside `ensure_provider_current` AFTER its
    /// provider:model fast-path return, so toggling `hybrid_compaction_enabled`
    /// or `context_window_override` did nothing until a model switch or a
    /// restart. They are settings-derived, not provider-derived, so they belong
    /// on the settings path.
    /// Async since PAI-3 P3b: resolving the window now consults the model
    /// catalog, which is a repository read. The one caller already awaits.
    async fn apply_goose_env_knobs(
        &self,
        settings: &pond_core::user_data::domain::settings::Settings,
    ) {
        let resolution = self
            .resolve_window(
                &settings.chat_provider,
                &settings.chat_model,
                settings.context_window_override,
            )
            .await;
        let effective_ctx = resolution.tokens;
        // Stored BEFORE the signature guard below returns early: the budget
        // paths read this field every turn, while the env knobs are only
        // re-exported when something actually changed.
        *self.last_window.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((settings.chat_provider.clone(), resolution));
        // Same placement, same reason: stored BEFORE the signature guard below
        // returns early, because the trimmer reads it every turn while the env
        // knobs are only re-exported when something changed. PAI-4 P3.
        *self
            .last_verbatim_days
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(settings.compaction_verbatim_days);
        let knobs = goose_env_knobs(
            &settings.chat_provider,
            effective_ctx,
            settings.hybrid_compaction_enabled,
        );
        let signature = knobs
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("")))
            .collect::<Vec<_>>()
            .join(";");
        {
            let mut last = self
                .last_env_signature
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == signature {
                return;
            }
            *last = signature.clone();
        }

        // SAFETY: set_var is unsafe in multi-threaded programs per Rust 1.66+,
        // but Goose already calls set_var for OLLAMA_HOST/OLLAMA_TIMEOUT in the
        // same code path, so we follow the existing pattern. The signature guard
        // above keeps this to actual changes rather than every turn.
        #[allow(unused_unsafe)]
        unsafe {
            for (key, value) in &knobs {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
        tracing::info!(
            provider = %settings.chat_provider,
            model = %settings.chat_model,
            effective_ctx,
            knobs = %signature,
            "Applied Goose context/compaction env knobs"
        );
    }

    /// Determine the context limit for Goose's compaction logic.
    ///
    /// For local/GGUF: returns a generous ceiling. The actual KV-cache allocation
    /// is dynamically sized per-request by `estimate_max_context_for_memory()`
    /// inside Goose's inference engine, based on available RAM and the model's
    /// KV cache cost per token. This value just prevents Goose from targeting
    /// its default 128K compaction threshold (unreachable on local models).
    ///
    /// For HTTP providers (Ollama, llamafile): uses the model's reported context
    /// window from capabilities (model name heuristics).
    ///
    /// `override_tokens` (Settings.context_window_override, 0 = unset) wins over
    /// every heuristic when > 0 — except a registry-pinned local context size,
    /// which the engine itself ranks higher (see `registry_context_size`). This
    /// is the escape hatch for deployments whose
    /// real limit is neither the model's max nor the cuda ceiling — e.g. a
    /// Jetson running Ollama with a hand-tuned KV cache, or a tool-heavy GIAP
    /// prompt (~12K tokens for the 57 giap-* tools) that overflows the default
    /// heuristic and forces a compaction loop. The value flows into
    /// GOOSE_CONTEXT_LIMIT and thus into Ollama's `options.num_ctx`, so the
    /// reported limit, the request's num_ctx, and the KV cache all agree.
    ///
    /// The PROMPT-side clamp that used to live here as `prompt_budget_ctx` now
    /// lives in `pond-core` as [`ContextGovernor::prompt_window`], unchanged —
    /// its rationale moved with it.

    /// The context size the engine will ACTUALLY allocate for a local model,
    /// when the registry pins one.
    ///
    /// `context_cap` in goose-local-inference ranks `settings.context_size`
    /// above GOOSE_CONTEXT_LIMIT and above its own memory estimate, so a
    /// stamped value is not a hint — it is the real `n_ctx`. On Jetson,
    /// `apply_jetson_settings` stamps 4096 at every provider init. A larger
    /// GIAP-side number (heuristic or `context_window_override`) therefore
    /// cannot widen the window; it only makes GIAP budget history the engine
    /// has no room for, and the engine responds by logging "Prompt exceeds
    /// context limit" and truncating. Platforms that leave `context_size`
    /// unset — macOS/Metal via `apply_platform_settings` — return `None` and
    /// keep the override/heuristic path below.
    fn registry_context_size(model: &str) -> Option<usize> {
        use goose::providers::local_inference::local_model_registry::get_registry;

        let registry = get_registry().lock().ok()?;
        let entry = registry.get_model(model)?;
        entry.settings.context_size.map(|c| c as usize)
    }

    /// `ModelRecord.context_length` for a provider/model pair, when the catalog
    /// has a row for it.
    ///
    /// This is rung 3 of the context governor and the reason this adapter
    /// carries a `model_repo` at all. It matters most for Ollama, where there
    /// is no registry pin to read and `OllamaCatalogProvider` has already
    /// written the real window it got from `POST /api/show`'s `model_info` map.
    /// Without this lookup that value sits in the database and the adapter
    /// guesses from the model's name instead.
    /// Takes the repo as an argument rather than reading `self.model_repo` so
    /// the lookup — id derivation included — is testable against a stub.
    async fn catalog_context_length(
        repo: Option<&Arc<dyn ModelRepository>>,
        provider: &str,
        model: &str,
    ) -> Option<u32> {
        let repo = repo?;
        let id = ModelRecord::id_for(&ModelCategory::for_chat_provider(provider), model);
        match repo.get_by_id(&id).await {
            Ok(Some(record)) => record.context_length,
            Ok(None) => None,
            Err(e) => {
                // A catalog read failure must not fail a turn: the governor
                // still has three rungs below this one.
                tracing::warn!(model_id = %id, error = %e, "catalog context_length lookup failed");
                None
            }
        }
    }

    /// Resolve the window for a provider/model pair, reading the process-global
    /// model registry for the pinned size and the catalog for the declared one.
    async fn resolve_window(
        &self,
        provider: &str,
        model: &str,
        override_tokens: u32,
    ) -> WindowResolution {
        let pinned = match provider {
            "local" | "gguf" => Self::registry_context_size(model),
            _ => None,
        };
        Self::resolve_window_from(
            self.model_repo.as_ref(),
            provider,
            model,
            override_tokens,
            pinned,
        )
        .await
    }

    /// Everything `resolve_window` does except the process-global registry read
    /// — which is the one part a test cannot stand up. Split out so the WIRING
    /// is covered and not just the precedence: this phase exists because every
    /// caller of `resolve_window_with` passed a hardcoded `None` for the
    /// catalog, and a test that also passes the value by hand would have gone
    /// on passing for as long as that was true.
    ///
    /// The catalog read is skipped whenever a registry pin exists, because rung
    /// 2 wins outright — no point paying for a row the governor will discard.
    async fn resolve_window_from(
        repo: Option<&Arc<dyn ModelRepository>>,
        provider: &str,
        model: &str,
        override_tokens: u32,
        pinned: Option<usize>,
    ) -> WindowResolution {
        let catalog = match pinned {
            Some(_) => None,
            None => Self::catalog_context_length(repo, provider, model).await,
        };
        Self::resolve_window_with(provider, model, override_tokens, pinned, catalog)
    }

    /// Precedence, extracted so it can be tested without the process-global
    /// model registry.
    ///
    /// The precedence itself lives in `pond-core`'s [`ContextGovernor`] so that
    /// the trimmer, telemetry and the monitor cannot drift from it — this is
    /// only the adapter's half, which supplies the registry value the domain
    /// cannot reach.
    fn resolve_window_with(
        provider: &str,
        model: &str,
        override_tokens: u32,
        pinned: Option<usize>,
        catalog: Option<u32>,
    ) -> WindowResolution {
        ContextGovernor::resolve(&ContextInputs {
            provider,
            model,
            override_tokens,
            registry_pinned: pinned,
            catalog_context_length: catalog,
            // Deliberately absent on this path: it is settings-scoped and
            // process-wide, and one session's last turn is not evidence about
            // it. Session-scoped callers pass their own reading.
            engine_reported: None,
            // The adapter's own capability cache is stale on turn one (see
            // `thinking_section_applies`), so the name heuristic is the more
            // reliable answer here. Callers holding a live capability value
            // supply it themselves.
            capability_window: None,
        })
    }

    /// The token counter the budget paths use.
    ///
    /// Prefers the tiktoken-backed counter; falls back to the chars/4 heuristic
    /// if it cannot be built. Neither is exact for a GGUF model — see
    /// `crate::token_counter` for what exactness would cost — so the
    /// overshoot-feedback correction in `turn_trimmer` stays load-bearing
    /// either way.
    async fn token_counter(&self) -> &dyn PondTokenCounter {
        static HEURISTIC: HeuristicTokenCounter = HeuristicTokenCounter;
        match self.token_counter_cell().await {
            Some(c) => c.as_ref(),
            None => &HEURISTIC,
        }
    }

    /// The same counter, as an owned handle.
    ///
    /// The turn stream is an `async_stream` closure with a `'static` bound and
    /// no `&self`, so it cannot borrow the OnceCell. Held behind an `Arc` rather
    /// than constructed per turn because `TiktokenCounter` carries a blake3-keyed
    /// LRU — building a second one would throw the cache away and load
    /// `o200k_base` again.
    async fn token_counter_handle(&self) -> Arc<dyn PondTokenCounter> {
        match self.token_counter_cell().await {
            Some(c) => c.clone(),
            None => Arc::new(HeuristicTokenCounter),
        }
    }

    /// Builds (once) and returns the tiktoken counter, or `None` if it could
    /// not be built. Shared by both accessors so there is exactly one init.
    async fn token_counter_cell(&self) -> &Option<Arc<crate::token_counter::TiktokenCounter>> {
        self.token_counter
            .get_or_init(|| async {
                match crate::token_counter::TiktokenCounter::new().await {
                    Ok(c) => Some(Arc::new(c)),
                    Err(e) => {
                        tracing::warn!("token counter unavailable, falling back to chars/4: {e}");
                        None
                    }
                }
            })
            .await
    }

    /// The RAW resolved window, together with the provider it belongs to.
    ///
    /// Prefers the resolution cached by `apply_goose_env_knobs`, which runs on
    /// the settings path before any turn reaches the trimmer. The settings load
    /// is the cold path only — a session hydrated before the first turn has
    /// configured a provider.
    ///
    /// The provider is not decoration: `ContextGovernor::prompt_window` needs it
    /// to decide whether the preamble is re-prefilled locally, and that decision
    /// is what makes the budget profile asymmetric. Callers wanting budgets want
    /// [`GooseAdapter::turn_profile`], not this — history budgets get the whole
    /// window on purpose and only the preamble is clamped, and conflating the
    /// two silently grows the KV prefix on local providers.
    async fn window_and_provider(&self) -> (String, WindowResolution) {
        if let Some(cached) = self
            .last_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return cached;
        }
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let resolution = self
            .resolve_window(
                &settings.chat_provider,
                &settings.chat_model,
                settings.context_window_override,
            )
            .await;
        (settings.chat_provider, resolution)
    }

    /// PAI-4 P3's verbatim horizon for this turn, or `None` when age weighting
    /// is off.
    ///
    /// Prefers the value cached by `apply_goose_env_knobs`, exactly as
    /// `window_and_provider` does, so the per-turn trim costs no settings read.
    /// The cold path is a session trimmed before the settings path has ever
    /// run; falling back to `Settings::default()` there rather than to "off"
    /// keeps a missing cache from silently disabling the feature.
    /// The guard is released in its own scope BEFORE the settings await. A
    /// `MutexGuard` held across an await makes the whole future non-`Send`, and
    /// `AgentPort`'s boxed futures require `Send` — so the first version of this
    /// compiled nowhere and failed only under
    /// `cargo check -p pond-adapters-goose`, which the fast-crate lint pass does
    /// not run. `window_and_provider` avoids the same trap by cloning out of the
    /// lock and returning early.
    async fn verbatim_horizon(&self) -> Option<std::time::Duration> {
        let cached = *self
            .last_verbatim_days
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let days = match cached {
            Some(days) => days,
            None => {
                self.settings_repo
                    .get()
                    .await
                    .unwrap_or_default()
                    .compaction_verbatim_days
            }
        };
        pond_core::models::services::context::turn_trimmer::verbatim_horizon_from_days(days)
    }

    /// The budget profile for this turn: history from the full resolved window,
    /// preamble from the prompt-side clamp.
    ///
    /// Every budget path in this adapter goes through here since PAI-3 P5. It
    /// used to be the caller's job to remember `ContextGovernor::prompt_window`,
    /// and only two of the four sites did — so `trim_goose_history` and
    /// `hydrate_goose_session` budgeted history against a profile whose preamble
    /// fields had scaled with the whole window, while the preamble they were
    /// budgeting alongside had been built from the 8,192 clamp. One profile per
    /// turn, carrying both windows, is what makes those two agree.
    /// PAI-6 P4. Takes the session id because the reservation is per
    /// CONVERSATION: a subagent is a second claim on the window of the
    /// conversation that spawned it, and another session's turn is entitled to
    /// its whole budget.
    ///
    /// Read here rather than at each consumer so there is exactly one place
    /// that can forget. `turn_profile` is already the single producer of every
    /// budget in this adapter (PAI-3 P5 made it so, after two of four call
    /// sites forgot the prompt-side clamp), and a reservation applied at one of
    /// three call sites would be the same defect again.
    /// PAI-5 P5 is applied here, and this is the only place that reads what
    /// reasoning has actually cost. `None` -- no storage wired, or a read that
    /// failed -- means the anchor stands, which is the value the curve already
    /// shipped, so the failure direction is "no change".
    async fn turn_profile(&self, giap_session_id: &str) -> CompactionProfile {
        let (provider, window) = self.window_and_provider().await;
        let observed = self.observed_reasoning_samples().await;
        Self::profile_for_session(
            &crate::orchestrator::process_device_ledger(),
            &provider,
            window.tokens,
            giap_session_id,
            &observed,
        )
    }

    /// Recent measured `reasoning_tokens`, for PAI-5 P5's reserve.
    ///
    /// Bounded by ROWS SCANNED rather than by samples returned, so a pond with
    /// thinking switched off does not walk its whole history once per turn --
    /// see the port docs. 400 rows is roughly a fortnight of ordinary use and
    /// costs one indexed read against `idx_session_messages_created_at`, which
    /// is noise beside a prefill measured in seconds.
    ///
    /// A failed read answers with no samples rather than propagating: a turn
    /// must not fail because the pond could not work out how much room to leave
    /// itself, and no samples means the anchor.
    async fn observed_reasoning_samples(&self) -> Vec<u32> {
        const SCAN_ROWS: usize = 400;
        let Some(storage) = &self.giap_session_storage else {
            return Vec::new();
        };
        match storage.recent_reasoning_samples(SCAN_ROWS).await {
            Ok(samples) => samples,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "could not read reasoning history; keeping the anchor output reserve"
                );
                Vec::new()
            }
        }
    }

    /// The ledger read and the profile build, with no live adapter around them.
    ///
    /// Split out because the KEY is the part that can be wrong. Reading the
    /// ledger under an id production never writes — a Goose session id where a
    /// GIAP one belongs, which is a mix-up this codebase has already made once
    /// (`resolve_goose_session`) — leaves every reservation reading 0.0 in
    /// production, so a live child shrinks nothing and the parent's next turn
    /// budgets as though it owned the whole window. That mutation was applied
    /// to the line above and the whole suite stayed green: `turn_profile` needs
    /// a settings repo and a provider, so nothing could run it, and the guard
    /// that stood for it only grepped for `reserved_fraction(`.
    ///
    /// Taking the ledger as an argument rather than reaching for the
    /// process-wide one is what makes it runnable — and the ledger stays
    /// process-wide at the one call site, for the reason recorded on
    /// [`process_device_ledger`](crate::orchestrator::process_device_ledger).
    fn profile_for_session(
        ledger: &crate::orchestrator::DeviceLedger,
        provider: &str,
        resolved_window: usize,
        giap_session_id: &str,
        observed_reasoning: &[u32],
    ) -> CompactionProfile {
        Self::profile_for(
            provider,
            resolved_window,
            ledger.reserved_fraction(giap_session_id),
            observed_reasoning,
        )
    }

    /// The pure half of [`GooseAdapter::turn_profile`], split out so the pairing
    /// is testable without a live adapter.
    ///
    /// Which window goes in which slot is the entire phase, and getting it
    /// backwards compiles: passing the clamp as the context window would cap
    /// history at the 8K budget on a 32K box, and passing the raw window as the
    /// prompt window would grow the KV prefix — the thing invariant 1 forbids.
    ///
    /// `reserved_fraction` is PAI-6 P4's live-child claim, and it is applied
    /// AFTER `for_windows` for the same reason: scaling `resolved_window` by it
    /// instead would shrink the preamble allowances and move the prefix, which
    /// costs a full re-prefill to save tokens on a working set the trimmer was
    /// about to cut. `0.0` — the answer on every turn of a pond that never
    /// delegates — returns exactly the profile this function returned before
    /// P4 existed.
    fn profile_for(
        provider: &str,
        resolved_window: usize,
        reserved_fraction: f32,
        observed_reasoning: &[u32],
    ) -> CompactionProfile {
        let profile = CompactionProfile::for_windows(
            resolved_window,
            ContextGovernor::prompt_window(provider, resolved_window),
        )
        .with_history_reserved(reserved_fraction);

        // PAI-5 P5. Applied AFTER `for_windows` for the same reason
        // `with_history_reserved` is: the anchor curve is the input, not the
        // output. `observed_output_reserve` floors at whatever the curve chose,
        // so a pond with too little evidence -- which is every pond on its first
        // day, and every pond with thinking switched off, forever -- gets
        // exactly the profile this function returned before P5 existed.
        let reserve = pond_core::models::services::context::context_budget::observed_output_reserve(
            observed_reasoning,
            profile.output_reserve_tokens,
            profile.context_window_tokens,
        );
        CompactionProfile {
            output_reserve_tokens: reserve,
            ..profile
        }
    }

    /// Whether the ACTIVE model can accept image content.
    ///
    /// For the in-process engine this is the model registry's answer — does
    /// this GGUF declare an mmproj — because the registry is the same thing the
    /// engine gates its multimodal path on. HTTP providers have no registry to
    /// ask, so they get `ModelCapabilities::name_implies_vision`, which mirrors
    /// the registry's own verdicts (E1B excluded) and recognises the vision
    /// models an Ollama install actually serves.
    ///
    /// DECLARED, not downloaded. The encoder is ~941 MB and lands in the
    /// background, so "the bytes exist" flips mid-session; this does not. Two
    /// things depend on that stability: `ModelCapabilities.vision`, and the
    /// `<vision>` section of the system prompt, which sits inside the KV-cached
    /// static prefix. A turn that actually needs the encoder before it has
    /// landed is refused up front in `chat_stream`, with a message that says so.
    fn model_supports_vision(provider: &str, model: &str) -> bool {
        match provider {
            "local" | "gguf" => crate::vision_encoder::declares_vision(model),
            _ => pond_core::models::domain::model_capabilities::ModelCapabilities::name_implies_vision(
                model,
            ),
        }
    }

    /// Whether THIS turn's system prompt should carry the `<vision>` section.
    ///
    /// Model capability is necessary but not sufficient: `capabilities()`
    /// reports `vision = false` in voice mode, and a prompt that asserts a
    /// capability the adapter simultaneously denies is a contradiction the model
    /// pays for. Voice turns are transcribed speech with no attachment path, so
    /// the section is pure prompt cost there — the same trade `thinking` already
    /// makes in voice mode.
    ///
    /// `voice` is the INSTANCE-level flag (CLI `--input whisper`), not the
    /// per-request one, for two reasons. It is the only signal `capabilities()`
    /// can see, so keying off it is what makes the two agree on every input. And
    /// it is fixed for the life of the process, so it cannot flip the static
    /// prefix between turns of one session and forfeit the KV cache — which a
    /// per-request flag, alternating text and voice turns, would.
    fn vision_section_applies(provider: &str, model: &str, voice: bool) -> bool {
        !voice && Self::model_supports_vision(provider, model)
    }

    /// Whether THIS turn's system prompt should carry the `<thinking>` section
    /// (and, in step with it, the engine's `enable_thinking` request-param).
    ///
    /// Voice mode always says no: reasoning tokens waste TTS time and leak as
    /// spoken text if any filter layer misses them.
    ///
    /// In `"auto"` the answer comes from the model NAME, deliberately, and not
    /// from the `model_capabilities` cache. That cache is only refreshed inside
    /// the provider-SWAP branch of `ensure_provider_current`, which runs LATER
    /// in the same turn that builds the prompt. On the first turn of a process
    /// it therefore still holds `ModelCapabilities::default()`, whose `thinking`
    /// is false — so turn 1 rendered a prompt without the section and turn 2
    /// rendered one with it, 78 characters appearing at the top of the static
    /// prefix. That moved `prefix_hash`, and with it the engine's KV
    /// prompt-session prefix, so every session paid one full re-prefill on its
    /// second turn: 3.7 s on the Orin, for the turn the cache exists to make
    /// nearly free. `from_model_name` is pure and cheap, and agrees with the
    /// cache the moment the cache is right.
    fn thinking_section_applies(mode: &str, model: &str, voice: bool) -> bool {
        if voice {
            return false;
        }
        match mode {
            "on" => true,
            "off" => false,
            _ => {
                pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                    model,
                )
                .thinking
            }
        }
    }

    /// Whether THIS turn may emit `AgentStreamEvent::Thinking` frames at all.
    ///
    /// Gated at the PRODUCER rather than at the SSE seam, deliberately. Three
    /// separate consumers read this event — `routes.rs` forwards it on
    /// `/chat/stream` and again on `/agent/chat/stream`, and `pond-server`'s CLI
    /// printer writes it dimmed to stderr — and exactly one of them has ever
    /// consulted `show_thinking`. The CLI printer is also the terminal voice
    /// loop, where PAI-5 invariant 2 says reasoning is unspeakable text. A gate
    /// here is the only one all three inherit, and it is the contract
    /// `AgentStreamEvent::Thinking`'s own doc comment already claims ("only
    /// emitted when `show_thinking` is enabled").
    ///
    /// `voice` is the OR of the instance flag (CLI `--input whisper`) and the
    /// per-request one (the desktop voice pipeline), unlike
    /// `vision_section_applies` — this value never reaches `PromptState`, so it
    /// cannot move the static prefix between turns, and the per-request flag is
    /// the only thing that catches a voice turn on a text-started process.
    ///
    /// On failure the access narrows: a settings load that falls back to
    /// `Settings::default()` gets `show_thinking: false` and emits nothing.
    fn reasoning_frames_enabled(show_thinking: bool, voice: bool) -> bool {
        show_thinking && !voice
    }

    /// Is this a voice turn? The OR of the instance flag and the request flag.
    ///
    /// Extracted from `chat_stream` because a review proved the composition was
    /// the unguarded half of P1's gate. `reasoning_frames_enabled` had a test
    /// for all four of ITS rows, and the source guard pinned the token
    /// `is_voice` — but nothing pinned what `is_voice` MEANT, so dropping
    /// `|| request.voice_mode` left all 108 tests green while reasoning leaked
    /// to every desktop voice turn with `show_thinking` on.
    ///
    /// That mutation is not hypothetical on the shipped product. `main.rs`
    /// constructs the serve-mode adapter with `voice_mode: false` hardcoded, so
    /// `instance` is ALWAYS false in the desktop/server process and `request` is
    /// the only signal that ever goes true — `voice_turn(false, true)` is the
    /// entire defence for the surface most users are on. It is a truth table, so
    /// it gets tested as one.
    fn voice_turn(instance: bool, request: bool) -> bool {
        instance || request
    }

    /// The reasoning text a single agent message contributes to the stream.
    ///
    /// The producers already exist and were being dropped one call short of the
    /// seam. `goose-local-inference` reads llama.cpp's `reasoning_content` delta
    /// and sends `Message::assistant().with_thinking(..)`; the shared OpenAI
    /// format does the same for every HTTP provider that separates reasoning
    /// (Ollama's qwen3 / deepseek-r1 among them). Both arrive here inside
    /// `AgentEvent::Message`, and `as_concat_text()` filters on `as_text()`,
    /// which returns `None` for `Thinking` — so the structured channel was
    /// parsed upstream, thrown away here, and then re-derived downstream by
    /// `ThoughtFilter` scraping tags out of prose.
    ///
    /// `RedactedThinking` is dropped on purpose: its payload is provider
    /// ciphertext (`data`), meaningful only when replayed back to the same
    /// provider. Rendering it would put opaque base64 in the user's thinking
    /// panel, and it is a data-out surface with no readable content to justify
    /// it.
    ///
    /// Nothing is persisted here — PAI-5 P6 owns that, and until it lands
    /// reasoning stays out of `session_messages` and out of any replayed
    /// context.
    ///
    /// **These are fragments, not frames.** The name is kept because the gate
    /// living inside this function is the load-bearing P1 decision and must not
    /// move to the call site, but what comes out is raw and may be a lone
    /// space. It goes into [`ReasoningCoalescer`], which is the only thing that
    /// produces an `AgentStreamEvent::Thinking`.
    fn reasoning_frames(msg: &Message, emit: bool) -> Vec<String> {
        if !emit {
            return Vec::new();
        }
        Self::reasoning_blocks(msg)
    }

    /// The reasoning fragments a message carries, with no display gate.
    ///
    /// Split out of `reasoning_frames` so the *count* and the *display* read
    /// the same content through one extraction, while only the display is gated
    /// on `show_thinking`.
    ///
    /// Deliberately **not** trimmed and **not** filtered for blanks. It used to
    /// be both, back when one message was assumed to be one whole block; on the
    /// delta providers that assumption made every inter-word space disappear.
    /// `ReasoningCoalescer::flush` does the trim, once, on the assembled
    /// passage.
    fn reasoning_blocks(msg: &Message) -> Vec<String> {
        msg.content
            .iter()
            .filter_map(|c| c.as_thinking())
            .map(|t| t.thinking.clone())
            .collect()
    }

    /// Whether this message ends the reasoning passage in flight.
    ///
    /// Defined as "the message carries something **this adapter would yield**":
    /// a tool request, a tool response, or non-empty answer text. That
    /// definition is what makes coalescing correct for every provider family
    /// rather than only the delta ones:
    ///
    /// - A whole-block provider (anthropic, or any non-streaming response) is
    ///   followed by text or a tool call, so its block flushes on its own and
    ///   is emitted verbatim, unmerged. Two of its blocks can only be adjacent
    ///   across a tool round-trip, which itself flushes.
    /// - A delta provider accumulates across the `Usage` and `HistoryReplaced`
    ///   events that interleave its fragments — those are different
    ///   `AgentEvent` variants and never reach the buffer — and lands as one
    ///   passage.
    ///
    /// `Thinking` and `RedactedThinking` never end a block, and neither does a
    /// message carrying only a `SystemNotification`: GIAP yields nothing for
    /// one, so splitting there would be invisible to the user and would cut a
    /// passage in half for no reason.
    fn message_ends_reasoning(msg: &Message) -> bool {
        use goose::conversation::message::MessageContent;
        msg.content.iter().any(|c| {
            matches!(
                c,
                MessageContent::ToolRequest(_) | MessageContent::ToolResponse(_)
            )
        }) || !msg.as_concat_text().is_empty()
    }

    /// PAI-5 P2. Tokens this message spent on reasoning.
    ///
    /// **Ungated on purpose.** The model decodes its reasoning whether or not
    /// `show_thinking` is on, so a count that moved with a display setting would
    /// report a different cost for the same turn depending on a checkbox — and
    /// PAI-5 P5 derives an output reserve from exactly this number. The gate
    /// belongs on the frame, which is a data-out surface; the count is a number
    /// about a turn and carries none of the text.
    ///
    /// **GIAP-derived, and inexact.** No provider GIAP ships reports a reasoning
    /// count: Goose's `Usage` has input/output/total/cache_read/cache_write and
    /// nothing else, and the providers that separate reasoning do it in a
    /// content channel. So this counts the text we received, through the
    /// `TokenCounter` port, whose every implementation says `is_exact() ==
    /// false`. It is not subtracted from the provider's completion count — see
    /// `UsageStats::reasoning_tokens`.
    fn count_reasoning_tokens(msg: &Message, counter: &dyn PondTokenCounter) -> u32 {
        Self::reasoning_blocks(msg)
            .iter()
            .map(|t| counter.count(t) as u32)
            .sum()
    }

    /// Render the `CURRENT CONTEXT` time value for the given moment.
    ///
    /// Text mode keeps plain `HH:MM` digits — unambiguous to read. Voice mode
    /// instead renders a spoken-English phrase (`spoken_time`) so the model
    /// never has to convert digits to words itself: small on-device models
    /// are unreliable at that two-digit conversion and default to a
    /// "H:0<last digit>" pattern (e.g. 5:23 spoken back as "five oh three").
    fn format_current_time(now: chrono::DateTime<chrono::Local>, voice: bool) -> String {
        use chrono::Timelike;
        if voice {
            pond_core::models::services::voice::spoken_time::spoken_time(now.hour(), now.minute())
        } else {
            now.format("%H:%M").to_string()
        }
    }

    /// Append the `<vision>` section to a prompt template when the model can see.
    ///
    /// Appended to the TEMPLATE, before Tera runs, rather than to the rendered
    /// prefix: that way it lands inside `static_prefix` and `prefix_hash` covers
    /// it for free, so a model switch that changes vision capability rebuilds
    /// the prefix and a switch that does not leaves the KV cache alone. The
    /// section itself is Jinja-free and renders verbatim.
    ///
    /// Not applied when `Settings.custom_system_prompt` is set: that is a full
    /// override which `build_prompt_partition` uses INSTEAD of the template, so
    /// its author owns the whole prompt including this section.
    fn apply_vision_section(template: String, vision: bool, compact: bool) -> String {
        if !vision {
            return template;
        }
        format!(
            "{template}\n{}",
            pond_core::prompts::vision_capability_section(compact)
        )
    }

    /// Stamp the engine-level `enable_thinking` request-param onto a ModelConfig.
    ///
    /// Only for the in-process GGUF engine: it is the one provider that reads
    /// this param (`goose-local-inference/src/lib.rs`, where only `Some(false)`
    /// acts — `true` leaves the model registry's own default alone), and HTTP
    /// providers can serialize `request_params` straight into request bodies,
    /// where an unknown key is a liability.
    ///
    /// Why it matters: GIAP's `thinking_mode` (and voice mode) only ever shaped
    /// the PROMPT, so with thinking "off" the engine still applied its registry
    /// default of `true` — the template opened a reasoning channel and GIAP
    /// relied on the ThoughtFilter to catch the leakage. Passing the param makes
    /// suppression template-level, which is where it belongs.
    fn with_thinking_param(
        provider: &str,
        cfg: goose_providers::model::ModelConfig,
        enable_thinking: bool,
    ) -> goose_providers::model::ModelConfig {
        if !matches!(provider, "local" | "gguf") {
            return cfg;
        }
        cfg.with_merged_request_params(HashMap::from([(
            "enable_thinking".to_string(),
            serde_json::Value::Bool(enable_thinking),
        )]))
    }

    /// Hot-swap the Goose provider when `chat_provider` / `chat_model` in
    /// settings changes, or re-stamp its ModelConfig when only the engine-level
    /// thinking flag changed (`enable_thinking`, resolved per request from
    /// `thinking_mode` + voice + model capabilities).
    async fn ensure_provider_current(
        &self,
        settings: &pond_core::user_data::domain::settings::Settings,
        session_id: &str,
        enable_thinking: bool,
    ) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);
        // Kept as well as compared: PAI-4 P5 needs to know whether the MODEL
        // changed or only the provider, and by the time the swap block runs
        // `last_provider_key` has already been overwritten with the new one.
        let previous_key = {
            let last = self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            last.clone()
        };
        let key_unchanged = previous_key == key;
        let thinking_unchanged = {
            let last = self
                .last_thinking_param
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *last == Some(enable_thinking)
        };
        // HTTP providers never carry the param (see with_thinking_param), so a
        // thinking change is a no-op for them — record it and skip the re-stamp.
        if key_unchanged
            && !thinking_unchanged
            && !matches!(settings.chat_provider.as_str(), "local" | "gguf")
        {
            *self
                .last_thinking_param
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(enable_thinking);
        } else if key_unchanged && !thinking_unchanged {
            // Same model, different thinking flag: re-stamp the ModelConfig on
            // the RETAINED provider rather than rebuilding it. The static prompt
            // prefix changes too (thinking_enabled feeds PromptState), so the
            // KV prefix is being rebuilt this turn regardless.
            let cached = self
                .current_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some((p, cfg)) = cached {
                let cfg = Self::with_thinking_param(&settings.chat_provider, cfg, enable_thinking);
                self.agent
                    .update_provider(p.clone(), cfg.clone(), session_id)
                    .await?;
                *self
                    .current_provider
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some((p, cfg));
                *self
                    .last_thinking_param
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(enable_thinking);
                {
                    let mut configured = self
                        .provider_configured_sessions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    configured.clear();
                    configured.insert(session_id.to_string());
                }
                // PAI-4 P5. Same model, reconfigured provider — the comment
                // above already knew "the KV prefix is being rebuilt this turn
                // regardless"; this is that fact written where the compaction
                // path can read it. `ProviderRebuilt` rather than
                // `ModelSwapped` because the model did not change, and a trace
                // that cannot tell those apart is worth less than one that can.
                self.note_prefix_invalidated(InvalidationReason::ProviderRebuilt);
                tracing::info!(
                    enable_thinking,
                    "Re-stamped engine thinking flag on the current provider"
                );
                return Ok(());
            }
        }
        if key_unchanged {
            // The provider object is current, but Goose resolves the MODEL
            // per session: a Goose session created after the last swap has no
            // model_config row, and its reply path (and session naming) falls
            // back to the GLOBAL goose config — on a dev machine that can be a
            // stale ~/.config/goose/config.yaml naming a long-gone model, which
            // surfaces as "Model not found: <old model>" on every new session.
            // Configure this session with the retained pair exactly once.
            let session_configured = self
                .provider_configured_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(session_id);
            if session_configured {
                tracing::debug!("[model-switch] provider already current: {}", key);
                return Ok(());
            }
            let cached = self
                .current_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some((p, cfg)) = cached {
                self.agent.update_provider(p, cfg, session_id).await?;
                self.provider_configured_sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(session_id.to_string());
                tracing::debug!(
                    "[model-switch] session {} configured with current provider {}",
                    session_id,
                    key
                );
            }
            return Ok(());
        }
        tracing::debug!("[model-switch] provider change detected -> {}", key);

        // GOOSE_CONTEXT_LIMIT / GOOSE_AUTO_COMPACT_THRESHOLD /
        // GOOSE_TOOL_PAIR_SUMMARIZATION are exported by `apply_goose_env_knobs`,
        // which the caller runs BEFORE this function (they must be set before
        // ModelConfig construction reads the context limit) and on every turn, so
        // a settings toggle takes effect without a model switch or restart.

        // The third element is where that provider SENDS: the resolved base URL
        // for the HTTP-backed arms, `None` for in-process inference. Captured
        // here — the same instant the env vars are set — because this is the
        // one moment the binding between provider and destination is explicit;
        // the shim gates and records against it (see `GiapProviderShim`).
        let provider: Option<(
            Arc<dyn Provider>,
            goose_providers::model::ModelConfig,
            Option<String>,
        )> = match settings.chat_provider.as_str() {
            // In-process GGUF inference via llama.cpp — no HTTP server needed.
            // Registers the model in Goose's local_model_registry so
            // LocalInferenceProvider can locate the .gguf file on disk.
            "local" | "gguf" => {
                let model_name = if settings.chat_model.is_empty() {
                    "llamafile".to_string()
                } else {
                    settings.chat_model.clone()
                };
                // Register the GGUF model path in Goose's global registry
                // Register the model and get back its CANONICAL registry
                // key: "gemma-4-E2B-it" and "gemma-4-E2B-it-Q4_K_M" both
                // name the same GGUF file, and letting them fork into two
                // registry ids splits sessions across identities and can
                // keep two multi-GB copies of one model resident in the
                // engine's per-id model cache.
                let registry_key = match self.data_dir {
                    Some(ref dd) => Self::register_gguf_model(&model_name, dd),
                    None => model_name.trim_end_matches(".gguf").to_string(),
                };
                // Phase F1. `register_gguf_model` leaves `mmproj_path: None`
                // (GIAP registers a bare stem, which goose's featured-model
                // lookup cannot match), and the engine's vision gate is
                // exactly that field. Attach the encoder if it is on disk;
                // otherwise start fetching it in the background and stamp the
                // registry when it lands — `resolve_model_path` runs on every
                // generation, so no restart is needed. Non-blocking on
                // purpose: the encoder is ~1 GB.
                if let Some(ref dd) = self.data_dir {
                    crate::vision_encoder::ensure_mmproj_available(dd, &registry_key);
                }
                let cfg = goose_providers::model::ModelConfig::new(&registry_key);
                tracing::debug!(
                    "[model-switch] building LocalInferenceProvider for '{}'...",
                    model_name
                );
                // Wire the HF-token / config resolvers before first use — the
                // ProviderDef path does this; the direct constructor does not.
                goose::providers::local_inference::configure_local_inference();
                match goose::providers::local_inference::LocalInferenceProvider::from_env().await {
                    Ok(p) => {
                        tracing::debug!(
                            "[model-switch] LocalInferenceProvider ready for '{}'",
                            model_name
                        );
                        tracing::info!("Built LocalInferenceProvider for model '{}'", model_name);
                        Some((Arc::new(p), cfg, None))
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[model-switch] FAILED to build LocalInferenceProvider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!(
                            "Failed to build local inference provider for '{}': {e}",
                            model_name
                        );
                        None
                    }
                }
            }
            // llamafile uses the Ollama wire protocol over HTTP.
            "llamafile" => {
                std::env::set_var("OLLAMA_HOST", &self.llamafile_url);
                std::env::set_var("OLLAMA_TIMEOUT", "600");
                let model_name = if settings.chat_model.is_empty() {
                    "llamafile".to_string()
                } else {
                    settings.chat_model.clone()
                };
                let cfg = goose_providers::model::ModelConfig::new(&model_name);
                tracing::debug!(
                    "[model-switch] building llamafile OllamaProvider for '{}'...",
                    model_name
                );
                match goose::providers::ollama_def::from_env(None).await {
                    Ok(p) => {
                        tracing::debug!(
                            "[model-switch] llamafile provider ready for '{}'",
                            model_name
                        );
                        Some((Arc::new(p), cfg, Some(self.llamafile_url.clone())))
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[model-switch] FAILED to build llamafile provider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!("Failed to build llamafile provider: {e}");
                        None
                    }
                }
            }
            "ollama" => {
                let ollama_host = std::env::var("GIAP_OLLAMA_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
                std::env::set_var("OLLAMA_HOST", &ollama_host);
                std::env::set_var("OLLAMA_TIMEOUT", "600");
                let model_name = if settings.chat_model.is_empty() {
                    "llama3.2".to_string()
                } else {
                    settings.chat_model.clone()
                };
                tracing::debug!(
                    "[model-switch] building Ollama provider for '{}'...",
                    model_name
                );
                let cfg = goose_providers::model::ModelConfig::new(&model_name);
                match goose::providers::ollama_def::from_env(None).await {
                    Ok(p) => {
                        tracing::debug!(
                            "[model-switch] Ollama provider ready for '{}'",
                            model_name
                        );
                        Some((Arc::new(p), cfg, Some(ollama_host.clone())))
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[model-switch] FAILED to build Ollama provider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!("Failed to build ollama provider: {e}");
                        None
                    }
                }
            }
            _ => {
                tracing::debug!(
                    "[model-switch] unknown provider '{}', keeping current",
                    settings.chat_provider
                );
                None
            }
        };

        if let Some((p, model_cfg, endpoint)) = provider {
            let model_cfg =
                Self::with_thinking_param(&settings.chat_provider, model_cfg, enable_thinking);
            // Every provider Goose sees is wrapped in the GIAP shim — the
            // last-mile veto over system prompt, message injections, and the
            // tools list, and (for the HTTP-backed arms) the PAI-2 egress gate
            // on the endpoint resolved above (see provider_shim.rs).
            let p: Arc<dyn Provider> = Arc::new(crate::provider_shim::GiapProviderShim::new(
                p,
                self.shim_controls.clone(),
                endpoint,
            ));
            tracing::debug!(
                "[model-switch] swapping Goose provider to {}:{} for session {}",
                settings.chat_provider,
                settings.chat_model,
                session_id
            );
            tracing::info!(
                target: "giap::trace",
                kind = "provider_swap",
                session_id = %session_id,
                provider = %settings.chat_provider,
                model = %settings.chat_model,
                "Switching Goose provider"
            );
            self.agent
                .update_provider(p.clone(), model_cfg.clone(), session_id)
                .await?;
            *self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = key.clone();
            *self
                .last_thinking_param
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(enable_thinking);
            {
                let mut configured = self
                    .provider_configured_sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                configured.clear();
                configured.insert(session_id.to_string());
            }
            *self
                .current_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some((p, model_cfg));
            // Align goose's global model fallback with the active pair. Goose
            // reads Config::global() (env first, then ~/.config/goose/
            // config.yaml) for any session without a model_config — session
            // naming among them — and a developer machine's config.yaml can
            // name a model that no longer exists. The env override makes that
            // fallback resolve the model GIAP is actually serving.
            std::env::set_var("GOOSE_MODEL", &settings.chat_model);

            // Update model capabilities from the new model name
            let mut caps =
                pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                    &settings.chat_model,
                );
            caps.vision =
                Self::model_supports_vision(&settings.chat_provider, &settings.chat_model);
            tracing::debug!(
                "[model-switch] capabilities: thinking={}, vision={}, context={}k",
                caps.thinking,
                caps.vision,
                caps.context_window_tokens / 1000
            );
            *self
                .model_capabilities
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = caps;

            // Reset prefix hash so the system prompt is rebuilt with the new model's
            // capabilities on the next turn. KV-cache is invalidated by the provider
            // swap anyway — no cache to preserve.
            *self
                .last_prefix_hash
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = 0;

            // PAI-4 P5. Two of the six reasons meet here and the difference is
            // real: `key` is "provider:model", so reaching this block means one
            // of the two changed. A different model is `ModelSwapped`; the same
            // model behind a different provider (an Ollama model moved onto the
            // in-process engine, say) is `ProviderRebuilt`. Both cost the same
            // prefill; only one of them explains a change in the answers.
            self.note_prefix_invalidated(Self::provider_change_reason(
                &previous_key,
                &settings.chat_model,
            ));

            tracing::debug!("[model-switch] swap complete, key={}", key);
        } else {
            tracing::debug!(
                "[model-switch] no provider built for {}:{}",
                settings.chat_provider,
                settings.chat_model
            );
        }
        Ok(())
    }

    /// Arc-clone handle for the per-session last-prompt-token feedback map,
    /// usable inside the 'static stream closure.
    fn last_prompt_tokens_handle(&self) -> Arc<Mutex<HashMap<String, u32>>> {
        // The map lives behind the adapter's Arc; expose a shared handle by
        // storing it in an Arc on first use. (Field is Mutex<HashMap>; wrap
        // the read/write through a dedicated Arc kept in self via OnceLock.)
        self.last_prompt_tokens_arc
            .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
            .clone()
    }

    /// Deterministically trim goose's stored conversation for this session:
    /// strip stale <system-context> blocks from prior user turns, splice the
    /// rolling <conversation-summary>, drop the oldest complete turns beyond the
    /// profile's history budget, and cap how many historical images keep real
    /// pixels. Never calls a model; errors are logged and skipped — a failed
    /// trim must never block the turn.
    ///
    /// Runs only when `hybrid_compaction_enabled` (the default), which is also
    /// what gates the image cap.
    async fn trim_goose_history(&self, goose_sid: &str, giap_session_id: &str) {
        use pond_core::models::services::context::turn_trimmer::{
            trim_history, CurrentTurn, TrimMessage, TrimRole,
        };

        let conversation = match self.session_manager.get_session(goose_sid, true).await {
            Ok(s) => match s.conversation {
                Some(c) => c,
                None => return,
            },
            Err(e) => {
                tracing::debug!("trim: goose session unavailable: {e}");
                return;
            }
        };
        let source = conversation.messages().clone();
        if source.is_empty() {
            return;
        }

        // Rolling summary from GIAP storage (idle-refreshed).
        let rolling_summary = match &self.giap_session_storage {
            Some(storage) => storage
                .get_rolling_summary(giap_session_id)
                .await
                .ok()
                .and_then(|(s, _)| s),
            None => None,
        };

        let profile = self.turn_profile(giap_session_id).await;
        let last_real = self
            .last_prompt_tokens_handle()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(giap_session_id)
            .copied();

        // Wall-clock now, in unix seconds, for PAI-4 P3's age weighting.
        // `Message::created` is the same epoch. A clock that cannot be read at
        // all yields `None` ages, which the trimmer treats as recent — the
        // narrowing direction, and never a failed turn.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());

        let trim_input: Vec<TrimMessage> = source
            .iter()
            .enumerate()
            .map(|(index, m)| {
                let has_tool_response = m.content.iter().any(|c| {
                    matches!(
                        c,
                        goose::conversation::message::MessageContent::ToolResponse(_)
                    )
                });
                let text = m.as_concat_text();
                let role = if has_tool_response {
                    TrimRole::ToolResult
                } else {
                    match m.role {
                        rmcp::model::Role::User => TrimRole::User,
                        rmcp::model::Role::Assistant => TrimRole::Assistant,
                    }
                };
                let is_summary = text.trim_start().starts_with("<conversation-summary>");
                // `saturating_sub` is the clock-skew rule, and it matches the
                // one `resume_compaction::idle_gap_since` already uses: a
                // message stamped in the future reads as age 0 (recent), never
                // as an enormous positive age that would degrade it.
                let age_secs = now_secs.map(|now| now.saturating_sub(m.created.max(0) as u64));
                TrimMessage {
                    index,
                    role,
                    text,
                    is_summary,
                    age_secs,
                }
            })
            .collect();

        // `CurrentTurn::NotYetAppended` is the same fact the image cap below
        // already relies on: this runs before `Agent::reply`, so the newest user
        // message in the conversation is the PREVIOUS turn's, not this one's.
        // The trimmer used to assume the opposite and spare it, which left that
        // turn's `<system-context>` — its date, its selected memories, its
        // turn-budget note — to be re-prefilled as though it were current, and
        // put two conflicting blocks in front of the model. It also meant
        // `outcome.changed` was true on every turn from the third onwards, so
        // the early return below never fired and every turn rewrote goose's
        // whole message table.
        let outcome = trim_history(
            trim_input,
            &profile,
            rolling_summary.as_deref(),
            last_real,
            self.token_counter().await,
            CurrentTurn::NotYetAppended,
            self.verbatim_horizon().await,
            // PAI-4 P5. Read through the port method rather than the field, so
            // whatever a future caller (P7's compact endpoint) sees is exactly
            // what the trimmer acted on — one reading, not two.
            PrefixCacheState::posture_of(
                pond_core::models::ports::agent::Agent::prefix_cache_state(self).as_ref(),
            ),
        );

        // ── Live-history image cap (phase F2, live half) ──────────────────
        //
        // Same policy the hydration replay uses, applied to the conversation
        // the engine already holds: only the most recent image-bearing turn
        // keeps real pixels, everything older degrades to a text placeholder.
        //
        // Without it every image in the transcript is re-encoded on every
        // later turn. Measured on a four-turn production conversation: 1, then
        // 2, then 3 encodes per turn at 0.7-2.7s each, prefill 28s -> 37s. The
        // cost is unbounded in the length of the conversation.
        //
        // The image on the turn about to be sent is NOT counted — it has not
        // been appended to Goose's conversation yet, so it is not history, and
        // this session still gets one fresh image plus one from before.
        let (had_images, keep_images, images_dropped) =
            plan_live_image_cap(&source, &outcome.messages);

        // A conversation with no images (or one already inside the budget) must
        // come out byte-identical: the trimmer's own `changed` flag is still the
        // only thing that can trigger a rewrite.
        if !outcome.changed && images_dropped == 0 {
            return;
        }

        // Rebuild: original messages survive untouched unless (a) they are the
        // spliced summary (fresh user message), (b) their text changed AND they
        // are plain-text messages, (c) they carry an oversized structured tool
        // response, whose TEXT bodies are truncated in place, or (d) they carry
        // images over the history budget.
        //
        // (c) and (d) are the only cases that rewrite a structured message, and
        // both do so by cloning the original and editing its content in place —
        // ids, annotations, error flags and the tool-request/response pairing
        // are preserved byte-for-byte. That pairing is load-bearing: an orphaned
        // or re-keyed tool response is rejected by the provider, which is why
        // (d) refuses to touch any message carrying tool parts at all.
        let mut rebuilt: Vec<goose::conversation::message::Message> = Vec::new();
        for (i, tm) in outcome.messages.iter().enumerate() {
            if tm.is_summary || tm.index == usize::MAX {
                rebuilt.push(goose::conversation::message::Message::user().with_text(&tm.text));
                continue;
            }
            let original = &source[tm.index];
            if let Some(truncated) = truncate_tool_response_text(
                original,
                pond_core::models::services::context_budget::TOOL_RESULT_MAX_BYTES,
            ) {
                rebuilt.push(truncated);
                continue;
            }
            // (d) an image-bearing message over the history budget, or one whose
            // text was rewritten — the text-only branch below cannot reach it,
            // so before this its stale <system-context> also survived forever.
            if had_images[i] > 0
                && (keep_images[i] < had_images[i] || original.as_concat_text() != tm.text)
            {
                rebuilt.push(cap_message_images(original, keep_images[i], &tm.text));
                continue;
            }
            let text_only = original
                .content
                .iter()
                .all(|c| matches!(c, goose::conversation::message::MessageContent::Text(_)));
            if text_only && original.as_concat_text() != tm.text {
                let mut m = match original.role {
                    rmcp::model::Role::User => {
                        goose::conversation::message::Message::user().with_text(&tm.text)
                    }
                    rmcp::model::Role::Assistant => {
                        goose::conversation::message::Message::assistant().with_text(&tm.text)
                    }
                };
                m.id = original.id.clone();
                m.created = original.created;
                rebuilt.push(m);
            } else {
                rebuilt.push(original.clone());
            }
        }

        // Second guard, behind `outcome.changed`.
        //
        // `replace_conversation` is not an update — it is `BEGIN IMMEDIATE;
        // DELETE FROM messages WHERE session_id = ?` plus one INSERT per
        // surviving message, each with a fresh `serde_json::to_string` of its
        // content. Turn N therefore rewrites roughly 2(N-1) rows, and an
        // image-bearing message carries its base64 inline, so a long
        // conversation rewrites hundreds of KB per turn onto the Jetson's eMMC —
        // into a database the REST API never reads.
        //
        // `outcome.changed` is the real fix and is now honest (see the
        // `CurrentTurn` argument above). This hash catches the rest: any path
        // that sets `changed` or drops an image but produces a conversation
        // identical to the one already stored.
        let rebuilt_fingerprint = conversation_fingerprint(&rebuilt);
        let previous_fingerprint = conversation_fingerprint(&source);
        if rebuilt_fingerprint == previous_fingerprint {
            tracing::debug!(
                target: "giap::trace",
                kind = "history_trim_skipped",
                session_id = %giap_session_id,
                messages = rebuilt.len(),
                "trim produced an identical conversation; not rewriting the engine store"
            );
            return;
        }

        let rebuilt_conversation = goose::conversation::Conversation::new_unvalidated(rebuilt);
        match self
            .session_manager
            .replace_conversation(goose_sid, &rebuilt_conversation)
            .await
        {
            Ok(()) => tracing::info!(
                target: "giap::trace",
                kind = "history_trim",
                session_id = %giap_session_id,
                dropped_turns = outcome.dropped_turns,
                estimated_tokens = outcome.estimated_tokens,
                summary_spliced = rolling_summary.is_some(),
                images_dropped,
            ),
            Err(e) => tracing::warn!("trim: replace_conversation failed: {e}"),
        }
    }

    /// Memories topically relevant to `message`, each paired with its cosine
    /// similarity when one is known.
    ///
    /// Semantic path when an embedding provider is wired: embed the message and
    /// rank by cosine over stored vectors. The similarity is recomputed here
    /// from each hit's own embedding — `search_similar` returns fragments, not
    /// scores, and the blend in `rank_by_relevance` needs the score.
    ///
    /// Keyword LIKE path otherwise (no provider, or embedding this message
    /// failed): stopword-filtered terms, and no similarity to report.
    ///
    /// Never fails — retrieval trouble degrades the prompt, it must not fail the
    /// turn.
    async fn topical_memories(
        &self,
        message: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Vec<(MemoryFragment, Option<f32>)> {
        // Short-circuit before embedding. A Guest turn can match nothing, and
        // embedding the message anyway would spend CPU on a query whose only
        // possible answer is "no rows" -- and would hand the raw utterance to
        // the embedding provider for no reason.
        if scope.excludes_everything() {
            return Vec::new();
        }
        if let Some(provider) = &self.embedding_provider {
            // `embed_query`: this is the thing being searched WITH. On an
            // asymmetric retriever (nomic) the document prefix would put it in the
            // wrong manifold; providers without the distinction inherit `embed`.
            match provider.embed_query(message).await {
                Ok(query_vector) => {
                    match self
                        .memory_repo
                        .search_similar(&query_vector, scope, limit)
                        .await
                    {
                        Ok(hits) => {
                            return hits
                                .into_iter()
                                .map(|fragment| {
                                    // `search_similar` degrades to search_recent when
                                    // NOTHING in the store is embedded; those hits have
                                    // no vector and so carry no similarity.
                                    //
                                    // A vector of a different WIDTH is likewise carrying
                                    // no usable similarity — it came from another
                                    // embedding model. It must map to `None` (forfeit the
                                    // similarity term, exactly as a recency-only hit
                                    // does) and NOT to `Some(0.0)`, which is a real score
                                    // that drags the blend down and quietly reverts
                                    // injection to importance+recency.
                                    let similarity = fragment
                                        .embedding
                                        .as_deref()
                                        .filter(|e| e.len() == query_vector.len())
                                        .map(|e| cosine_similarity(&query_vector, e));
                                    (fragment, similarity)
                                })
                                .collect();
                        }
                        Err(e) => tracing::warn!("memory: semantic search failed: {e}"),
                    }
                }
                Err(e) => tracing::warn!("memory: embedding the turn failed, using keywords: {e}"),
            }
        }

        let keywords = pond_core::user_data::services::memory_relevance::keyword_terms(message);
        if keywords.is_empty() {
            return vec![];
        }
        match self
            .memory_repo
            .search_by_content(&keywords, scope, limit)
            .await
        {
            Ok(hits) => hits.into_iter().map(|m| (m, None)).collect(),
            Err(e) => {
                tracing::warn!("memory: keyword search failed: {e}");
                vec![]
            }
        }
    }

    // ── Phase D2: per-session tool selection ────────────────────────────────

    /// Embeddings of the scorable (non-core, registered) group descriptions.
    ///
    /// Computed once per process. `None` means the work could not be done at all
    /// (no embedder, or every embed failed) — which callers must treat as "do not
    /// narrow", never as "no groups matched".
    /// The group-description vectors, computed on first SUCCESSFUL use.
    ///
    /// `get_or_try_init`, not `get_or_init`, and that is the whole point: a
    /// `OnceCell<Option<_>>` initialised to `None` keeps that `None` for the
    /// lifetime of the process, so one failed attempt disabled narrowing until
    /// the server was restarted.
    ///
    /// That is not a hypothetical on the target hardware. The Jetson's embedding
    /// model is downloaded in a task `main.rs` deliberately does not await, so
    /// the first session of a fresh install embeds against a file that has not
    /// arrived. Under `get_or_init` that install then ran with all 66 tool
    /// schemas in every prompt, for every session, with the trace still
    /// reporting `mode = "relevant"`. `get_or_try_init` leaves the cell empty on
    /// `Err`, so the next session tries again and picks the model up as soon as
    /// it lands.
    async fn group_description_embeddings(&self) -> Option<&Vec<(String, Vec<f32>)>> {
        self.group_embeddings
            .get_or_try_init(|| async {
                let provider = self.embedding_provider.as_ref().ok_or(())?;
                let available: Vec<String> = registered_extensions().to_vec();
                let scorable =
                    pond_core::mcp::services::tool_selection::scorable_groups(&available);
                let mut out = Vec::with_capacity(scorable.len());
                for (extension, description) in scorable {
                    match provider.embed(description).await {
                        Ok(v) => out.push((extension.to_string(), v)),
                        // One bad description should not disable the feature, but
                        // it does mean that group can never be scored — so it is
                        // simply absent from the scores, and `select_groups`'
                        // "unavailable groups are never selected" rule keeps it
                        // dormant until the escape hatch pulls it in.
                        Err(e) => {
                            tracing::warn!("tool selection: embedding '{extension}' failed: {e}")
                        }
                    }
                }
                // Empty is a FAILURE, not a result. Returning `Err` is what keeps
                // the cell uninitialised so a later session retries.
                if out.is_empty() {
                    return Err(());
                }
                Ok(out)
            })
            .await
            .ok()
    }

    /// The tool groups for this session, resolving (and persisting) them on first
    /// use.
    ///
    /// Resolution order: in-process cache, then the persisted row, then scoring.
    /// Sticky by design — re-scoring per turn would rewrite the tools JSON every
    /// turn and destroy the KV prefix reuse this feature exists to protect.
    async fn resolve_session_tool_groups(
        &self,
        giap_session_id: &str,
        first_message: &str,
        memories: &str,
        skills: &str,
        scope: &ProfileScope,
    ) -> SessionGroups {
        use pond_core::mcp::services::tool_selection as sel;

        // The boundary, derived before anything is selected or restored.
        //
        // Applied to the candidates going IN rather than subtracted after, which
        // works because `select_groups` filters the core set by `available`
        // (`tool_selection.rs`) — the comment that used to sit here claimed a
        // pre-filter "would not stick because select_groups puts core groups back
        // unconditionally", and that has not been true for as long as the filter
        // has been there. Doing it once means `permitted` is the single fact both
        // the dormant note and the escape hatch read.
        let permitted = self.permitted_groups(scope);

        if let Some(cached) = self
            .session_tool_groups
            .read()
            .await
            .get(giap_session_id)
            .cloned()
        {
            self.remember_permitted(giap_session_id, &permitted).await;
            return SessionGroups {
                loaded: cached,
                permitted,
            };
        }

        if let Some(storage) = &self.giap_session_storage {
            if let Ok(Some(groups)) = storage.get_session_tool_groups(giap_session_id).await {
                if !groups.is_empty() {
                    // Clamp what was persisted to what is permitted NOW. A group
                    // is stored per session and the speaker's scope is resolved
                    // per turn, so a session that was identified when it was
                    // saved and is not now must not get its groups back.
                    let groups: Vec<String> = groups
                        .into_iter()
                        .filter(|g| permitted.iter().any(|p| p == g))
                        .collect();
                    self.session_tool_groups
                        .write()
                        .await
                        .insert(giap_session_id.to_string(), groups.clone());
                    self.remember_permitted(giap_session_id, &permitted).await;
                    tracing::debug!(
                        session_id = %giap_session_id,
                        groups = ?groups,
                        "tool selection: restored persisted groups"
                    );
                    return SessionGroups {
                        loaded: groups,
                        permitted,
                    };
                }
            }
        }

        let available: Vec<String> = permitted.clone();
        // Three signals, scored independently and merged with max. Concatenating
        // them let a kilobyte of memories drown a short question — see
        // `selection_signals`. The skills signal exists so an active skill can
        // pull in the tool groups its own instructions call for, independent of
        // whatever the opening message happened to say.
        let signals = sel::selection_signals(first_message, memories, skills);

        // Score, or fall back to every group. Both the "no group embeddings" and
        // the "embedding this signal failed" paths widen — the asymmetry is
        // deliberate (a missing tool is a wrong answer, a surplus one is tokens).
        let scores: Option<Vec<sel::GroupScore>> = match (
            self.group_description_embeddings().await,
            self.embedding_provider.as_ref(),
        ) {
            (Some(group_vectors), Some(provider)) => {
                let mut per_signal: Vec<Vec<sel::GroupScore>> = Vec::with_capacity(signals.len());
                let mut failed = None;
                for signal in &signals {
                    match provider.embed(signal).await {
                        Ok(query) => per_signal.push(
                            group_vectors
                                .iter()
                                .map(|(extension, v)| sel::GroupScore {
                                    extension: extension.clone(),
                                    score: cosine_similarity(&query, v),
                                })
                                .collect(),
                        ),
                        Err(e) => failed = Some(e),
                    }
                }
                match (per_signal.is_empty(), failed) {
                    // Every signal failed to embed — widen, as before.
                    (true, Some(e)) => {
                        tracing::warn!("tool selection: embedding the opening message failed: {e}");
                        None
                    }
                    (true, None) => None,
                    // At least one embedded: score on what we have rather than
                    // discarding a good signal because its partner failed.
                    _ => Some(sel::merge_scores(&per_signal)),
                }
            }
            _ => None,
        };

        let selection = sel::select_groups(
            &available,
            scores.as_deref(),
            sel::DEFAULT_RELEVANCE_THRESHOLD,
        );

        // The narrowing SILENTLY DID NOT HAPPEN, and that has to be visible.
        //
        // `SelectionBasis::NoEmbedder` widens to every permitted group, which is
        // the right call (a missing tool is a wrong answer; a surplus one is
        // tokens). What was wrong is that the trace below still reported
        // `mode = "relevant"`, so the only tell was `groups_total ==
        // tools_total`. On the Jetson the embedding model is fetched in a
        // deliberately un-awaited task, so the first session of a fresh install
        // embeds against a file that is not there yet — and
        // `group_description_embeddings` used to latch that failure into a
        // `OnceCell` for the whole process. An operator who set "relevant" got
        // all 66 tools for the lifetime of the server and no line said so.
        if matches!(selection.basis, sel::SelectionBasis::NoEmbedder) {
            tracing::warn!(
                target: "giap::trace",
                kind = "tool_selection_widened",
                session_id = %giap_session_id,
                reason = if self.embedding_provider.is_none() {
                    "no_embedder"
                } else {
                    "embed_failed"
                },
                groups = permitted.len(),
                "tool selection asked for narrowing and could not narrow"
            );
        }

        if tracing::enabled!(tracing::Level::DEBUG) {
            if let Some(scores) = scores.as_deref() {
                let mut ranked: Vec<&sel::GroupScore> = scores.iter().collect();
                ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
                let top: Vec<String> = ranked
                    .iter()
                    .take(5)
                    .map(|s| format!("{}={:.3}", s.extension, s.score))
                    .collect();
                tracing::debug!(
                    session_id = %giap_session_id,
                    threshold = sel::DEFAULT_RELEVANCE_THRESHOLD,
                    top_scores = %top.join(" "),
                    "tool selection: group scores"
                );
            }
        }

        self.session_tool_groups
            .write()
            .await
            .insert(giap_session_id.to_string(), selection.groups.clone());
        self.remember_permitted(giap_session_id, &permitted).await;
        if let Some(storage) = &self.giap_session_storage {
            if let Err(e) = storage
                .set_session_tool_groups(giap_session_id, &selection.groups)
                .await
            {
                // Non-fatal: the in-process cache still keeps the session stable
                // for this run; only cross-restart stickiness is lost.
                tracing::warn!("tool selection: persisting groups failed: {e}");
            }
        }
        SessionGroups {
            loaded: selection.groups,
            permitted,
        }
    }

    /// The groups this scope may ever hold, per PAI-1.
    ///
    /// The rule itself is `tool_selection::permitted_groups`, in pond-core beside
    /// `groups_denied_to_guests`, because it is a domain boundary rather than an
    /// adapter concern — and because a pure function of (available, scope) can be
    /// unit-tested, which a method reaching for a process-global `OnceLock`
    /// cannot. This wrapper only supplies the registered list and the log line.
    fn permitted_groups(&self, scope: &ProfileScope) -> Vec<String> {
        let available = registered_extensions();
        let kept = pond_core::mcp::services::tool_selection::permitted_groups(available, scope);
        if kept.len() != available.len() {
            tracing::info!(
                withheld = available.len() - kept.len(),
                "unidentified speaker: personal-data tool groups withheld"
            );
        }
        kept
    }

    /// Engine session id → GIAP session id.
    ///
    /// The escape hatch is keyed by the GIAP session, but the only trustworthy
    /// thing an MCP tool can learn about its caller is goose's `agent-session-id`
    /// from `_meta` (see `session_meta.rs`, which rejects the alternatives by
    /// name). So the translation happens here rather than the tool guessing.
    ///
    /// A linear scan: the map holds live conversations, `sse_semaphore` caps
    /// those at 4, and this runs only when the model calls `enable_tool_group`.
    fn giap_session_for_engine(&self, engine_session_id: &str) -> Option<String> {
        self.goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(_, goose_sid)| goose_sid.as_str() == engine_session_id)
            .map(|(giap_sid, _)| giap_sid.clone())
    }

    async fn remember_permitted(&self, giap_session_id: &str, permitted: &[String]) {
        self.session_permitted_groups
            .write()
            .await
            .insert(giap_session_id.to_string(), permitted.to_vec());
    }

    /// Attach the embedding provider used by per-turn memory retrieval.
    ///
    /// Without it the injection path keeps working, just on the keyword LIKE
    /// fallback — which is why this is a builder rather than a `new()` argument.
    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Attach the model catalog, so the context governor's rung 3
    /// (`ModelRecord.context_length`) becomes reachable from this adapter.
    ///
    /// A builder rather than a `new()` argument for the same reason as the
    /// embedding provider: without it every budget path still works, just on a
    /// worse answer — the model-name heuristic, which returns 4,096 for
    /// anything it does not recognise.
    pub fn with_model_repo(mut self, repo: Arc<dyn ModelRepository>) -> Self {
        self.model_repo = Some(repo);
        self
    }

    /// Attach GIAP session storage so the deterministic turn trimmer can
    /// splice the rolling `<conversation-summary>` into the model's history.
    pub fn with_giap_session_storage(
        mut self,
        storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>,
    ) -> Self {
        self.giap_session_storage = Some(storage);
        self
    }

    /// Register a GGUF model in Goose's global `local_model_registry` so that
    /// `LocalInferenceProvider` can find the file at `$data_dir/models/gguf/`.
    ///
    /// Handles two formats:
    /// - Bare stem: `"qwen2.5-3b-instruct-q4_k_m"` → looks for `{stem}.gguf`
    /// - Raw filename: `"model.gguf"` → uses as-is
    ///
    /// Returns the CANONICAL registry key: a quant-suffixed spelling
    /// ("gemma-4-E2B-it-Q4_K_M") collapses to the display stem
    /// ("gemma-4-E2B-it") whenever both unambiguously name the same file, so
    /// the two spellings can never fork into separate registry ids — which
    /// would split sessions and keep two copies of one model in the engine's
    /// per-id cache. Callers MUST build their `ModelConfig` from the returned
    /// key. Idempotent: skips registration if the model is already known.
    fn register_gguf_model(model_name: &str, data_dir: &std::path::Path) -> String {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, LocalModelEntry, LocalModelStorage, ToolCallingMode,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // Canonical registry key: collapse a redundant quant suffix, then keep
        // the requested spelling for everything else. Only the file we point
        // at is resolved from the ORIGINAL name, so an explicit quant choice
        // still pins its exact file.
        let stem = canonical_model_stem(model_name, &gguf_dir);
        let filename = resolve_gguf_filename(model_name, &gguf_dir);
        let local_path = gguf_dir.join(&filename);
        if !local_path.exists() {
            tracing::warn!(
                "GGUF model file not found at {} — LocalInferenceProvider may fail to load",
                local_path.display()
            );
        }

        match get_registry().lock() {
            Ok(mut registry) => {
                let registry: &mut goose::providers::local_inference::local_model_registry::LocalModelRegistry = &mut registry;
                // Register when the model is absent, OR when a stale entry points
                // at a file that no longer exists. The stale case is what older
                // builds left behind: they stored the display name and derived
                // `{name}.gguf`, so the persisted registry (models/registry.json)
                // holds an entry whose `local_path` never existed. Skipping it —
                // as a plain `has_model` check would — leaves the bad path in
                // place and inference keeps failing with "Model not downloaded".
                // `add_model` upserts, so re-registering repairs it in place.
                let needs_register = registry
                    .get_model(&stem)
                    .map(|entry| !entry.local_path.exists())
                    .unwrap_or(true);

                if needs_register {
                    // Carry the EXISTING tuning block over when we are repairing
                    // a stale entry, rather than resetting to defaults.
                    //
                    // `ModelSettings::default()` has `context_size: None`, so a
                    // re-registration dropped the platform stamp
                    // (`apply_jetson_settings`' 16384 on the Orin). Whichever
                    // call won the race decided the window: a turn was observed
                    // running with a 32,768-cell KV context instead of 16,384 —
                    // ~576 MiB of KV in two buffers against ~288 MiB, with the
                    // SWA buffer landing within ~200 MiB of the NvMap wall.
                    // Repairing a bad `local_path` must not also un-tune the
                    // model.
                    let mut settings = registry
                        .get_model(&stem)
                        .map(|entry| entry.settings.clone())
                        .unwrap_or_default();
                    // GIAP's local GGUFs (gemma family) support llama.cpp native
                    // tool calling; force it rather than relying on Auto detection.
                    settings.tool_calling = ToolCallingMode::ForceNative;
                    let entry = LocalModelEntry {
                        id: stem.clone(),
                        repo_id: format!("local/{}", stem),
                        filename: filename.clone(),
                        quantization: String::new(),
                        local_path,
                        source_url: String::new(),
                        backend_id: None,
                        // GIAP owns the file under its own data dir — Goose must
                        // not treat it as deletable Goose-managed storage.
                        storage: LocalModelStorage::ManualPath,
                        settings,
                        size_bytes: 0,
                        mmproj_path: None,
                        mmproj_source_url: None,
                        mmproj_size_bytes: 0,
                        mmproj_checked: false,
                        shard_files: vec![],
                    };
                    match registry.add_model(entry) {
                        Ok(_) => {
                            tracing::info!("Registered GGUF model '{}' in local registry", stem)
                        }
                        Err(e) => tracing::warn!("Could not register GGUF model '{}': {}", stem, e),
                    }
                } else if let Some(entry) = registry.get_model(&stem) {
                    let mut s = entry.settings.clone();
                    if s.tool_calling == ToolCallingMode::Auto {
                        s.tool_calling = ToolCallingMode::ForceNative;
                        let _ = registry.update_model_settings(&stem, s);
                    }
                }
            }
            Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
        }
        stem
    }

    pub async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();

        // Phase F1: fail an image turn EARLY and specifically.
        //
        // Without this the engine silently rewrites each image part into
        // "[Image attached - image input is not supported with the currently
        // selected model]" and the model answers as if it had looked, which is
        // the worst possible outcome. Two distinguishable causes, two messages.
        if !request.images.is_empty() && matches!(settings.chat_provider.as_str(), "local" | "gguf")
        {
            let model = settings.chat_model.as_str();
            if !crate::vision_encoder::declares_vision(model) {
                anyhow::bail!(
                    "The active model ({model}) cannot read images. Switch to a vision-capable \
                     model such as gemma-4-E2B-it and try again."
                );
            }
            let ready = self
                .data_dir
                .as_ref()
                .is_some_and(|dd| crate::vision_encoder::mmproj_ready(dd, model));
            if !ready {
                if let Some(ref dd) = self.data_dir {
                    // A turn is the strongest signal that the encoder is wanted;
                    // make sure a fetch is running even if the provider was built
                    // before this code existed.
                    crate::vision_encoder::ensure_mmproj_available(dd, model);
                }
                anyhow::bail!(
                    "The vision encoder for {model} is still downloading. Image input becomes \
                     available as soon as it finishes - no restart needed. Your message was not \
                     sent."
                );
            }
        }

        // Stash the user message and session ID so MCP tool handlers can read
        // them for ToolCaller param generation and outbound HTTP trace events.
        pond_mcp_server::set_last_user_message(&request.message);
        pond_mcp_server::set_current_session_id(&session_id);

        // Settings-derived Goose knobs, re-applied every turn (cheaply — see
        // apply_goose_env_knobs) so toggling hybrid compaction or the context
        // override takes effect immediately. Must still run BEFORE session
        // hydration and ModelConfig construction: it exports GOOSE_CONTEXT_LIMIT
        // for Goose's own use, and it populates `last_window`, which is where
        // the GIAP-side budget paths now get the window from.
        self.apply_goose_env_knobs(&settings).await;

        // Goose maintains its own sessions.db with auto-generated IDs.
        let goose_sid = self.resolve_goose_session(&session_id).await;

        // ── 0. Load GIAP builtin MCP extensions (once per session) ────────────
        {
            let needs_load = !self
                .loaded_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&goose_sid);
            if needs_load {
                let extensions = registered_extensions();
                let total = extensions.len();
                let loaded = self.add_builtin_extensions(extensions, &goose_sid).await;
                self.loaded_sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(goose_sid.clone());

                // Verify tools are discovered; enumerate at debug, summarise once at info.
                let tools = self.agent.list_tools(&goose_sid, None).await;
                for t in &tools {
                    tracing::debug!("giap tool available: {}", t.name);
                }
                tracing::info!(
                    target: "giap::trace",
                    "giap extensions ready: {loaded}/{total} loaded, {} tools",
                    tools.len()
                );
            }
        }

        // ── 1-4. System prompt, extras, skills, memory — fetched in parallel ─
        // Two different numbers, deliberately.
        //
        // `memory_limit` is how many memories are INJECTED. `candidate_limit` is
        // how many are RETRIEVED for ranking. They used to be the same value,
        // which meant the ranker only ever saw `search_recent(5)` union
        // `search_similar(5)` — at most 10 rows. On the device that made 14 of
        // 24 fragments invisible on every single turn, and the two genuinely
        // useful preferences ("User prefers concise greetings", "The user
        // prefers concise answers") had never once been candidates. The blend
        // weights were fine; the pool they ranked was the defect.
        //
        // Widening is close to free: sqlite_memory's semantic search already
        // SELECTs every embedded active row and cosines all of them, then
        // truncates in Rust — so a bigger limit is the same query and the same
        // arithmetic. Nothing downstream changes: the injection cap and the
        // token budget still decide what actually reaches the prompt.
        // PAI-1 P5: an unidentified speaker gets no memory injection at all.
        // Gating here rather than at the queries means the whole fetch, rank
        // and render pipeline is skipped, and it costs nothing in KV prefix --
        // memories ride the user message's <system-context>, never the system
        // prefix (see the comment at the block assembly below).
        let turn_scope = request.profile_scope.clone();
        let memory_limit = if settings.agent_memory_inject && turn_scope.allows_personal_data() {
            Some(settings.agent_memory_limit as usize)
        } else {
            None
        };
        let candidate_limit =
            memory_limit.map(|limit| (limit * MEMORY_CANDIDATE_FANOUT).max(MEMORY_CANDIDATE_FLOOR));

        let (
            template_result,
            devices_result,
            extras_result,
            skills_result,
            recent_memories,
            relevant_memories,
        ) = tokio::join!(
            self.template_repo.get(&settings.prompt_style),
            self.device_repo.list_devices(),
            self.extras_repo.list_active(),
            self.skill_repo.list_active(),
            // Recent memories (recency-based)
            async {
                match candidate_limit {
                    Some(limit) => self.memory_repo.search_recent(&turn_scope, limit).await,
                    None => Ok(vec![]),
                }
            },
            // Topical memories — semantic when an embedder is wired, keyword
            // LIKE otherwise. Embedding one short message is a few ms on CPU,
            // and it happens inside this join! so it overlaps the other fetches.
            async {
                match candidate_limit {
                    Some(limit) => {
                        self.topical_memories(&request.message, &turn_scope, limit)
                            .await
                    }
                    None => vec![],
                }
            },
        );

        // Merge recency + topical, keeping the similarity score of anything that
        // came back from the semantic search. `None` means "recency-only hit",
        // which the ranking blend scores as zero similarity.
        let mut memory_candidates: Vec<(MemoryFragment, Option<f32>)> = recent_memories
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m, None))
            .collect();
        for (fragment, similarity) in relevant_memories {
            match memory_candidates
                .iter_mut()
                .find(|(existing, _)| existing.id == fragment.id)
            {
                Some(entry) => entry.1 = similarity,
                None => memory_candidates.push((fragment, similarity)),
            }
        }

        let template_content = template_result
            .ok()
            .flatten()
            .map(|t| t.content)
            .unwrap_or_else(|| fallback_prompt().to_string());

        // Voice detection is shared by prompt construction (disables thinking)
        // and the session turn cap (#105 — voice_max_turns). Check both the
        // instance-level flag (CLI --input whisper) and the per-request flag
        // (desktop voice pipeline sends voice_mode: true).
        let voice_instance = self.voice_mode.load(std::sync::atomic::Ordering::Relaxed);
        let is_voice = Self::voice_turn(voice_instance, request.voice_mode);

        // Resolve thinking mode from settings + capabilities.
        // Voice mode always disables thinking — reasoning tokens waste TTS time
        // and leak as spoken text if any filter layer misses them.
        //
        // Hoisted out of the PromptState block below because it now drives two
        // things that must agree: the prompt's <thinking> section AND the
        // engine-level `enable_thinking` request-param (B4). Previously only the
        // prompt knew, so the engine kept its registry default of `true` and the
        // ThoughtFilter had to mop up the leakage.
        let thinking_enabled =
            Self::thinking_section_applies(&settings.thinking_mode, &settings.chat_model, is_voice);

        // The turn's budget profile, built once from the resolution
        // `apply_goose_env_knobs` cached at the top of this function.
        //
        // Both consumers below (the prompt tier and the memory-injection
        // budget) used to re-resolve it independently through the static
        // `effective_context_window`. That was already duplication; once rung 3
        // became a repository read it would also have been two extra catalog
        // round trips per turn, and the block below is synchronous so it could
        // not have awaited them anyway.
        //
        // Since PAI-3 P5 it is one profile rather than two: `turn_profile`
        // carries the full window for history and the clamped prompt window for
        // the preamble, so the two can no longer be built from different numbers
        // by accident.
        let turn_profile = self.turn_profile(&session_id).await;
        let effective_ctx = turn_profile.context_window_tokens;

        let prompt_state = {
            use chrono::Local;
            let now = Local::now();
            let devices = devices_result.unwrap_or_default();
            let device_count = devices.len();
            let has_home_devices = device_count > 0;
            let online_device_names = devices
                .iter()
                .filter(|d| d.is_online)
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            // The prompt tier follows the PROMPT-side window, which for local
            // inference is clamped so a huge KV cache never selects the verbose
            // tier (see `CompactionProfile::for_windows`). The profile itself
            // now knows that; this is no longer a second construction that has
            // to remember the clamp.
            let compact_prompt = turn_profile.use_compact_prompt();

            // Prose tool lines, from the registry or not at all.
            //
            // There is no static fallback any more. It was a hardcoded list of
            // 13 names of which nine matched no tool the dispatcher would answer
            // to, and because `InMemoryToolRegistry::new()` seeded itself from
            // the same list, the registry branch served it too — so the fallback
            // being "only for the None case" was never the protection it looked
            // like. Builtins reach the model as native tool schemas generated
            // from the real handlers; what the registry adds is external MCP
            // extension tools, which those schemas do not describe in prose.
            let available_tools: Vec<String> = match &self.tool_registry {
                Some(registry) => registry.prompt_description_lines(compact_prompt).await,
                None => Vec::new(),
            };

            PromptState {
                current_date: now.format("%A, %-d %B %Y").to_string(),
                current_time: Self::format_current_time(now, is_voice),
                device_count,
                has_home_devices,
                online_device_names,
                voice_mode: is_voice,
                canvas_mode: request.canvas_mode,
                available_tools,
                thinking_enabled,
                compact_prompt,
                // Local llama.cpp providers inject the full tools JSON via the
                // model's chat template (native tool calling) — the prompt
                // template must not render its own "Available tools:" listing
                // on top of that, or every schema is fed to the model twice.
                native_tools_json: matches!(settings.chat_provider.as_str(), "local" | "gguf"),
                prefix_hash: None, // filled by build_prompt_partition below
            }
        };

        // Phase F3: tell a multimodal model that it IS multimodal.
        //
        // Nothing else in the prompt says so, and the omission is not theoretical:
        // asked "what is in the image above?" with a fully encoded 252-token image
        // in context, Gemma-4-E4B answered "I cannot directly describe the content
        // of an image you provide. I am a text-based assistant." The section also
        // draws the line between an image ATTACHED to the message and a live
        // CAMERA frame, because the same model answered "what do you see?" by
        // offering camera frames while an attachment sat in front of it.
        //
        // Gated on the model, never rendered for a text-only one — telling a
        // blind model it can see manufactures a hallucination from nothing —
        // and off in voice mode, where `capabilities()` already reports
        // vision = false and no attachment can reach the turn.
        let template_content = Self::apply_vision_section(
            template_content,
            Self::vision_section_applies(
                &settings.chat_provider,
                &settings.chat_model,
                voice_instance,
            ),
            prompt_state.compact_prompt,
        );

        // Per-turn dynamic context (date/time, profile). Moved from system
        // prompt to user message to keep system+tools prefix token-stable.
        let mut dynamic_suffix_for_user_msg = String::new();

        // ── Partitioned prompt: static prefix + dynamic suffix ──────────
        // When prefix_cache_prompt is enabled (default), the system prompt is
        // split into a stable static prefix and a per-turn dynamic suffix.
        // The static prefix is only rebuilt when its hash changes (settings
        // update, device change, model switch), allowing local inference
        // providers to reuse their KV-cache for the stable portion.
        //
        // When disabled, falls back to rebuilding the full system prompt every
        // turn (legacy behavior, useful for debugging or HTTP-only providers
        // where KV-cache reuse doesn't apply).
        if settings.prefix_cache_prompt {
            // PAI-1 P6. Resolved at the API edge and carried on the request:
            // the adapter has no ProfileRepository, and giving it one would put
            // "whose preferences are these" behind the same boundary the
            // identity resolution deliberately sits in front of.
            //
            // KV-prefix safe: build_prompt_partition puts profile lines in the
            // dynamic suffix, which rides <system-context> in the USER message,
            // never the static prefix. So a speaker switch mid-session costs no
            // re-prefill.
            let partition = build_prompt_partition(
                &settings,
                request.profile_context.as_ref(),
                &prompt_state,
                &template_content,
            );

            // Publish the authoritative prefix to the provider shim — the
            // last-mile veto rebuilds any Goose-mutated system prompt from it.
            self.shim_controls
                .set_system_prefix(partition.static_prefix.clone());

            // Check whether the static prefix changed. Drop the MutexGuard
            // before any `.await` to keep the future `Send`.
            let prefix_changed = {
                let last_hash = self
                    .last_prefix_hash
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *last_hash != partition.prefix_hash
            };

            if prefix_changed {
                tracing::info!(
                    new_hash = %partition.prefix_hash,
                    "Static prefix changed — rebuilding system prompt"
                );
                self.agent
                    .override_system_prompt(partition.static_prefix)
                    .await;
                {
                    let mut last_hash = self
                        .last_prefix_hash
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *last_hash = partition.prefix_hash;
                }
                // PAI-4 P5. The prefix moved, so this turn pays a full
                // re-prefill whatever else happens. Note that
                // `note_prefix_rebuilt` does NOT clear the reason: the new
                // prefix has served nothing yet, and the trimmer — which runs
                // later in this same turn — must read Cold, not Warm.
                self.note_prefix_invalidated(InvalidationReason::PromptChanged);
                self.note_prefix_rebuilt(partition.prefix_hash);
            } else {
                tracing::debug!(
                    hash = %partition.prefix_hash,
                    "Static prefix unchanged — skipping override_system_prompt (KV-cache reuse)"
                );
                // PAI-4 P5. The one place a prefix earns its warm standing:
                // the engine is about to serve a turn off a cache it already
                // holds. Everything else in this file can only take that away.
                self.note_prefix_served();
            }

            // Dynamic suffix (date/time, profile) goes into <system-context> in the
            // user message — NOT the system prompt. Keeps prefix token-stable.
            dynamic_suffix_for_user_msg = partition.dynamic_suffix;
        } else {
            // Legacy path: rebuild full system prompt every turn
            let system_prompt = pond_core::prompts::build_system_prompt_from_template_full(
                &settings,
                None,
                Some(&prompt_state),
                &template_content,
            );
            self.shim_controls.set_system_prefix(system_prompt.clone());
            self.agent.override_system_prompt(system_prompt).await;
            // PAI-4 P5. The legacy path rebuilds the whole system prompt every
            // turn, so on it the prefix is cold every turn — unconditionally,
            // with no hash to compare. Saying so is what keeps the trimmer's
            // posture honest here rather than silently warm.
            self.note_prefix_invalidated(InvalidationReason::PromptChanged);
        }

        // GIAP-owned system-prompt appendix, re-attached by the provider shim
        // after it vetoes Goose's own appendages.
        //
        // This is the ONLY delivery path. Each body used to be pushed here AND
        // handed to `Agent::extend_system_prompt`, which meant Goose built its
        // own `# Additional Instructions:` block that `enforce_system` then threw
        // away — two String allocations and a `prompt_manager` mutex per extra
        // per turn, plus a Goose-side prompt build that `sanitize_unicode_tags`
        // -scans every extra ever registered in the process, all discarded.
        //
        // The map was the worse half: `remove_system_prompt_extra` has no callers
        // anywhere, so a skill the user deactivated stayed in Goose's `IndexMap`
        // for the lifetime of the process, held out of the model only by the
        // prefix match in `enforce_system`. Never adding it is what actually
        // fixes that.
        let mut shim_appendix: Vec<String> = Vec::new();

        // Extras and skills are appended AFTER the partitioned prompt and sit
        // OUTSIDE prefix_hash by design (see models/services/prompt_builder.rs).
        // Each body is wrapped in an <extension-notes> envelope so small models
        // can tell injected extension guidance apart from the core prompt
        // sections of the v2 tag skeleton.
        if let Ok(extras) = extras_result {
            for extra in extras {
                let body = format!(
                    "<extension-notes name=\"{}\">\n{}\n</extension-notes>",
                    extra.key, extra.instruction
                );
                shim_appendix.push(body);
            }
        }

        // Progressive disclosure, matching goose's own Agent Skills: only
        // name + description sit in the prompt on every turn. Full
        // instructions are loaded on demand via `giap-device__load_skill`
        // (see `device.rs`), so an active skill no longer costs a full
        // `MAX_CONTENT_LEN` on every turn regardless of relevance.
        //
        // The same name+description text also becomes a tool-selection
        // signal below (6c) — a skill whose instructions call for tools
        // outside the session's opening-message-scored groups (e.g. a
        // "task reminder" skill needing `giap-schedule`) would otherwise
        // never see those tools, no matter how clearly it says to use them.
        let mut skill_selection_signal = String::new();
        if let Ok(skills) = skills_result {
            if !skills.is_empty() {
                let mut skills_body = String::from(
                    "Active user-defined skills, as \"name: description\". When one looks \
                     relevant to what the user is asking, call giap-device__load_skill(name) \
                     to get its full instructions before acting on it.",
                );
                for skill in &skills {
                    skills_body.push_str(&format!("\n- {}: {}", skill.name, skill.description));
                    skill_selection_signal
                        .push_str(&format!("{}: {}\n", skill.name, skill.description));
                }
                shim_appendix.push(format!(
                    "<extension-notes name=\"skills\">\n{}\n</extension-notes>",
                    skills_body
                ));
            }
        }

        self.shim_controls
            .session(&goose_sid)
            .set_turn_appendix(if shim_appendix.is_empty() {
                None
            } else {
                Some(shim_appendix.join("\n\n"))
            });

        // ── Token-budgeted memory injection ──────────────────────────────
        // Memories go into <system-context> in the user message (not the system
        // prompt) to keep the prefix token-stable for KV cache reuse.
        let mut memory_block_for_user_msg = String::new();
        //
        // The memory budget is a PREAMBLE budget: local inference re-prefills
        // every injected memory token each turn, so it stays bounded even on a
        // 32K context. `turn_profile`'s preamble fields come from the clamped
        // prompt window for exactly that reason, while its history budget keeps
        // the real one (see `CompactionProfile::for_windows`).
        let compaction_profile = &turn_profile;

        if !memory_candidates.is_empty() {
            // Blended relevance (similarity + importance + recency decay) so a
            // topical memory can displace the standing high-importance identity
            // block instead of always losing to it.
            pond_core::user_data::services::memory_relevance::rank_by_relevance(
                &mut memory_candidates,
                chrono::Utc::now(),
            );

            // Apply fragment count limit from the compaction profile.
            memory_candidates.truncate(compaction_profile.max_memory_fragments);

            // Apply token budget: estimate tokens per fragment using the
            // chars/4 heuristic, keep fragments until the budget is spent.
            let token_budget = compaction_profile.memory_token_budget;
            let mut tokens_used: usize = 0;
            let mut budgeted: Vec<&MemoryFragment> = Vec::new();
            for (m, _) in &memory_candidates {
                let estimated_tokens = m.content.len() / 4 + 1;
                if tokens_used + estimated_tokens > token_budget && !budgeted.is_empty() {
                    break;
                }
                tokens_used += estimated_tokens;
                budgeted.push(m);
            }

            if !budgeted.is_empty() {
                let block = budgeted
                    .iter()
                    .map(|m| {
                        let seg = m
                            .segment
                            .as_ref()
                            .map(|s| format!("{:?}", s).to_lowercase())
                            .unwrap_or_default();
                        if seg.is_empty() {
                            format!("- {}", m.content)
                        } else {
                            format!("- [{}] {}", seg, m.content)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                tracing::debug!(
                    fragments_injected = budgeted.len(),
                    fragments_available = memory_candidates.len(),
                    tokens_used,
                    token_budget,
                    semantic = self.embedding_provider.is_some(),
                    "Memory injection (budget from CompactionProfile ctx={})",
                    effective_ctx,
                );

                memory_block_for_user_msg = block;

                // Record access for decay tracking — fire-and-forget in background
                // to avoid blocking the inference hot path with sequential DB writes.
                let ids: Vec<String> = budgeted.iter().map(|m| m.id.clone()).collect();
                let repo = self.memory_repo.clone();
                tokio::spawn(async move {
                    for id in ids {
                        let _ = repo.record_access(&id).await;
                    }
                });
            }
        }

        // ── 4b. Upcoming schedules context ──────────────────────────────────
        // TODO: inject schedule context once GooseAdapter has a SchedulerPort ref.
        // The old global-state path (registry.rs) has been removed.

        // ── 5. Provider hot-swap ──────────────────────────────────────────────
        if let Err(e) = self
            .ensure_provider_current(&settings, &goose_sid, thinking_enabled)
            .await
        {
            tracing::warn!("Provider update failed (continuing with current provider): {e}");
        }

        // ── 6. Extension cleanup ──────────────────────────────────────────────
        // Strip Goose default extensions that would pollute the prompt.
        // Only do this once per session — subsequent turns skip the strip loop.
        {
            let already_stripped = self
                .defaults_stripped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&goose_sid);
            if !already_stripped {
                // PAI-6 P2: one copy of these ten names, in
                // `orchestrator.rs :: GOOSE_STRIPPED_BUILTINS`. This block is
                // the guard; the `is_builtin` closure below is a prompt filter;
                // the orchestrator's plan builder refuses on the same list. All
                // three used to be able to drift, and only one of them enforced
                // anything, so a canary pinning one proved nothing about the
                // others.
                let strip_list: &[&str] = &crate::orchestrator::GOOSE_STRIPPED_BUILTINS;
                // Only remove what is actually loaded. `remove_extension` drops
                // the extension agent-globally and then calls
                // `persist_extension_state`, which is a `get_session` read plus
                // a `sessions` UPDATE — so a name that was never added still
                // cost two round trips to not-remove. GIAP registers its own
                // builtins and none of these ten, so in the normal case this
                // whole block now issues no writes at all.
                let present: std::collections::HashSet<String> = self
                    .agent
                    .list_extensions()
                    .await
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect();
                let user_exts = self.user_extensions.read().await;
                for ext in strip_list {
                    if !user_exts.contains(*ext) && present.contains(*ext) {
                        self.agent.remove_extension(ext, &goose_sid).await.ok();
                    }
                }
                drop(user_exts);
                self.defaults_stripped
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(goose_sid.clone());
                // Invalidate tool cache since extensions changed.
                *self.cached_tools.write().await = None;
            }
        }

        // ── 6b. Extension tool discovery ─────────────────────────────────────
        // Use cached tools when available — only re-query MCP servers when the
        // cache has been invalidated (extensions added/removed/defaults stripped).
        let allowed_tools = {
            let cache = self.cached_tools.read().await;
            if let Some(cached) = cache.as_ref() {
                cached.clone()
            } else {
                drop(cache);
                // Cache miss — query all tools and rebuild.
                let all_tools = self.agent.list_tools(&goose_sid, None).await;
                let tools_set: std::collections::HashSet<String> =
                    all_tools.iter().map(|t| t.name.to_string()).collect();

                // Group tools by extension prefix and inject external extension
                // descriptions so the agent knows about MCP tools.
                let mut ext_map: HashMap<String, Vec<(String, String)>> = HashMap::new();
                for tool in &all_tools {
                    let name = tool.name.as_ref();
                    if let Some(sep) = name.find("__") {
                        let ext_name = &name[..sep];
                        let tool_name = &name[sep + 2..];
                        let desc = tool
                            .description
                            .as_deref()
                            .unwrap_or("No description")
                            .to_string();
                        ext_map
                            .entry(ext_name.to_string())
                            .or_default()
                            .push((tool_name.to_string(), desc));
                    }
                }

                // Filter out built-in GIAP extensions (already covered by the
                // available_tools section in the prompt) and Goose defaults.
                // `default` and `suggestions` are Goose plumbing that carries no
                // tools — they are filtered out of the prompt but never
                // stripped, which is why they are named here and not in
                // `GOOSE_STRIPPED_BUILTINS`.
                let is_builtin = |name: &str| {
                    registered_extensions().iter().any(|e| e == name)
                        || crate::orchestrator::GOOSE_STRIPPED_BUILTINS.contains(&name)
                        || matches!(name, "default" | "suggestions")
                };

                let external_extensions: Vec<(String, Vec<(String, String)>)> = ext_map
                    .into_iter()
                    .filter(|(name, _)| !is_builtin(name.as_str()))
                    .collect();

                if !external_extensions.is_empty() {
                    let mut desc_lines = Vec::with_capacity(external_extensions.len() * 6);
                    desc_lines.push("# MCP Extensions".to_string());
                    desc_lines.push(
                        "The following MCP extensions are loaded. Use their tools when the user's request matches."
                            .to_string(),
                    );

                    for (ext_name, tools) in &external_extensions {
                        desc_lines.push(format!("\n## {}", ext_name));
                        for (tool_name, tool_desc) in tools {
                            desc_lines.push(format!("  - {}: {}", tool_name, tool_desc));
                        }
                    }

                    let ext_description = desc_lines.join("\n");

                    tracing::info!(
                        extensions = external_extensions.len(),
                        "Injecting {} external extension(s) into system prompt",
                        external_extensions.len(),
                    );

                    // Mirrored to the shim only, for the same reason the
                    // per-turn extras are: Goose's copy is rebuilt into an
                    // appendix the veto discards.
                    self.shim_controls
                        .set_extension_appendix(Some(ext_description));
                } else {
                    self.shim_controls.set_extension_appendix(None);
                }

                *self.cached_tools.write().await = Some(tools_set.clone());
                tools_set
            }
        };

        // ── 6c. Phase D2: per-session tool relevance ─────────────────────────
        // `allowed_tools` above is the full registered union. Goose keeps sending
        // all of it (`prepare_tools_and_prompt` -> `list_tools(None)`), so the
        // narrowing happens HERE and is enforced by the shim on every provider
        // call — no extension add/remove churn, and a mid-turn widen through the
        // escape hatch lands on the very next call of the same reply loop.
        //
        // This does NOT decide whether the model uses tools (working agreement:
        // trust the model, no keyword pre-classification). It decides which
        // extension SCHEMAS are physically in the prompt, for cost — 59 tools at
        // ~100 tokens each through the Gemma template is ~5.9K of an 8K-class
        // on-device budget. The model still chooses natively, and can pull in any
        // dormant group itself via giap-toolkit.
        let mut dormant_groups_note = String::new();
        // What this turn is ENTITLED to, as opposed to what it is carrying.
        //
        // Narrowing is a prompt-cost decision about this turn's own prompt. The
        // turn can widen to any permitted group at any moment via
        // `enable_tool_group`, so its entitlement is the permitted set and its
        // allow-set is merely where it happens to be standing. Delegation is
        // bounded by the entitlement, for reasons in the `for_turn` call below.
        //
        // `None` in "all" mode, where the two are the same set.
        let mut entitled_tools: Option<HashSet<String>> = None;
        let allowed_tools = if settings.tool_selection_is_relevant() {
            let groups = self
                .resolve_session_tool_groups(
                    &session_id,
                    &request.message,
                    &memory_block_for_user_msg,
                    &skill_selection_signal,
                    &turn_scope,
                )
                .await;
            let selected: HashSet<String> =
                pond_core::mcp::services::tool_selection::filter_tools_by_groups(
                    allowed_tools.iter(),
                    &groups.loaded,
                )
                .into_iter()
                .collect();

            // PERMITTED, not registered. Dormant = permitted minus loaded, so a
            // group withheld from this speaker is not on the menu either — it
            // used to be, under a line that tells the model enabling it makes its
            // tools available immediately.
            dormant_groups_note = pond_core::mcp::services::tool_selection::dormant_groups_note(
                &groups.permitted,
                &groups.loaded,
            );

            // The delegation ceiling: every tool this turn could reach, not just
            // the ones it is carrying. Same guest subtraction as the allow-set,
            // applied to the same source, so it can never be the wider set.
            entitled_tools = Some(
                pond_core::mcp::services::tool_selection::filter_tools_by_groups(
                    allowed_tools.iter(),
                    &groups.permitted,
                )
                .into_iter()
                .collect(),
            );

            tracing::info!(
                target: "giap::trace",
                kind = "tool_selection",
                session_id = %session_id,
                mode = "relevant",
                groups = ?groups.loaded,
                groups_total = groups.permitted.len(),
                groups_registered = registered_extensions().len(),
                tools = selected.len(),
                tools_total = allowed_tools.len(),
            );
            selected
        } else {
            tracing::info!(
                target: "giap::trace",
                kind = "tool_selection",
                session_id = %session_id,
                mode = "all",
                groups_total = registered_extensions().len(),
                tools = allowed_tools.len(),
                tools_total = allowed_tools.len(),
            );
            allowed_tools
        };

        // ── 6d. PAI-1 P5, enforced in BOTH selection modes ───────────────────
        //
        // The group-level subtraction lives inside `resolve_session_tool_groups`,
        // which is only reached from the `tool_selection_is_relevant()` branch
        // above. `default_tool_selection_mode()` is "all", so on a DEFAULT
        // install that branch never runs and this set went to the model
        // untouched -- a Guest kept `giap-memory` and could recall, search or
        // `forget_memory` the entire household. P5 was recorded as landed while
        // being inert on every default pond.
        //
        // This set is what gets published to the shim, so it is the only place
        // every mode converges. Subtracting here is idempotent with the
        // group-level pass, which stays because it is what bounds
        // `enable_tool_group` and what the dormant-groups note is built from.
        //
        // The sentence that used to end this paragraph said the group pass "keeps
        // withheld groups out of the dormant-groups note". It did the opposite:
        // the note was built from `registered_extensions()`, so every withheld
        // group appeared on it. `permitted_groups` is what makes the claim true.
        let allowed_tools = if turn_scope.excludes_everything() {
            let before = allowed_tools.len();
            let kept: HashSet<String> =
                pond_core::mcp::services::tool_selection::subtract_guest_denied_tools(
                    allowed_tools.iter(),
                )
                .into_iter()
                .collect();
            if kept.len() != before {
                tracing::info!(
                    target: "giap::trace",
                    kind = "guest_tools_withheld",
                    session_id = %session_id,
                    removed = before - kept.len(),
                    kept = kept.len(),
                    "unidentified speaker: personal-data tools withheld from the turn"
                );
            }
            kept
        } else {
            allowed_tools
        };

        // The entitlement passes through the SAME subtraction, in the same
        // branch, on the same condition. A ceiling that skipped it would be the
        // widening half of the exact defect this section exists to close — and
        // the delegation authority is built from it below.
        let entitled_tools = entitled_tools.map(|tools| {
            if turn_scope.excludes_everything() {
                pond_core::mcp::services::tool_selection::subtract_guest_denied_tools(tools.iter())
                    .into_iter()
                    .collect()
            } else {
                tools
            }
        });

        tracing::debug!(target: "pond_adapters_goose::goose_agent", "Allowed tools for turn: {:?}", allowed_tools);

        // Publish the allow-set to this SESSION's shim controls — anything Goose
        // adds on its own (platform tools, final_output) is vetoed at the last
        // mile, and anything Phase D left dormant never reaches the model.
        //
        // The handle is retained: the tool-call guard below reads it LIVE so a
        // group the model enables mid-turn is admitted immediately, and the
        // escape hatch widens the same entry.
        let session_controls = self.shim_controls.session(&goose_sid);
        session_controls.set_allowed_tools(allowed_tools.clone());

        // ── 7. GooseMode from model_role ──────────────────────────────────────
        let goose_mode = GooseMode::Auto;
        self.agent
            .update_goose_mode(goose_mode, &goose_sid)
            .await
            .ok();

        // ── 8. Run the agentic loop ───────────────────────────────────────────
        // Build the <system-context> block with per-turn dynamic data (date/time,
        // memories, turn budget). Everything variable lives HERE, in the user
        // message, so the system prompt + tool tokens stay byte-stable across
        // turns and the local engine can reuse its KV prefix.
        //
        // Reasoning-budget note (B2): the model is otherwise blind to its turn
        // budget — Goose carries "N/M turns used" in its own <turn-context>, and
        // the GIAP shim strips that precisely because it is NOT byte-stable and
        // would break prefix reuse if it rode the system prompt.
        //
        // PER-REQUEST, not per-turn: this user message is built ONCE before
        // `agent.reply`, so a live turn counter is not available here. What the
        // model can act on either way is the size of the budget — and, when
        // uncapped, that it should keep going rather than stop to ask.
        let max_turns = settings.effective_max_turns(is_voice);
        let turn_budget_block = pond_core::models::services::turn_budget::turn_budget_note(
            (!settings.turns_are_uncapped(is_voice)).then_some(max_turns),
        );

        let user_text = {
            let mut msg = String::with_capacity(512 + request.message.len());
            // Always present now (the budget note is unconditional), so the
            // <system-context> envelope is too.
            msg.push_str("<system-context>\n");
            if !dynamic_suffix_for_user_msg.is_empty() {
                msg.push_str(&dynamic_suffix_for_user_msg);
                msg.push('\n');
            }
            if !memory_block_for_user_msg.is_empty() {
                msg.push_str("<memories>\n");
                msg.push_str(&memory_block_for_user_msg);
                msg.push_str("\n</memories>\n");
            }
            // D2: what the model could load but currently cannot see. Rides the
            // user message, never the system prompt — it is session-specific and
            // the prefix must stay byte-identical across sessions for KV reuse.
            // Empty (zero tokens) whenever nothing is dormant, so the default
            // "all" mode is unaffected.
            if !dormant_groups_note.is_empty() {
                msg.push_str(&dormant_groups_note);
                msg.push('\n');
            }
            msg.push_str(&turn_budget_block);
            msg.push('\n');
            // Last inside the envelope, so the answer's shape is the closest
            // instruction to where the answer gets written. The system prefix
            // remains authoritative; this is a restatement of the part that
            // decays with distance. See `answer_contract`'s module docs.
            msg.push_str(&pond_core::models::services::answer_contract::answer_contract());
            msg.push('\n');
            msg.push_str("</system-context>\n");
            msg.push_str("<user-message>\n");
            msg.push_str(&request.message);
            msg.push_str("\n</user-message>");
            msg
        };
        // Held as pieces rather than one built message: empty-turn recovery
        // re-engages with a steered variant, and the prompt must actually differ
        // between attempts or a deterministic model repeats itself.
        let turn_text = user_text.clone();
        let turn_images = request.images.clone();
        // PAI-4 P5. A turn carrying an image forfeits KV retention outright —
        // the reason MAX_HISTORY_REPLAY_IMAGES is 1. Recorded here, before the
        // trim below, so the trimmer sees this turn's posture and not the
        // previous turn's.
        if !turn_images.is_empty() {
            self.note_prefix_invalidated(InvalidationReason::MultimodalTurn);
        }
        let turn_goose_sid = goose_sid.clone();

        let agent_clone = self.agent.clone();
        let last_prompt_tokens_map = self.last_prompt_tokens_handle();
        // Live handle for the tool-call guard inside the 'static stream closure.
        let guard_controls = session_controls.clone();

        // ── Deterministic in-turn trim (hybrid compaction, GIAP-owned) ──
        if settings.hybrid_compaction_enabled {
            self.trim_goose_history(&goose_sid, &session_id).await;
        }

        let user_msg_len = request.message.len();
        let turn_start = std::time::Instant::now();

        // Whether this turn may surface the model's reasoning at all. Resolved
        // here, from the settings already in hand, so the 'static stream closure
        // below carries a decision rather than a repository handle. Does not
        // touch `PromptState` and so cannot move the KV prefix.
        let emit_reasoning = Self::reasoning_frames_enabled(settings.show_thinking, is_voice);

        // PAI-5 P2. An owned counter for the 'static stream closure, resolved
        // here rather than inside it. Deliberately AFTER the trim above, which
        // already builds this counter on the default configuration, so the
        // common case pays nothing new. Nothing here reaches `PromptState`, so
        // the KV prefix cannot move.
        let reasoning_counter = self.token_counter_handle().await;

        // Cancellation token: when the stream is dropped (e.g. voice interrupt),
        // the DropGuard fires and cancels the token.  Goose's agent loop checks
        // `is_token_cancelled()` at each turn boundary and exits early, so
        // interruption propagates faster than waiting for the channel-drop path
        // through spawn_blocking.
        let cancel_token = CancellationToken::new();
        let cancel_guard = cancel_token.clone().drop_guard();

        // ── PAI-6 P3: this turn's delegation authority ────────────────────────
        //
        // The ceiling on anything this turn delegates to. Deliberately built
        // HERE and from these two values:
        //
        // - `turn_scope` is `request.profile_scope`, which `resolve_turn_scope`
        //   decided at the API edge from the session's stored identity. Nothing
        //   a model emits reaches it, and `AgentRequest` is never deserialized
        //   from an HTTP body, so it is not forgeable by a caller either.
        // - the tool set is this turn's ENTITLEMENT, which is `allowed_tools`
        //   widened back to every group this session is PERMITTED to hold, and
        //   then put through the same guest subtraction in the same branch. In
        //   "all" mode the two are the same set and this is `allowed_tools`
        //   itself. Passing the catalog, or the set before section 6d, would make
        //   every intersection downstream a no-op — the exact shape PAI-1 P5
        //   shipped and had to repair, and
        //   `the_turn_authority_is_built_from_the_published_allow_set` fails if
        //   this call is ever moved above that subtraction.
        //
        //   Entitlement rather than allow-set, because narrowing is a decision
        //   about THIS turn's prompt budget and a child gets its own prompt. A
        //   parent holding 4 core groups + 1 scored, three of whose cores are on
        //   `groups_denied_to_subagents`, handed a research role asking for
        //   `giap-knowledge` + `giap-news` an EMPTY set — unless its opening
        //   message happened to score those two. No authority was gained by the
        //   old bound: the parent can reach any permitted group itself with
        //   `enable_tool_group`, so `loaded` was never a boundary, only a
        //   position.
        //
        // The lease is moved into the stream closure beside `cancel_guard`, so
        // the authority dies with the turn: a delegation can only ever be
        // authorised while the turn that authorised it is still running.
        let authority_lease = self.turn_authorities.publish(
            &goose_sid,
            pond_core::shared::domain::orchestration::DelegationAuthority::for_turn(
                session_id.clone(),
                turn_scope.clone(),
                entitled_tools
                    .as_ref()
                    .unwrap_or(&allowed_tools)
                    .iter()
                    .map(String::as_str),
            ),
            cancel_token.clone(),
        );

        // The provider this turn will actually reply through, for PAI-6 P4's
        // device claim below. Taken from the same settings load the rest of the
        // turn used, so it cannot disagree with what `turn_profile` budgeted
        // against.
        let turn_provider = settings.chat_provider.clone();
        let device_session_id = session_id.clone();

        // ── PAI-6 P6: where this turn hears about its own delegations ────────
        //
        // Subscribed HERE rather than inside the closure, and to the GIAP
        // session id, which is what a `TaskSpec` names as its parent. The
        // ordering matters in one direction only: a frame published before the
        // subscription exists is dropped, and `spawn` cannot run before the
        // stream does — it refuses any delegation whose parent turn is not
        // published, and this turn publishes its authority two statements up.
        //
        // Moved into the closure beside the authority lease and the device
        // claim, so it is dropped by whatever ends the turn, including the
        // client hanging up. Its `Drop` takes the bus entry with it, so a child
        // that outlives its parent's stream publishes into nothing rather than
        // into a stale channel.
        let mut progress = crate::orchestrator::process_progress_bus().subscribe(&session_id);

        let stream = async_stream::stream! {
            // Hold the guard — dropped when the stream is dropped → cancels token.
            let _guard = cancel_guard;
            // Same lifetime, same reason: dropped with the stream, which revokes
            // this turn's authority to delegate.
            let _authority_lease = authority_lease;

            // ── PAI-6 P4 / invariant 3: this turn's claim on the device ───────
            //
            // The half of invariant 3 P2 left open. The subagent semaphore
            // serialised children against each other; it did not serialise them
            // against the PARENT, and `goose-local-inference` keeps exactly one
            // retained KV prefix per model slot for the whole process — so a
            // child replying between two of this turn's provider calls does not
            // queue, it overwrites, and this turn pays a 3.7 s re-prefill on its
            // next call.
            //
            // Taken INSIDE the stream rather than before it is returned, so the
            // HTTP response has already started and the client sees a stream
            // that is waiting rather than a request that is hanging. Held for
            // the whole turn, and released by `Drop` on every ending there is,
            // including the client hanging up.
            //
            // `None` on a provider that runs somewhere else: there is no shared
            // prefix to protect and hosted conversations still run four abreast.
            // A synchronous delegation does NOT queue behind this claim — it
            // inherits it, see `claim_device_for_turn`.
            let _device = crate::orchestrator::claim_device_for_turn(
                &device_session_id,
                &turn_provider,
                &cancel_token,
            ).await;

            yield Ok(AgentStreamEvent::Status { content: "Agent working...".to_string() });
            let mut total_output_chars: usize = 0;
            // Track tool call ID → tool name so ToolResult events carry the tool name.
            let mut tool_id_to_name: HashMap<String, String> = HashMap::new();
            // Tool-call ids the allow-set guard refused to surface. Their results
            // are dropped when they arrive rather than being emitted with an
            // empty tool name.
            let mut suppressed_tool_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            // Wall-clock start per tool call (keyed by Goose tool-call ID) for latency.
            let mut tool_call_starts: HashMap<String, std::time::Instant> = HashMap::new();

            tracing::info!(
                target: "giap::trace",
                kind = "turn_start",
                session_id = %session_id,
                model = %settings.chat_model,
                provider = %settings.chat_provider,
                message_len = user_msg_len,
            );

                let mut turn_stats = pond_core::shared::domain::turn_stats::TurnStats::default();
                let mut saw_usage = false;
                // ── Empty-turn recovery (GIAP-owned) ─────────────────────────
                // A turn that yields no text and no tool call is a failure of the
                // harness, not an answer, so it never reaches the user as silence.
                // Goose detects the empty turn and hands straight back
                // (GOOSE_MAX_EMPTY_TURN_RETRIES=0); GIAP re-engages with a changed
                // prompt, and says something actionable once the budget is spent.
                let mut attempt: usize = 0;
                // PAI-5 P1 (granularity). One reasoning passage in flight.
                // Declared outside `'attempts` only so it is obviously a single
                // buffer; every attempt flushes it before it ends, so no
                // reasoning ever crosses a re-engagement boundary.
                let mut reasoning = ReasoningCoalescer::default();
                'attempts: loop {
                    let attempt_text = if attempt == 0 {
                        turn_text.clone()
                    } else {
                        format!("{turn_text}\n\n{EMPTY_TURN_STEER}")
                    };
                    let attempt_msg =
                        attach_images(Message::user().with_text(&attempt_text), &turn_images);
                    // The completeness check, armed for this turn only.
                    //
                    // Goose's reply loop, on a turn that finishes WITHOUT calling
                    // a tool, re-prompts "check whether the goal has been fully
                    // met; if not, continue working toward it" and iterates once
                    // more. It is guarded on a goal being set, and nothing in
                    // GIAP ever set one -- so the arm was dead and the loop
                    // terminated on "the model stopped asking for tools", never
                    // on "the question was answered".
                    //
                    // Measured 2026-08-12, "what time is it in the first 10
                    // states of the USA alphabetically?": gemma-4-E2B made ZERO
                    // tool calls and answered with a single time for all ten
                    // states -- its own local clock, in fact -- and every layer
                    // reported success. gemma-4-E4B declined honestly instead,
                    // so the size of the failure is model-dependent, but the
                    // absence of any check is not.
                    //
                    // `set_session_goal` and NOT `set_goal`: one retained
                    // `Arc<GooseAgent>` serves up to four concurrent chat
                    // streams (`sse_semaphore`), and the process-wide slot would
                    // put one household member's request text into another
                    // member's turn. That is a PAI-1 boundary crossing, not a
                    // tidiness point, and it is why the fork carries the
                    // session-keyed variant.
                    //
                    // The RAW request, never `turn_text`: the assembled text
                    // carries `<system-context>` with memories and the turn
                    // budget, and feeding those back as a goal would restate
                    // household memories to the model as something to satisfy.
                    //
                    // Gated because it costs roughly TWICE the inferences per
                    // turn: the check re-arms whenever the model does more work,
                    // so it fired three times on one measured turn rather than
                    // once. That is the mechanism and not a defect -- capping it
                    // at a single check would have stopped the seven-tool-call
                    // turn that finally produced an answer at around its fourth.
                    // Defaulted ON because the measurements say it is worth the
                    // cost; see `Settings::goal_check_enabled` for why this is a
                    // household setting rather than a `ModelClass` tier.
                    agent_clone
                        .set_session_goal(
                            &turn_goose_sid,
                            settings
                                .goal_check_enabled
                                .then(|| request.message.clone()),
                        )
                        .await;
                    let attempt_cfg = goose::agents::types::SessionConfig {
                        id: turn_goose_sid.clone(),
                        schedule_id: None,
                        // Voice requests get the tighter #105 cap so a runaway loop
                        // can't keep the speaker silent for the full text-chat turn
                        // budget. `agent_max_turns = 0` resolves to the uncapped
                        // sentinel here.
                        max_turns: Some(max_turns),
                        retry_config: None,
                    };
                    // Text or a tool call — anything the user actually receives.
                    let mut produced_visible = false;
                let mut goose_stream = match agent_clone.reply(attempt_msg, attempt_cfg, Some(cancel_token.clone())).await {
                    Ok(s) => s,
                    Err(e) => {
                        yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                        return;
                    }
                };

                // Labelled, and the label is load-bearing: the guard
                // `the_last_reasoning_block_of_a_turn_is_flushed_after_the_event_loop`
                // finds this loop's closing brace to prove the trailing flush is
                // outside it, and a bare `loop {` would match the `'attempts`
                // loop above instead.
                'engine: loop {
                    // PAI-6 P6. Two things can happen next: the engine yields,
                    // or a delegation running underneath this turn's `delegate`
                    // tool call says something. The second has no other route
                    // here — the child runs INSIDE the poll of `goose_stream`,
                    // so between the tool call and its result this stream would
                    // otherwise yield nothing at all for the whole of the
                    // child's run. `next_parent_step` owns the select, the
                    // bias, and the cancel-safety argument for both branches.
                    let event_result = match crate::orchestrator::next_parent_step(
                        &mut goose_stream,
                        &mut progress,
                    ).await {
                        crate::orchestrator::ParentStep::Progress(frame) => {
                            // Straight out, unabsorbed. It is not an engine
                            // event: it never becomes text, a tool result, or
                            // anything else this turn persists. PAI-6
                            // invariant 4 — only the delegation's RESULT does,
                            // and that arrives as the tool response below.
                            yield Ok(frame.into());
                            continue 'engine;
                        }
                        crate::orchestrator::ParentStep::EngineEnded => break 'engine,
                        crate::orchestrator::ParentStep::Engine(event_result) => event_result,
                    };
                    match event_result {
                        Ok(event) => match event {
                            goose::agents::AgentEvent::Message(msg) => {
                                // Surface the model's reasoning BEFORE the tool
                                // calls and the answer text of the same message,
                                // which is the order the provider produced them
                                // in. Deliberately does NOT set
                                // `produced_visible`: reasoning alone leaves the
                                // user with nothing, and marking the turn visible
                                // would suppress the empty-turn recovery below.
                                // PAI-5 P2. Counted before the display gate and
                                // outside it: the reasoning was decoded either
                                // way, so the cost is the same whether or not
                                // anybody is shown it.
                                *turn_stats.reasoning_tokens.get_or_insert(0) +=
                                    Self::count_reasoning_tokens(&msg, reasoning_counter.as_ref());
                                // PAI-5 P1 (granularity). Buffer the fragment;
                                // emit only when the passage is finished. On
                                // the local/gguf path one message is one TOKEN
                                // PIECE, so yielding per message rendered a
                                // paragraph per token with the spacing trimmed
                                // out of it.
                                reasoning.push(&msg, emit_reasoning);
                                let ends_block = Self::message_ends_reasoning(&msg);
                                let overflowed = reasoning.over_cap();
                                if overflowed {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        bytes = reasoning.len(),
                                        "reasoning passage exceeded the coalescer cap; \
                                         emitting it as a partial block",
                                    );
                                }
                                if ends_block || overflowed {
                                    if let Some(content) = reasoning.flush() {
                                        yield Ok(AgentStreamEvent::Thinking { content });
                                    }
                                }
                                // Emit tool call and result events
                                for content in &msg.content {
                                    match content {
                                        goose::conversation::message::MessageContent::ToolRequest(tr) => {
                                            if let Ok(tool_call) = &tr.tool_call {
                                                let tool_name = tool_call.name.to_string();
                                                // Guard: suppress tool calls not in the validated schema.
                                                // An empty allowed set means no extensions loaded —
                                                // every call is a hallucination and must be blocked.
                                                //
                                                // Read LIVE from the session's shim controls rather than
                                                // a snapshot: `enable_tool_group` widens the set mid-turn
                                                // and the very next call must be admitted, or the escape
                                                // hatch would enable a group and then block its use.
                                                if !guard_controls.is_tool_allowed(&tool_name) {
                                                    // NOT a block. Goose keeps every extension
                                                    // loaded agent-wide, collects every
                                                    // ToolRequest regardless of whether its
                                                    // schema was published, and yields
                                                    // AgentEvent::Message BEFORE dispatching. So
                                                    // by the time this runs the tool either has
                                                    // run or is about to, and nothing here can
                                                    // stop it. What this does is refuse to
                                                    // SURFACE the call — and, via
                                                    // `suppressed_tool_ids` below, refuse to
                                                    // surface its result.
                                                    //
                                                    // A real execution gate needs an inspector
                                                    // registered with goose's
                                                    // ToolInspectionManager, whose `add_inspector`
                                                    // is private and whose field is pub(super) —
                                                    // i.e. it needs a fork patch, tracked in
                                                    // docs/goose-patch-management.md.
                                                    suppressed_tool_ids.insert(tr.id.clone());
                                                    tracing::warn!(
                                                        tool = %tool_name,
                                                        tool_id = %tr.id,
                                                        "tool call outside this session's allow-set; suppressing its call and result events (the tool itself still runs)",
                                                    );
                                                    continue;
                                                }
                                                tool_id_to_name.insert(tr.id.clone(), tool_name.clone());
                                                tool_call_starts.insert(tr.id.clone(), std::time::Instant::now());
                                                tracing::info!(
                                                    target: "giap::trace",
                                                    kind = "tool_call",
                                                    session_id = %session_id,
                                                    tool = %tool_name,
                                                    tool_id = %tr.id,
                                                );
                                                produced_visible = true;
                                                yield Ok(AgentStreamEvent::ToolCall {
                                                    id: tr.id.clone(),
                                                    tool: tool_name,
                                                    input: tool_call.arguments.clone().map(serde_json::Value::Object),
                                                });
                                            }
                                        }
                                        goose::conversation::message::MessageContent::ToolResponse(tr) => {
                                            // A suppressed call's RESULT must never reach the
                                            // client. This used to rely on absence from
                                            // `tool_id_to_name`, which the guard's `continue`
                                            // caused — but absence only blanked the NAME:
                                            // `unwrap_or_default()` gave "" and the content was
                                            // streamed anyway. On a Guest turn that meant a
                                            // withheld `giap-memory__recall_memories` returned
                                            // the household's memories to the device under an
                                            // empty tool name. Track suppression explicitly.
                                            if suppressed_tool_ids.remove(&tr.id) {
                                                tracing::warn!(
                                                    target: "giap::trace",
                                                    kind = "tool_result_suppressed",
                                                    session_id = %session_id,
                                                    tool_id = %tr.id,
                                                    "dropped the result of a tool outside this session's allow-set",
                                                );
                                                tool_call_starts.remove(&tr.id);
                                                continue;
                                            }
                                            // Surface BOTH arms. A failed dispatch still
                                            // reaches the model — goose puts the error into
                                            // its own conversation — so dropping the Err here
                                            // only blinded the UI and pond_system.db. The turns
                                            // that most needed explaining were the ones that
                                            // left a tool_call with no matching tool_result.
                                            let (content_text, failed) = match &tr.tool_result {
                                                Ok(tool_result) => (
                                                    tool_result
                                                        .content
                                                        .iter()
                                                        .filter_map(|c| match c.deref() {
                                                            rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                                            _ => None,
                                                        })
                                                        .collect::<Vec<_>>()
                                                        .join("\n"),
                                                    false,
                                                ),
                                                Err(e) => (format!("Error: {e}"), true),
                                            };

                                            let tool_name = tool_id_to_name
                                                .get(&tr.id)
                                                .cloned()
                                                .unwrap_or_default();
                                            let tool_latency_ms = tool_call_starts
                                                .remove(&tr.id)
                                                .map(|s| s.elapsed().as_millis() as u64)
                                                .unwrap_or(0);
                                            tracing::info!(
                                                target: "giap::trace",
                                                kind = "tool_result",
                                                session_id = %session_id,
                                                tool = %tool_name,
                                                tool_id = %tr.id,
                                                latency_ms = tool_latency_ms,
                                                result_len = content_text.len(),
                                                failed = failed,
                                            );
                                            if failed {
                                                tracing::warn!(
                                                    tool = %tool_name,
                                                    tool_id = %tr.id,
                                                    error = %content_text,
                                                    "tool call failed",
                                                );
                                            }
                                            yield Ok(AgentStreamEvent::ToolResult {
                                                id: tr.id.clone(),
                                                tool: tool_name,
                                                content: content_text,
                                            });
                                        }
                                        _ => {}
                                    }
                                }
                                // Emit raw text — the SSE layer's stateful ThoughtFilter
                                // handles stripping of <think>, <thought>, and
                                // <|channel>thought...<channel|> tags across chunk
                                // boundaries.  A per-chunk strip here interferes with
                                // the stateful filter (it eats close tags the filter
                                // is waiting for, causing answer text to be swallowed).
                                let raw_text = msg.as_concat_text();
                                if !raw_text.is_empty() && raw_text.trim() == GOOSE_EMPTY_TURN_MESSAGE {
                                    // Goose reporting the turn produced nothing. That is
                                    // a signal to the harness, not an answer to the user
                                    // — swallow it so the recovery loop below re-engages.
                                    tracing::warn!(
                                        session_id = %session_id,
                                        "goose reported an empty turn",
                                    );
                                } else if !raw_text.is_empty() {
                                    // Goose signals "budget exhausted" by streaming a
                                    // fixed sentence as ordinary assistant text (see
                                    // GOOSE_MAX_TURNS_MESSAGE). Re-emit it as a
                                    // structured event so a client can offer a real
                                    // continue action; the text still goes through so
                                    // history, persistence, and voice stay consistent.
                                    let hit_turn_limit = raw_text.trim() == GOOSE_MAX_TURNS_MESSAGE;
                                    produced_visible = true;
                                    total_output_chars += raw_text.len();
                                    yield Ok(AgentStreamEvent::Text { content: raw_text });
                                    if hit_turn_limit {
                                        tracing::info!(
                                            target: "giap::trace",
                                            kind = "turn_limit_reached",
                                            session_id = %session_id,
                                            max_turns,
                                        );
                                        yield Ok(AgentStreamEvent::TurnLimitReached { max_turns });
                                    }
                                }
                            }
                            goose::agents::AgentEvent::HistoryReplaced(_) => {
                                yield Ok(AgentStreamEvent::Status { content: "Compacting context...".to_string() });
                            }
                            // Per-inference usage from the provider. A turn can hold
                            // several inferences (tool round-trips): the FINAL one's
                            // input is the turn's real context load; outputs sum.
                            goose::agents::AgentEvent::Usage(pu) => {
                                saw_usage = true;
                                turn_stats.inference_count += 1;
                                if let Some(input) = pu.usage.input_tokens {
                                    turn_stats.prompt_tokens = input.max(0) as u32;
                                }
                                if let Some(output) = pu.usage.output_tokens {
                                    turn_stats.completion_tokens += output.max(0) as u32;
                                }
                                if let Some(stats) = &pu.stats {
                                    if turn_stats.ttft_ms.is_none() {
                                        turn_stats.ttft_ms = stats.time_to_first_token_ms;
                                    }
                                    if let Some(load) = stats.model_load_ms {
                                        turn_stats.model_load_ms =
                                            Some(turn_stats.model_load_ms.unwrap_or(0) + load);
                                    }
                                    if let Some(prefill) = stats.prefill_ms {
                                        turn_stats.prefill_ms =
                                            Some(turn_stats.prefill_ms.unwrap_or(0) + prefill);
                                    }
                                    if let Some(elapsed) = stats.elapsed_ms {
                                        let decode =
                                            elapsed.saturating_sub(stats.prefill_ms.unwrap_or(0));
                                        turn_stats.decode_ms =
                                            Some(turn_stats.decode_ms.unwrap_or(0) + decode);
                                    }
                                    if let Some(n_ctx) = stats.effective_context_tokens {
                                        turn_stats.context_limit_tokens = Some(n_ctx as u32);
                                    }
                                    if let Some(draft) = &stats.draft {
                                        turn_stats.draft_accept_rate = Some(draft.accept_rate as f32);
                                    }
                                }
                            }
                            _ => {}
                        },
                        Err(e) => {
                            yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                        }
                    }
                }

                    // PAI-5 P1 (granularity). The passage the stream ended on.
                    // A turn CAN end in pure reasoning — gemma-4-E2B closes its
                    // thinking block and emits end_of_turn with no text — and a
                    // coalescer that drops that block is strictly worse than
                    // the confetti it replaced. Inside `'attempts` so it is
                    // both the per-attempt and the end-of-stream flush.
                    if let Some(content) = reasoning.flush() {
                        yield Ok(AgentStreamEvent::Thinking { content });
                    }
                    if produced_visible {
                        break 'attempts;
                    }
                    if attempt >= MAX_EMPTY_TURN_REENGAGEMENTS {
                        tracing::warn!(
                            session_id = %session_id,
                            attempts = attempt + 1,
                            "empty turn: re-engagement budget spent",
                        );
                        total_output_chars += EMPTY_TURN_EXHAUSTED_MESSAGE.len();
                        yield Ok(AgentStreamEvent::Text {
                            content: EMPTY_TURN_EXHAUSTED_MESSAGE.to_string(),
                        });
                        break 'attempts;
                    }
                    attempt += 1;
                    tracing::warn!(
                        session_id = %session_id,
                        attempt,
                        max = MAX_EMPTY_TURN_REENGAGEMENTS,
                        "empty turn: re-engaging with a steered prompt",
                    );
                    yield Ok(AgentStreamEvent::Status {
                        content: format!(
                            "No response — re-engaging ({attempt}/{MAX_EMPTY_TURN_REENGAGEMENTS})"
                        ),
                    });
                }
            // What the empty-turn recovery actually cost, recorded rather than
            // logged. `attempt` is incremented once per re-engagement and never
            // on the exhaustion path, so it is already "attempts beyond the
            // first": 0 for an ordinary turn, and on the exhausted path the
            // number of steered retries that also came back silent.
            //
            // This is the hidden half of what thinking costs. A re-engaged turn
            // is not a slow turn, it is the whole turn again — prefill, tools
            // and all — and until this line the only trace was a WARN nobody
            // roots their log at.
            turn_stats.reengagements = attempt as u32;

            // Per-turn usage from the provider's per-inference Usage events.
            // Fall back to the chars/4 heuristic only when the provider emitted
            // no Usage events at all (some HTTP providers).
            let usage = if saw_usage {
                turn_stats.context_used_tokens = Some(turn_stats.prompt_tokens);
                pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: turn_stats.prompt_tokens,
                    completion_tokens: turn_stats.completion_tokens,
                    // Reported alongside, never deducted. The provider's output
                    // count probably already covers the reasoning decode, but it
                    // is the engine's number and this one is ours; subtracting
                    // would corrupt the measured one to flatter the derived one.
                    reasoning_tokens: turn_stats.reasoning_tokens,
                }
            } else {
                pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: (user_msg_len / 4).max(1) as u32,
                    completion_tokens: (total_output_chars / 4).max(1) as u32,
                    // `total_output_chars` never contained the reasoning:
                    // thinking blocks are not `as_text()`, so they never reached
                    // the text accumulator. Additive here, with no overlap.
                    reasoning_tokens: turn_stats.reasoning_tokens,
                }
            };
            turn_stats.finalize_rates();
            if saw_usage {
                last_prompt_tokens_map
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(session_id.clone(), turn_stats.prompt_tokens);
            }
            let total_latency_ms = turn_start.elapsed().as_millis() as u64;
            tracing::info!(
                target: "giap::trace",
                kind = "turn_end",
                session_id = %session_id,
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                total_latency_ms,
                ttft_ms = turn_stats.ttft_ms,
                prefill_ms = turn_stats.prefill_ms,
                decode_tok_per_sec = turn_stats.decode_tok_per_sec,
                context_used_tokens = turn_stats.context_used_tokens,
                context_limit_tokens = turn_stats.context_limit_tokens,
                inference_count = turn_stats.inference_count,
            );
            let stats = saw_usage.then_some(turn_stats);
            yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: Some(usage), stats });
        };

        Ok(Box::pin(stream))
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        let mut caps = self
            .model_capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Voice mode disables expensive/leaky capabilities: thinking tokens
        // waste TTS time, vision/audio inputs aren't used in voice flow.
        //
        // `vision = false` here is load-bearing for the prompt too:
        // `vision_section_applies` reads the SAME instance-level flag, so the
        // `<vision>` section cannot assert a capability this method denies.
        if self.voice_mode.load(std::sync::atomic::Ordering::Relaxed) {
            caps.thinking = false;
            caps.vision = false;
            caps.audio_input = false;
        }
        caps
    }

    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let mut stream: futures::stream::BoxStream<'static, Result<AgentStreamEvent>> =
            self.chat_stream(request).await?;
        let mut full_text = String::new();
        let mut tool_call_ids = Vec::new();

        while let Some(event_result) = stream.next().await {
            match event_result? {
                AgentStreamEvent::Text { content } => {
                    full_text.push_str(&content);
                }
                AgentStreamEvent::ToolCall { id, .. } => {
                    tool_call_ids.push(id);
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow!(content));
                }
                _ => {}
            }
        }

        if full_text.is_empty() {
            return Err(anyhow!("Received empty response from Goose agent"));
        }

        let mut metadata = HashMap::new();
        if !tool_call_ids.is_empty() {
            metadata.insert(
                "tool_calls".to_string(),
                serde_json::to_string(&tool_call_ids).unwrap_or_default(),
            );
        }

        Ok(AgentResponse {
            text: full_text,
            metadata,
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
        self.chat_stream(request).await
    }

    async fn call_tool(
        &self,
        session_id: &str,
        tool_name: &str,
        args_json: &str,
    ) -> Result<String> {
        let goose_sid = self.resolve_goose_session(session_id).await;
        let session = self
            .session_manager
            .get_session(&goose_sid, false)
            .await
            .map_err(|e| anyhow!("Failed to get session: {e}"))?;

        // Parse the JSON args into the Map that rmcp expects.
        let arguments: serde_json::Map<String, serde_json::Value> =
            if args_json.is_empty() || args_json == "{}" {
                serde_json::Map::new()
            } else {
                serde_json::from_str(args_json).unwrap_or_default()
            };

        let tool_call = rmcp::model::CallToolRequestParams::new(tool_name.to_string())
            .with_arguments(arguments);

        let request_id = uuid::Uuid::new_v4().to_string();
        let (_req_id, dispatch_result) = self
            .agent
            .dispatch_tool_call(tool_call, request_id, None, &session)
            .await;

        match dispatch_result {
            Ok(mut tool_call_result) => {
                // ToolCallResult.result is a Future — await it to get the actual result.
                let tool_result = tool_call_result.result.as_mut().await;
                match tool_result {
                    Ok(call_result) => {
                        let text = call_result
                            .content
                            .iter()
                            .filter_map(|c| match c.deref() {
                                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        Ok(text)
                    }
                    Err(e) => Err(anyhow!("Tool returned error: {}", e.message)),
                }
            }
            Err(e) => Err(anyhow!("Tool dispatch failed: {}", e.message)),
        }
    }

    /// Release the Goose engine session paired with a deleted GIAP session,
    /// so its messages and `usage_ledger` rows do not outlive the GIAP row
    /// that referenced them.
    ///
    /// Deliberately does NOT go through `resolve_goose_session`: that method
    /// CREATES (and hydrates) a fresh engine session for a GIAP id it does
    /// not recognise, which is exactly wrong on a delete path — a session
    /// with no prior engine pairing has nothing to forget, and conjuring one
    /// just to immediately delete it would pay for a full history replay for
    /// no reason. Instead this looks up an already-resolved pairing (process
    /// cache, then the persisted one) and does nothing if there isn't one.
    ///
    /// The persisted pairing is used as found, without re-validating it
    /// against Goose first (unlike `resolve_goose_session`): if it is already
    /// stale, `SessionManager::delete_session` simply fails with "not found",
    /// which is logged and swallowed below like every other failure here.
    async fn forget_session(&self, session_id: &str) {
        let cached = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();

        let goose_sid = match cached {
            Some(gid) => Some(gid),
            None => match &self.giap_session_storage {
                Some(storage) => storage
                    .get_engine_session_id(session_id)
                    .await
                    .ok()
                    .flatten(),
                None => None,
            },
        };

        // Drop the in-process pairing unconditionally, before attempting the
        // engine-side delete: whether or not the delete below succeeds, this
        // GIAP id is being removed and must never resolve to this (or any)
        // Goose session again on a later turn.
        self.goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);

        let Some(goose_sid) = goose_sid else {
            // No engine session was ever paired with this GIAP id — e.g. a
            // session created and deleted before its first turn. Nothing to
            // release.
            return;
        };

        if let Err(e) = self.session_manager.delete_session(&goose_sid).await {
            // Best-effort: the pond row is about to be deleted regardless.
            // Leaving this engine session behind is strictly better than
            // failing the user's delete request over storage this API
            // doesn't even expose.
            tracing::warn!(
                "Failed to delete Goose engine session '{goose_sid}' for GIAP session \
                 '{session_id}': {e}"
            );
        }
    }

    /// PAI-4 P5. This adapter is the only implementor that returns `Some`,
    /// because it is the only one that tracks a prefix at all — `Agent`'s
    /// default `None` is correct for every mock and for any agent whose engine
    /// keeps no KV cache we can see.
    ///
    /// A snapshot, not a handle. The state is `Copy` and every caller acts on
    /// it immediately; handing out a lock would let a compaction decision
    /// straddle the prompt assembly it is describing.
    fn prefix_cache_state(&self) -> Option<PrefixCacheState> {
        Some(*self.prefix_cache.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

// ── PAI-6 P2: driving a child agent ─────────────────────────────────────────
//
// The mechanism half of orchestration. Every policy decision was already made
// in `pond-core` (the `TaskSpec`) or in `orchestrator.rs` (the `ChildPlan`);
// nothing below chooses a scope, a tool or a turn budget. It lives in this file
// rather than in `orchestrator.rs` because it needs `agent`, `session_manager`,
// `current_provider`, `settings_repo` and `template_repo`, all of which are
// private fields — and adding public accessors for them would put the parent's
// live provider and session manager on this crate's API surface for the sake of
// one caller in the same crate.
impl GooseAdapter {
    /// The parent's live engine surface, for [`crate::orchestrator::build_child_plan`].
    ///
    /// The tool inventory comes from the PARENT's own Goose session, which is
    /// what makes "a child's tools are a subset of the parent's" structural: a
    /// tool the parent does not have loaded cannot appear here, so no plan can
    /// name it. Passing the catalog instead would make every downstream
    /// intersection a no-op — the shape PAI-1 P5 shipped and had to repair.
    pub(crate) async fn child_environment(
        &self,
        parent_session_id: &str,
    ) -> Result<crate::orchestrator::ChildEnvironment> {
        let settings = self.settings_repo.get().await?;
        let template_content = self
            .template_repo
            .get(&settings.prompt_style)
            .await
            .ok()
            .flatten()
            .map(|t| t.content)
            .unwrap_or_else(|| fallback_prompt().to_string());

        // A deliberately lean `PromptState`. A subagent gets a fraction of the
        // window by construction (`context_fraction`), so it always gets the
        // compact prompt; it has no devices to talk about and no prose tool
        // list, because the envelope names its tools exactly; and
        // `thinking_enabled` is false because nothing consumes a child's
        // reasoning — the drain loop keeps `as_concat_text()`, which drops
        // `MessageContent::Thinking` — so paying for it would be pure cost.
        let prompt_state = PromptState {
            current_date: String::new(),
            current_time: String::new(),
            device_count: 0,
            has_home_devices: false,
            online_device_names: String::new(),
            voice_mode: false,
            canvas_mode: false,
            available_tools: Vec::new(),
            thinking_enabled: false,
            compact_prompt: true,
            native_tools_json: matches!(settings.chat_provider.as_str(), "local" | "gguf"),
            prefix_hash: None,
        };

        // THE respecification of this phase. The bullet said "render
        // subagent_system.md"; that file was deleted on 2026-08-06 with the
        // whole of `giap_prompts.rs`, and Goose's own copy is not substitutable
        // (`build_subagent_prompt` renders it unconditionally and overrides the
        // child's system prompt from inside a function GIAP cannot reach). This
        // is the live path — the same `build_prompt_partition` the parent's turn
        // uses — so a child introduces itself as this assistant rather than as
        // "a specialized subagent within the goose AI framework, created by
        // AAIF". Only the static prefix is taken: the dynamic suffix is date,
        // time and profile lines, and a child is given its task in words.
        let partition = build_prompt_partition(&settings, None, &prompt_state, &template_content);

        let goose_sid = self.resolve_goose_session(parent_session_id).await;
        let mut parent_tools: std::collections::BTreeMap<
            String,
            std::collections::BTreeSet<String>,
        > = std::collections::BTreeMap::new();
        for tool in self.agent.list_tools(&goose_sid, None).await {
            let name = tool.name.to_string();
            // Unprefixed names, because that is what Goose matches
            // `available_tools` against: `dispatch_tool_call` checks
            // `is_tool_available(&resolved.actual_tool_name)`, not the
            // `extension__tool` name the model sees. A name with no prefix at
            // all belongs to no extension — Goose plumbing, or one of the
            // `unprefixed_tools: true` platform extensions — and
            // `split_extension_tool` drops it rather than guess an owner.
            let Some((extension, bare)) = crate::orchestrator::split_extension_tool(&name) else {
                continue;
            };
            parent_tools
                .entry(extension.to_string())
                .or_default()
                .insert(bare.to_string());
        }

        Ok(crate::orchestrator::ChildEnvironment {
            provider_name: settings.chat_provider.clone(),
            base_system_prefix: partition.static_prefix,
            parent_tools,
        })
    }

    /// Create the child's engine session.
    ///
    /// `SessionType::SubAgent` so Goose's own bookkeeping knows what it is, and
    /// so anything that later enumerates sessions can tell a delegation apart
    /// from a conversation. The row has to exist before the plan runs:
    /// `Agent::update_provider` ends in `session_manager.update(id).apply()`,
    /// which errors on a missing row and surfaces as "Failed to set provider on
    /// sub agent" — the same failure class as the recorded `resolve_goose_session`
    /// foreign-key incident, wearing a provider's clothes.
    pub(crate) async fn open_child_session(&self, role: &str) -> Result<String> {
        let session = self
            .session_manager
            .create_session(
                std::env::current_dir().unwrap_or_default(),
                format!("giap-subagent:{role}"),
                goose::session::session_manager::SessionType::SubAgent,
                GooseMode::Auto,
            )
            .await
            .map_err(|e| anyhow!("Failed to create subagent session for role '{role}': {e}"))?;
        Ok(session.id)
    }

    /// Delete a child's engine session once its run is over.
    ///
    /// Also drops its `ShimControls` entry. That map evicts OLDEST-FIRST with no
    /// regard for whether an entry is live, so leaving a child's entry to age
    /// out would mean sixty-four delegations silently evicting a long-running
    /// PARENT's allow-set — after which the parent's turns are pass-through and
    /// a Guest's `subtract_guest_denied_tools` result goes with them. PAI-6 P3.
    pub(crate) async fn release_child_session(&self, child_session_id: &str) {
        self.shim_controls.forget_session(child_session_id);
        if let Err(e) = self.session_manager.delete_session(child_session_id).await {
            tracing::warn!("Failed to release subagent engine session '{child_session_id}': {e}");
        }
    }

    /// Run one child agent to completion, or until `cancel` trips.
    ///
    /// This is the ~120 lines that would otherwise have been Goose's
    /// `get_agent_messages`. See `orchestrator.rs`'s module doc for why they are
    /// here instead of behind a sixth fork patch.
    pub(crate) async fn run_child_agent(
        &self,
        plan: crate::orchestrator::ChildPlan,
        cancel: CancellationToken,
    ) -> Result<crate::orchestrator::ChildOutcome> {
        let (provider, model_config) = self
            .current_provider
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| {
                anyhow!(
                    "no provider is configured on this pond yet - a subagent cannot be given one"
                )
            })?;

        // PAI-6 P7. The role's model, if the plan decided it gets one. The
        // decision was made in `build_child_plan`, which is the only place that
        // holds both the role's request and the provider the parent is on; the
        // whole of what happens here is applying it, and the applying lives in
        // `child_model_config` because this function cannot be reached without a
        // live provider. Note the child's session gets its own `ModelConfig` on
        // the SAME provider object — the parent's cached pair is untouched, so a
        // delegation cannot move the model out from under the parent's turn.
        let model_config = crate::orchestrator::child_model_config(&plan.model, model_config);

        // A FRESH agent, deliberately. `Agent::with_config` builds an
        // `ExtensionManager` with no extensions and nothing auto-loads defaults
        // into it, so the child starts with an empty tool surface and gets
        // exactly what the plan puts in. That is what makes invariants 1, 2 and
        // 6 structural rather than checked.
        //
        // `scheduler_service: None` keeps `manage_schedule_tool` off the child's
        // list. `GooseMode::Auto` is MANDATORY, not a preference: any
        // approval-requiring mode hangs forever on the child's
        // `confirmation_rx`, because nothing forwards an ActionRequired message
        // to a parent. The consequence is that the child's TOOL SET is its only
        // safety boundary, which is why `available_tools` is populated
        // explicitly and why nothing that actuates a device belongs in a role.
        // ── PAI-6 P3: the child's second tool layer, and its prompt ───────────
        //
        // Published BEFORE the child's first provider call, which is the whole
        // requirement: `ShimControls::existing_session` deliberately does not
        // create entries, so a session GIAP never chatted in resolves to `None`
        // and `enforce_tools(tools, &None)` is a silent no-op. A subagent is
        // exactly that kind of session. Without this the child's only tool
        // boundary is `ExtensionConfig::available_tools`, which is real (it
        // refuses inside `dispatch_tool_call`) but is one layer, and it only
        // stops the CALL — the tool is still listed to the model, which then
        // spends turns trying it.
        //
        // The system override is not a nicety either. A child's prompt is the
        // parent's static prefix plus GIAP's delegation envelope, so
        // `enforce_system`'s `incoming.starts_with(prefix)` matches and the
        // rebuild would discard the envelope — the turn budget, "you cannot
        // delegate", and the exact tool names — and splice in the GLOBAL
        // extension appendix instead. Silently: the rebuild "succeeds", so the
        // `system_appendix_dropped` warning does not fire.
        let child_controls = self.shim_controls.session(&plan.child_session_id);
        child_controls.set_allowed_tools(plan.allowed_tool_names.iter().cloned().collect());
        child_controls.set_system_override(Some(plan.system_prompt.clone()));

        let config = AgentConfig::new(
            self.session_manager.clone(),
            goose::config::permission::PermissionManager::instance(),
            None,
            GooseMode::Auto,
            true,
            GoosePlatform::GooseCli,
        );
        let child = GooseAgent::with_config(config);

        child
            .update_provider(provider, model_config, &plan.child_session_id)
            .await
            .map_err(|e| anyhow!("Failed to set the provider on the subagent: {e}"))?;

        // Goose's own child loop `debug!`s and SWALLOWS an extension that fails
        // to load, so a subagent whose only useful extension never started runs
        // anyway and returns a plausible-looking wrong answer. Owning the loop
        // means this can fail loudly instead.
        for extension in &plan.extensions {
            child
                .add_extension(extension.clone(), &plan.child_session_id)
                .await
                .map_err(|e| {
                    anyhow!(
                        "Failed to load extension '{}' for subagent role '{}': {e}",
                        extension.name(),
                        plan.role
                    )
                })?;
        }

        child
            .override_system_prompt(plan.system_prompt.clone())
            .await;

        // Read once here and again after the drain, and unioned. Neither read
        // alone is the audit: this one can only report what `add_extension` was
        // just handed — which `child_extensions` already refused, so on its own
        // it is an audit that cannot fail — and the post-run read alone would
        // miss an extension that loaded and was removed again mid-run.
        let mut loaded_extensions: std::collections::BTreeSet<String> = child
            .list_extensions()
            .await
            .into_iter()
            .map(|e| e.to_string())
            .collect();

        let session_config = goose::agents::SessionConfig {
            id: plan.child_session_id.clone(),
            schedule_id: None,
            // Always `Some`. Goose's `build_subagent_prompt` does
            // `.expect("TaskConfig always sets max_turns")`, and while that
            // particular panic is not on this path, an unbounded child on a
            // 2-4B on-device model is minutes of wall clock with the parent's
            // turn blocked behind it.
            max_turns: Some(plan.max_turns),
            retry_config: None,
        };
        let user_message = Message::user().with_text(&plan.user_message);

        // PAI-4 P5. A child replies through the SAME engine and the same
        // provider as its parent, with its own system prompt and its own tool
        // block, so the single retained KV prefix the parent's next turn hopes
        // to be served off is about to be overwritten. Nothing said so, and the
        // parent's next turn compares hashes it alone owns — finds them equal,
        // takes the `prefix_changed == false` branch, and calls
        // `note_prefix_served()`, recording as WARM a prefix that is cold. The
        // trimmer's age rung reads that posture.
        //
        // Recorded BEFORE the reply rather than after it, because the honest
        // answer to "did the child touch the provider" once `reply` has been
        // started is "assume yes": a stream that fails part-way has still
        // prefilled. Over-reporting cold costs one re-compaction decision;
        // under-reporting it costs a silent 3.7s re-prefill the compaction path
        // thought it had avoided.
        //
        // `PromptChanged` is the closest of the six existing reasons — the
        // static prefix the engine holds really is a different one. A
        // `DelegatedRun` variant would read better in a trace and is a
        // `pond-core` change this one does not own.
        self.note_prefix_invalidated(InvalidationReason::PromptChanged);

        let mut stream =
            goose::session_context::with_session_id(Some(plan.child_session_id.clone()), async {
                child
                    .reply(user_message.clone(), session_config, Some(cancel.clone()))
                    .await
            })
            .await
            .map_err(|e| anyhow!("Failed to start the subagent reply: {e}"))?;

        // One `AgentEvent::Message` is a FRAGMENT, not a turn — see the
        // provider table above `ReasoningCoalescer` in this file, and
        // `ChildTurns`, which owns the rule. Counting them as turns is what
        // reported every successful delegation as `TurnBudgetExhausted`;
        // assigning `last_text` per message is what made the "answer" the last
        // streamed word.
        let mut turns = crate::orchestrator::ChildTurns::default();
        while let Some(event) = stream.next().await {
            match event {
                Ok(goose::agents::AgentEvent::Message(msg)) => {
                    // The reduction itself is `child_stream_step`, next to
                    // `ChildTurns` and tested from a `Vec` of fragments. What
                    // is left here — the two arguments — is the only part that
                    // needs a live engine, and it is what the tripwire reads:
                    // `as_concat_text()` filters on `as_text()`, which returns
                    // `None` for `MessageContent::Thinking`, and that is
                    // load-bearing rather than incidental. PAI-5's reasoning
                    // gate lives at this adapter's own producer, a path a child
                    // does not go through, so a child's reasoning would
                    // otherwise reach the parent as its answer.
                    crate::orchestrator::child_stream_step(
                        &mut turns,
                        msg.role == rmcp::model::Role::Assistant,
                        &msg.as_concat_text(),
                    );
                    // PAI-6 P6. The only other thing this loop takes from a
                    // child's message, and the reason it is a call rather than
                    // an inline `for content in &msg.content`: reading the
                    // content list directly is what loses `as_concat_text()`'s
                    // accidental reasoning gate, so the reading is done by a
                    // function whose return type cannot carry reasoning, answer
                    // text, or the call's ARGUMENTS. See `child_tool_names`.
                    for tool in crate::orchestrator::child_tool_names(&msg) {
                        crate::orchestrator::report_child_progress(
                            &plan.parent_session_id,
                            &plan.task_id,
                            &plan.role,
                            pond_core::shared::domain::agent::SubagentStatus::Tool,
                            Some(tool),
                        );
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        role = %plan.role,
                        "subagent stream error, ending the run: {e}"
                    );
                    break;
                }
            }
        }
        drop(stream);
        let (last_text, assistant_turns) = turns.finish();

        // The post-run half of the invariant-2 audit. `add_extension` is not the
        // only way an extension can arrive — a Goose sync could re-arm a
        // `default_enabled` platform extension, which is what
        // `EnabledExtensionsState::extensions_or_default` does — and asking
        // before the run could only ever echo back what was just handed over.
        loaded_extensions.extend(
            child
                .list_extensions()
                .await
                .into_iter()
                .map(|e| e.to_string()),
        );

        Ok(crate::orchestrator::ChildOutcome {
            last_text,
            assistant_turns,
            loaded_extensions,
        })
    }
}

/// Shrink the text bodies of an oversized structured tool response, or `None`
/// when the message carries no tool response over `max_chars`.
///
/// GIAP's `TOOL_RESULT_MAX_BYTES` used to reach only the trimmer's token
/// ESTIMATE: the rebuild kept structured `ToolResponse` messages whole, so a
/// 50K-char tool result was re-prefilled verbatim on every single turn until its
/// entire turn aged out — the estimate said 1.5K, the engine paid for 50K.
///
/// The rewrite is deliberately surgical: the message is cloned and only
/// `RawContent::Text` bodies inside the response are replaced. The response id,
/// its annotations, `is_error`, and the tool-request/response pairing all survive
/// untouched, because a re-keyed or orphaned tool response is rejected by the
/// provider outright.
///
/// `structured_content` is left alone: it is arbitrary tool-defined JSON that
/// cannot be truncated without risking invalid data, and the text bodies are
/// what the chat template renders.
/// A cheap stable digest of a conversation, used to decide whether rewriting the
/// engine's message table would change anything.
///
/// Hashes the JSON form of each message's content rather than the content itself
/// because `MessageContent` does not implement `Hash` — and the JSON is the
/// faithful proxy here, since it is exactly the bytes `replace_conversation`
/// would write. Serializing every message once per turn sounds expensive next to
/// the alternative until you price the alternative: a transaction, a whole-table
/// DELETE, and one INSERT per message, each doing this same serialization anyway.
///
/// `id` and `created` are included because the rebuild deliberately preserves
/// them — a message that kept its identity but changed its text must still
/// register as different.
fn conversation_fingerprint(messages: &[goose::conversation::message::Message]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    messages.len().hash(&mut hasher);
    for m in messages {
        m.id.hash(&mut hasher);
        m.created.hash(&mut hasher);
        match m.role {
            rmcp::model::Role::User => 0u8,
            rmcp::model::Role::Assistant => 1u8,
        }
        .hash(&mut hasher);
        match serde_json::to_string(&m.content) {
            Ok(json) => json.hash(&mut hasher),
            // Unserializable content cannot be compared, so refuse to claim the
            // conversation is unchanged: hash something unique to this message
            // so the fingerprints differ and the write proceeds.
            Err(_) => {
                "unserializable".hash(&mut hasher);
                std::ptr::from_ref(m).addr().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn truncate_tool_response_text(
    message: &goose::conversation::message::Message,
    max_chars: usize,
) -> Option<goose::conversation::message::Message> {
    use goose::conversation::message::MessageContent;
    use pond_core::models::services::context_budget::truncate_head_tail;

    let oversized = message.content.iter().any(|c| match c {
        MessageContent::ToolResponse(tr) => tr.tool_result.as_ref().is_ok_and(|r| {
            r.content.iter().any(
                |c| matches!(&c.raw, rmcp::model::RawContent::Text(t) if t.text.len() > max_chars),
            )
        }),
        _ => false,
    });
    if !oversized {
        return None;
    }

    let mut rewritten = message.clone();
    for content in rewritten.content.iter_mut() {
        let MessageContent::ToolResponse(tr) = content else {
            continue;
        };
        let Ok(result) = tr.tool_result.as_mut() else {
            continue;
        };
        for part in result.content.iter_mut() {
            if let rmcp::model::RawContent::Text(text) = &mut part.raw {
                if let Some(truncated) = truncate_head_tail(&text.text, max_chars) {
                    text.text = truncated;
                }
            }
        }
    }
    Some(rewritten)
}

/// The `.gguf` filename to register for a model name, tolerating a missing
/// quantization suffix.
///
/// Model names stored in settings and role assignments are frequently the
/// catalog *display* name (e.g. `gemma-4-E2B-it`), while the file on disk keeps
/// its quant suffix (`gemma-4-E2B-it-Q4_K_M.gguf`). Naively appending `.gguf`
/// therefore points at a file that does not exist, and inference fails with
/// "Model not downloaded" even though the model is present. This resolves the
/// name to a real file so a display name still loads.
///
/// Resolution order:
/// 1. an explicit `.gguf` name is taken verbatim;
/// 2. an exact `{name}.gguf` on disk wins;
/// 3. otherwise a quant variant `{name}-*.gguf` (or `{name}.*.gguf`) — only
///    files that actually exist are considered, and the choice is
///    deterministic (lexicographically first) so repeated runs agree;
/// 4. failing all that, the naive `{name}.gguf`, so the caller's
///    file-not-found warning still fires.
fn resolve_gguf_filename(model_name: &str, gguf_dir: &std::path::Path) -> String {
    if model_name.ends_with(".gguf") {
        return model_name.to_string();
    }

    let exact = format!("{model_name}.gguf");
    if gguf_dir.join(&exact).exists() {
        return exact;
    }

    if let Ok(entries) = std::fs::read_dir(gguf_dir) {
        let mut variants: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|f| f.ends_with(".gguf"))
            .filter(|f| {
                let base = f.trim_end_matches(".gguf");
                // A quant variant is the model name, a separator, then a
                // quantization tag. Requiring the tag matters: model names
                // contain hyphens too, so "gemma-4-E2B" is a prefix of the
                // *different* model "gemma-4-E2B-it-Q4_K_M" — and must not
                // match it. Only a real quant suffix counts.
                base.strip_prefix(model_name)
                    .and_then(|rest| rest.strip_prefix(['-', '.']))
                    .is_some_and(looks_like_quant_tag)
            })
            .collect();
        variants.sort();
        if let Some(filename) = variants.into_iter().next() {
            return filename;
        }
    }

    exact
}

/// Attach a turn's image attachments to a message (phase F1).
///
/// Images ride the USER message, never the system prefix: the prefix must stay
/// byte-identical across turns for the engine's KV prompt-session cache to reuse
/// it, and a text-only turn in a session that once had an image must keep that
/// property. Order is preserved so "the first picture" means what the user meant.
///
/// A model without an mmproj is NOT second-guessed here — that check happens
/// once, up front, in `chat_stream`, where it can produce an actionable error
/// instead of a silently rewritten prompt.
fn attach_images(
    msg: Message,
    images: &[pond_core::models::domain::message::ImageAttachment],
) -> Message {
    images
        .iter()
        .fold(msg, |m, img| m.with_image(&img.data, &img.mime_type))
}

/// Number of image parts carried by a Goose message.
fn image_part_count(msg: &Message) -> usize {
    msg.content
        .iter()
        .filter(|c| matches!(c, goose::conversation::message::MessageContent::Image(_)))
        .count()
}

/// True when the message carries a tool request or response part.
///
/// The image cap never rewrites these. Tool request/response pairing is
/// load-bearing — an orphaned or re-keyed response is rejected outright by the
/// provider — and the one path allowed to rebuild such a message is
/// `truncate_tool_response_text`, which preserves ids and error flags
/// byte-for-byte. Camera tools DO return images inside a tool response; those
/// ride the tool-result truncation path, not this one.
fn has_tool_parts(msg: &Message) -> bool {
    use goose::conversation::message::MessageContent as C;
    msg.content.iter().any(|c| {
        matches!(
            c,
            C::ToolRequest(_)
                | C::ToolResponse(_)
                | C::ToolConfirmationRequest(_)
                // An elicitation or a tool confirmation awaiting an answer: part
                // of the same request/response bookkeeping, and just as unsafe
                // to rewrite.
                | C::ActionRequired(_)
                | C::FrontendToolRequest(_)
        )
    })
}

/// Rebuild `original` with at most `keep` of its image parts, its text replaced
/// by `text`, and ONE placeholder describing whatever images were dropped.
///
/// The message is CLONED and only its `content` replaced, so id, timestamp,
/// role and metadata survive exactly. The LEADING images are the ones kept, so
/// ordinal 0 stays ordinal 0 — the same rule the hydration replay uses.
///
/// Text parts collapse into the position of the first one. That keeps the text
/// on the same side of the images as the model originally saw it, which is the
/// only ordering property a multimodal template cares about.
///
/// # Exactly one placeholder, whatever the state
///
/// Capping is STAGED: with a budget of one, a two-image message is capped 2 to 1
/// when it becomes history, then 1 to 0 when a newer image turn arrives. By the
/// second pass the first pass's placeholder is already part of the message — and
/// part of `as_concat_text()`, so `text` carries it too and the text does not
/// even register as changed. Appending unconditionally would leave the model
/// reading two stand-ins for the same attachment, one of them stale. So every
/// existing placeholder is stripped first and exactly one is re-emitted for the
/// state the message ends up in: the partial wording while an image survives,
/// the all-dropped wording once none do.
fn cap_message_images(original: &Message, keep: usize, text: &str) -> Message {
    use goose::conversation::message::MessageContent as C;
    use pond_core::models::services::context::image_history::{
        contains_history_image_placeholder, history_image_placeholder,
        strip_history_image_placeholders,
    };

    // A placeholder already in the transcript means images were dropped on an
    // earlier pass, so one is still owed even if this pass drops nothing.
    let placeholder_owed = contains_history_image_placeholder(text)
        || original.content.iter().any(|c| match c {
            C::Text(t) => contains_history_image_placeholder(&t.text),
            _ => false,
        });
    let wanted_text = strip_history_image_placeholders(text);
    // `placeholder_owed` forces the text collapse below, which is what actually
    // removes the earlier pass's placeholder. Today the comparison alone would
    // do it — a placeholder in the message is in `as_concat_text()`, and
    // `wanted_text` has none, so the two always differ — but that reasoning
    // leans on Goose's concatenation including every text part. If a submodule
    // sync ever changed that, the stale placeholder would survive next to the
    // fresh one, which is the exact bug this is here to prevent.
    let text_changed = placeholder_owed || original.as_concat_text() != wanted_text;
    let mut kept = 0usize;
    let mut dropped = 0usize;
    let mut text_emitted = false;
    let mut content: Vec<C> = Vec::with_capacity(original.content.len() + 1);

    for part in &original.content {
        match part {
            C::Image(_) => {
                if kept < keep {
                    kept += 1;
                    content.push(part.clone());
                } else {
                    dropped += 1;
                }
            }
            C::Text(_) if text_changed => {
                if !text_emitted {
                    text_emitted = true;
                    // An empty part is dropped rather than emitted: some
                    // providers reject empty text content outright.
                    if !wanted_text.is_empty() {
                        content.push(C::text(wanted_text.as_ref()));
                    }
                }
            }
            other => content.push(other.clone()),
        }
    }

    if dropped > 0 || placeholder_owed {
        content.push(C::text(history_image_placeholder(kept)));
    }

    let mut capped = original.clone();
    capped.content = content;
    capped
}

/// Per-message image counts and the cap plan for a trimmed conversation.
///
/// Returns `(had, keep, dropped_total)`, all aligned with `trimmed`. A message
/// that carries tool parts, or that has no source row (the spliced summary,
/// `index == usize::MAX`), counts as zero and is therefore never rewritten.
///
/// `dropped_total == 0` means the conversation already fits the policy and must
/// be left byte-identical.
fn plan_live_image_cap(
    source: &[Message],
    trimmed: &[pond_core::models::services::context::turn_trimmer::TrimMessage],
) -> (Vec<usize>, Vec<usize>, usize) {
    use pond_core::models::services::context::image_history::{
        dropped_image_count, plan_history_images,
    };

    let had: Vec<usize> = trimmed
        .iter()
        .map(|tm| match source.get(tm.index) {
            Some(m) if !has_tool_parts(m) => image_part_count(m),
            _ => 0,
        })
        .collect();
    let keep = plan_history_images(&had);
    let dropped = dropped_image_count(&had, &keep);
    (had, keep, dropped)
}

/// Whether `tag` begins with a GGUF quantization marker (`Q4_K_M`, `Q6_K`,
/// `Q8_0`, `IQ4_XS`, `F16`, `F32`, `BF16`, …). Deliberately conservative: it
/// only needs to tell a quant suffix apart from a continuation of the model
/// name (`it`, `instruct`), not to validate every possible tag.
pub(crate) fn looks_like_quant_tag(tag: &str) -> bool {
    let digit_after = |prefix: &str| {
        tag.strip_prefix(prefix)
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_digit())
    };
    tag.starts_with("F16")
        || tag.starts_with("F32")
        || tag.starts_with("BF16")
        || digit_after("IQ")
        || digit_after("Q")
}

/// Collapse a redundant quantization suffix in a model name to its display
/// stem — "gemma-4-E2B-it-Q4_K_M" → "gemma-4-E2B-it" — but ONLY when both
/// spellings unambiguously resolve to the same file on disk. Without this,
/// the two spellings fork into separate registry ids: sessions split across
/// model identities, the static-prefix hash churns, and the engine's per-id
/// model cache can hold two multi-GB copies of one GGUF.
///
/// An explicit quant choice that differs from what the display stem would
/// resolve to (two quant files present, the user pinned the one the stem
/// would not pick) keeps its own identity — pinning stays honoured. A name
/// whose file is missing is left untouched (no evidence to collapse on).
fn canonical_model_stem(model_name: &str, gguf_dir: &std::path::Path) -> String {
    let stem = model_name.trim_end_matches(".gguf");
    let Some((base, tag)) = stem.rsplit_once(['-', '.']) else {
        return stem.to_string();
    };
    if base.is_empty() || !looks_like_quant_tag(tag) {
        return stem.to_string();
    }
    if resolve_gguf_filename(base, gguf_dir) == resolve_gguf_filename(stem, gguf_dir) {
        base.to_string()
    } else {
        stem.to_string()
    }
}

/// Phase D2 escape hatch. Driven by the `giap-toolkit` MCP extension, which is
/// in the always-on core set, so the model can always reach this even in a
/// heavily narrowed session.
#[async_trait]
impl pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionControl for GooseAdapter {
    async fn group_status(
        &self,
        engine_session_id: &str,
    ) -> Vec<pond_core::mcp::ports::tools::tool_selection_control::ToolGroupStatus> {
        use pond_core::mcp::domain::tool_group::{find_group, group_of_tool};
        use pond_core::mcp::ports::tools::tool_selection_control::ToolGroupStatus;

        // The caller can only know goose's session; the maps are keyed by GIAP's.
        let session_id = match self.giap_session_for_engine(engine_session_id) {
            Some(s) => s,
            None => String::new(),
        };
        let session_id = session_id.as_str();
        let settings = self.settings_repo.get().await.unwrap_or_default();
        // Not narrowing? Then every registered group is loaded, and saying so
        // truthfully is better than implying there is something to enable.
        let loaded: Option<Vec<String>> = if settings.tool_selection_is_relevant() {
            self.session_tool_groups
                .read()
                .await
                .get(session_id)
                .cloned()
        } else {
            None
        };

        // Tool counts come from the live cache when warm — the honest number for
        // "what will this cost me" — and are omitted rather than guessed if cold.
        let mut counts: HashMap<String, usize> = HashMap::new();
        if let Some(cache) = self.cached_tools.read().await.as_ref() {
            for tool in cache {
                if let Some(ext) = group_of_tool(tool) {
                    *counts.entry(ext.to_string()).or_insert(0) += 1;
                }
            }
        }

        registered_extensions()
            .iter()
            .filter_map(|extension| {
                let group = find_group(extension)?;
                Some(ToolGroupStatus {
                    extension: extension.clone(),
                    description: group.description.to_string(),
                    loaded: match &loaded {
                        Some(groups) => groups.iter().any(|g| g == extension),
                        None => true,
                    },
                    core: group.core,
                    tool_count: counts.get(extension).copied().unwrap_or(0),
                })
            })
            .collect()
    }

    async fn enable_group(
        &self,
        engine_session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionError>
    {
        use pond_core::mcp::domain::tool_group::is_catalog_extension;
        use pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionError;

        // Resolve the caller's own session before anything is widened. A tool
        // that cannot be attributed must not widen ANY session -- the previous
        // code read a process-global, so an unattributed call widened whichever
        // session last started a turn.
        let Some(session_id) = self.giap_session_for_engine(engine_session_id) else {
            return Err(ToolSelectionError::NotActive);
        };
        let session_id = session_id.as_str();

        let group = group.trim();
        if !is_catalog_extension(group) {
            return Err(ToolSelectionError::UnknownGroup(group.to_string()));
        }
        if !registered_extensions().iter().any(|e| e == group) {
            return Err(ToolSelectionError::GroupNotRegistered(group.to_string()));
        }

        // The PAI-1 boundary, checked where the widening happens.
        //
        // Catalog membership and registration were the only two checks here, and
        // neither knows who is asking. `giap-toolkit` is deliberately not on the
        // guest denylist — a guest is meant to be able to load neutral groups —
        // so an unidentified speaker could name `giap-memory` and be handed the
        // household's memory tools. Refused as GroupNotRegistered rather than a
        // new variant: from the caller's side a group it may not have is
        // indistinguishable from one that is not there, and a distinct error
        // would tell it the group exists and is being kept from it.
        if let Some(permitted) = self.session_permitted_groups.read().await.get(session_id) {
            if !permitted.iter().any(|p| p == group) {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "tool_group_widen_refused",
                    session_id = %session_id,
                    group,
                    "enable_tool_group named a group outside this session's boundary"
                );
                return Err(ToolSelectionError::GroupNotRegistered(group.to_string()));
            }
        }

        let groups = {
            let mut map = self.session_tool_groups.write().await;
            // No entry means selection never ran for this session (mode is
            // "all", or the tool was reached from a non-chat path). Nothing is
            // being narrowed, so there is nothing to widen.
            let Some(entry) = map.get_mut(session_id) else {
                return Err(ToolSelectionError::NotActive);
            };
            if !entry.iter().any(|g| g == group) {
                entry.push(group.to_string());
                entry.sort();
            }
            entry.clone()
        };

        if let Some(storage) = &self.giap_session_storage {
            if let Err(e) = storage.set_session_tool_groups(session_id, &groups).await {
                // Non-fatal: the widen holds for this run either way.
                tracing::warn!("tool selection: persisting the widened groups failed: {e}");
            }
        }

        // Admit the group's tools on the NEXT provider call — including the next
        // call of the turn that just invoked this, which is what makes
        // enable-then-use work in one turn. Both the shim's veto and the
        // stream's tool-call guard read this entry live.
        let goose_sid = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();
        if let Some(goose_sid) = goose_sid {
            let newly_allowed: Vec<String> = match self.cached_tools.read().await.as_ref() {
                Some(cache) => cache
                    .iter()
                    .filter(|t| pond_core::mcp::domain::tool_group::group_of_tool(t) == Some(group))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            if newly_allowed.is_empty() {
                // A cold cache means the widen does not take effect this turn,
                // and `giap-toolkit` has already told the model "its tools are
                // available now — go ahead and call the one you need". It calls,
                // the guard suppresses, and a turn of a 4-turn budget is gone.
                //
                // Say so instead. `NotReady` is a tool SUCCESS carrying the
                // explanation (see `toolkit.rs`), so the model can spend the turn
                // on something else and try again — which is the difference
                // between a wasted turn and a wasted sentence.
                tracing::warn!(
                    target: "giap::trace",
                    kind = "tool_group_enable_deferred",
                    session_id = %session_id,
                    group,
                    "enabled a group while the tool cache was cold"
                );
                return Err(ToolSelectionError::NotReady(group.to_string()));
            }
            self.shim_controls
                .session(&goose_sid)
                .extend_allowed_tools(newly_allowed);
        }

        Ok(groups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── thinking section stability ────────────────────────────────────────

    /// The regression that cost a full re-prefill on every session's second
    /// turn: in "auto", turn 1 and turn 2 must agree, which they only do if the
    /// answer comes from the model name rather than a cache filled in later.
    #[test]
    fn auto_thinking_is_decided_by_the_model_name_alone() {
        assert!(GooseAdapter::thinking_section_applies(
            "auto",
            "gemma-4-E2B-it",
            false
        ));
        assert!(!GooseAdapter::thinking_section_applies(
            "auto",
            "llama-3.2-3b",
            false
        ));
    }

    // ── PAI-4 P5: which reason a provider swap records ────────────────────

    #[test]
    fn the_same_model_behind_a_new_provider_is_a_rebuild_not_a_swap() {
        assert_eq!(
            GooseAdapter::provider_change_reason("ollama:gemma4:e2b", "gemma4:e2b"),
            InvalidationReason::ProviderRebuilt
        );
        assert_eq!(
            GooseAdapter::provider_change_reason("local:gemma-4-E2B-it", "gemma-4-E2B-it"),
            InvalidationReason::ProviderRebuilt
        );
    }

    /// The colon-in-the-model-name case, which is not hypothetical: every
    /// Ollama tag has one. Splitting the key from the right would compare
    /// "e2b" with "gemma4:e2b" and report a model swap on every provider
    /// change — a reason nobody could trust in a trace.
    #[test]
    fn a_model_name_containing_a_colon_survives_the_key_split() {
        assert_eq!(
            GooseAdapter::provider_change_reason("ollama:gemma4:e2b", "gemma4:e4b"),
            InvalidationReason::ModelSwapped
        );
        assert_eq!(
            GooseAdapter::provider_change_reason("llamafile:gemma4:e2b", "gemma4:e2b"),
            InvalidationReason::ProviderRebuilt
        );
    }

    #[test]
    fn a_different_model_is_a_swap_and_so_is_the_very_first_provider() {
        assert_eq!(
            GooseAdapter::provider_change_reason("local:gemma-4-E2B-it", "gemma-4-E4B-it"),
            InvalidationReason::ModelSwapped
        );
        // Startup: `last_provider_key` is still empty, so there is no previous
        // model to have kept.
        assert_eq!(
            GooseAdapter::provider_change_reason("", "gemma-4-E2B-it"),
            InvalidationReason::ModelSwapped
        );
    }

    #[test]
    fn explicit_thinking_modes_ignore_the_model_and_voice_always_wins() {
        assert!(GooseAdapter::thinking_section_applies(
            "on",
            "llama-3.2-3b",
            false
        ));
        assert!(!GooseAdapter::thinking_section_applies(
            "off",
            "gemma-4-E2B-it",
            false
        ));
        for mode in ["on", "off", "auto"] {
            assert!(
                !GooseAdapter::thinking_section_applies(mode, "gemma-4-E2B-it", true),
                "voice mode must suppress <thinking> regardless of mode ({mode})"
            );
        }
    }

    // ── PAI-5 P1: the structured reasoning channel ────────────────────────

    /// PAI-5 invariant 2: voice mode never renders reasoning. It is unspeakable
    /// text, and the terminal voice loop prints `Thinking` frames to stderr
    /// unconditionally, so a leak here is a leak all the way to the speaker.
    ///
    /// This is the clause that has no second line of defence. `show_thinking`
    /// is also checked at the SSE seam for the `ThoughtFilter` capture path
    /// (`routes.rs` builds the filter with `settings.show_thinking &&
    /// !req.voice_mode`), but the structured frames this phase introduces are
    /// forwarded there unconditionally — the producer is the only gate.
    #[test]
    fn a_voice_turn_never_surfaces_reasoning() {
        for show_thinking in [true, false] {
            assert!(
                !GooseAdapter::reasoning_frames_enabled(show_thinking, true),
                "voice mode leaked reasoning with show_thinking={show_thinking}"
            );
        }
        assert!(GooseAdapter::reasoning_frames_enabled(true, false));
        assert!(
            !GooseAdapter::reasoning_frames_enabled(false, false),
            "a user who turned thinking off must not receive reasoning frames"
        );
    }

    /// The OTHER half of the gate, which was unguarded until a review broke it.
    ///
    /// `reasoning_frames_enabled` had all four of its rows tested and the source
    /// guard pinned the identifier `is_voice`, but nothing pinned the
    /// COMPOSITION. Deleting `|| request.voice_mode` from `chat_stream` left
    /// every one of the 108 tests in this crate green, and on the shipped
    /// desktop that mutation makes reasoning leak to every voice turn, silently:
    /// `main.rs` builds the serve-mode adapter with `voice_mode: false`
    /// hardcoded, so the instance flag is never true there and the request flag
    /// is the whole defence.
    ///
    /// The row that matters is therefore `(false, true)`. It is asserted first
    /// and by name so the failure names the surface it breaks, rather than
    /// reporting a bare `assert!(false)` from the middle of a loop.
    #[test]
    fn a_request_flagged_voice_is_a_voice_turn_even_on_a_text_started_process() {
        assert!(
            GooseAdapter::voice_turn(false, true),
            "the shipped desktop hardcodes the instance flag to false, so the per-request \
             flag is the ONLY signal that a turn is spoken. Dropping it re-opens the leak \
             P1 closed, and every gate downstream keeps looking correct."
        );
        assert!(
            GooseAdapter::voice_turn(true, false),
            "the CLI `--input whisper` instance flag must still count on its own"
        );
        assert!(GooseAdapter::voice_turn(true, true));
        assert!(
            !GooseAdapter::voice_turn(false, false),
            "a text turn on a text process must not be treated as voice, or reasoning \
             is suppressed for everyone"
        );
    }

    /// A settings read that fails falls back to `Settings::default()`. On that
    /// path access must NARROW, not widen.
    #[test]
    fn the_settings_default_emits_no_reasoning() {
        let fallback = pond_core::user_data::domain::settings::Settings::default();
        assert!(!GooseAdapter::reasoning_frames_enabled(
            fallback.show_thinking,
            false
        ));
    }

    /// What was actually being thrown away. `as_concat_text()` filters on
    /// `as_text()`, which returns `None` for `Thinking`, so a message carrying
    /// both reached the stream as answer text only.
    #[test]
    fn reasoning_is_lifted_out_of_a_message_that_also_carries_an_answer() {
        let msg = Message::assistant()
            .with_thinking("  the user asked about the porch light  ", "")
            .with_text("The porch light is on.");

        assert_eq!(
            msg.as_concat_text(),
            "The porch light is on.",
            "as_concat_text is still the answer-only view; that is the whole reason \
             a separate lift is needed"
        );
        // The lift is RAW — padding and all. Normalisation is the coalescer's
        // job and happens once per passage, not once per fragment.
        assert_eq!(
            GooseAdapter::reasoning_frames(&msg, true),
            vec!["  the user asked about the porch light  ".to_string()],
        );
        assert!(
            GooseAdapter::reasoning_frames(&msg, false).is_empty(),
            "the gate is applied inside the lift, not only at the call site"
        );
        // And the frame a user actually sees is the trimmed passage.
        let mut coalescer = ReasoningCoalescer::default();
        coalescer.push(&msg, true);
        assert_eq!(
            coalescer.flush(),
            Some("the user asked about the porch light".to_string()),
        );
    }

    /// THE guard for PAI-5 P1's granularity clause, and the sequence the design
    /// doc demands verbatim.
    ///
    /// On the shipped headline configuration — a Jetson on `chat_provider =
    /// local` / gguf with `show_thinking = true` — goose emits one
    /// `AgentEvent::Message` per token piece
    /// (`goose-local-inference/src/llamacpp/inference_native_tools.rs` calls
    /// `push_structured_reasoning` from inside the per-token callback). One
    /// frame per message therefore meant one `<p>` per token in `Chat.tsx`,
    /// with every inter-word space eaten by a per-fragment `.trim()`.
    ///
    /// The count assertion is not decoration: a coalescer that emitted the
    /// right text in three pieces would still be the bug.
    #[test]
    fn a_reasoning_passage_arrives_as_one_frame_with_its_spacing_intact() {
        let mut coalescer = ReasoningCoalescer::default();
        let mut frames: Vec<String> = Vec::new();
        for fragment in [" the user", " asked about", " the light"] {
            let msg = Message::assistant().with_thinking(fragment, "");
            coalescer.push(&msg, true);
            assert!(
                !GooseAdapter::message_ends_reasoning(&msg),
                "a message carrying only reasoning must not end the passage, or \
                 every delta flushes and nothing was coalesced"
            );
        }
        if let Some(content) = coalescer.flush() {
            frames.push(content);
        }

        assert_eq!(
            frames.len(),
            1,
            "a single reasoning passage produced {} frames; the consumer renders \
             one <p> per frame, so this is the per-token confetti P1 shipped. \
             Frames: {:?}",
            frames.len(),
            frames
        );
        assert_eq!(
            frames[0], "the user asked about the light",
            "the passage lost its inter-fragment whitespace. A per-fragment trim \
             joins the deltas as \"the userasked aboutthe light\"; the trim must \
             happen ONCE, on the assembled passage."
        );
    }

    /// Coalescing must not become "merge the whole turn". `anthropic.rs`
    /// accumulates `ThinkingDelta` internally and emits exactly ONE
    /// `with_thinking(..)` at `content_block_stop`, as does every non-streaming
    /// response path — so on those providers a message IS a complete block, and
    /// fusing two of them would invent a passage the model never wrote.
    ///
    /// Two whole blocks can only be adjacent across something the user sees, so
    /// the flush condition ("this message carries something the adapter would
    /// yield") separates them without knowing which provider it is talking to.
    #[test]
    fn a_whole_block_provider_is_not_merged_into_one_giant_block() {
        const FIRST: &str = "The user wants the porch light. I should check the registry.";
        const SECOND: &str = "The registry says it exists and is off. I can turn it on.";

        let mut coalescer = ReasoningCoalescer::default();
        let mut frames: Vec<String> = Vec::new();

        // Mirrors the stream's own sequence: push, then flush iff this message
        // carries something the adapter would yield.
        fn feed(msg: Message, coalescer: &mut ReasoningCoalescer, frames: &mut Vec<String>) {
            coalescer.push(&msg, true);
            if GooseAdapter::message_ends_reasoning(&msg) {
                if let Some(content) = coalescer.flush() {
                    frames.push(content);
                }
            }
        }

        feed(
            Message::assistant().with_thinking(FIRST, ""),
            &mut coalescer,
            &mut frames,
        );
        // Something visible: the answer text that closes the first block.
        feed(
            Message::assistant().with_text("Checking the registry."),
            &mut coalescer,
            &mut frames,
        );
        feed(
            Message::assistant().with_thinking(SECOND, ""),
            &mut coalescer,
            &mut frames,
        );
        if let Some(content) = coalescer.flush() {
            frames.push(content);
        }

        assert_eq!(
            frames.len(),
            2,
            "two complete provider blocks came out as {} frame(s). Merging them \
             fuses reasoning the model emitted separately. Frames: {:?}",
            frames.len(),
            frames
        );
        assert_eq!(frames[0], FIRST);
        assert_eq!(frames[1], SECOND);
    }

    /// The worst failure mode a coalescer can have: buffering a passage and
    /// then never emitting it. That is strictly worse than the confetti it
    /// replaced, because the user sees nothing at all.
    ///
    /// It is a real case, not a hypothetical — a turn can end in pure reasoning
    /// (gemma-4-E2B closes its thinking block and emits end_of_turn with no
    /// text), which is exactly why the empty-turn re-engagement loop exists in
    /// this file.
    #[test]
    fn a_block_that_ends_the_turn_is_not_dropped() {
        let mut coalescer = ReasoningCoalescer::default();
        for fragment in ["I should", " check the", " device registry."] {
            let msg = Message::assistant().with_thinking(fragment, "");
            coalescer.push(&msg, true);
            assert!(!GooseAdapter::message_ends_reasoning(&msg));
        }
        assert_eq!(
            coalescer.flush(),
            Some("I should check the device registry.".to_string()),
            "the turn ended in pure reasoning and the buffered passage was lost. \
             The stream needs a flush AFTER the goose event loop drains, not only \
             inside it."
        );
    }

    /// The display gate owns the BUFFER, not just the frame. With the gate shut
    /// nothing is stored, so a voice turn or a `show_thinking = false` turn
    /// holds no reasoning text in memory at all — and a later flush cannot
    /// resurrect it. Access narrows on failure: `Settings::default()` must
    /// produce no frame either.
    #[test]
    fn the_display_gate_still_owns_the_buffer() {
        let mut coalescer = ReasoningCoalescer::default();
        for fragment in [" the user", " asked about", " the light"] {
            coalescer.push(&Message::assistant().with_thinking(fragment, ""), false);
        }
        assert_eq!(
            coalescer.flush(),
            None,
            "reasoning was buffered with the display gate shut; on a voice turn \
             that is unspeakable text one flush away from the speaker"
        );

        let fallback = pond_core::user_data::domain::settings::Settings::default();
        let emit = GooseAdapter::reasoning_frames_enabled(fallback.show_thinking, false);
        let mut coalescer = ReasoningCoalescer::default();
        coalescer.push(
            &Message::assistant().with_thinking("something private", ""),
            emit,
        );
        assert_eq!(
            coalescer.flush(),
            None,
            "the settings-read fallback emitted reasoning; a scope-widening default \
             is a bug"
        );
    }

    /// `RedactedThinking` is provider ciphertext, meaningful only when replayed
    /// to the same provider. Rendering it would put opaque base64 in the user's
    /// thinking panel — a data-out surface with nothing readable to justify it.
    /// Empty and whitespace-only blocks are dropped for the same reason a blank
    /// SSE frame is: it renders as a flicker and says nothing.
    ///
    /// Aimed at the FLUSH, not at the lift. The lift is now raw on purpose — a
    /// lone `" "` fragment is the space between two words and must survive it —
    /// so asserting "the raw lift returned a non-empty whitespace string" would
    /// be the guard quietly degrading into a restatement of the change. The
    /// claim that matters is the one at the surface: no frame reaches the
    /// stream.
    #[test]
    fn ciphertext_and_blank_reasoning_never_reach_the_stream() {
        let msg = Message::assistant()
            .with_redacted_thinking("ZW5jcnlwdGVkLXJlYXNvbmluZw==")
            .with_thinking("   ", "")
            .with_thinking("\n\t", "");

        assert!(
            !GooseAdapter::reasoning_frames(&msg, true)
                .iter()
                .any(|f| f.contains("ZW5jcnlwdGVk")),
            "provider ciphertext entered the reasoning channel; RedactedThinking \
             must be dropped in the lift, not merely trimmed later"
        );

        let mut coalescer = ReasoningCoalescer::default();
        coalescer.push(&msg, true);
        assert_eq!(
            coalescer.flush(),
            None,
            "redacted or blank reasoning produced a frame"
        );
    }

    /// The wiring guard, and the one that matters. The two functions above can
    /// both be correct while the stream yields `Thinking` from somewhere else
    /// entirely — which is exactly the shape of this phase's predecessor bug,
    /// where the producer existed upstream and the consumer existed downstream
    /// and nothing joined them. Asserted against the source because the stream
    /// body is an `async_stream` closure over a live Goose agent and cannot be
    /// driven from a unit test.
    /// The stream body with every `//` comment removed, one entry per line.
    ///
    /// Round 1 shipped a guard that a COMMENT satisfied: the egress guard
    /// searched for bare symbols, so prose naming the tracker certified two
    /// files that did not call it. Every structural assertion in this file
    /// reads code only.
    fn stream_body_code() -> Vec<String> {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        body.lines()
            .map(|l| match l.find("//") {
                Some(i) => l[..i].to_string(),
                None => l.to_string(),
            })
            .collect()
    }

    /// The answer contract is inside the envelope, not beside it.
    ///
    /// Placement is the whole claim. Inside `<system-context>` it is stripped
    /// from prior turns by `turn_trimmer::strip_system_context`, so it costs its
    /// own tokens once and never accumulates down a conversation. Pushed after
    /// `</system-context>` it would survive in every historical turn, and a
    /// twenty-turn chat would carry twenty copies of it.
    ///
    /// A source scan rather than a behavioural test because the envelope is built
    /// inline in a 700-line async stream body with no seam to call — the same
    /// reason `the_goal_is_armed_per_session_and_from_the_raw_request` scans.
    /// The child's half IS tested behaviourally, in
    /// `orchestrator::tests::the_child_is_told_to_return_a_finding_not_a_travelogue`.
    #[test]
    fn the_answer_contract_rides_inside_the_system_context_envelope() {
        let lines = stream_body_code();

        let open = lines
            .iter()
            .position(|l| l.contains("push_str(\"<system-context>"))
            .expect("the <system-context> envelope is no longer opened here");
        let close = lines
            .iter()
            .skip(open)
            .position(|l| l.contains("push_str(\"</system-context>"))
            .map(|i| i + open)
            .expect("the <system-context> envelope is no longer closed here");

        let inside = lines[open..close].join("\n");
        assert!(
            inside.contains("answer_contract()"),
            "the answer contract is not inside the <system-context> envelope. Outside \
             it, the trimmer does not strip it from prior turns and every historical \
             turn keeps a copy. Envelope:\n{inside}"
        );

        // And after the budget note, so the last thing before the request is the
        // shape of the reply rather than a number of steps.
        let budget_at = inside
            .find("turn_budget_block")
            .expect("the turn-budget note left the envelope");
        let contract_at = inside
            .find("answer_contract()")
            .expect("checked immediately above");
        assert!(
            contract_at > budget_at,
            "the answer contract is emitted before the turn budget. It is the closest \
             instruction to where the answer gets written, which is the only reason it \
             is restated here at all."
        );
    }

    #[test]
    fn every_thinking_frame_leaves_through_the_gate() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        let code = stream_body_code();

        // NOT a count. There are two flush sites now (one per completed block,
        // one after the event loop drains), and pinning "== 2" is the exact
        // shape that certified a fourth ungated egress entry point in round 1:
        // a THIRD raw yield would satisfy a bumped number. Instead every yield
        // must be able to name the coalescer immediately above it, so the
        // assertion scales with however many flush sites the stream grows.
        let yields: Vec<usize> = code
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("yield Ok(AgentStreamEvent::Thinking"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !yields.is_empty(),
            "nothing yields a Thinking frame any more; PAI-5 P1's structured \
             reasoning channel has been removed"
        );
        for i in &yields {
            let window = &code[i.saturating_sub(3)..*i];
            assert!(
                window.iter().any(|l| l.contains("reasoning.flush()")),
                "the Thinking frame yielded at line {} does not come out of \
                 ReasoningCoalescer::flush. The coalescer is where the once-per-\
                 passage trim and the buffered display gate live, so a raw yield \
                 here re-opens both the per-token confetti and the path by which \
                 ungated reasoning reaches a voice session.",
                i + 1
            );
        }
        assert_eq!(
            code.iter()
                .filter(|l| l.contains("reasoning.push(&msg, emit_reasoning)"))
                .count(),
            1,
            "the gated push into the reasoning coalescer must appear exactly once. \
             A second, ungated push would fill the buffer on a voice turn and the \
             next flush would emit it."
        );
        assert!(
            body.contains("Self::reasoning_frames_enabled(settings.show_thinking, is_voice)"),
            "emit_reasoning is no longer bound from show_thinking AND the voice flag"
        );
        // Pinning the identifier `is_voice` says nothing about what it holds.
        // A review deleted `|| request.voice_mode` from its binding and this
        // guard stayed green, along with the other 107 tests. Pin the
        // composition too, and keep it in the testable unit so the truth table
        // above is the real assertion and this is only the wiring.
        assert!(
            body.contains("let is_voice = Self::voice_turn(voice_instance, request.voice_mode);"),
            "is_voice is no longer composed by voice_turn(instance, request). If the \
             per-request flag was dropped, reasoning leaks to every desktop voice turn: \
             the serve-mode adapter hardcodes the instance flag to false."
        );
    }

    /// The flush that is easiest to delete and hardest to notice.
    ///
    /// `a_block_that_ends_the_turn_is_not_dropped` proves the coalescer can
    /// emit a trailing passage; it cannot prove the STREAM asks it to. Removing
    /// the flush that sits after the goose event loop leaves every unit test
    /// green and silently drops the last reasoning block of every turn that
    /// ends in reasoning.
    ///
    /// Anchored on the loop's own closing brace rather than on "after the
    /// `while let` line", because the in-loop flush also sits after that line —
    /// a guard written that way would pass with the trailing flush deleted,
    /// which is the whole failure it exists to catch.
    #[test]
    fn the_last_reasoning_block_of_a_turn_is_flushed_after_the_event_loop() {
        let code = stream_body_code();

        let loop_start = code
            .iter()
            .position(|l| l.contains("'engine: loop {"))
            .expect(
                "the goose event loop is gone. PAI-6 P6 turned it from a `while let` \
                 over `goose_stream.next()` into a labelled `loop` that selects the \
                 engine against the subagent progress channel; the label is what this \
                 guard anchors on, because a bare `loop {` matches the `'attempts` \
                 loop above it",
            );
        let indent = |l: &String| l.len() - l.trim_start().len();
        let loop_indent = indent(&code[loop_start]);
        let loop_end = (loop_start + 1..code.len())
            .find(|&i| code[i].trim() == "}" && indent(&code[i]) == loop_indent)
            .expect("could not find the end of the goose event loop");
        let recovery = code
            .iter()
            .position(|l| l.contains("if produced_visible {"))
            .expect("the empty-turn recovery check is gone");
        assert!(
            loop_end < recovery,
            "the event loop no longer closes before the empty-turn recovery check; \
             this guard's anchors have rotted and must be re-derived"
        );

        assert!(
            code[loop_end + 1..recovery]
                .iter()
                .any(|l| l.contains("reasoning.flush()")),
            "there is no reasoning.flush() between the end of the goose event loop \
             (line {}) and the empty-turn recovery check (line {}). Without it the \
             LAST reasoning passage of the turn is buffered and never emitted — and \
             a turn ending in pure reasoning is real, not hypothetical: it is the \
             case the re-engagement loop directly below exists to handle. A \
             coalescer that drops the final block is worse than the per-token \
             frames it replaced.",
            loop_end + 1,
            recovery + 1
        );
    }

    // ── PAI-5 P2: reasoning tokens ────────────────────────────────────────

    /// The claim P2 exists to make: the COUNT does not move with the display
    /// setting. `show_thinking` decides whether a person is shown the reasoning;
    /// it does not decide whether the model spent the tokens. If this ever
    /// couples, a Jetson running the shipped default (`show_thinking = false`)
    /// reports every turn as having done no thinking at all, and PAI-5 P5 sizes
    /// its output reserve from that lie.
    #[test]
    fn reasoning_is_counted_even_when_it_is_not_shown() {
        use pond_core::models::services::context::token_counting::HeuristicTokenCounter;
        let msg = Message::assistant()
            .with_thinking(
                "The user asked about the porch light. I should check the device \
                 registry before claiming it exists.",
                "",
            )
            .with_text("The porch light is off.");

        // Display gate shut: nothing leaves as a frame.
        assert!(
            GooseAdapter::reasoning_frames(&msg, false).is_empty(),
            "the display gate stopped gating"
        );
        // Count is taken anyway.
        let counted = GooseAdapter::count_reasoning_tokens(&msg, &HeuristicTokenCounter);
        assert!(
            counted > 0,
            "reasoning must be counted even when show_thinking is off; got {counted}"
        );
        // And it is the same number the gate-open case would produce.
        assert_eq!(
            counted,
            GooseAdapter::count_reasoning_tokens(&msg, &HeuristicTokenCounter),
            "the count depends on something other than the message"
        );
    }

    /// A message with no thinking channel counts zero, not "some of the answer".
    /// The failure this rules out is counting `as_concat_text()` by mistake,
    /// which would double-report every ordinary turn as reasoning.
    #[test]
    fn answer_text_is_not_reasoning() {
        use pond_core::models::services::context::token_counting::HeuristicTokenCounter;
        let msg = Message::assistant()
            .with_text("A long and perfectly ordinary answer with no reasoning channel at all.");
        assert_eq!(
            GooseAdapter::count_reasoning_tokens(&msg, &HeuristicTokenCounter),
            0
        );
    }

    /// The wiring guard. The two tests above can both pass while the stream
    /// counts inside the display gate — which is the exact regression that would
    /// make the number meaningless on the shipped configuration. Asserted
    /// structurally: the accumulation must appear ABOVE the `reasoning_frames`
    /// loop, i.e. outside it, and must not mention `emit_reasoning`.
    #[test]
    fn the_reasoning_count_is_taken_outside_the_display_gate() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        let lines = stream_body_code();

        // Re-anchored on the coalescer push. The old anchor was the
        // `for content in Self::reasoning_frames(&msg, emit_reasoning)` line,
        // which this phase deleted — and it was `.expect()`ed, so the guard
        // would have PANICKED rather than reported anything useful.
        let gate_line = lines
            .iter()
            .position(|l| l.contains("reasoning.push(&msg, emit_reasoning)"))
            .expect("the gated push is gone; the P1 guard should have caught this first");

        let window = &lines[gate_line.saturating_sub(8)..gate_line];
        window
            .iter()
            .position(|l| l.contains("Self::count_reasoning_tokens(&msg,"))
            .unwrap_or_else(|| {
                panic!(
                    "the reasoning token count is not taken in the eight lines before the \
                     display gate. If it moved inside `for content in reasoning_frames(..)`, \
                     the count is now zero whenever show_thinking is off — which is the \
                     shipped default, so every Jetson turn would report no thinking."
                )
            });
        // The WHOLE window, not just the line the call sits on. The original
        // guard checked only `window[count_line]`, so the most natural form of
        // this regression — wrapping the accumulation in `if emit_reasoning {`
        // on the PRECEDING line — passed green, as did a `let gated = ...`
        // computed above it. Both were demonstrated by a reviewer against this
        // exact test.
        //
        // Worth stating plainly, because it is what makes the regression
        // invisible rather than merely wrong: `chat.rs::stream_response_inner`
        // builds its `AgentRequest` with `voice_mode: true` unconditionally, and
        // that is the ONLY path that persists the count. So on `run_chat`,
        // `emit_reasoning` is always false — a gated count would write
        // `Some(0)` for 100% of the corpus PAI-5 P5 reads, and every test that
        // checks the pure counter would still pass.
        for line in window {
            assert!(
                !line.contains("emit_reasoning"),
                "the reasoning accumulation is now conditioned on emit_reasoning \
                 (`{}`); the cost of a turn must not depend on whether anybody is \
                 watching. On the only path that persists this number the flag is \
                 always false, so this reports every turn as having done no thinking.",
                line.trim()
            );
        }
        assert!(
            body.contains("turn_stats.reasoning_tokens.get_or_insert(0)"),
            "the count no longer accumulates into TurnStats, so nothing downstream sees it"
        );
        // The carry-out. Accumulating into `TurnStats` and then building
        // `UsageStats` with a literal `None` is this phase's own named failure
        // mode ("produced, reaches Done, and is dropped") and it was unguarded:
        // a reviewer replaced BOTH arms with `None` and all 108 tests passed.
        // There are two arms because the usage build has a reported-usage path
        // and a fallback path; a regression that fixes only one is worse than
        // one that fixes neither, because it depends on the provider.
        assert_eq!(
            body.matches("reasoning_tokens: turn_stats.reasoning_tokens,")
                .count(),
            2,
            "both UsageStats arms must carry the counted reasoning out of the stream. \
             A literal `None` in either one drops the number on the providers that take \
             that path, and every unit test here still passes because they all call the \
             pure counter."
        );
    }

    /// The re-engagement count is only true if every exit from `'attempts`
    /// passes through it.
    ///
    /// Structural, and it has to be: the counter lives inside a several-hundred
    /// line `async_stream` that needs a real goose `Agent`, a provider and a
    /// model to drive. What can be checked without one is the property that
    /// actually breaks — placement. `'attempts` has three exits (answered,
    /// budget spent, and the ordinary fallthrough), so an assignment written
    /// one line too early is skipped by two of them and reports 0 for exactly
    /// the turns worth counting: the ones that went silent.
    ///
    /// This is the same shape as the reasoning-count guard above it. That count
    /// was produced correctly, reached `Done`, and was dropped on the way out;
    /// a reviewer replaced both carry-out arms with `None` and all 108 tests
    /// passed. A number nobody guards the wiring of is a number that quietly
    /// becomes zero.
    #[test]
    fn the_re_engagement_count_is_taken_after_every_exit_from_the_attempts_loop() {
        let lines = stream_body_code();

        let assign = lines
            .iter()
            .position(|l| l.contains("turn_stats.reengagements ="))
            .expect(
                "nothing assigns `turn_stats.reengagements`. The empty-turn recovery is \
                 unmeasured again: a re-engaged turn is the whole turn a second time, \
                 prefill included, and without this the only trace is a WARN.",
            );

        // Assigned FROM the counter, not from a literal. `= 0` compiles, keeps
        // this guard's first assertion green, and reports every turn as
        // ordinary.
        assert!(
            lines[assign].contains("attempt"),
            "`turn_stats.reengagements` is assigned from something other than the \
             attempt counter (`{}`), so the field no longer says what the turn cost.",
            lines[assign].trim()
        );

        let breaks: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("break 'attempts"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            breaks.len() >= 2,
            "expected the several exits from `'attempts` this guard is about; found {}. \
             The loop has been reshaped and this test is now checking nothing.",
            breaks.len()
        );
        let last_break = *breaks.last().unwrap();
        assert!(
            assign > last_break,
            "`turn_stats.reengagements` is assigned at line {assign}, above the exit at \
             line {last_break}. Every `break 'attempts` after the assignment skips it, and \
             the turns that skip it are the silent ones -- so the count would read 0 for \
             precisely the turns it exists to measure.",
        );

        // And before the stats are sealed, or the value never leaves.
        let finalize = lines
            .iter()
            .position(|l| l.contains("turn_stats.finalize_rates()"))
            .expect("the stream no longer finalizes its TurnStats");
        assert!(
            assign < finalize,
            "the re-engagement count is assigned after `finalize_rates`, i.e. after the \
             turn's stats have been sealed and sent.",
        );
    }

    /// The completeness check must be armed per SESSION and from the RAW
    /// request. Both halves are load-bearing and both fail silently.
    ///
    /// Structural for the same reason as the re-engagement guard: the call sits
    /// inside an `async_stream` that needs a real goose `Agent`, a provider and
    /// a model to drive. What can be checked without one is which method is
    /// called and what is handed to it, and those are exactly the two things
    /// that go wrong.
    ///
    /// **`set_goal` would be a cross-profile disclosure.** `AppState` bounds
    /// concurrent chat streams with `Semaphore::new(4)` and all four share one
    /// retained `Arc<GooseAgent>`, so the process-wide slot means one household
    /// member's request text is injected into another member's turn as a user
    /// message. The fork carries `set_session_goal` precisely so this host can
    /// arm the check at all.
    ///
    /// **`turn_text` would restate household memories as a goal.** The
    /// assembled text wraps the request in `<system-context>` carrying injected
    /// memories, dormant-tool notes and the turn budget. The goal is echoed back
    /// to the model as "check whether this has been fully met" — feeding it the
    /// envelope would ask the model to satisfy the memories.
    #[test]
    fn the_goal_is_armed_per_session_and_from_the_raw_request() {
        let lines = stream_body_code();

        let armed = lines
            .iter()
            .position(|l| l.contains("set_session_goal("))
            .expect(
                "nothing arms the per-session goal. Goose's completeness check is guarded on a \
                 goal being set, so without this the reply loop terminates when the model stops \
                 asking for tools and never when the question was answered -- which on 2026-08-12 \
                 was a ten-item query answered with zero tool calls and no objection.",
            );

        // A WINDOW, not the anchor line. The call spans several lines once the
        // argument is built, and a single-line assertion silently stopped
        // matching the moment the setting gate was added -- reporting "nothing
        // arms the goal" while the goal was armed immediately below.
        let window = lines[armed..(armed + 10).min(lines.len())].join("\n");

        assert!(
            window.contains("request.message"),
            "the goal is armed from something other than the raw request. `turn_text` carries \
             <system-context> with injected memories and the turn budget, and the goal is echoed \
             back to the model as a thing to satisfy. Window:\n{window}"
        );
        assert!(
            !window.contains("turn_text"),
            "the goal is armed from `turn_text`, which is the assembled envelope rather than the \
             request. Window:\n{window}"
        );
        assert!(
            window.contains("goal_check_enabled"),
            "the goal is armed unconditionally. It costs roughly twice the inferences per turn, \
             so it rides `Settings::goal_check_enabled`. Window:\n{window}"
        );

        // The process-wide setter must not appear in the stream at all.
        for (i, line) in lines.iter().enumerate() {
            assert!(
                !line.contains(".set_goal("),
                "line {i} calls the process-wide `set_goal` (`{}`). One retained agent serves up \
                 to four concurrent chat streams, so that slot puts one member's request text \
                 into another member's turn. Use `set_session_goal`.",
                line.trim()
            );
        }

        // Armed BEFORE the reply that reads it, or the first attempt runs unguarded.
        let reply = lines
            .iter()
            .position(|l| l.contains("agent_clone.reply("))
            .expect("the stream no longer calls agent_clone.reply");
        assert!(
            armed < reply,
            "the goal is armed at line {armed}, after the reply at line {reply} that reads it.",
        );
    }

    // ── context window precedence ─────────────────────────────────────────

    /// The Jetson case that motivated `registry_context_size`: the device
    /// stamps 4096 into the registry at every provider init, so a bigger
    /// `context_window_override` must NOT be believed — GIAP would budget
    /// history the engine cannot hold, and llama.cpp truncates the prompt.
    #[test]
    fn a_pinned_local_context_outranks_a_larger_override() {
        let r =
            GooseAdapter::resolve_window_with("local", "gemma-4-E2B-it", 16384, Some(4096), None);
        assert_eq!(r.tokens, 4096);
        assert_eq!(
            r.source,
            pond_core::models::services::context::context_governor::WindowSource::Registry
        );
    }

    /// Unpinned local (macOS/Metal leaves context_size unset) keeps the
    /// override as the escape hatch, then the generous ceiling.
    #[test]
    fn an_unpinned_local_model_falls_back_to_override_then_ceiling() {
        use pond_core::models::services::context::context_governor::WindowSource;

        let overridden =
            GooseAdapter::resolve_window_with("local", "gemma-4-E2B-it", 16384, None, None);
        assert_eq!(overridden.tokens, 16384);
        assert_eq!(overridden.source, WindowSource::Override);

        let ceiling = GooseAdapter::resolve_window_with("local", "gemma-4-E2B-it", 0, None, None);
        assert_eq!(ceiling.tokens, 32768);
        assert_eq!(ceiling.source, WindowSource::Heuristic);
    }

    /// HTTP providers have no registry to pin them; the model's own reported
    /// window still answers when no override is set.
    #[test]
    fn http_providers_are_unaffected_by_the_registry_rule() {
        assert_eq!(
            GooseAdapter::resolve_window_with("ollama", "gemma4:e2b", 8192, None, None).tokens,
            8192
        );
        assert!(
            GooseAdapter::resolve_window_with("ollama", "gemma4:e2b", 0, None, None).tokens > 0
        );
    }

    /// PAI-3 P3b: the catalog value the adapter now reads has to actually reach
    /// the governor. Before this phase every construction site passed `None`,
    /// so `WindowSource::CatalogRecord` was a rung nothing in production could
    /// produce, and an Ollama model whose real window `POST /api/show` had
    /// already reported still got the 4,096 substring-match default.
    #[test]
    fn a_catalog_window_reaches_the_governor_from_the_adapter() {
        use pond_core::models::services::context::context_governor::WindowSource;

        // Without it: the name heuristic, which does not recognise this model.
        let guessed =
            GooseAdapter::resolve_window_with("ollama", "some-unknown-model", 0, None, None);
        assert_eq!(guessed.tokens, 4096);
        assert_eq!(guessed.source, WindowSource::Heuristic);

        // With it: the catalog's number, tagged as such -- but BOUNDED, because
        // ollama runs on this box. This asserted 131_072 when it was written,
        // which was the same wrong belief three other tests held: that ollama is
        // a hosted provider paying no local prefill. It serves over HTTP and
        // runs here. f770f4de made rung 3 clamp anything `runs_on_this_device`,
        // so the declared maximum is now held to UNPINNED_LOCAL_CEILING.
        let known = GooseAdapter::resolve_window_with(
            "ollama",
            "some-unknown-model",
            0,
            None,
            Some(131_072),
        );
        assert_eq!(
            known.tokens, 32_768,
            "a declared maximum is not an allocation, and ollama prefills locally"
        );
        assert_eq!(known.source, WindowSource::CatalogRecord);

        // A hosted provider keeps the raw declared window: nothing on this box
        // prefills it. This is the case rung 3 exists for, and it is what makes
        // the assertion above a boundary rather than a blanket clamp.
        let hosted = GooseAdapter::resolve_window_with("openai", "gpt-4o", 0, None, Some(131_072));
        assert_eq!(hosted.tokens, 131_072);
        assert_eq!(hosted.source, WindowSource::CatalogRecord);

        // A registry pin still wins -- it is the allocation, the catalog value
        // is the model's declared maximum.
        let pinned = GooseAdapter::resolve_window_with(
            "local",
            "gemma-4-E2B-it",
            0,
            Some(4096),
            Some(131_072),
        );
        assert_eq!(pinned.tokens, 4096);
        assert_eq!(pinned.source, WindowSource::Registry);
    }

    /// A model catalog holding exactly one row, for the wiring test below.
    struct StubCatalog {
        record: Option<ModelRecord>,
        /// Every id the adapter asked for, so the test can assert the id was
        /// DERIVED correctly and not merely that a number came back.
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl StubCatalog {
        fn holding(id: &str, context_length: Option<u32>) -> Self {
            Self {
                record: Some(ModelRecord {
                    id: id.to_string(),
                    category: ModelCategory::Ollama,
                    name: "gemma4:e2b".to_string(),
                    filename: None,
                    description: String::new(),
                    size_mb: 0,
                    url: None,
                    hf_id: None,
                    ram_estimate_mb: None,
                    recommended_role: None,
                    context_length,
                    quantization: None,
                    asr_language: None,
                    asr_size: None,
                    tts_engine: None,
                    tts_voice_name: None,
                    config_filename: None,
                    config_url: None,
                    tts_url: None,
                    sample_rate: None,
                    downloaded: true,
                    is_custom: false,
                }),
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ModelRepository for StubCatalog {
        async fn list_all(&self) -> Result<Vec<ModelRecord>> {
            Ok(self.record.clone().into_iter().collect())
        }
        async fn list_by_category(&self, _c: &ModelCategory) -> Result<Vec<ModelRecord>> {
            Ok(vec![])
        }
        async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>> {
            self.asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(id.to_string());
            Ok(self.record.as_ref().filter(|r| r.id == id).cloned())
        }
        async fn upsert(&self, _m: &ModelRecord) -> Result<()> {
            Ok(())
        }
        async fn set_downloaded(&self, _id: &str, _d: bool) -> Result<()> {
            Ok(())
        }
        async fn list_assignments(
            &self,
        ) -> Result<Vec<pond_core::models::domain::model_record::ModelRoleAssignment>> {
            Ok(vec![])
        }
        async fn get_assignment(
            &self,
            _r: &str,
        ) -> Result<Option<pond_core::models::domain::model_record::ModelRoleAssignment>> {
            Ok(None)
        }
        async fn set_assignment(&self, _r: &str, _m: &str) -> Result<()> {
            Ok(())
        }
        async fn clear_assignment(&self, _r: &str) -> Result<()> {
            Ok(())
        }
    }

    /// The wiring, not the precedence.
    ///
    /// PAI-3 P3 landed the data and left every `ContextInputs` construction site
    /// passing `catalog_context_length: None`, so rung 3 was a rung nothing in
    /// production could produce and every test of it passed the value in by
    /// hand. This one goes through the repository the adapter actually holds:
    /// break the lookup — hardcode `None`, derive the wrong id, drop the
    /// `with_model_repo` plumbing — and it fails.
    #[tokio::test]
    async fn the_adapter_reads_the_catalog_it_was_given() {
        use pond_core::models::services::context::context_governor::WindowSource;

        // No catalog: the name heuristic answers, and for gemma 4 it answers
        // 128,000 -- a round number somebody typed, not a number the model
        // declares.
        let blind = GooseAdapter::resolve_window_from(None, "ollama", "gemma4:e2b", 0, None).await;
        assert_eq!(blind.tokens, 128_000);
        assert_eq!(blind.source, WindowSource::Heuristic);

        // A model the heuristic does not recognise at all gets the
        // conservative default -- this is the case rung 3 rescues.
        let unknown =
            GooseAdapter::resolve_window_from(None, "ollama", "some-unknown-model", 0, None).await;
        assert_eq!(unknown.tokens, 4096);
        assert_eq!(unknown.source, WindowSource::Heuristic);

        // With the catalog: the row IS read, and then bounded. Ollama's
        // `model_info` reports 131,072 and the governor holds an on-device
        // provider to UNPINNED_LOCAL_CEILING, because a declared maximum is not
        // an allocation (f770f4de).
        //
        // 32,768 still cannot be reached by the fallback path -- the heuristic
        // for this model is 128,000 -- so this assertion keeps the anti-vacuity
        // property it was written for, and `source` pins it besides. That
        // mattered: the value changed and the reason the test exists did not.
        let catalog: Arc<dyn ModelRepository> =
            Arc::new(StubCatalog::holding("ollama/gemma4:e2b", Some(131_072)));
        let seen =
            GooseAdapter::resolve_window_from(Some(&catalog), "ollama", "gemma4:e2b", 0, None)
                .await;
        assert_eq!(
            seen.tokens, 32_768,
            "the catalog row's context_length never reached the governor"
        );
        assert_eq!(seen.source, WindowSource::CatalogRecord);

        // The id has to be derived the way the catalog stores it -- category,
        // not provider. A lookup that asks for the wrong key returns None and
        // degrades SILENTLY to the heuristic, so assert the key, not just the
        // answer. "local" and "ollama" are different provider strings that must
        // reach different categories.
        let stub = Arc::new(StubCatalog::holding("ollama/gemma4:e2b", Some(131_072)));
        let probe: Arc<dyn ModelRepository> = stub.clone();
        let _ =
            GooseAdapter::resolve_window_from(Some(&probe), "ollama", "gemma4:e2b", 0, None).await;
        let _ = GooseAdapter::resolve_window_from(Some(&probe), "local", "gemma-4-E2B-it", 0, None)
            .await;
        assert_eq!(
            stub.asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            ["ollama/gemma4:e2b", "gguf/gemma-4-E2B-it"],
            "the catalog id must be derived from the CATEGORY the row is keyed by"
        );

        // A registry pin short-circuits the catalog read entirely: rung 2 wins,
        // so the row is never fetched.
        let counting = Arc::new(StubCatalog::holding("gguf/gemma-4-E2B-it", Some(131_072)));
        let counting_port: Arc<dyn ModelRepository> = counting.clone();
        let pinned = GooseAdapter::resolve_window_from(
            Some(&counting_port),
            "local",
            "gemma-4-E2B-it",
            0,
            Some(4096),
        )
        .await;
        assert_eq!(pinned.tokens, 4096);
        assert_eq!(pinned.source, WindowSource::Registry);
        assert!(
            counting
                .asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "a pinned registry size must not pay for a catalog read it will discard"
        );
    }

    /// The regression guard for PAI-3 P1: the budget paths must not recover the
    /// context window from the process environment. `GOOSE_CONTEXT_LIMIT` is
    /// still WRITTEN (it flows into Ollama's `options.num_ctx`), but nothing in
    /// this adapter may read it back — that indirection is what let a Jetson
    /// budget history against a phantom 8192.
    #[test]
    fn no_budget_path_reads_the_context_limit_from_the_environment() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !body.contains("env::var(\"GOOSE_CONTEXT_LIMIT\")"),
            "context windows must come from ContextGovernor, not the environment"
        );
    }

    // ── PAI-3 P5: the adapter builds ONE asymmetric profile per turn ─────

    /// What `turn_profile` reads out of the ledger on a pond with nothing
    /// delegating, which is every pond today. Named so these tests keep saying
    /// what they are about — the two windows — rather than carrying a bare
    /// `0.0` that reads like a tolerance.
    const NO_LIVE_CHILD: f32 = 0.0;

    /// What `turn_profile` reads out of the message store on a pond that has
    /// not measured any reasoning -- which is every pond until `show_thinking`
    /// is on and a model that reasons has run. PAI-5 P5's derivation floors at
    /// the anchor, so this must reproduce the pre-P5 profile exactly, and
    /// `an_unmeasured_pond_gets_exactly_the_profile_it_got_before_pai_5_p5`
    /// asserts that rather than leaving it to these constants to imply.
    const NO_REASONING_HISTORY: &[u32] = &[];

    /// PAI-5 P5's wiring guard, and it is the one that matters: the derivation
    /// is unit-tested in pond-core, but nothing there can see whether the
    /// ADAPTER passes the anchor as the floor or, say, passes zero -- which
    /// would compile, and would let a quiet pond talk its own reserve down to
    /// nothing and resume ending conversations mid-generation.
    #[test]
    fn an_unmeasured_pond_gets_exactly_the_profile_it_got_before_pai_5_p5() {
        for window in [4_096usize, 8_192, 32_768] {
            let unmeasured = GooseAdapter::profile_for("local", window, NO_LIVE_CHILD, &[]);
            let anchor = pond_core::models::services::context::context_budget::CompactionProfile::for_windows(
                window,
                ContextGovernor::prompt_window("local", window),
            );
            assert_eq!(
                unmeasured.output_reserve_tokens, anchor.output_reserve_tokens,
                "at window {window} an unmeasured pond's reserve moved. P5 must be inert until                  there is evidence, or every install changes behaviour on upgrade for no reason"
            );
        }
    }

    /// And the other direction, without which the test above is satisfied by a
    /// feature that does nothing at all.
    #[test]
    fn a_pond_that_reasons_expensively_gets_a_bigger_reserve_than_the_anchor() {
        let unmeasured = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, &[]);

        // The sample cost is derived from the anchor rather than written down.
        // The first draft used a flat 900, and at this window two times that
        // quantises to exactly the anchor -- so the test failed for a true
        // reason that had nothing to do with the wiring it was checking.
        // Sizing off the anchor keeps it meaningful if the curve moves.
        let dear: Vec<u32> = vec![unmeasured.output_reserve_tokens as u32; 40];
        let measured = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, &dear);

        assert!(
            measured.output_reserve_tokens > unmeasured.output_reserve_tokens,
            "40 turns of {}-token reasoning did not move the reserve ({} vs {}). The adapter is \
             not passing the samples through, and that is invisible from pond-core -- the \
             derivation's own tests would all still pass",
            unmeasured.output_reserve_tokens,
            measured.output_reserve_tokens,
            unmeasured.output_reserve_tokens
        );
    }

    /// The wiring guard. `for_windows` is unit-tested in pond-core; what this
    /// asserts is that the adapter hands it the two windows the right way round,
    /// which is the half that cannot be checked from inside pond-core and the
    /// half that used to be four call sites remembering (or not) to clamp.
    #[test]
    fn a_local_turn_budgets_history_from_the_window_and_the_preamble_from_the_clamp() {
        // A Mac that resolved 32,768: four times the KV cache of the clamp.
        let big = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY);
        let clamped =
            GooseAdapter::profile_for("local", 8_192, NO_LIVE_CHILD, NO_REASONING_HISTORY);

        // Preamble: frozen at the clamp's allowance. If this grows, TTFT grows
        // with it on every single turn, because it is the KV prefix.
        assert_eq!(big.system_prompt_budget, clamped.system_prompt_budget);
        assert_eq!(big.memory_token_budget, clamped.memory_token_budget);
        assert_eq!(big.max_memory_fragments, clamped.max_memory_fragments);
        assert!(
            big.use_compact_prompt(),
            "a 32K KV cache selected the verbose prompt tier on a local provider"
        );

        // History: scaled with the real window, and it took the tokens the
        // preamble was not allowed to have.
        assert_eq!(big.context_window_tokens, 32_768);
        assert_eq!(big.history_token_budget, 24_000);
        assert!(big.history_token_budget > clamped.history_token_budget);
    }

    /// PAI-6 P4's half of the same wiring question.
    ///
    /// pond-core proves that `CompactionProfile::with_history_reserved` moves
    /// history and nothing else; this proves the ADAPTER applies it that way
    /// rather than by scaling the window it resolves — which is the
    /// implementation that compiles, reads well, re-derives every preamble
    /// allowance, and moves the KV prefix. Delegating would then cost the
    /// parent a full re-prefill on its next turn, which is the opposite of the
    /// context isolation the whole workstream is justified by.
    #[test]
    fn a_live_child_shrinks_the_parents_history_and_leaves_its_prefix_alone() {
        let alone = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY);
        let sharing = GooseAdapter::profile_for("local", 32_768, 0.5, NO_REASONING_HISTORY);

        assert!(
            sharing.history_token_budget < alone.history_token_budget,
            "a live child did not shrink the parent's history budget: {} vs {}",
            sharing.history_token_budget,
            alone.history_token_budget
        );
        assert_eq!(
            sharing.context_window_tokens, alone.context_window_tokens,
            "the resolved window moved, so the reservation was applied by scaling the window - \
             which re-derives every preamble allowance and moves the KV prefix"
        );
        assert_eq!(
            sharing.prompt_window_tokens, alone.prompt_window_tokens,
            "the prompt-side clamp moved under a reservation"
        );
        assert_eq!(
            sharing.system_prompt_budget, alone.system_prompt_budget,
            "the system prompt allowance moved under a reservation, so the preamble is rebuilt \
             at a different size and the parent pays a re-prefill for having delegated"
        );
        assert_eq!(
            sharing.memory_token_budget, alone.memory_token_budget,
            "the memory allowance moved under a reservation"
        );
        assert_eq!(
            sharing.use_compact_prompt(),
            alone.use_compact_prompt(),
            "the prompt tier flipped under a reservation"
        );
    }

    /// The SEAM between the ledger and the profile, which had no behavioural
    /// guard at all — only a grep for `reserved_fraction(`, which any key
    /// expression satisfies.
    ///
    /// A reservation is filed under the GIAP session id. Reading it back under
    /// a derived one — `goose-{id}`, or the output of `resolve_goose_session`,
    /// which is the mix-up this codebase has already made once — returns 0.0
    /// for every session on the pond, so a live child shrinks nothing and both
    /// agents budget as though they owned the whole window. That is invisible
    /// in production: the number is right, it is just always the number for a
    /// session that does not exist.
    #[test]
    fn a_parents_budget_shrinks_for_its_own_sessions_children_and_for_nobody_elses() {
        let ledger = Arc::new(crate::orchestrator::DeviceLedger::default());
        let _child = ledger.reserve("sess-A", 0.5);

        let delegating = GooseAdapter::profile_for_session(
            &ledger,
            "local",
            32_768,
            "sess-A",
            NO_REASONING_HISTORY,
        );
        let bystander = GooseAdapter::profile_for_session(
            &ledger,
            "local",
            32_768,
            "sess-B",
            NO_REASONING_HISTORY,
        );

        assert!(
            delegating.history_token_budget < bystander.history_token_budget,
            "a session with a live child budgeted {} history tokens and a session with none \
             budgeted {}; the reservation is being looked up under a key nothing writes, so \
             every parent on this pond reads 0.0 whatever its children are holding",
            delegating.history_token_budget,
            bystander.history_token_budget
        );
        assert_eq!(
            bystander.history_token_budget,
            GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY)
                .history_token_budget,
            "a session with no live child of its own was charged for somebody else's, so the \
             lookup is not keyed by session at all"
        );
    }

    /// HTTP providers pay no local prefill, so nothing is clamped and nothing is
    /// redistributed — the symmetric profile, unchanged from before this phase.
    #[test]
    fn an_http_turn_is_not_clamped_at_all() {
        let p = GooseAdapter::profile_for("ollama", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY);
        assert_eq!(p.system_prompt_budget, 6_000);
        assert_eq!(p.memory_token_budget, 1_500);
        assert_eq!(p.history_token_budget, 20_000);
        assert!(!p.use_compact_prompt());
    }

    // ── F1: image attachment onto the user message ───────────────────────

    fn img(data: &str, mime: &str) -> pond_core::models::domain::message::ImageAttachment {
        pond_core::models::domain::message::ImageAttachment {
            data: data.to_string(),
            mime_type: mime.to_string(),
        }
    }

    /// Pull out (base64, mime) for every image part, in order.
    fn image_parts(msg: &Message) -> Vec<(String, String)> {
        msg.content
            .iter()
            .filter_map(|c| match c {
                goose::conversation::message::MessageContent::Image(i) => {
                    Some((i.data.clone(), i.mime_type.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_text_only_turn_gets_no_image_parts() {
        let msg = attach_images(Message::user().with_text("hello"), &[]);
        assert!(image_parts(&msg).is_empty());
        assert_eq!(msg.as_concat_text(), "hello");
    }

    /// Every image must survive, in the order the user picked them — a follow-up
    /// that says "the second one" depends on it.
    #[test]
    fn every_image_is_attached_and_order_is_preserved() {
        let images = vec![
            img("AAAA", "image/png"),
            img("BBBB", "image/jpeg"),
            img("CCCC", "image/webp"),
        ];
        let msg = attach_images(Message::user().with_text("look"), &images);
        assert_eq!(
            image_parts(&msg),
            vec![
                ("AAAA".to_string(), "image/png".to_string()),
                ("BBBB".to_string(), "image/jpeg".to_string()),
                ("CCCC".to_string(), "image/webp".to_string()),
            ]
        );
    }

    /// The text part must stay FIRST and unmodified: it carries the
    /// `<system-context>`/`<user-message>` envelope the rest of the pipeline
    /// (and the trimmer's stale-context stripper) matches on.
    #[test]
    fn the_text_envelope_is_untouched_by_attachment() {
        let envelope =
            "<system-context>\n<turn-budget/>\n</system-context>\n<user-message>\nhi\n</user-message>";
        let msg = attach_images(
            Message::user().with_text(envelope),
            &[img("AAAA", "image/png")],
        );
        assert_eq!(msg.as_concat_text(), envelope);
        assert!(matches!(
            msg.content.first(),
            Some(goose::conversation::message::MessageContent::Text(_))
        ));
        assert_eq!(image_parts(&msg).len(), 1);
    }

    // ── F3: the vision-capability prompt section ─────────────────────────

    /// The failure this guards is the whole point of the section: a text-only
    /// model told it can see will describe an image that was never there.
    #[test]
    fn a_text_only_model_never_gets_the_vision_section() {
        for compact in [false, true] {
            let out =
                GooseAdapter::apply_vision_section("<identity>x</identity>".into(), false, compact);
            assert_eq!(out, "<identity>x</identity>");
            assert!(!out.contains("<vision>"));
        }
    }

    #[test]
    fn a_vision_model_gets_exactly_one_vision_section_at_the_tier_it_pays_for() {
        for compact in [false, true] {
            let out =
                GooseAdapter::apply_vision_section("<identity>x</identity>".into(), true, compact);
            assert!(out.starts_with("<identity>x</identity>"));
            assert_eq!(out.matches("<vision>").count(), 1);
            assert!(out.contains(pond_core::prompts::vision_capability_section(compact)));
        }
        // The compact tier must not pay for the verbose wording.
        let verbose = GooseAdapter::apply_vision_section(String::new(), true, false);
        let compact = GooseAdapter::apply_vision_section(String::new(), true, true);
        assert!(compact.len() < verbose.len());
    }

    /// The registry (mmproj presence), not the `gemma-4*` name heuristic, is the
    /// truth for the in-process engine: E1B is a gemma-4 with no vision encoder.
    #[test]
    fn local_vision_capability_comes_from_the_mmproj_registry() {
        for provider in ["local", "gguf"] {
            assert!(GooseAdapter::model_supports_vision(
                provider,
                "gemma-4-E2B-it"
            ));
            assert!(GooseAdapter::model_supports_vision(
                provider,
                "gemma-4-E4B-it-Q4_K_M"
            ));
            assert!(
                !GooseAdapter::model_supports_vision(provider, "gemma-4-E1B-it"),
                "E1B declares no mmproj — the name heuristic would wrongly say yes"
            );
            assert!(!GooseAdapter::model_supports_vision(
                provider,
                "Llama-3.2-3B-Instruct"
            ));
        }
    }

    /// HTTP providers have no registry to ask, so they fall back to the name
    /// rule — which has to reach the SAME verdict the registry would, including
    /// the E1B exclusion, and has to recognise the vision models an Ollama
    /// install actually serves. Both directions were wrong before: E1B was told
    /// it could see, and every `*-vision` / `*-vl` model was told it could not.
    #[test]
    fn http_vision_capability_matches_the_registry_and_covers_real_ollama_tags() {
        for provider in ["ollama", "llamafile", "openai"] {
            assert!(
                !GooseAdapter::model_supports_vision(provider, "gemma-4-E1B-it"),
                "{provider}: E1B has no vision encoder on ANY provider"
            );
            assert!(
                !GooseAdapter::model_supports_vision(provider, "gemma3n:e1b"),
                "{provider}: same model, Ollama's spelling"
            );
            for model in [
                "gemma-4-E4B-it",
                "gemma3n:e4b",
                "llama3.2-vision:11b",
                "qwen2.5-vl:7b",
                "minicpm-v:8b",
                "pixtral-12b",
            ] {
                assert!(
                    GooseAdapter::model_supports_vision(provider, model),
                    "{provider}/{model} accepts images"
                );
            }
            assert!(!GooseAdapter::model_supports_vision(
                provider,
                "Llama-3.2-3B-Instruct"
            ));
            assert!(!GooseAdapter::model_supports_vision(
                provider,
                "llama3.2:3b"
            ));
        }
    }

    /// D4: the prompt must not assert a capability `capabilities()` denies.
    /// Voice mode forces `caps.vision = false`, so the section is off there too
    /// — driven by the same instance-level flag, so the two cannot disagree.
    #[test]
    fn voice_mode_suppresses_the_vision_section_just_as_capabilities_does() {
        let vision_model = ("ollama", "gemma-4-E4B-it");
        assert!(GooseAdapter::vision_section_applies(
            vision_model.0,
            vision_model.1,
            false
        ));
        assert!(
            !GooseAdapter::vision_section_applies(vision_model.0, vision_model.1, true),
            "voice mode reports vision=false; the prompt must not claim otherwise"
        );
        // A text-only model stays off in both modes.
        for voice in [false, true] {
            assert!(!GooseAdapter::vision_section_applies(
                "ollama",
                "Llama-3.2-3B-Instruct",
                voice
            ));
        }
    }

    /// Voice mode must render a spoken-word time, never raw `HH:MM` digits —
    /// that's the whole point of `format_current_time` (small on-device
    /// models mangle digit-to-words conversion themselves; see spoken_time).
    #[test]
    fn voice_mode_renders_spoken_time_text_mode_keeps_digits() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 3, 5, 23, 0)
            .unwrap();

        let voice = GooseAdapter::format_current_time(now, true);
        assert_eq!(voice, "five twenty-three in the morning");

        let text = GooseAdapter::format_current_time(now, false);
        assert_eq!(text, "05:23");
    }

    // ── F2 live half: the image cap on the in-turn trimmer ───────────────

    fn trim_msg(
        index: usize,
        text: &str,
    ) -> pond_core::models::services::context::turn_trimmer::TrimMessage {
        pond_core::models::services::context::turn_trimmer::TrimMessage {
            index,
            role: pond_core::models::services::context::turn_trimmer::TrimRole::User,
            text: text.to_string(),
            is_summary: false,
            // The image cap is age-blind: it runs AFTER `trim_history` over
            // whatever survived, and its policy lives in `image_history`.
            age_secs: None,
        }
    }

    fn user_with_images(text: &str, images: &[&str]) -> Message {
        images.iter().fold(Message::user().with_text(text), |m, d| {
            m.with_image(*d, "image/png")
        })
    }

    /// The KV invariant guard: a conversation that never had an image must come
    /// out of the cap with nothing to do, so the trimmer's own `changed` flag
    /// stays the only thing that can rewrite it.
    #[test]
    fn a_text_only_conversation_plans_no_image_change() {
        let source = vec![
            Message::user().with_text("hi"),
            Message::assistant().with_text("hello"),
            Message::user().with_text("bye"),
        ];
        let trimmed = vec![trim_msg(0, "hi"), trim_msg(1, "hello"), trim_msg(2, "bye")];
        let (had, keep, dropped) = plan_live_image_cap(&source, &trimmed);
        assert_eq!(had, vec![0, 0, 0]);
        assert_eq!(keep, vec![0, 0, 0]);
        assert_eq!(dropped, 0);
    }

    /// One image is inside the budget: still nothing to rewrite.
    #[test]
    fn a_single_historical_image_is_left_alone() {
        let source = vec![user_with_images("look", &["AAAA"])];
        let (_, _, dropped) = plan_live_image_cap(&source, &[trim_msg(0, "look")]);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn only_the_newest_image_bearing_turn_keeps_pixels() {
        let source = vec![
            user_with_images("first", &["AAAA"]),
            Message::assistant().with_text("ok"),
            user_with_images("second", &["BBBB", "CCCC"]),
        ];
        let trimmed = vec![
            trim_msg(0, "first"),
            trim_msg(1, "ok"),
            trim_msg(2, "second"),
        ];
        let (had, keep, dropped) = plan_live_image_cap(&source, &trimmed);
        assert_eq!(had, vec![1, 0, 2]);
        assert_eq!(keep, vec![0, 0, 1], "newest-first, leading image kept");
        assert_eq!(dropped, 2);
    }

    /// A spliced `<conversation-summary>` has no source row (`usize::MAX`) and
    /// must not index out of bounds or steal budget.
    #[test]
    fn the_spliced_summary_row_counts_as_no_images() {
        let source = vec![user_with_images("look", &["AAAA", "BBBB"])];
        let mut summary = trim_msg(usize::MAX, "<conversation-summary>x</conversation-summary>");
        summary.is_summary = true;
        let (had, keep, dropped) = plan_live_image_cap(&source, &[summary, trim_msg(0, "look")]);
        assert_eq!(had, vec![0, 2]);
        assert_eq!(keep, vec![0, 1]);
        assert_eq!(dropped, 1);
    }

    /// Tool request/response pairing is load-bearing — the cap must not so much
    /// as count those messages, let alone rebuild them.
    #[test]
    fn messages_carrying_tool_parts_are_never_capped() {
        let request = Message::assistant().with_tool_request(
            "call-1",
            Ok(rmcp::model::CallToolRequestParams::new(
                "look_at_camera_snapshot".to_string(),
            )),
        );
        // A camera tool DOES return frames inside its response — the cap must
        // still keep its hands off, or the request/response pair breaks.
        let response = tool_response_message("call-1", "front-door, person");
        assert!(has_tool_parts(&request));
        assert!(has_tool_parts(&response));

        let source = vec![
            user_with_images("first", &["AAAA"]),
            request,
            response,
            user_with_images("second", &["BBBB"]),
        ];
        let trimmed = vec![
            trim_msg(0, "first"),
            trim_msg(1, ""),
            trim_msg(2, "front-door, person"),
            trim_msg(3, "second"),
        ];
        let (had, keep, dropped) = plan_live_image_cap(&source, &trimmed);
        assert_eq!(
            &had[1..3],
            &[0, 0],
            "tool messages contribute nothing to the plan"
        );
        assert_eq!(&keep[1..3], &[0, 0]);
        // The two plain image turns are still capped normally around them.
        assert_eq!(dropped, 1);
    }

    /// A partially-capped message keeps its leading image AND gets a stand-in
    /// for the ones that went — but the stand-in must describe the DROPPED
    /// images. The all-dropped wording here would assert "an image attached
    /// here is no longer available" with an image sitting in the same message,
    /// which is precisely the contradiction the `<vision>` section exists to
    /// prevent.
    #[test]
    fn a_partially_capped_message_does_not_deny_the_image_it_still_shows() {
        use pond_core::models::services::context::image_history::{
            HISTORY_IMAGE_PLACEHOLDER, HISTORY_IMAGE_PLACEHOLDER_MARKER,
            HISTORY_IMAGE_PLACEHOLDER_PARTIAL,
        };
        let original = user_with_images("look at these", &["AAAA", "BBBB", "CCCC"]);
        let capped = cap_message_images(&original, 1, "look at these");
        assert_eq!(
            image_parts(&capped),
            vec![("AAAA".into(), "image/png".into())]
        );
        let text = capped.as_concat_text();
        assert!(text.contains("look at these"));
        assert!(text.contains(HISTORY_IMAGE_PLACEHOLDER_PARTIAL));
        assert!(
            !text.contains(HISTORY_IMAGE_PLACEHOLDER),
            "the all-dropped wording contradicts the surviving image: {text}"
        );
        assert_eq!(text.matches(HISTORY_IMAGE_PLACEHOLDER_MARKER).count(), 1);
    }

    /// Dropping every image still leaves the model told that something visual
    /// was there — otherwise "what colour is this?" invites an invention.
    #[test]
    fn a_fully_stripped_image_turn_still_says_an_image_was_there() {
        let original = user_with_images("what colour is this?", &["AAAA"]);
        let capped = cap_message_images(&original, 0, "what colour is this?");
        assert!(image_parts(&capped).is_empty());
        assert!(capped.as_concat_text().contains(
            pond_core::models::services::context::image_history::HISTORY_IMAGE_PLACEHOLDER
        ));
    }

    #[test]
    fn capping_preserves_message_identity() {
        let mut original = user_with_images("look", &["AAAA", "BBBB"]);
        original.id = Some("msg-7".into());
        original.created = 1_234_567;
        let capped = cap_message_images(&original, 1, "look");
        assert_eq!(capped.id.as_deref(), Some("msg-7"));
        assert_eq!(capped.created, 1_234_567);
        assert_eq!(capped.role, original.role);
    }

    /// An image-bearing user turn also carries the `<system-context>` envelope,
    /// and the text-only rewrite branch can never reach it — so before the cap
    /// existed, its stale block was re-prefilled for the life of the session.
    #[test]
    fn capping_applies_the_trimmed_text_to_an_image_turn() {
        let stale = "<system-context>\nToday is Tuesday\n</system-context>\n<user-message>look</user-message>";
        let original = user_with_images(stale, &["AAAA"]);
        let trimmed_text =
            pond_core::models::services::context::turn_trimmer::strip_system_context(stale)
                .into_owned();
        let capped = cap_message_images(&original, 0, &trimmed_text);
        assert!(!capped.as_concat_text().contains("<system-context>"));
        assert!(capped
            .as_concat_text()
            .contains("<user-message>look</user-message>"));
    }

    /// Second pass over a FULLY capped conversation must be a no-op: the
    /// message has no image parts left, so it cannot collect a second
    /// placeholder or churn the prefix.
    #[test]
    fn capping_is_idempotent() {
        let original = user_with_images("look", &["AAAA", "BBBB"]);
        let once = cap_message_images(&original, 0, "look");
        let text = once.as_concat_text();
        let (had, keep, dropped) =
            plan_live_image_cap(std::slice::from_ref(&once), &[trim_msg(0, &text)]);
        assert_eq!(had, vec![0]);
        assert_eq!(keep, vec![0]);
        assert_eq!(dropped, 0, "nothing left to drop on a second pass");
    }

    /// The path the idempotence test above CANNOT reach, and the one that
    /// actually happens: a 2-image message is capped in two stages — 2 to 1 when
    /// it becomes history, 1 to 0 when a newer image turn arrives. On the second
    /// stage the message still has an image, so the cap runs again; the text has
    /// not changed (the pass-1 placeholder is already inside `as_concat_text()`),
    /// so nothing stops a second placeholder from being appended.
    ///
    /// Drives it through the real plan/cap pair rather than calling
    /// `cap_message_images` twice by hand, so the trimmer's text-feedback loop
    /// is part of the test.
    #[test]
    fn staged_capping_converges_to_exactly_one_placeholder() {
        use pond_core::models::services::context::image_history::{
            HISTORY_IMAGE_PLACEHOLDER, HISTORY_IMAGE_PLACEHOLDER_MARKER,
            HISTORY_IMAGE_PLACEHOLDER_PARTIAL,
        };

        // Stage 1: the 2-image turn is the newest, budget 1 -> keep the leading
        // image, one placeholder for the dropped one.
        let original = user_with_images("look at these", &["AAAA", "BBBB"]);
        let (had, keep, dropped) = plan_live_image_cap(
            std::slice::from_ref(&original),
            &[trim_msg(0, &original.as_concat_text())],
        );
        assert_eq!((had[0], keep[0], dropped), (2, 1, 1));
        let stage1 = cap_message_images(&original, keep[0], &original.as_concat_text());
        assert_eq!(image_parts(&stage1).len(), 1);
        assert_eq!(
            stage1
                .as_concat_text()
                .matches(HISTORY_IMAGE_PLACEHOLDER_MARKER)
                .count(),
            1
        );

        // Stage 2: a newer image turn arrives, so the budget moves on and the
        // older message loses its last image. Its trimmer text is whatever the
        // message now concatenates to — placeholder included, which is exactly
        // why `text_changed` cannot be the guard.
        let newer = user_with_images("and this one", &["CCCC"]);
        let source = vec![stage1.clone(), newer];
        let trimmed = vec![
            trim_msg(0, &source[0].as_concat_text()),
            trim_msg(1, &source[1].as_concat_text()),
        ];
        let (had, keep, dropped) = plan_live_image_cap(&source, &trimmed);
        assert_eq!((had[0], keep[0]), (1, 0), "the older turn loses its image");
        assert_eq!((had[1], keep[1]), (1, 1), "the newest turn keeps its own");
        assert_eq!(dropped, 1);

        let stage2 = cap_message_images(&source[0], keep[0], &trimmed[0].text);
        let text = stage2.as_concat_text();
        assert!(image_parts(&stage2).is_empty());
        assert_eq!(
            text.matches(HISTORY_IMAGE_PLACEHOLDER_MARKER).count(),
            1,
            "one placeholder for the message's state, not one per pass: {text}"
        );
        assert!(
            text.contains(HISTORY_IMAGE_PLACEHOLDER),
            "no image survives now, so the all-dropped wording is the true one: {text}"
        );
        assert!(
            !text.contains(HISTORY_IMAGE_PLACEHOLDER_PARTIAL),
            "the stale partial wording claims an image is still shown: {text}"
        );
        assert!(
            text.contains("look at these"),
            "the user's own text survives"
        );

        // Stage 3: a third pass changes nothing further.
        let stage3 = cap_message_images(&stage2, 0, &text);
        assert_eq!(stage3.as_concat_text(), text);
    }

    // ── B3: the Goose cap-message coupling ───────────────────────────────

    /// Canary for the copied `GOOSE_MAX_TURNS_MESSAGE`. The upstream constant is
    /// private, so GIAP matches on a duplicate string; if a Goose sync rewords
    /// it, cap detection silently stops working and the Continue affordance
    /// never appears. This test reads the fork source so that failure is loud.
    #[test]
    fn goose_cap_message_is_still_verbatim() {
        let agent_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../goose/crates/goose/src/agents/agent.rs");
        let Ok(source) = std::fs::read_to_string(&agent_rs) else {
            // Submodule not initialised (`git submodule update --init`).
            // Skipping beats failing on a checkout that cannot see the fork.
            eprintln!("skipping: {} unavailable", agent_rs.display());
            return;
        };
        assert!(
            source.contains(&format!(
                "MAX_TURNS_MESSAGE: &str = \"{GOOSE_MAX_TURNS_MESSAGE}\""
            )),
            "Goose's MAX_TURNS_MESSAGE no longer matches GOOSE_MAX_TURNS_MESSAGE — \
             update the constant in goose_agent.rs or turn-limit detection is dead"
        );
    }

    /// The detection itself: exact match (with surrounding whitespace tolerated),
    /// and nothing else.
    #[test]
    fn only_the_cap_sentence_counts_as_a_turn_limit() {
        let hit = |t: &str| t.trim() == GOOSE_MAX_TURNS_MESSAGE;
        assert!(hit(GOOSE_MAX_TURNS_MESSAGE));
        assert!(hit(&format!("\n{GOOSE_MAX_TURNS_MESSAGE}\n")));
        assert!(!hit("I've reached the maximum number of actions."));
        assert!(!hit(&format!(
            "{GOOSE_MAX_TURNS_MESSAGE} Also here is more."
        )));
        assert!(!hit("Would you like me to continue?"));
        assert!(!hit(""));
    }

    // ── C2: env-knob decision table ──────────────────────────────────────

    fn knob(knobs: &[(&'static str, Option<String>)], key: &str) -> Option<String> {
        knobs
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| v.clone())
    }

    #[test]
    fn hybrid_compaction_disables_goose_compaction_and_tool_pair_summaries() {
        let knobs = goose_env_knobs("local", 4096, true);
        assert_eq!(knob(&knobs, "GOOSE_CONTEXT_LIMIT").as_deref(), Some("4096"));
        assert_eq!(
            knob(&knobs, "GOOSE_AUTO_COMPACT_THRESHOLD").as_deref(),
            Some("1.0")
        );
        assert_eq!(
            knob(&knobs, "GOOSE_TOOL_PAIR_SUMMARIZATION").as_deref(),
            Some("false"),
            "the deterministic trimmer owns tool-result pruning on-device"
        );
    }

    #[test]
    fn without_hybrid_compaction_goose_keeps_its_own_defaults() {
        let knobs = goose_env_knobs("local", 8192, false);
        assert_eq!(knob(&knobs, "GOOSE_CONTEXT_LIMIT").as_deref(), Some("8192"));
        // Unset, not "0.8" — absence restores Goose's own default.
        assert!(knob(&knobs, "GOOSE_AUTO_COMPACT_THRESHOLD").is_none());
        assert!(knob(&knobs, "GOOSE_TOOL_PAIR_SUMMARIZATION").is_none());
    }

    /// Tool-pair summarization is only disabled for the in-process engine: an
    /// HTTP provider's spare capacity is not ours to conserve.
    #[test]
    fn http_providers_keep_goose_tool_pair_summarization() {
        for provider in ["ollama", "llamafile"] {
            let knobs = goose_env_knobs(provider, 32768, true);
            assert!(
                knob(&knobs, "GOOSE_TOOL_PAIR_SUMMARIZATION").is_none(),
                "{provider}"
            );
            assert_eq!(
                knob(&knobs, "GOOSE_AUTO_COMPACT_THRESHOLD").as_deref(),
                Some("1.0"),
                "{provider}"
            );
        }
    }

    /// Empty-turn recovery has one owner. Goose's own retry re-sends an
    /// unchanged conversation, which a deterministic local model answers
    /// identically — so it must be off on every provider and in both compaction
    /// modes, leaving GIAP's steered re-engagement as the only recovery path.
    #[test]
    fn goose_never_retries_empty_turns_itself() {
        for provider in ["local", "gguf", "ollama", "llamafile"] {
            for hybrid in [true, false] {
                assert_eq!(
                    knob(
                        &goose_env_knobs(provider, 4096, hybrid),
                        "GOOSE_MAX_EMPTY_TURN_RETRIES"
                    )
                    .as_deref(),
                    Some("0"),
                    "{provider} hybrid={hybrid}"
                );
            }
        }
    }

    /// The steer must actually change the prompt — a retry that alters nothing
    /// reproduces the same empty turn and just costs another prefill.
    #[test]
    fn empty_turn_steer_is_non_empty_and_distinct_from_the_user_text() {
        let user_text = "any news on the expressway toll?";
        let steered = format!("{user_text}\n\n{EMPTY_TURN_STEER}");
        assert_ne!(steered, user_text);
        assert!(
            steered.starts_with(user_text),
            "the original ask must survive"
        );
        assert!(!EMPTY_TURN_STEER.trim().is_empty());
    }

    /// Goose's empty-turn sentence is matched verbatim, so a wording drift on an
    /// upstream sync silently disables recovery. Pin it.
    #[test]
    fn goose_empty_turn_sentinel_is_pinned() {
        assert_eq!(
            GOOSE_EMPTY_TURN_MESSAGE,
            "The model returned an empty response. Please resend your message to continue."
        );
        assert_ne!(GOOSE_EMPTY_TURN_MESSAGE, EMPTY_TURN_EXHAUSTED_MESSAGE);
    }

    /// The knob set doubles as the change signature that gates `set_var`, so a
    /// settings change MUST alter it and an unchanged setting must not.
    #[test]
    fn knob_signature_changes_only_when_a_setting_changes() {
        let sig = |p, ctx, hybrid| {
            goose_env_knobs(p, ctx, hybrid)
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(";")
        };
        assert_eq!(sig("local", 4096, true), sig("local", 4096, true));
        assert_ne!(sig("local", 4096, true), sig("local", 4096, false));
        assert_ne!(sig("local", 4096, true), sig("local", 8192, true));
        assert_ne!(sig("local", 4096, true), sig("ollama", 4096, true));
    }

    // ── C3: structured tool-response truncation ──────────────────────────

    fn tool_response_message(id: &str, body: &str) -> goose::conversation::message::Message {
        goose::conversation::message::Message::user().with_tool_response(
            id,
            Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::Content::text(body.to_string()),
            ])),
        )
    }

    fn tool_response_text(message: &goose::conversation::message::Message) -> String {
        message
            .content
            .iter()
            .filter_map(|c| match c {
                goose::conversation::message::MessageContent::ToolResponse(tr) => {
                    tr.tool_result.as_ref().ok()
                }
                _ => None,
            })
            .flat_map(|r| r.content.iter())
            .filter_map(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn a_small_tool_response_is_not_rewritten() {
        let msg = tool_response_message("call-1", "sunny, 21C");
        assert!(truncate_tool_response_text(&msg, 1_500).is_none());
    }

    #[test]
    fn plain_text_messages_are_never_rewritten() {
        let msg = goose::conversation::message::Message::assistant()
            .with_text("x".repeat(10_000).as_str());
        assert!(truncate_tool_response_text(&msg, 1_500).is_none());
    }

    /// The bug this closes: an oversized structured tool result used to be
    /// re-prefilled verbatim every turn because the rebuild kept it whole.
    #[test]
    fn an_oversized_tool_response_is_truncated_head_and_tail() {
        let body = format!("FIRST-LINE{}LAST-LINE", "x".repeat(50_000));
        let msg = tool_response_message("call-1", &body);
        let out = truncate_tool_response_text(&msg, 1_500).expect("should truncate");
        let text = tool_response_text(&out);
        assert!(text.len() < 1_600, "len {}", text.len());
        assert!(text.starts_with("FIRST-LINE"));
        assert!(text.ends_with("LAST-LINE"));
        assert!(text.contains("[... truncated "));
    }

    /// Pairing preservation — the property that matters most here: an orphaned
    /// or re-keyed tool response is rejected outright by the provider, so the
    /// rewrite must preserve the response id, the content-part count, the role,
    /// and the error flag. Only the text shrinks.
    #[test]
    fn truncation_preserves_the_tool_call_pairing_structure() {
        let body = "y".repeat(40_000);
        let original = tool_response_message("call-abc", &body);
        let rewritten = truncate_tool_response_text(&original, 1_500).expect("should truncate");

        assert_eq!(rewritten.role, original.role);
        assert_eq!(rewritten.content.len(), original.content.len());

        let ids = |m: &goose::conversation::message::Message| {
            m.content
                .iter()
                .filter_map(|c| match c {
                    goose::conversation::message::MessageContent::ToolResponse(tr) => {
                        Some((tr.id.clone(), tr.tool_result.is_ok()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&rewritten), ids(&original));
        assert_eq!(ids(&rewritten), vec![("call-abc".to_string(), true)]);

        let parts = |m: &goose::conversation::message::Message| {
            m.content
                .iter()
                .filter_map(|c| match c {
                    goose::conversation::message::MessageContent::ToolResponse(tr) => {
                        tr.tool_result.as_ref().ok()
                    }
                    _ => None,
                })
                .map(|r| (r.content.len(), r.is_error))
                .collect::<Vec<_>>()
        };
        assert_eq!(parts(&rewritten), parts(&original));
        assert!(tool_response_text(&rewritten).len() < tool_response_text(&original).len());
    }

    /// An error tool result is truncated too, and stays an error.
    #[test]
    fn an_error_tool_response_keeps_its_error_flag() {
        let msg = goose::conversation::message::Message::user().with_tool_response(
            "call-err",
            Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::Content::text("z".repeat(20_000)),
            ])),
        );
        let out = truncate_tool_response_text(&msg, 1_500).expect("should truncate");
        let is_error = out.content.iter().any(|c| match c {
            goose::conversation::message::MessageContent::ToolResponse(tr) => tr
                .tool_result
                .as_ref()
                .is_ok_and(|r| r.is_error == Some(true)),
            _ => false,
        });
        assert!(is_error);
        assert!(tool_response_text(&out).contains("[... truncated "));
    }

    #[test]
    fn quant_spelling_collapses_to_display_stem() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("gemma-4-E2B-it-Q4_K_M.gguf"), b"gguf").unwrap();
        assert_eq!(
            canonical_model_stem("gemma-4-E2B-it-Q4_K_M", tmp.path()),
            "gemma-4-E2B-it"
        );
        // The display spelling is already canonical.
        assert_eq!(
            canonical_model_stem("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it"
        );
        // Both spellings now share one registry id.
    }

    #[test]
    fn explicit_quant_pin_keeps_its_identity_when_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("gemma-4-E4B-it-Q4_K_M.gguf"), b"gguf").unwrap();
        std::fs::write(tmp.path().join("gemma-4-E4B-it-Q4_K_S.gguf"), b"gguf").unwrap();
        // The display stem would resolve Q4_K_M (lexicographic); a pin on
        // Q4_K_S therefore stays its own id.
        assert_eq!(
            canonical_model_stem("gemma-4-E4B-it-Q4_K_S", tmp.path()),
            "gemma-4-E4B-it-Q4_K_S"
        );
        // The matching pin collapses.
        assert_eq!(
            canonical_model_stem("gemma-4-E4B-it-Q4_K_M", tmp.path()),
            "gemma-4-E4B-it"
        );
    }

    #[test]
    fn missing_file_and_non_quant_tails_are_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            canonical_model_stem("gemma-4-E2B-it-Q4_K_M", tmp.path()),
            "gemma-4-E2B-it-Q4_K_M"
        );
        assert_eq!(
            canonical_model_stem("llama-3.2-3b-instruct", tmp.path()),
            "llama-3.2-3b-instruct"
        );
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"gguf").unwrap();
    }

    #[test]
    fn exact_match_is_preferred() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it.gguf");
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it.gguf"
        );
    }

    /// The bug this fixes: a display name resolves to its quant-suffixed file.
    #[test]
    fn display_name_resolves_to_its_quant_file() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it-Q4_K_M.gguf"
        );
    }

    #[test]
    fn an_explicit_gguf_name_is_taken_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_gguf_filename("whatever-Q8_0.gguf", tmp.path()),
            "whatever-Q8_0.gguf"
        );
    }

    /// A shorter name must not swallow a longer sibling: `gemma-4-E2B` is not
    /// a prefix-with-separator of `gemma-4-E2B-it`, so it must not match it.
    #[test]
    fn a_bare_prefix_does_not_match() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        // No file for "gemma-4-E2B" exists, and the -it- file is a different
        // model, so we fall back to the naive name rather than mis-resolving.
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B", tmp.path()),
            "gemma-4-E2B.gguf"
        );
    }

    #[test]
    fn missing_file_falls_back_to_the_naive_name() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_gguf_filename("not-installed", tmp.path()),
            "not-installed.gguf"
        );
    }

    /// Among several quant variants the choice is deterministic.
    #[test]
    fn variant_choice_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E4B-it-Q4_K_S.gguf");
        touch(tmp.path(), "gemma-4-E4B-it-Q4_K_M.gguf");
        // Lexicographically first: ...Q4_K_M before ...Q4_K_S.
        assert_eq!(
            resolve_gguf_filename("gemma-4-E4B-it", tmp.path()),
            "gemma-4-E4B-it-Q4_K_M.gguf"
        );
    }

    #[test]
    fn quant_tags_are_told_apart_from_name_continuations() {
        for q in [
            "Q4_K_M", "Q6_K", "Q8_0", "Q4_0", "IQ4_XS", "F16", "F32", "BF16",
        ] {
            assert!(looks_like_quant_tag(q), "{q} should read as a quant tag");
        }
        for not in ["it", "instruct", "it-Q4_K_M", "chat", ""] {
            assert!(!looks_like_quant_tag(not), "{not} is not a quant tag");
        }
    }

    /// The exact production shape: two sibling models where one name is a
    /// prefix of the other. Each must resolve to its own file.
    #[test]
    fn sibling_models_do_not_cross_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        touch(tmp.path(), "gemma-4-E4B-it-Q4_K_M.gguf");
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it-Q4_K_M.gguf"
        );
        assert_eq!(
            resolve_gguf_filename("gemma-4-E4B-it", tmp.path()),
            "gemma-4-E4B-it-Q4_K_M.gguf"
        );
    }

    #[tokio::test]
    #[ignore = "requires llamafile at http://127.0.0.1:8080"]
    async fn test_goose_adapter_chat_stream_live() {
        let adapter = GooseAdapter::with_llamafile(None).await.unwrap();
        let request = AgentRequest {
            message: "Say hello and nothing else".to_string(),
            session_id: "test-session".to_string(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
            profile_scope: ProfileScope::Household,
            profile_context: None,
        };

        let mut stream = adapter.chat_stream(request).await.unwrap();
        let mut saw_text = false;

        while let Some(event_result) = stream.next().await {
            let event = event_result.unwrap();
            match event {
                AgentStreamEvent::Text { .. } => saw_text = true,
                AgentStreamEvent::Done { .. } => break,
                _ => {}
            }
        }
        assert!(saw_text);
    }
}
