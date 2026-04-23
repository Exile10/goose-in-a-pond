use std::sync::Arc;
use pond_core::domain::memory::MemoryFragment;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use crate::registry::GiapServiceHandles;

// ── Parameter structs for tools that accept arguments ────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecallMemoriesParams {
    /// Optional keyword to search for in memory content.
    pub query: Option<String>,
    /// Maximum number of memories to return (default 10).
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveMemoryParams {
    /// The content to remember.
    pub content: String,
    /// Optional comma-separated tags (e.g. "preferences,home").
    pub tags: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetRecipeParams {
    /// The recipe name (slug).
    pub name: String,
}

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct GiapMcpServer {
    services: Arc<GiapServiceHandles>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl GiapMcpServer {
    pub fn new(services: Arc<GiapServiceHandles>) -> Self {
        Self {
            services,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get the current weather conditions for the configured location.")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.services.weather {
            None => Ok(CallToolResult::success(vec![Content::text(
                "The weather service is not configured on this GIAP instance. \
                 Inform the user that they need to configure a weather location in their settings. \
                 DO NOT attempt to fetch weather using any other tool, shell command, or external request.",
            )])),
            Some(w) => match w.current().await {
                Ok(data) => Ok(CallToolResult::success(vec![Content::text(
                    data.as_context_block(),
                )])),
                Err(e) => Err(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Weather fetch error: {}", e),
                    None,
                )),
            },
        }
    }

    #[tool(description = "List all registered devices on this GIAP instance.")]
    async fn list_registered_devices(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.services.device_registry.list_devices().await {
            Ok(devices) => {
                let text = if devices.is_empty() {
                    "No devices registered.".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| {
                            format!(
                                "- {} ({}): {}",
                                d.name,
                                d.device_type,
                                if d.is_online { "online" } else { "offline" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing devices: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "List all scheduled tasks on this GIAP instance.")]
    async fn list_schedules(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.services.scheduler {
            None => Ok(CallToolResult::success(vec![Content::text(
                "The scheduler service is not configured on this GIAP instance. \
                 Inform the user directly. DO NOT attempt to use any other tool to create or list schedules.",
            )])),
            Some(s) => match s.list_tasks().await {
                Ok(tasks) => {
                    let text = if tasks.is_empty() {
                        "No scheduled tasks.".to_string()
                    } else {
                        tasks
                            .iter()
                            .map(|t| {
                                format!(
                                    "- {} [{}]: {} ({})",
                                    t.label,
                                    t.id,
                                    t.cron,
                                    if t.paused { "paused" } else { "active" }
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    Ok(CallToolResult::success(vec![Content::text(text)]))
                }
                Err(e) => Err(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Error listing schedules: {}", e),
                    None,
                )),
            },
        }
    }

    #[tool(description = "Get the current user profile: name, assistant name, timezone, and location.")]
    async fn get_user_profile(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let settings = self.services.settings_repo.get().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Settings error: {}", e), None)
        })?;
        let location = if settings.weather_location_name.is_empty() {
            "not configured".to_string()
        } else {
            settings.weather_location_name.clone()
        };
        let text = format!(
            "User: {}\nAssistant name: {}\nTimezone: {}\nLocation: {}",
            settings.user_name, settings.assistant_name, settings.timezone, location,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Get the current model role assignments: which provider and model handles Chat, Think, and Task requests.")]
    async fn get_model_assignments(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.services.settings_repo.get().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Settings error: {}", e), None)
        })?;
        let think_provider = s.think_provider.as_deref().unwrap_or(&s.chat_provider);
        let think_model    = s.think_model.as_deref().unwrap_or(&s.chat_model);
        let task_provider  = s.task_provider.as_deref().unwrap_or(&s.chat_provider);
        let task_model     = s.task_model.as_deref().unwrap_or(&s.chat_model);
        let text = format!(
            "Chat:  {}/{}\nThink: {}/{}\nTask:  {}/{}",
            s.chat_provider, s.chat_model,
            think_provider, think_model,
            task_provider, task_model,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Recall recent memories, optionally filtered by a keyword.")]
    async fn recall_memories(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecallMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.0.limit.unwrap_or(10) as usize;
        let fragments = self.services.memory_repo
            .search_recent(None, limit)
            .await
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Memory error: {}", e), None))?;

        let filtered: Vec<_> = if let Some(ref q) = params.0.query {
            let q_lower = q.to_lowercase();
            fragments.into_iter().filter(|f| f.content.to_lowercase().contains(&q_lower)).collect()
        } else {
            fragments
        };

        let text = if filtered.is_empty() {
            "No memories found.".to_string()
        } else {
            filtered.iter()
                .map(|f| format!("[{}] {}", f.created_at.format("%Y-%m-%d"), f.content))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Save a new memory fragment for future recall.")]
    async fn save_memory(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = uuid::Uuid::new_v4().to_string();
        let content = params.0.content.clone();
        let tag_list: Vec<String> = params.0.tags
            .unwrap_or_default()
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let fragment = MemoryFragment {
            id,
            profile_id: None,
            session_id: None,
            content: content.clone(),
            embedding: None,
            source: "mcp_tool".to_string(),
            tags: tag_list,
            created_at: chrono::Utc::now(),
        };
        self.services.memory_repo.add(fragment).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Failed to save memory: {}", e), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(format!("Memory saved: {}", content))]))
    }

    #[tool(description = "List all active user skills injected into the assistant's context.")]
    async fn list_skills(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let skills = self.services.skill_repo.list_active().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Skills error: {}", e), None)
        })?;
        let text = if skills.is_empty() {
            "No active skills.".to_string()
        } else {
            skills.iter()
                .map(|s| format!("- {} ({})", s.name, s.id))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Get the YAML definition of a named agent recipe.")]
    async fn get_recipe(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetRecipeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let recipe = self.services.recipe_repo.get_by_name(&params.0.name).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Recipe error: {}", e), None)
        })?;
        match recipe {
            None => Ok(CallToolResult::success(vec![Content::text(
                format!("Recipe '{}' not found.", params.0.name),
            )])),
            Some(r) => Ok(CallToolResult::success(vec![Content::text(
                format!("Recipe: {}\n{}\n\n{}", r.name, r.description, r.yaml),
            )])),
        }
    }
}

#[tool_handler]
impl ServerHandler for GiapMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-mcp-server",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP (Goose In A Pond) MCP server. This is your primary interface for interacting with the local home environment. \
                 It provides tools for fetching current weather, managing the smart home device registry, \
                 recalling and saving personal memories, and accessing assistant skills. \
                 Always prefer these tools for home-related tasks.",
            )
    }
}
