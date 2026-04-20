use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::ports::agent::{Agent as AgentPort, AgentRequest, AgentResponse};
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::prompt_extra::PromptExtraRepository;
use pond_core::ports::prompt_template::PromptTemplateRepository;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use pond_core::prompts::build_system_prompt_from_template;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::extension_manager::GiapGooseExtensionManager;

/// Minimal hard-coded fallback — used only when the DB has no template for the
/// current `prompt_style`. Not a full system prompt: just enough to be safe.
const FALLBACK_PROMPT: &str =
    "You are {{assistant_name}}, a privacy-first local AI home assistant. \
     No data leaves this home. Be concise, warm, and practical. \
     No Markdown. Never emit pipeline control tokens.";

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
/// 6. Auto-loads the `"giap"` builtin MCP extension (once per session).
/// 7. Runs Goose's full agentic loop and returns aggregated text + tool-call metadata.
pub struct GooseAdapter {
    agent:            Arc<GooseAgent>,
    session_manager:  Arc<SessionManager>,
    settings_repo:    Arc<dyn SettingsRepository>,
    template_repo:    Arc<dyn PromptTemplateRepository>,
    extras_repo:      Arc<dyn PromptExtraRepository>,
    skill_repo:       Arc<dyn UserSkillRepository>,
    memory_repo:      Arc<dyn MemoryRepository>,
    llamafile_url:    String,
    /// Tracks the last `"chat_provider:chat_model"` key we wired into Goose.
    last_provider_key: Mutex<String>,
    /// Sessions that have already had the "giap" builtin extension loaded.
    loaded_extensions: Mutex<HashSet<String>>,
    /// Maps GIAP session IDs → Goose session IDs (Goose auto-generates its own IDs).
    goose_session_map: Mutex<HashMap<String, String>>,
}

impl GooseAdapter {
    /// Primary factory — all repos are injected by `pond-server/main.rs`.
    pub async fn new(
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Arc<dyn PromptTemplateRepository>,
        extras_repo:   Arc<dyn PromptExtraRepository>,
        skill_repo:    Arc<dyn UserSkillRepository>,
        memory_repo:   Arc<dyn MemoryRepository>,
        llamafile_url: String,
    ) -> Result<Self> {
        let session_manager = Arc::new(SessionManager::instance());
        let permission_manager = goose::config::permission::PermissionManager::instance();

        let config = AgentConfig::new(
            session_manager.clone(),
            permission_manager,
            None,
            GooseMode::Auto,
            false,
            GoosePlatform::GooseCli,
        );

        let agent = GooseAgent::with_config(config);

        Ok(Self {
            agent: Arc::new(agent),
            session_manager,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            memory_repo,
            llamafile_url,
            last_provider_key: Mutex::new(String::new()),
            loaded_extensions: Mutex::new(HashSet::new()),
            goose_session_map: Mutex::new(HashMap::new()),
        })
    }

    /// Convenience factory for non-server use (tests, CLI one-shots).
    /// Uses mock repos and connects to llamafile at `host`.
    pub async fn with_llamafile(host: Option<&str>) -> Result<Self> {
        use pond_core::services::mock_settings::MockSettingsRepository;
        use pond_core::services::mock_prompt_template::MockPromptTemplateRepository;
        use pond_core::services::mock_prompt_extra::MockPromptExtraRepository;
        use pond_core::services::mock_skill::MockSkillRepository;
        use pond_core::services::mock_memory::MockMemoryRepository;

        let url = host.unwrap_or("http://127.0.0.1:8080").to_string();
        Self::new(
            Arc::new(MockSettingsRepository::default()),
            Arc::new(MockPromptTemplateRepository::default()),
            Arc::new(MockPromptExtraRepository::default()),
            Arc::new(MockSkillRepository::default()),
            Arc::new(MockMemoryRepository::default()),
            url,
        ).await
    }

    /// Returns an `GiapGooseExtensionManager` for managing Goose extensions on a session.
    pub fn extension_manager(&self, session_id: String) -> GiapGooseExtensionManager {
        GiapGooseExtensionManager::new(self.agent.clone(), session_id)
    }

