use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort;
use pond_core::models::ports::agent::{
    Agent as AgentPort, AgentRequest, AgentResponse, AgentStreamEvent,
};
use pond_core::models::ports::embedding::EmbeddingProvider;
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

/// Minimal hard-coded fallback — used only when the DB has no template for the
/// current `prompt_style`. Not a full system prompt: just enough to be safe.
const FALLBACK_PROMPT: &str = "You are {{assistant_name}}, a privacy-first local AI copilot. \
     No data leaves this device. Be concise and practical. \
     Help with everyday tasks, research, writing, coding, and home control. \
     No Markdown. Never emit pipeline control tokens. \
     IMPORTANT: Only use tools listed in your schema. \
     Never use shell, bash, python, curl, or any execution tool. \
     If a service is unavailable, tell the user directly.\n\n\
     ## Tools\n\
     You have tools for weather, scheduling, memory, device management, knowledge lookup, \
     and system operations. Tool schemas describe each one. Use them when the user's request \
     matches — do not guess answers that tools could provide accurately.\n\
     When unsure about something, check your tools first. No matching tool? Tell the user honestly.\n\n\
     ## Memory\n\
     When the user shares personal information, save it immediately with save_memory. \
     Check recall_memories before knowledge lookups. \
     Corrections are highest priority.\n\n\
     ## Output Quality\n\
     Never fabricate URLs, statistics, dates, or quotes. Use a tool or say you don't know. \
     Keep responses concise. Synthesize tool results — do not parrot raw output.\n\n\
     ## Per-Turn Context\n\
     User messages use XML tags: <system-context> has date/time and <memories>. \
     <user-message> has the actual request. Only respond to <user-message>.";

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
const GOOSE_MAX_TURNS_MESSAGE: &str = "I've reached the maximum number of actions I can do without user input. Would you like me to continue?";

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
) -> [(&'static str, Option<String>); 3] {
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
    ]
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
    /// Per-turn controls for the [`GiapProviderShim`] wrapped around every
    /// provider handed to Goose — GIAP's last-mile veto over the system
    /// prompt, Goose's `<turn-context>` message injection, and the tools list.
    shim_controls: Arc<crate::provider_shim::ShimControls>,
    /// Maps GIAP session IDs → Goose session IDs (Goose auto-generates its own IDs).
    goose_session_map: Mutex<HashMap<String, String>>,
    /// Goose sessions that have already had GIAP builtin extensions loaded.
    /// Extensions are loaded once per session on first use.
    loaded_sessions: Mutex<HashSet<String>>,
    /// Dynamic tool registry — provides tool descriptions for the system prompt.
    /// When `None`, falls back to the static `giap_tool_description_lines()`.
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
    /// Embeddings of the scorable group descriptions, computed on first use.
    /// The descriptions are `&'static str` constants, so one pass is enough for
    /// the process lifetime.
    group_embeddings: tokio::sync::OnceCell<Option<Vec<(String, Vec<f32>)>>>,
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
            llamafile_url,
            data_dir,
            extension_manager,
            last_provider_key: Mutex::new(String::new()),
            current_provider: Mutex::new(None),
            provider_configured_sessions: Mutex::new(HashSet::new()),
            last_thinking_param: Mutex::new(None),
            last_env_signature: Mutex::new(String::new()),
            shim_controls: Arc::new(crate::provider_shim::ShimControls::default()),
            goose_session_map: Mutex::new(HashMap::new()),
            loaded_sessions: Mutex::new(HashSet::new()),
            tool_registry,
            user_extensions: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            voice_mode: std::sync::atomic::AtomicBool::new(false),
            model_capabilities: Mutex::new(
                pond_core::models::domain::model_capabilities::ModelCapabilities::default(),
            ),
            last_prefix_hash: Mutex::new(0),
            giap_session_storage: None,
            last_prompt_tokens_arc: std::sync::OnceLock::new(),
            cached_tools: tokio::sync::RwLock::new(None),
            defaults_stripped: Mutex::new(HashSet::new()),
            session_tool_groups: tokio::sync::RwLock::new(HashMap::new()),
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
            None, // tool_registry — falls back to static giap_tool_description_lines()
        )
        .await
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
    }

    /// Remove an extension from the user-tracking set.
    pub async fn untrack_user_extension(&self, name: &str) {
        self.user_extensions.write().await.remove(name);
        // Invalidate tool cache — removed extension means tools changed.
        *self.cached_tools.write().await = None;
    }

    /// Add a named builtin extension to a Goose session (idempotent).
    pub async fn add_builtin_extension(&self, name: &str, session_id: &str) -> Result<()> {
        let config = ExtensionConfig::Builtin {
            name: name.to_string(),
            description: String::new(),
            display_name: None,
            timeout: Some(600),
            bundled: Some(false),
            available_tools: vec![],
        };

        // Also register it with the extension manager so it can be re-enabled if disabled
        self.extension_manager
            .register_config(name.to_string(), config.clone())
            .await;

        self.agent
            .add_extension(config, session_id)
            .await
            .map_err(|e| anyhow!("Failed to add builtin extension '{}': {}", name, e))
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
        // fail every agent call for the session.
        if let Some(storage) = &self.giap_session_storage {
            if let Ok(Some(gid)) = storage.get_engine_session_id(giap_sid).await {
                if self.session_manager.get_session(&gid, false).await.is_ok() {
                    self.goose_session_map
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(giap_sid.to_string(), gid.clone());
                    tracing::debug!("Restored persisted goose session pairing {giap_sid} -> {gid}");
                    return gid;
                }
                tracing::info!(
                    "Persisted goose session '{gid}' for '{giap_sid}' is gone — re-creating"
                );
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
    /// Text-only on purpose: `pond_system.db` stores tool calls and results as
    /// separate rows and cannot reconstruct a provider-valid request/response
    /// pair, and an orphaned tool response breaks the provider outright. The
    /// user/assistant turns plus the rolling summary are what the model needs to
    /// stay coherent about the conversation.
    ///
    /// Budgeting reuses the same `trim_history` the in-turn trimmer uses, so a
    /// long history is cut to the profile's budget exactly the way a live
    /// conversation would have been. Any failure is logged and skipped — losing
    /// the replay degrades the turn, it must never fail it.
    async fn hydrate_goose_session(&self, goose_sid: &str, giap_session_id: &str) {
        use pond_core::models::domain::message::Role as GiapRole;
        use pond_core::models::services::context::context_budget::CompactionProfile;
        use pond_core::models::services::context::turn_trimmer::{plan_replay, TrimRole};

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

        let rows: Vec<(TrimRole, String)> = history
            .into_iter()
            .filter_map(|m| match m.message.role {
                GiapRole::User => Some((TrimRole::User, m.message.content)),
                GiapRole::Assistant => Some((TrimRole::Assistant, m.message.content)),
                // Tool rows are dropped (see above); System never reaches history.
                GiapRole::Tool | GiapRole::System => None,
            })
            .collect();
        if rows.is_empty() {
            return;
        }

        let rolling_summary = storage
            .get_rolling_summary(giap_session_id)
            .await
            .ok()
            .and_then(|(s, _)| s);

        let effective_ctx: usize = std::env::var("GOOSE_CONTEXT_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);
        let profile = CompactionProfile::from_context_window(effective_ctx);

        // Trailing-user drop, blank filtering, budget cut and summary splice all
        // live in pond-core's `plan_replay` so they are unit-tested there.
        let planned = plan_replay(rows, &profile, rolling_summary.as_deref());
        if planned.is_empty() {
            return;
        }

        let replayed: Vec<Message> = planned
            .iter()
            .map(|tm| match tm.role {
                TrimRole::Assistant => Message::assistant().with_text(&tm.text),
                // The spliced summary rides a user message, like the trimmer's.
                _ => Message::user().with_text(&tm.text),
            })
            .collect();
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
    fn apply_goose_env_knobs(&self, settings: &pond_core::user_data::domain::settings::Settings) {
        let effective_ctx = Self::effective_context_window(
            &settings.chat_provider,
            &settings.chat_model,
            settings.context_window_override,
        );
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
    /// every heuristic when > 0. This is the escape hatch for deployments whose
    /// real limit is neither the model's max nor the cuda ceiling — e.g. a
    /// Jetson running Ollama with a hand-tuned KV cache, or a tool-heavy GIAP
    /// prompt (~12K tokens for the 57 giap-* tools) that overflows the default
    /// heuristic and forces a compaction loop. The value flows into
    /// GOOSE_CONTEXT_LIMIT and thus into Ollama's `options.num_ctx`, so the
    /// reported limit, the request's num_ctx, and the KV cache all agree.
    /// Context size used for PROMPT-side budgets (template tier, memory
    /// injection) — as opposed to history budgets.
    ///
    /// For local in-process inference every preamble token is re-prefilled on
    /// every turn (no KV prompt-session cache yet) at roughly 0.5–1K tok/s,
    /// so a large context window must buy HISTORY room, not a more verbose
    /// preamble: an unclamped 32K profile on the Mac selected the full
    /// template tier + a 1.5K-token memory budget and produced a 9.4K-token
    /// prompt (~17s TTFT) for a one-line question. Clamping to the 8K-class
    /// profile keeps the compact tier + bounded memories regardless of how
    /// big the KV cache is. HTTP providers keep the raw window — their
    /// preamble is not paid for in local prefill.
    fn prompt_budget_ctx(provider: &str, effective_ctx: usize) -> usize {
        match provider {
            "local" | "gguf" => effective_ctx.min(8192),
            _ => effective_ctx,
        }
    }

    fn effective_context_window(provider: &str, model: &str, override_tokens: u32) -> usize {
        if override_tokens > 0 {
            return override_tokens as usize;
        }
        match provider {
            "local" | "gguf" => {
                // Generous ceiling — the actual allocation is constrained by
                // available memory at inference time, not this value.
                // Jetson (8GB): memory estimation yields ~3-6K depending on model.
                // macOS M4 (18GB): yields ~16-40K depending on model.
                #[cfg(feature = "cuda")]
                {
                    8192 // conservative ceiling for 8GB Jetson
                }
                #[cfg(not(feature = "cuda"))]
                {
                    32768 // generous ceiling — memory estimation constrains further
                }
            }
            _ => {
                // HTTP providers — use model-reported context window.
                let caps =
                    pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                        model,
                    );
                caps.context_window_tokens as usize
            }
        }
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
        let key_unchanged = {
            let last = self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *last == key
        };
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

        let provider: Option<(Arc<dyn Provider>, goose_providers::model::ModelConfig)> =
            match settings.chat_provider.as_str() {
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
                    let cfg = goose_providers::model::ModelConfig::new(&registry_key);
                    tracing::debug!(
                        "[model-switch] building LocalInferenceProvider for '{}'...",
                        model_name
                    );
                    // Wire the HF-token / config resolvers before first use — the
                    // ProviderDef path does this; the direct constructor does not.
                    goose::providers::local_inference::configure_local_inference();
                    match goose::providers::local_inference::LocalInferenceProvider::from_env()
                        .await
                    {
                        Ok(p) => {
                            tracing::debug!(
                                "[model-switch] LocalInferenceProvider ready for '{}'",
                                model_name
                            );
                            tracing::info!(
                                "Built LocalInferenceProvider for model '{}'",
                                model_name
                            );
                            Some((Arc::new(p), cfg))
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
                            Some((Arc::new(p), cfg))
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
                            Some((Arc::new(p), cfg))
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

        if let Some((p, model_cfg)) = provider {
            let model_cfg =
                Self::with_thinking_param(&settings.chat_provider, model_cfg, enable_thinking);
            // Every provider Goose sees is wrapped in the GIAP shim — the
            // last-mile veto over system prompt, message injections, and the
            // tools list (see provider_shim.rs).
            let p: Arc<dyn Provider> = Arc::new(crate::provider_shim::GiapProviderShim::new(
                p,
                self.shim_controls.clone(),
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
            let caps =
                pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                    &settings.chat_model,
                );
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
    /// rolling <conversation-summary>, and drop the oldest complete turns
    /// beyond the profile's history budget. Never calls a model; errors are
    /// logged and skipped — a failed trim must never block the turn.
    async fn trim_goose_history(&self, goose_sid: &str, giap_session_id: &str) {
        use pond_core::models::services::context::context_budget::CompactionProfile;
        use pond_core::models::services::context::turn_trimmer::{
            trim_history, TrimMessage, TrimRole,
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

        let effective_ctx: usize = std::env::var("GOOSE_CONTEXT_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);
        let profile = CompactionProfile::from_context_window(effective_ctx);
        let last_real = self
            .last_prompt_tokens_handle()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(giap_session_id)
            .copied();

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
                TrimMessage {
                    index,
                    role,
                    text,
                    is_summary,
                }
            })
            .collect();

        let outcome = trim_history(trim_input, &profile, rolling_summary.as_deref(), last_real);
        if !outcome.changed {
            return;
        }

        // Rebuild: original messages survive untouched unless (a) they are the
        // spliced summary (fresh user message), (b) their text changed AND they
        // are plain-text messages, or (c) they carry an oversized structured
        // tool response, whose TEXT bodies are truncated in place.
        //
        // (c) is the only case that rewrites a structured message, and it does
        // so by cloning the original and replacing text inside it — ids,
        // annotations, error flags and the tool-request/response pairing are
        // preserved byte-for-byte. That pairing is load-bearing: an orphaned or
        // re-keyed tool response is rejected by the provider.
        let mut rebuilt: Vec<goose::conversation::message::Message> = Vec::new();
        for tm in &outcome.messages {
            if tm.is_summary || tm.index == usize::MAX {
                rebuilt.push(goose::conversation::message::Message::user().with_text(&tm.text));
                continue;
            }
            let original = &source[tm.index];
            if let Some(truncated) = truncate_tool_response_text(
                original,
                pond_core::models::services::context_budget::TOOL_RESULT_MAX_CHARS,
            ) {
                rebuilt.push(truncated);
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
        limit: usize,
    ) -> Vec<(MemoryFragment, Option<f32>)> {
        if let Some(provider) = &self.embedding_provider {
            match provider.embed(message).await {
                Ok(query_vector) => {
                    match self
                        .memory_repo
                        .search_similar(&query_vector, None, limit)
                        .await
                    {
                        Ok(hits) => {
                            return hits
                                .into_iter()
                                .map(|fragment| {
                                    // `search_similar` degrades to search_recent when
                                    // NOTHING in the store is embedded; those hits have
                                    // no vector and so carry no similarity.
                                    let similarity = fragment
                                        .embedding
                                        .as_deref()
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
            .search_by_content(&keywords, None, limit)
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
    async fn group_description_embeddings(&self) -> Option<&Vec<(String, Vec<f32>)>> {
        self.group_embeddings
            .get_or_init(|| async {
                let provider = self.embedding_provider.as_ref()?;
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
                (!out.is_empty()).then_some(out)
            })
            .await
            .as_ref()
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
    ) -> Vec<String> {
        use pond_core::mcp::services::tool_selection as sel;

        if let Some(cached) = self
            .session_tool_groups
            .read()
            .await
            .get(giap_session_id)
            .cloned()
        {
            return cached;
        }

        if let Some(storage) = &self.giap_session_storage {
            if let Ok(Some(groups)) = storage.get_session_tool_groups(giap_session_id).await {
                if !groups.is_empty() {
                    self.session_tool_groups
                        .write()
                        .await
                        .insert(giap_session_id.to_string(), groups.clone());
                    tracing::debug!(
                        session_id = %giap_session_id,
                        groups = ?groups,
                        "tool selection: restored persisted groups"
                    );
                    return groups;
                }
            }
        }

        let available: Vec<String> = registered_extensions().to_vec();
        let signal = sel::selection_signal(first_message, memories);

        // Score, or fall back to every group. Both the "no group embeddings" and
        // the "embedding this signal failed" paths widen — the asymmetry is
        // deliberate (a missing tool is a wrong answer, a surplus one is tokens).
        let scores: Option<Vec<sel::GroupScore>> = match (
            self.group_description_embeddings().await,
            self.embedding_provider.as_ref(),
        ) {
            (Some(group_vectors), Some(provider)) => match provider.embed(&signal).await {
                Ok(query) => Some(
                    group_vectors
                        .iter()
                        .map(|(extension, v)| sel::GroupScore {
                            extension: extension.clone(),
                            score: cosine_similarity(&query, v),
                        })
                        .collect(),
                ),
                Err(e) => {
                    tracing::warn!("tool selection: embedding the opening message failed: {e}");
                    None
                }
            },
            _ => None,
        };

        let selection = sel::select_groups(
            &available,
            scores.as_deref(),
            sel::DEFAULT_RELEVANCE_THRESHOLD,
        );

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
        selection.groups
    }

    /// Attach the embedding provider used by per-turn memory retrieval.
    ///
    /// Without it the injection path keeps working, just on the keyword LIKE
    /// fallback — which is why this is a builder rather than a `new()` argument.
    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
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
            get_registry, LocalModelEntry, LocalModelStorage, ModelSettings, ToolCallingMode,
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
                    let mut settings = ModelSettings::default();
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

        // Stash the user message and session ID so MCP tool handlers can read
        // them for ToolCaller param generation and outbound HTTP trace events.
        pond_mcp_server::set_last_user_message(&request.message);
        pond_mcp_server::set_current_session_id(&session_id);

        // Settings-derived Goose knobs, re-applied every turn (cheaply — see
        // apply_goose_env_knobs) so toggling hybrid compaction or the context
        // override takes effect immediately. Must run BEFORE anything that reads
        // GOOSE_CONTEXT_LIMIT: session hydration and ModelConfig construction do.
        self.apply_goose_env_knobs(&settings);

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
                let mut loaded = 0usize;
                for ext_name in extensions {
                    match self.add_builtin_extension(ext_name, &goose_sid).await {
                        Ok(()) => {
                            loaded += 1;
                            tracing::debug!("giap extension loaded: {ext_name}");
                        }
                        // A failed extension is a real problem — surface it.
                        Err(e) => tracing::warn!("giap extension failed to load: {ext_name}: {e}"),
                    }
                }
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
        let memory_limit = if settings.agent_memory_inject {
            Some(settings.agent_memory_limit as usize)
        } else {
            None
        };

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
                match memory_limit {
                    Some(limit) => self.memory_repo.search_recent(None, limit).await,
                    None => Ok(vec![]),
                }
            },
            // Topical memories — semantic when an embedder is wired, keyword
            // LIKE otherwise. Embedding one short message is a few ms on CPU,
            // and it happens inside this join! so it overlaps the other fetches.
            async {
                match memory_limit {
                    Some(limit) => self.topical_memories(&request.message, limit).await,
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
            .unwrap_or_else(|| FALLBACK_PROMPT.to_string());

        // Voice detection is shared by prompt construction (disables thinking)
        // and the session turn cap (#105 — voice_max_turns). Check both the
        // instance-level flag (CLI --input whisper) and the per-request flag
        // (desktop voice pipeline sends voice_mode: true).
        let is_voice =
            self.voice_mode.load(std::sync::atomic::Ordering::Relaxed) || request.voice_mode;

        // Resolve thinking mode from settings + capabilities.
        // Voice mode always disables thinking — reasoning tokens waste TTS time
        // and leak as spoken text if any filter layer misses them.
        //
        // Hoisted out of the PromptState block below because it now drives two
        // things that must agree: the prompt's <thinking> section AND the
        // engine-level `enable_thinking` request-param (B4). Previously only the
        // prompt knew, so the engine kept its registry default of `true` and the
        // ThoughtFilter had to mop up the leakage.
        let thinking_enabled = if is_voice {
            false
        } else {
            let caps = self
                .model_capabilities
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            match settings.thinking_mode.as_str() {
                "on" => true,
                "off" => false,
                _ => caps.thinking, // "auto" — enable when model supports it
            }
        };

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
            // Derive compact_prompt from the PROMPT-side context budget: for
            // local inference the profile is clamped so a huge KV cache never
            // selects the verbose tier (see prompt_budget_ctx).
            let effective_ctx = Self::effective_context_window(
                &settings.chat_provider,
                &settings.chat_model,
                settings.context_window_override,
            );
            let compact_prompt =
                pond_core::models::services::context_budget::CompactionProfile::from_context_window(
                    Self::prompt_budget_ctx(&settings.chat_provider, effective_ctx),
                )
                .use_compact_prompt();

            // Tool description lines: dynamic from registry, static fallback.
            let available_tools: Vec<String> = match &self.tool_registry {
                Some(registry) => registry.prompt_description_lines(compact_prompt).await,
                None => pond_core::prompts::giap_tool_description_lines().to_vec(),
            };

            PromptState {
                current_date: now.format("%A, %-d %B %Y").to_string(),
                current_time: now.format("%H:%M").to_string(),
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
            let partition = build_prompt_partition(
                &settings,
                None, // ProfileContext — TODO: wire when profile port is available
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
                let mut last_hash = self
                    .last_prefix_hash
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *last_hash = partition.prefix_hash;
            } else {
                tracing::debug!(
                    hash = %partition.prefix_hash,
                    "Static prefix unchanged — skipping override_system_prompt (KV-cache reuse)"
                );
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
        }

        // GIAP-owned system-prompt appendix, re-attached by the provider shim
        // after it vetoes Goose's own appendages. Everything GIAP delivers via
        // goose extras below is mirrored here so the veto never loses it.
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
                shim_appendix.push(body.clone());
                self.agent.extend_system_prompt(extra.key, body).await;
            }
        }

        if let Ok(skills) = skills_result {
            for skill in skills {
                let key = format!("skill:{}", skill.name);
                let body = format!(
                    "<extension-notes name=\"{}\">\n{}\n</extension-notes>",
                    key, skill.content
                );
                shim_appendix.push(body.clone());
                self.agent.extend_system_prompt(key, body).await;
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
        // Derive a CompactionProfile for MEMORY INJECTION from the
        // prompt-side context budget: local inference re-prefills every
        // injected memory token each turn, so the budget stays bounded even
        // on a 32K context (see prompt_budget_ctx). History budgets elsewhere
        // keep the real window.
        let effective_ctx = Self::effective_context_window(
            &settings.chat_provider,
            &settings.chat_model,
            settings.context_window_override,
        );
        let compaction_profile =
            pond_core::models::services::context_budget::CompactionProfile::from_context_window(
                Self::prompt_budget_ctx(&settings.chat_provider, effective_ctx),
            );

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
                let strip_list: &[&str] = &[
                    "developer",
                    "computercontroller",
                    "extensionmanager",
                    "todo",
                    "apps",
                    "analyze",
                    "summon",
                    "summarize",
                    "orchestrator",
                    "tom",
                ];
                let user_exts = self.user_extensions.read().await;
                for ext in strip_list {
                    if !user_exts.contains(*ext) {
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
                let is_builtin = |name: &str| {
                    registered_extensions().iter().any(|e| e == name)
                        || matches!(
                            name,
                            "default"
                                | "developer"
                                | "computercontroller"
                                | "extensionmanager"
                                | "todo"
                                | "apps"
                                | "analyze"
                                | "summon"
                                | "summarize"
                                | "orchestrator"
                                | "tom"
                                | "suggestions"
                        )
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

                    self.shim_controls
                        .set_extension_appendix(Some(ext_description.clone()));
                    self.agent
                        .extend_system_prompt("extensions".to_string(), ext_description)
                        .await;
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
        let allowed_tools = if settings.tool_selection_is_relevant() {
            let groups = self
                .resolve_session_tool_groups(
                    &session_id,
                    &request.message,
                    &memory_block_for_user_msg,
                )
                .await;
            let selected: HashSet<String> =
                pond_core::mcp::services::tool_selection::filter_tools_by_groups(
                    allowed_tools.iter(),
                    &groups,
                )
                .into_iter()
                .collect();

            dormant_groups_note = pond_core::mcp::services::tool_selection::dormant_groups_note(
                registered_extensions(),
                &groups,
            );

            tracing::info!(
                target: "giap::trace",
                kind = "tool_selection",
                session_id = %session_id,
                mode = "relevant",
                groups = ?groups,
                groups_total = registered_extensions().len(),
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
            msg.push_str("</system-context>\n");
            msg.push_str("<user-message>\n");
            msg.push_str(&request.message);
            msg.push_str("\n</user-message>");
            msg
        };
        let user_msg = Message::user().with_text(&user_text);
        let session_cfg = goose::agents::types::SessionConfig {
            id: goose_sid.clone(),
            schedule_id: None,
            // Voice requests get the tighter #105 cap so a runaway loop can't
            // keep the speaker silent for the full text-chat turn budget.
            // `agent_max_turns = 0` resolves to the uncapped sentinel here.
            max_turns: Some(max_turns),
            retry_config: None,
        };

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

        // Cancellation token: when the stream is dropped (e.g. voice interrupt),
        // the DropGuard fires and cancels the token.  Goose's agent loop checks
        // `is_token_cancelled()` at each turn boundary and exits early, so
        // interruption propagates faster than waiting for the channel-drop path
        // through spawn_blocking.
        let cancel_token = CancellationToken::new();
        let cancel_guard = cancel_token.clone().drop_guard();

        let stream = async_stream::stream! {
            // Hold the guard — dropped when the stream is dropped → cancels token.
            let _guard = cancel_guard;

            yield Ok(AgentStreamEvent::Status { content: "Agent working...".to_string() });
            let mut total_output_chars: usize = 0;
            // Track tool call ID → tool name so ToolResult events carry the tool name.
            let mut tool_id_to_name: HashMap<String, String> = HashMap::new();
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

            let mut goose_stream = match agent_clone.reply(user_msg, session_cfg, Some(cancel_token)).await {
                Ok(s) => s,
                Err(e) => {
                    yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                    return;
                }
            };

            let mut turn_stats = pond_core::shared::domain::turn_stats::TurnStats::default();
            let mut saw_usage = false;
            while let Some(event_result) = goose_stream.next().await {
                match event_result {
                    Ok(event) => match event {
                        goose::agents::AgentEvent::Message(msg) => {
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
                                                tracing::warn!(
                                                    tool = %tool_name,
                                                    "Blocked unauthorized tool call (not in schema or no tools loaded)",
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
                                            yield Ok(AgentStreamEvent::ToolCall {
                                                id: tr.id.clone(),
                                                tool: tool_name,
                                                input: tool_call.arguments.clone().map(serde_json::Value::Object),
                                            });
                                        }
                                    }
                                    goose::conversation::message::MessageContent::ToolResponse(tr) => {
                                        if let Ok(tool_result) = &tr.tool_result {
                                            let content_text = tool_result
                                                .content
                                                .iter()
                                                .filter_map(|c| match c.deref() {
                                                    rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                                    _ => None,
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n");

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
                                            );
                                            yield Ok(AgentStreamEvent::ToolResult {
                                                id: tr.id.clone(),
                                                tool: tool_name,
                                                content: content_text,
                                            });
                                        }
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
                            if !raw_text.is_empty() {
                                // Goose signals "budget exhausted" by streaming a
                                // fixed sentence as ordinary assistant text (see
                                // GOOSE_MAX_TURNS_MESSAGE). Re-emit it as a
                                // structured event so a client can offer a real
                                // continue action; the text still goes through so
                                // history, persistence, and voice stay consistent.
                                let hit_turn_limit = raw_text.trim() == GOOSE_MAX_TURNS_MESSAGE;
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
            // Per-turn usage from the provider's per-inference Usage events.
            // Fall back to the chars/4 heuristic only when the provider emitted
            // no Usage events at all (some HTTP providers).
            let usage = if saw_usage {
                turn_stats.context_used_tokens = Some(turn_stats.prompt_tokens);
                pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: turn_stats.prompt_tokens,
                    completion_tokens: turn_stats.completion_tokens,
                }
            } else {
                pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: (user_msg_len / 4).max(1) as u32,
                    completion_tokens: (total_output_chars / 4).max(1) as u32,
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
}

/// Shrink the text bodies of an oversized structured tool response, or `None`
/// when the message carries no tool response over `max_chars`.
///
/// GIAP's `TOOL_RESULT_MAX_CHARS` used to reach only the trimmer's token
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

/// Whether `tag` begins with a GGUF quantization marker (`Q4_K_M`, `Q6_K`,
/// `Q8_0`, `IQ4_XS`, `F16`, `F32`, `BF16`, …). Deliberately conservative: it
/// only needs to tell a quant suffix apart from a continuation of the model
/// name (`it`, `instruct`), not to validate every possible tag.
fn looks_like_quant_tag(tag: &str) -> bool {
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
        session_id: &str,
    ) -> Vec<pond_core::mcp::ports::tools::tool_selection_control::ToolGroupStatus> {
        use pond_core::mcp::domain::tool_group::{find_group, group_of_tool};
        use pond_core::mcp::ports::tools::tool_selection_control::ToolGroupStatus;

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
        session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionError>
    {
        use pond_core::mcp::domain::tool_group::is_catalog_extension;
        use pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionError;

        let group = group.trim();
        if !is_catalog_extension(group) {
            return Err(ToolSelectionError::UnknownGroup(group.to_string()));
        }
        if !registered_extensions().iter().any(|e| e == group) {
            return Err(ToolSelectionError::GroupNotRegistered(group.to_string()));
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
                tracing::warn!(
                    group,
                    "tool selection: enabled a group but the tool cache is cold — \
                     its tools land on the next turn"
                );
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
