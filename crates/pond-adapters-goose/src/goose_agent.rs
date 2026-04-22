use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::ports::agent::{Agent as AgentPort, AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::prompt_extra::PromptExtraRepository;
use pond_core::ports::prompt_template::PromptTemplateRepository;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use pond_core::prompts::build_system_prompt_from_template;
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::PathBuf;
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
    agent: Arc<GooseAgent>,
    session_manager: Arc<SessionManager>,
    settings_repo: Arc<dyn SettingsRepository>,
    template_repo: Arc<dyn PromptTemplateRepository>,
    extras_repo: Arc<dyn PromptExtraRepository>,
    skill_repo: Arc<dyn UserSkillRepository>,
    memory_repo: Arc<dyn MemoryRepository>,
    llamafile_url: String,
    /// GIAP data directory — used to resolve GGUF model paths under
    /// `$data_dir/models/gguf/` for the in-process LocalInferenceProvider.
    data_dir: Option<PathBuf>,
    /// Shared manager for extensions.
    extension_manager: Arc<GiapGooseExtensionManager>,
    /// Tracks the last "chat_provider:chat_model" key we wired into Goose.
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
        extras_repo: Arc<dyn PromptExtraRepository>,
        skill_repo: Arc<dyn UserSkillRepository>,
        memory_repo: Arc<dyn MemoryRepository>,
        llamafile_url: String,
        data_dir: Option<PathBuf>,
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

        let agent = Arc::new(GooseAgent::with_config(config));
        // Initialize extension manager with an empty session_id (it will be updated per call or we'll need to rethink its session_id binding)
        // Actually, the ExtensionManagerPort trait doesn't take session_id, so the manager must be bound to one, or we change the trait.
        // Looking at ExtensionManagerPort, it doesn't have session_id in methods.
        // This means GIAP currently assumes a single session or the manager is per-session.
        let extension_manager = Arc::new(GiapGooseExtensionManager::new(agent.clone(), "default".to_string()));

        Ok(Self {
            agent,
            session_manager,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            memory_repo,
            llamafile_url,
            data_dir,
            extension_manager,
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
            None,
        ).await
    }

    /// Returns an `GiapGooseExtensionManager` for managing Goose extensions on a session.
    pub fn extension_manager(&self) -> Arc<GiapGooseExtensionManager> {
        self.extension_manager.clone()
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
                if let Some(ref dd) = self.data_dir {
                    Self::register_gguf_model(&model_name, dd);
                }
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                match goose::providers::local_inference::LocalInferenceProvider::from_env(cfg, vec![]).await {
                    Ok(p) => {
                        tracing::info!("Built LocalInferenceProvider for model '{}'", model_name);
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        tracing::warn!("Failed to build local inference provider for '{}': {e}", model_name);
                        None
                    }
                }
            }
            // llamafile uses the Ollama wire protocol over HTTP.
            "llamafile" => {
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
                        tracing::warn!("Failed to build llamafile provider: {e}");
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

    /// Register a GGUF model in Goose's global `local_model_registry` so that
    /// `LocalInferenceProvider` can find the file at `$data_dir/models/gguf/`.
    ///
    /// Handles two formats:
    /// - Bare stem: `"qwen2.5-3b-instruct-q4_k_m"` → looks for `{stem}.gguf`
    /// - Raw filename: `"model.gguf"` → uses as-is
    ///
    /// Idempotent: skips registration if the model is already known.
    fn register_gguf_model(model_name: &str, data_dir: &std::path::Path) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, LocalModelEntry, ModelSettings,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // Derive filename and registry key from the model name
        let (stem, filename) = if model_name.ends_with(".gguf") {
            let s = model_name.trim_end_matches(".gguf").to_string();
            (s, model_name.to_string())
        } else {
            (model_name.to_string(), format!("{}.gguf", model_name))
        };

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
                if !registry.has_model(&stem) {
                    let entry = LocalModelEntry {
                        id:           stem.clone(),
                        repo_id:      format!("local/{}", stem),
                        filename:     filename.clone(),
                        quantization: String::new(),
                        local_path,
                        source_url:   String::new(),
                        settings:     ModelSettings::default(),
                        size_bytes:   0,
                    };
                    match registry.add_model(entry) {
                        Ok(_) => tracing::info!("Registered GGUF model '{}' in local registry", stem),
                        Err(e) => tracing::warn!("Could not register GGUF model '{}': {}", stem, e),
                    }
                }
            }
            Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
        }
    }

    pub async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();

        // Goose maintains its own sessions.db with auto-generated IDs.
        let goose_sid = self.resolve_goose_session(&session_id).await;

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
        if let Err(e) = self.ensure_provider_current(&settings, &goose_sid).await {
            tracing::warn!("Provider update failed (continuing with current provider): {e}");
        }

        // ── 6. Auto-load "giap" builtin extension ─────────────────────────────
        let needs_extension_load = {
            let loaded = self.loaded_extensions.lock().unwrap();
            !loaded.contains(&goose_sid)
        };
        if needs_extension_load {
            self.add_builtin_extension("giap", &goose_sid).await.ok();
            self.loaded_extensions.lock().unwrap().insert(goose_sid.clone());
        }

        // ── 7. GooseMode from model_role ──────────────────────────────────────
        let goose_mode = match model_role.as_str() {
            "task" => GooseMode::Auto,
            _ => GooseMode::Chat,
        };
        self.agent
            .update_goose_mode(goose_mode, &goose_sid)
            .await
            .ok();

        // ── 8. Run the agentic loop ───────────────────────────────────────────
        let user_msg = Message::user().with_text(&request.message);
        let session_cfg = goose::agents::types::SessionConfig {
            id: goose_sid.clone(),
            schedule_id: None,
            max_turns: Some(settings.agent_max_turns as u32),
            retry_config: None,
        };

        let agent_clone = self.agent.clone();

        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::Status { content: "Agent working...".to_string() });

            let mut goose_stream = match agent_clone.reply(user_msg, session_cfg, None).await {
                Ok(s) => s,
                Err(e) => {
                    yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                    return;
                }
            };

            while let Some(event_result) = goose_stream.next().await {
                match event_result {
                    Ok(event) => match event {
                        goose::agents::AgentEvent::Message(msg) => {
                            // Emit tool call and result events
                            for content in &msg.content {
                                match content {
                                    goose::conversation::message::MessageContent::ToolRequest(tr) => {
                                        if let Ok(tool_call) = &tr.tool_call {
                                            yield Ok(AgentStreamEvent::ToolCall {
                                                id: tr.id.clone(),
                                                tool: tool_call.name.to_string(),
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

                                            yield Ok(AgentStreamEvent::ToolResult {
                                                id: tr.id.clone(),
                                                tool: String::new(), // Goose ToolResponse doesn't store tool name directly in new version
                                                content: content_text,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            // Emit text
                            let text = msg.as_concat_text();
                            if !text.is_empty() {
                                yield Ok(AgentStreamEvent::Text { content: text });
                            }
                        }
                        goose::agents::AgentEvent::HistoryReplaced(_) => {
                            yield Ok(AgentStreamEvent::Status { content: "Compacting context...".to_string() });
                        }
                        _ => {}
                    },
                    Err(e) => {
                        yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                    }
                }
            }
            yield Ok(AgentStreamEvent::Done { session_id, model_role });
        };

        Ok(Box::pin(stream))
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires llamafile at http://127.0.0.1:8080"]
    async fn test_goose_adapter_chat_stream_live() {
        let adapter = GooseAdapter::with_llamafile(None).await.unwrap();
        let request = AgentRequest {
            message: "Say hello and nothing else".to_string(),
            session_id: "test-session".to_string(),
            model_role: "chat".to_string(),
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