    /// Add a named builtin extension to a Goose session (idempotent).
    pub async fn add_builtin_extension(&self, name: &str, session_id: &str) -> Result<()> {
        let config = ExtensionConfig::Builtin {
            name: name.to_string(),
            description: String::new(),
            display_name: None,
            timeout: None,
            bundled: Some(false),
            available_tools: vec![],
        };
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
    async fn resolve_goose_session(&self, giap_sid: &str) -> String {
        // Fast path: already mapped this session.
        if let Some(gid) = self.goose_session_map.lock().unwrap().get(giap_sid).cloned() {
            return gid;
        }
        // Try using the GIAP session_id as-is (e.g. if Goose already stored it).
        if self.session_manager.get_session(giap_sid, false).await.is_ok() {
            self.goose_session_map.lock().unwrap()
                .insert(giap_sid.to_string(), giap_sid.to_string());
            return giap_sid.to_string();
        }
        // Create a brand-new Goose session; use the GIAP id as the human name.
        match self.session_manager.create_session(
            std::env::current_dir().unwrap_or_default(),
            giap_sid.to_string(),
            goose::session::session_manager::SessionType::User,
            GooseMode::Auto,
        ).await {
            Ok(session) => {
                let gid = session.id.clone();
                self.goose_session_map.lock().unwrap()
                    .insert(giap_sid.to_string(), gid.clone());
                gid
            }
            Err(e) => {
                tracing::warn!("Failed to create Goose session for '{}': {e}", giap_sid);
                giap_sid.to_string()
            }
        }
    }

    /// Hot-swap the Goose provider when `chat_provider` / `chat_model` in settings changes.
    async fn ensure_provider_current(
        &self,
        settings: &pond_core::domain::settings::Settings,
        session_id: &str,
    ) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);
        {
            let last = self.last_provider_key.lock().unwrap();
            if *last == key {
                return Ok(());
            }
        }

        let provider: Option<Arc<dyn Provider>> = match settings.chat_provider.as_str() {
            // "local" (and legacy alias "gguf") uses llamafile as the HTTP backend
            // (same wire format as Ollama).
            // The server starts llamafile when provider=llamafile; for provider=local the
            // GooseAdapter is expected to be the inference path.  When llamafile IS running
            // (e.g. user started it manually or via `serve --provider llamafile`) this works
            // transparently.  Without a running server the agentic loop will return a
            // connection error.
            "local" | "gguf" | "llamafile" => {
                std::env::set_var("OLLAMA_HOST", &self.llamafile_url);
                let model_name = if settings.chat_model.is_empty() {
                    "llamafile".to_string()
                } else {
                    settings.chat_model.clone()
                };
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                match goose::providers::ollama::OllamaProvider::from_env(cfg).await {
                    Ok(p) => Some(Arc::new(p)),
                    Err(e) => {
                        tracing::warn!("Failed to build llamafile/local provider: {e}");
                        None
                    }
                }
            }
            "ollama" => {
                let model_name = if settings.chat_model.is_empty() {
                    "llama3.2".to_string()
                } else {
                    settings.chat_model.clone()
                };
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                match goose::providers::ollama::OllamaProvider::from_env(cfg).await {
                    Ok(p) => Some(Arc::new(p)),
                    Err(e) => {
                        tracing::warn!("Failed to build ollama provider: {e}");
                        None
                    }
                }
            }
            _ => None, // unknown provider — keep whatever Goose currently has
        };

        if let Some(p) = provider {
            tracing::info!(
                "Switching Goose provider to {}:{} for session {}",
                settings.chat_provider, settings.chat_model, session_id
            );
            self.agent.update_provider(p, session_id).await?;
            *self.last_provider_key.lock().unwrap() = key;
        }
        Ok(())
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let session_id = &request.session_id;

        // Goose maintains its own sessions.db with auto-generated IDs.
        // Ensure a Goose session exists for this GIAP session before calling agent.reply().
        let goose_sid = self.resolve_goose_session(session_id).await;
        let goose_sid = goose_sid.as_str();

        // ── 1. System prompt ──────────────────────────────────────────────────
        let template_content = self.template_repo
            .get(&settings.prompt_style)
            .await
            .ok()
            .flatten()
            .map(|t| t.content)
            .unwrap_or_else(|| FALLBACK_PROMPT.to_string());
        let system_prompt = build_system_prompt_from_template(&settings, &template_content);
        self.agent.override_system_prompt(system_prompt).await;

        // ── 2. System prompt extras ───────────────────────────────────────────
        if let Ok(extras) = self.extras_repo.list_active().await {
            for extra in extras {
                self.agent.extend_system_prompt(extra.key, extra.instruction).await;
            }
        }

        // ── 3. Active user skills ─────────────────────────────────────────────
        if let Ok(skills) = self.skill_repo.list_active().await {
            for skill in skills {
                self.agent
                    .extend_system_prompt(format!("skill:{}", skill.name), skill.content)
                    .await;
            }
        }

        // ── 4. Memory injection ───────────────────────────────────────────────
        if settings.agent_memory_inject {
            let limit = settings.agent_memory_limit as usize;
            if let Ok(memories) = self.memory_repo.search_recent(None, limit).await {
                if !memories.is_empty() {
                    let block = memories
                        .iter()
                        .map(|m| format!("- {}", m.content))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.agent
                        .extend_system_prompt(
                            "memories".to_string(),
                            format!("Relevant memories:\n{block}"),
                        )
                        .await;
                }
            }
        }

        // ── 5. Provider hot-swap ──────────────────────────────────────────────
        if let Err(e) = self.ensure_provider_current(&settings, goose_sid).await {
            tracing::warn!("Provider update failed (continuing with current provider): {e}");
        }

        // ── 6. Auto-load "giap" builtin extension ─────────────────────────────
        let needs_extension_load = {
            let loaded = self.loaded_extensions.lock().unwrap();
            !loaded.contains(goose_sid)
        };
        if needs_extension_load {
            self.add_builtin_extension("giap", goose_sid).await.ok();
            self.loaded_extensions.lock().unwrap().insert(goose_sid.to_string());
        }

        // ── 7. GooseMode from settings ────────────────────────────────────────
        let goose_mode = match settings.agent_goose_mode.as_str() {
            "chat"         => GooseMode::Chat,
            "smart_approve"=> GooseMode::SmartApprove,
            "approve"      => GooseMode::Approve,
            _              => GooseMode::Auto,
        };
        self.agent
            .update_goose_mode(goose_mode, goose_sid)
            .await
            .ok();

        // ── 8. Run the agentic loop ───────────────────────────────────────────
        let user_msg = Message::user().with_text(&request.message);
        let session_cfg = goose::agents::types::SessionConfig {
            id: goose_sid.to_string(),
            schedule_id: None,
            max_turns: Some(settings.agent_max_turns as u32),
            retry_config: None,
        };

        let mut stream = self.agent.reply(user_msg, session_cfg, None).await?;

        // ── 9. Collect streamed events ────────────────────────────────────────
        let mut text = String::new();
        let mut tool_call_ids: Vec<String> = vec![];

        while let Some(event_result) = stream.next().await {
            let event = event_result?;
            match event {
                goose::agents::AgentEvent::Message(msg) => {
                    let chunk = msg.as_concat_text();
                    if !chunk.is_empty() {
                        text.push_str(&chunk);
                    }
                    // Capture tool request IDs for metadata
                    for content in &msg.content {
                        if let goose::conversation::message::MessageContent::ToolRequest(tr) = content {
                            tracing::info!("Agent tool call id={}", tr.id);
                            tool_call_ids.push(tr.id.clone());
                        }
                    }
                }
                goose::agents::AgentEvent::HistoryReplaced(_) => {
                    tracing::debug!("Goose compacted context history");
                }
                _ => {}
            }
        }

        if text.is_empty() {
            return Err(anyhow!("Received empty response from Goose agent"));
        }

        let mut metadata = HashMap::new();
        if !tool_call_ids.is_empty() {
            metadata.insert(
                "tool_calls".to_string(),
                serde_json::to_string(&tool_call_ids).unwrap_or_default(),
            );
        }
        metadata.insert("session_id".to_string(), session_id.clone());

        Ok(AgentResponse { text, metadata })
    }
}
