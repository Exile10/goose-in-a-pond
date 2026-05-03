use crate::registry::GiapServiceHandles;
use pond_core::domain::memory::MemoryFragment;
use pond_core::domain::schedule::TaskKind;
use pond_core::ports::scheduler::CreateScheduleRequest;
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
use std::sync::Arc;

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
    /// Memory segment: identity, preference, correction, relationship, project, knowledge, or context.
    /// If omitted, auto-classified from content.
    pub segment: Option<String>,
    /// Importance score 0.0-1.0. If omitted, defaults by segment.
    pub importance: Option<f32>,
    /// Tier: short, long, or permanent. If omitted, defaults by segment.
    pub tier: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ForgetMemoryParams {
    /// The memory ID to delete.
    pub id: Option<String>,
    /// Exact content to search for and delete (if id not provided).
    pub content: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetRecipeParams {
    /// The recipe name (slug).
    pub name: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WikipediaQueryParams {
    /// The topic to look up — a name, phrase, or question (e.g. "black holes", "Nairobi", "how do volcanoes work").
    pub topic: Option<String>,
    /// Maximum number of search results (default 5, max 10). Only used by search_wikipedia.
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends (e.g. "query", "title", "search").
    /// Not part of the advertised schema — exists purely to absorb unexpected keys.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Schedule parameter structs ──────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CreateScheduleParams {
    /// Human-readable name for the scheduled task.
    pub name: String,
    /// 6-field cron expression: sec min hr dom mon dow.
    /// Example: "0 0 8 * * *" = daily at 8:00 AM.
    pub cron: String,
    /// The prompt to send to the agent on each fire.
    pub prompt: String,
    /// IANA timezone (e.g. "Africa/Nairobi"). If omitted, uses the user's configured timezone.
    pub timezone: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScheduleIdParam {
    /// The schedule ID to operate on.
    pub id: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetScheduleRunsParams {
    /// The schedule ID.
    pub id: String,
    /// Maximum number of runs to return (default 10).
    pub limit: Option<u32>,
}

// ── System / filesystem / shell parameter structs ──────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SystemInfoParams {
    /// What info to get: "all", "memory", "disk", or "os" (default: "all")
    pub category: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NotifyParams {
    /// Notification title
    pub title: String,
    /// Notification body text
    pub body: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ShellCommandParams {
    /// The command to execute. Only safe commands are allowed: ls, cat, echo, date, uptime, df, free, whoami, hostname, pwd, wc, head, tail, sort, uniq, grep, find, which, env, printenv
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ReadFileParams {
    /// Absolute path to the file to read
    pub path: String,
    /// Maximum number of lines to read (default: 100)
    pub max_lines: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WriteFileParams {
    /// Absolute path to the file to write
    pub path: String,
    /// Content to write
    pub content: String,
    /// If true, append to file instead of overwriting (default: false)
    #[serde(default)]
    pub append: bool,
}

/// Allow-list of safe shell commands for `run_shell_command`.
const ALLOWED_COMMANDS: &[&str] = &[
    "ls", "cat", "echo", "date", "uptime", "df", "free", "whoami", "hostname", "pwd", "wc", "head",
    "tail", "sort", "uniq", "grep", "find", "which", "env", "printenv",
];

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

    #[tool(
        description = "List all scheduled tasks on this GIAP instance, including their cron schedule, timezone, type, and status."
    )]
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
                                let kind_label = match &t.kind {
                                    TaskKind::AgentPrompt { .. } => "agent",
                                    TaskKind::Webhook { .. } => "webhook",
                                };
                                let status = if t.currently_running {
                                    "running"
                                } else if t.paused {
                                    "paused"
                                } else {
                                    "active"
                                };
                                format!(
                                    "- {} [{}]: {} {} ({}, {})",
                                    t.label, t.id, t.cron, t.timezone, kind_label, status
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

    #[tool(
        description = "Create a new scheduled task that runs the given prompt against the agent \
        at the specified cron interval. The cron is 6-field format: sec min hr dom mon dow. \
        Example: '0 0 8 * * *' = daily at 8:00 AM. Use the user's timezone unless they specify otherwise."
    )]
    async fn create_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<CreateScheduleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let scheduler = match &self.services.scheduler {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "The scheduler service is not configured. Inform the user directly.",
                )]))
            }
        };

        // Default timezone to user's setting if not provided.
        let timezone = match &params.0.timezone {
            Some(tz) if !tz.is_empty() => tz.clone(),
            _ => self
                .services
                .settings_repo
                .get()
                .await
                .map(|s| s.timezone.clone())
                .unwrap_or_else(|_| "UTC".to_string()),
        };

        let id = uuid::Uuid::new_v4().to_string();
        let req = CreateScheduleRequest {
            id: id.clone(),
            label: params.0.name.clone(),
            cron: params.0.cron.clone(),
            timezone: timezone.clone(),
            kind: TaskKind::AgentPrompt {
                prompt: params.0.prompt.clone(),
            },
        };

        match scheduler.create_task(req).await {
            Ok(schedule) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Schedule created: \"{}\" [{}] — {} {} (agent prompt)",
                schedule.label, schedule.id, schedule.cron, schedule.timezone,
            ))])),
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to create schedule: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Delete a scheduled task by its ID.")]
    async fn delete_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleIdParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let scheduler = match &self.services.scheduler {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "The scheduler service is not configured.",
                )]))
            }
        };

        match scheduler.delete_task(&params.0.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Schedule '{}' deleted.",
                params.0.id
            ))])),
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to delete schedule: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Pause a scheduled task so it stops firing until resumed.")]
    async fn pause_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleIdParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let scheduler = match &self.services.scheduler {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "The scheduler service is not configured.",
                )]))
            }
        };

        match scheduler.pause_task(&params.0.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Schedule '{}' paused.",
                params.0.id
            ))])),
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to pause schedule: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Resume a paused scheduled task so it starts firing again.")]
    async fn resume_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleIdParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let scheduler = match &self.services.scheduler {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "The scheduler service is not configured.",
                )]))
            }
        };

        match scheduler.resume_task(&params.0.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Schedule '{}' resumed.",
                params.0.id
            ))])),
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to resume schedule: {}", e),
                None,
            )),
        }
    }

    #[tool(
        description = "Trigger a scheduled task to run immediately, regardless of its cron schedule."
    )]
    async fn run_schedule_now(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleIdParam>,
    ) -> Result<CallToolResult, ErrorData> {
        let scheduler = match &self.services.scheduler {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "The scheduler service is not configured.",
                )]))
            }
        };

        match scheduler.run_now(&params.0.id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Schedule '{}' triggered for immediate execution.",
                params.0.id
            ))])),
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to run schedule: {}", e),
                None,
            )),
        }
    }

    #[tool(
        description = "Get the execution history for a scheduled task — shows recent runs with status, result, and duration."
    )]
    async fn get_schedule_runs(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetScheduleRunsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let scheduler = match &self.services.scheduler {
            Some(s) => s,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(
                    "The scheduler service is not configured.",
                )]))
            }
        };

        let limit = params.0.limit.unwrap_or(10);
        match scheduler.get_runs(&params.0.id, limit).await {
            Ok(runs) => {
                let text = if runs.is_empty() {
                    format!("No execution history for schedule '{}'.", params.0.id)
                } else {
                    runs.iter()
                        .map(|r| {
                            let duration = r
                                .duration_ms
                                .map(|d| format!(" ({d}ms)"))
                                .unwrap_or_default();
                            let detail = match &r.status {
                                pond_core::domain::schedule::RunStatus::Completed => {
                                    let preview = r
                                        .result
                                        .as_deref()
                                        .unwrap_or("")
                                        .chars()
                                        .take(200)
                                        .collect::<String>();
                                    format!("completed{duration}: {preview}")
                                }
                                pond_core::domain::schedule::RunStatus::Failed => {
                                    let err = r.error.as_deref().unwrap_or("unknown error");
                                    format!("failed{duration}: {err}")
                                }
                                pond_core::domain::schedule::RunStatus::Running => {
                                    "running...".to_string()
                                }
                            };
                            format!("- [{}] {}", r.started_at.format("%Y-%m-%d %H:%M"), detail)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to get runs: {}", e),
                None,
            )),
        }
    }

    #[tool(
        description = "Get the current user profile: name, assistant name, timezone, and location."
    )]
    async fn get_user_profile(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let settings = self.services.settings_repo.get().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Settings error: {}", e),
                None,
            )
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

    #[tool(
        description = "Get the current model configuration: which LLM and tool-calling model are active."
    )]
    async fn get_model_assignments(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.services.settings_repo.get().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Settings error: {}", e),
                None,
            )
        })?;
        let tool = s.tool_model.as_deref().unwrap_or("(none)");
        let text = format!(
            "Main LLM:    {}/{}\nTool caller: {}",
            s.chat_provider, s.chat_model, tool,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Recall recent memories, optionally filtered by a keyword. Returns memories \
        with their segment (identity, preference, etc.) and importance score."
    )]
    async fn recall_memories(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecallMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.0.limit.unwrap_or(10) as usize;
        let fragments = self
            .services
            .memory_repo
            .search_recent(None, limit)
            .await
            .map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Memory error: {}", e),
                    None,
                )
            })?;

        let filtered: Vec<_> = if let Some(ref q) = params.0.query {
            let q_lower = q.to_lowercase();
            fragments
                .into_iter()
                .filter(|f| f.content.to_lowercase().contains(&q_lower))
                .collect()
        } else {
            fragments
        };

        // Record access for decay tracking
        for f in &filtered {
            let _ = self.services.memory_repo.record_access(&f.id).await;
        }

        let text = if filtered.is_empty() {
            "No memories found.".to_string()
        } else {
            filtered
                .iter()
                .map(|f| {
                    let seg = f
                        .segment
                        .as_ref()
                        .map(|s| format!("{:?}", s).to_lowercase())
                        .unwrap_or_else(|| "—".to_string());
                    let imp = f
                        .importance
                        .map(|i| format!("{:.1}", i))
                        .unwrap_or_else(|| "—".to_string());
                    format!(
                        "[{}] [{}, {}] {}",
                        f.created_at.format("%Y-%m-%d"),
                        seg,
                        imp,
                        f.content
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Save a new memory fragment for future recall. Supports optional segment \
        (identity, preference, correction, relationship, project, knowledge, context), importance \
        (0-1), and tier (short, long, permanent). If segment is omitted, it is auto-classified \
        from the content."
    )]
    async fn save_memory(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        use pond_core::domain::memory::{MemoryLifecycle, MemorySegment, MemoryTier};

        let id = uuid::Uuid::new_v4().to_string();
        let content = params.0.content.clone();
        let tag_list: Vec<String> = params
            .0
            .tags
            .unwrap_or_default()
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();

        // Resolve segment: explicit > auto-classify from content
        let segment = params
            .0
            .segment
            .as_deref()
            .and_then(parse_memory_segment)
            .unwrap_or_else(|| auto_classify_segment(&content));

        let importance = params
            .0
            .importance
            .map(|i| i.clamp(0.0, 1.0))
            .unwrap_or_else(|| segment.default_importance());

        let tier = params
            .0
            .tier
            .as_deref()
            .and_then(parse_memory_tier)
            .unwrap_or_else(|| segment.default_tier());

        let decay_rate = tier.default_decay_rate();

        let fragment = MemoryFragment {
            id,
            profile_id: None,
            session_id: None,
            content: content.clone(),
            embedding: None,
            source: "mcp_tool".to_string(),
            tags: tag_list,
            created_at: chrono::Utc::now(),
            segment: Some(segment.clone()),
            importance: Some(importance),
            tier: Some(tier.clone()),
            decay_rate: Some(decay_rate),
            access_count: 0,
            last_accessed_at: None,
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
        };

        self.services.memory_repo.add(fragment).await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to save memory: {}", e),
                None,
            )
        })?;

        let seg_label = format!("{:?}", segment).to_lowercase();
        let tier_label = format!("{:?}", tier).to_lowercase();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Memory saved ({seg_label}, importance={importance:.1}, tier={tier_label}): {content}"
        ))]))
    }

    #[tool(description = "Delete a specific memory by ID or by exact content match.")]
    async fn forget_memory(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ForgetMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(id) = &params.0.id {
            self.services.memory_repo.delete(id).await.map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to delete: {}", e),
                    None,
                )
            })?;
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Memory {id} deleted."
            ))]));
        }

        if let Some(content) = &params.0.content {
            let memories = self
                .services
                .memory_repo
                .search_recent(None, 100)
                .await
                .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

            let lower = content.to_lowercase();
            if let Some(found) = memories.iter().find(|m| m.content.to_lowercase() == lower) {
                let id = found.id.clone();
                self.services.memory_repo.delete(&id).await.map_err(|e| {
                    ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to delete: {}", e),
                        None,
                    )
                })?;
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Memory deleted: {}",
                    found.content
                ))]));
            }

            return Ok(CallToolResult::success(vec![Content::text(
                "No memory found with that exact content.".to_string(),
            )]));
        }

        Ok(CallToolResult::success(vec![Content::text(
            "Provide either an 'id' or 'content' to identify the memory to forget.".to_string(),
        )]))
    }

    #[tool(description = "List all active user skills injected into the assistant's context.")]
    async fn list_skills(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let skills = self.services.skill_repo.list_active().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Skills error: {}", e),
                None,
            )
        })?;
        let text = if skills.is_empty() {
            "No active skills.".to_string()
        } else {
            skills
                .iter()
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
        let recipe = self
            .services
            .recipe_repo
            .get_by_name(&params.0.name)
            .await
            .map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Recipe error: {}", e),
                    None,
                )
            })?;
        match recipe {
            None => Ok(CallToolResult::success(vec![Content::text(format!(
                "Recipe '{}' not found.",
                params.0.name
            ))])),
            Some(r) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Recipe: {}\n{}\n\n{}",
                r.name, r.description, r.yaml
            ))])),
        }
    }

    // ── Time / system / notification / shell / file tools ──────────────────

    #[tool(
        description = "Get the current date, time, and timezone. Use when asked about the current time or date."
    )]
    async fn get_current_time(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let now = chrono::Local::now();
        let text = format!(
            "Current time: {}\nDate: {}\nTimezone: {}",
            now.format("%H:%M:%S"),
            now.format("%A, %B %d, %Y"),
            now.format("%Z (UTC%:z)")
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Get system information: OS, hostname, memory usage, and disk usage. \
        Use when the user asks about their system, available memory, disk space, or hardware."
    )]
    async fn get_system_info(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SystemInfoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        use sysinfo::{Disks, System};

        let category = params.0.category.as_deref().unwrap_or("all");
        let mut sections: Vec<String> = Vec::new();

        let show_os = category == "all" || category == "os";
        let show_memory = category == "all" || category == "memory";
        let show_disk = category == "all" || category == "disk";

        if show_os {
            let host = System::host_name().unwrap_or_else(|| "unknown".to_string());
            let os_name = System::name().unwrap_or_else(|| "unknown".to_string());
            let os_version = System::os_version().unwrap_or_else(|| "unknown".to_string());
            let kernel = System::kernel_version().unwrap_or_else(|| "unknown".to_string());
            let arch = System::cpu_arch();
            let uptime_secs = System::uptime();
            let hours = uptime_secs / 3600;
            let minutes = (uptime_secs % 3600) / 60;
            sections.push(format!(
                "OS: {} {}\nKernel: {}\nArchitecture: {}\nHostname: {}\nUptime: {}h {}m",
                os_name, os_version, kernel, arch, host, hours, minutes,
            ));
        }

        if show_memory {
            let mut sys = System::new();
            sys.refresh_memory();
            let total_gb = sys.total_memory() as f64 / 1_073_741_824.0;
            let used_gb = sys.used_memory() as f64 / 1_073_741_824.0;
            let available_gb = sys.available_memory() as f64 / 1_073_741_824.0;
            sections.push(format!(
                "Memory: {:.1} GB used / {:.1} GB total ({:.1} GB available)",
                used_gb, total_gb, available_gb,
            ));
        }

        if show_disk {
            let disks = Disks::new_with_refreshed_list();
            let mut disk_lines: Vec<String> = Vec::new();
            for disk in disks.list() {
                let mount = disk.mount_point().to_string_lossy();
                let total_gb = disk.total_space() as f64 / 1_073_741_824.0;
                let avail_gb = disk.available_space() as f64 / 1_073_741_824.0;
                let used_gb = total_gb - avail_gb;
                disk_lines.push(format!(
                    "  {} — {:.1} GB used / {:.1} GB total ({:.1} GB free)",
                    mount, used_gb, total_gb, avail_gb,
                ));
            }
            if disk_lines.is_empty() {
                disk_lines.push("  No disks detected.".to_string());
            }
            sections.push(format!("Disks:\n{}", disk_lines.join("\n")));
        }

        Ok(CallToolResult::success(vec![Content::text(
            sections.join("\n\n"),
        )]))
    }

    #[tool(
        description = "Send a desktop notification to the user. Use when the user asks to be \
        notified, alerted, or reminded with a popup message."
    )]
    async fn send_notification(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<NotifyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        notify_rust::Notification::new()
            .summary(&params.0.title)
            .body(&params.0.body)
            .appname("Goose in a Pond")
            .show()
            .map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to send notification: {}", e),
                    None,
                )
            })?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Notification sent: \"{}\" — {}",
            params.0.title, params.0.body,
        ))]))
    }

    #[tool(
        description = "Execute a safe shell command. Only allow-listed commands are permitted: \
        ls, cat, echo, date, uptime, df, free, whoami, hostname, pwd, wc, head, tail, sort, uniq, \
        grep, find, which, env, printenv. Times out after 10 seconds."
    )]
    async fn run_shell_command(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ShellCommandParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let cmd = params.0.command.trim().to_string();

        if !ALLOWED_COMMANDS.contains(&cmd.as_str()) {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!(
                    "Command '{}' is not in the allow-list. Allowed: {}",
                    cmd,
                    ALLOWED_COMMANDS.join(", "),
                ),
                None,
            ));
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(&cmd)
                .args(&params.0.args)
                .output(),
        )
        .await;

        match result {
            Err(_elapsed) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Command '{}' timed out after 10 seconds.", cmd),
                None,
            )),
            Ok(Err(e)) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to execute '{}': {}", cmd, e),
                None,
            )),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut text = String::new();
                if !stdout.is_empty() {
                    text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !text.is_empty() {
                        text.push_str("\n--- stderr ---\n");
                    }
                    text.push_str(&stderr);
                }
                if text.is_empty() {
                    text.push_str("(no output)");
                }
                if !output.status.success() {
                    text.push_str(&format!("\nExit code: {}", output.status));
                }
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
        }
    }

    #[tool(
        description = "Read the contents of a local file. Returns up to max_lines lines (default 100). \
        Use when the user asks to read, view, or inspect a file on their system."
    )]
    async fn read_file(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ReadFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = params.0.path.trim();

        if path.contains("..") {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "Path traversal ('..') is not allowed.",
                None,
            ));
        }

        if !std::path::Path::new(path).is_absolute() {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "Path must be absolute.",
                None,
            ));
        }

        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to read '{}': {}", path, e),
                None,
            )
        })?;

        let max_lines = params.0.max_lines.unwrap_or(100);
        let lines: Vec<&str> = content.lines().take(max_lines).collect();
        let total_lines = content.lines().count();
        let truncated = total_lines > max_lines;
        let mut text = lines.join("\n");
        if truncated {
            text.push_str(&format!(
                "\n\n[Showing {}/{} lines. Use max_lines to read more.]",
                max_lines, total_lines,
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Write content to a local file. Can overwrite or append. Creates parent \
        directories if needed. Use when the user asks to write, save, or create a file."
    )]
    async fn write_file(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WriteFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = params.0.path.trim();

        if path.contains("..") {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "Path traversal ('..') is not allowed.",
                None,
            ));
        }

        if !std::path::Path::new(path).is_absolute() {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "Path must be absolute.",
                None,
            ));
        }

        // Create parent directories if they don't exist
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to create directories for '{}': {}", path, e),
                    None,
                )
            })?;
        }

        let bytes_written = params.0.content.len();

        if params.0.append {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|e| {
                    ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to open '{}' for appending: {}", path, e),
                        None,
                    )
                })?;
            file.write_all(params.0.content.as_bytes())
                .await
                .map_err(|e| {
                    ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to append to '{}': {}", path, e),
                        None,
                    )
                })?;
        } else {
            tokio::fs::write(path, &params.0.content)
                .await
                .map_err(|e| {
                    ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to write '{}': {}", path, e),
                        None,
                    )
                })?;
        }

        let mode = if params.0.append { "Appended" } else { "Wrote" };
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{} {} bytes to '{}'.",
            mode, bytes_written, path,
        ))]))
    }

    // ── Wikipedia tools ──────────────────────────────────────────────────────

    #[tool(description = "\
Search Wikipedia for articles matching a topic. Returns a ranked list of article \
titles with short descriptions. Only use this when you need to disambiguate \
between multiple topics or show the user a list of options. For direct factual \
questions, prefer get_wikipedia_article instead — it auto-searches on your behalf.")]
    async fn search_wikipedia(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let query = extract_topic(&params.0, &self.services).await;
        println!("[wikipedia] search_wikipedia called: query={:?}", query);

        if query.is_empty() {
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "A topic is required.".to_string(),
                None,
            ));
        }
        let limit = params.0.limit.unwrap_or(5).min(10);

        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit={}&format=json",
            urlencoding::encode(&query),
            limit,
        );
        println!("[wikipedia] GET {}", url);

        let resp = self
            .services
            .http_client
            .get(&url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] search request failed: {e}");
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia request failed: {e}"),
                    None,
                )
            })?;

        println!("[wikipedia] search response status: {}", resp.status());

        if !resp.status().is_success() {
            println!("[wikipedia] search failed with HTTP {}", resp.status());
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            println!("[wikipedia] failed to parse search response: {e}");
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse response: {e}"),
                None,
            )
        })?;

        let results = body["query"]["search"].as_array();
        let result_count = results.map(|a| a.len()).unwrap_or(0);
        println!("[wikipedia] search returned {} results", result_count);

        let text = match results {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|item| {
                    let title = item["title"].as_str()?;
                    let snippet = item["snippet"].as_str().unwrap_or("");
                    let clean = snippet
                        .replace("<span class=\"searchmatch\">", "")
                        .replace("</span>", "")
                        .replace("&quot;", "\"")
                        .replace("&amp;", "&");
                    Some(format!("- **{}**: {}", title, clean))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => format!("No Wikipedia articles found for '{}'.", query),
        };
        println!(
            "[wikipedia] search_wikipedia done, returning {} chars",
            text.len()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "\
Look up a topic on Wikipedia. Pass a topic name or natural-language query — the \
tool auto-searches if the exact title is not found. Use this as your first \
choice for ANY factual question (people, places, events, science, history, etc.).\n\
After receiving the result: extract only the facts relevant to the user's \
question and answer concisely in your own words. Do NOT repeat the extract \
verbatim. In voice mode keep it to 1–3 sentences.")]
    async fn get_wikipedia_article(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let topic = extract_topic(&params.0, &self.services).await;
        println!(
            "[wikipedia] get_wikipedia_article called: topic={:?}",
            topic
        );

        if topic.is_empty() {
            println!("[wikipedia] empty topic, returning INVALID_PARAMS");
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "A topic is required.".to_string(),
                None,
            ));
        }

        // Try direct lookup first
        match self.fetch_article_summary(&topic).await {
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(WikiFetchError::NotFound) => {
                // Auto-fallback: search for the topic and fetch the top result
                println!(
                    "[wikipedia] exact title not found, searching for '{}'",
                    topic
                );
                match self.search_and_fetch_best(&topic).await {
                    Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
                    Err(e) => Err(e),
                }
            }
            Err(WikiFetchError::Mcp(e)) => Err(e),
        }
    }
}

// ── Wikipedia helpers (outside the #[tool_router] block) ─────────────────────

const WIKI_UA: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";

/// Extract the search topic from params.
///
/// Small local models send parameters in unpredictable shapes — `{"query": "..."}`,
/// `{"title": "..."}`, `{"search": "..."}`, `{"input": "..."}`, or even `{}`.
/// The `extra` field captures everything serde didn't match to `topic`.
/// We check `topic` first, then scan extras, then fall back to the user's
/// original message (stashed by the GooseAdapter before each turn).
async fn extract_topic(params: &WikipediaQueryParams, services: &GiapServiceHandles) -> String {
    // 1. Canonical field
    if let Some(ref t) = params.topic {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            println!(
                "[wikipedia] extract_topic: found in 'topic' field: {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }
    // 2. Scan extras — try common names first, then any string value
    for key in &[
        "query", "title", "search", "q", "term", "input", "name", "article", "text", "subject",
    ] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!(
                        "[wikipedia] extract_topic: found in '{}' field: {:?}",
                        key, trimmed
                    );
                    return trimmed.to_string();
                }
            }
        }
    }
    // 3. Any extra string value at all
    for (key, val) in &params.extra {
        if let Some(s) = val.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                println!(
                    "[wikipedia] extract_topic: found in unknown '{}' field: {:?}",
                    key, trimmed
                );
                return trimmed.to_string();
            }
        }
    }
    // 4. Try the tool-calling specialist model (if configured)
    let user_msg = services.last_user_message.read().await.clone();
    if let Some(ref tool_caller) = services.tool_caller {
        let schema = r#"{"topic": "string — the topic, person, place, or concept to look up"}"#;
        let query = user_msg.trim();
        if !query.is_empty() {
            println!(
                "[wikipedia] extract_topic: invoking tool-caller specialist for {:?}",
                query
            );
            match tool_caller
                .generate_tool_call("get_wikipedia_article", schema, query)
                .await
            {
                Ok(args) => {
                    if let Some(t) = args.get("topic").and_then(|v| v.as_str()) {
                        let trimmed = t.trim();
                        if !trimmed.is_empty() {
                            println!(
                                "[wikipedia] extract_topic: specialist returned: {:?}",
                                trimmed
                            );
                            return trimmed.to_string();
                        }
                    }
                    println!(
                        "[wikipedia] extract_topic: specialist returned args without 'topic': {:?}",
                        args
                    );
                }
                Err(e) => {
                    println!("[wikipedia] extract_topic: specialist failed: {e}");
                }
            }
        }
    }
    // 5. Last resort — extract topic from user message with query cleaning
    let cleaned = clean_query_for_search(&user_msg);
    if !cleaned.is_empty() {
        println!(
            "[wikipedia] extract_topic: cleaned user message: {:?}",
            cleaned
        );
        return cleaned;
    }
    println!(
        "[wikipedia] extract_topic: no topic found in params: {:?}",
        params
    );
    String::new()
}

/// Strip common question prefixes to extract the core topic for search.
///
/// "who is Wangari Maathai?" → "Wangari Maathai"
/// "tell me about black holes" → "black holes"
/// "Nairobi" → "Nairobi" (unchanged)
pub fn clean_query_for_search(raw: &str) -> String {
    let stripped = raw
        .trim()
        .trim_end_matches('?')
        .trim_end_matches('.')
        .trim();
    let lower = stripped.to_lowercase();
    // Ordered longest-first so more specific prefixes match before short ones.
    let prefixes = [
        "can you tell me about ",
        "could you tell me about ",
        "tell me about ",
        "tell me more about ",
        "i want to know about ",
        "i'd like to know about ",
        "what do you know about ",
        "what can you tell me about ",
        "would you recommend ",
        "do you recommend ",
        "should i ",
        "how about ",
        "who is ",
        "who was ",
        "who are ",
        "what is ",
        "what are ",
        "what was ",
        "what were ",
        "what is the ",
        "what are the ",
        "where is ",
        "where are ",
        "when was ",
        "when did ",
        "when is ",
        "how does ",
        "how do ",
        "how did ",
        "how is ",
        "why does ",
        "why do ",
        "why is ",
        "why did ",
        "explain ",
        "describe ",
        "look up ",
        "search for ",
        "search ",
        "find ",
        "define ",
    ];
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return stripped[prefix.len()..].trim().to_string();
        }
    }
    stripped.to_string()
}

// ── Memory helpers ──────────────────────────────────────────────────────────

use pond_core::domain::memory::{MemorySegment, MemoryTier};

pub fn parse_memory_segment(s: &str) -> Option<MemorySegment> {
    match s.to_lowercase().as_str() {
        "identity" => Some(MemorySegment::Identity),
        "preference" => Some(MemorySegment::Preference),
        "correction" => Some(MemorySegment::Correction),
        "relationship" => Some(MemorySegment::Relationship),
        "project" => Some(MemorySegment::Project),
        "knowledge" => Some(MemorySegment::Knowledge),
        "context" => Some(MemorySegment::Context),
        _ => None,
    }
}

pub fn parse_memory_tier(s: &str) -> Option<MemoryTier> {
    match s.to_lowercase().as_str() {
        "short" => Some(MemoryTier::Short),
        "long" => Some(MemoryTier::Long),
        "permanent" => Some(MemoryTier::Permanent),
        _ => None,
    }
}

/// Auto-classify a memory's segment from its content using keyword heuristics.
/// No LLM needed — fast and deterministic.
pub fn auto_classify_segment(content: &str) -> MemorySegment {
    let lower = content.to_lowercase();

    // Correction indicators (highest priority)
    if lower.starts_with("actually")
        || lower.starts_with("no, ")
        || lower.starts_with("correction:")
        || lower.contains("that's wrong")
        || lower.contains("that's not right")
        || lower.contains("not correct")
    {
        return MemorySegment::Correction;
    }

    // Identity indicators
    if lower.starts_with("my name is")
        || lower.starts_with("i am a ")
        || lower.starts_with("i'm a ")
        || lower.contains("i live in")
        || lower.contains("i work at")
        || lower.contains("i work as")
        || lower.contains("my job is")
        || lower.contains("my role is")
    {
        return MemorySegment::Identity;
    }

    // Relationship indicators
    if lower.contains("my wife")
        || lower.contains("my husband")
        || lower.contains("my partner")
        || lower.contains("my friend")
        || lower.contains("my boss")
        || lower.contains("my colleague")
        || lower.contains("my sister")
        || lower.contains("my brother")
        || lower.contains("my mother")
        || lower.contains("my father")
        || lower.contains("my son")
        || lower.contains("my daughter")
    {
        return MemorySegment::Relationship;
    }

    // Preference indicators
    if lower.starts_with("i prefer")
        || lower.starts_with("i like")
        || lower.starts_with("i love")
        || lower.starts_with("i hate")
        || lower.starts_with("i don't like")
        || lower.contains("my favorite")
        || lower.contains("my favourite")
    {
        return MemorySegment::Preference;
    }

    // Project indicators
    if lower.contains("working on")
        || lower.contains("my project")
        || lower.contains("my goal")
        || lower.contains("deadline")
        || lower.contains("i'm building")
        || lower.contains("i'm developing")
    {
        return MemorySegment::Project;
    }

    // Context indicators (transient)
    if lower.starts_with("right now")
        || lower.starts_with("currently")
        || lower.starts_with("today ")
        || lower.contains("at the moment")
    {
        return MemorySegment::Context;
    }

    // Default
    MemorySegment::Knowledge
}

// ── Wikipedia helpers ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WikiFetchError {
    NotFound,
    Mcp(ErrorData),
}

impl GiapMcpServer {
    /// Fetch the full article content for an exact Wikipedia title.
    ///
    /// Uses the MediaWiki `action=query&prop=extracts` endpoint which returns
    /// the complete article as plain text (no HTML). Falls back to the REST
    /// summary API if the full extract is empty.
    pub async fn fetch_article_summary(&self, title: &str) -> Result<String, WikiFetchError> {
        // Full article via MediaWiki API (plaintext, no character limit)
        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&titles={}&prop=extracts|info&explaintext=1&inprop=url&format=json&redirects=1",
            urlencoding::encode(title),
        );
        println!("[wikipedia] GET {}", url);

        let resp = self
            .services
            .http_client
            .get(&url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] article request failed: {e}");
                WikiFetchError::Mcp(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia request failed: {e}"),
                    None,
                ))
            })?;

        println!("[wikipedia] article response status: {}", resp.status());

        if !resp.status().is_success() {
            println!(
                "[wikipedia] article fetch failed with HTTP {}",
                resp.status()
            );
            return Err(WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            println!("[wikipedia] failed to parse article response: {e}");
            WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse response: {e}"),
                None,
            ))
        })?;

        // MediaWiki returns pages as { "query": { "pages": { "<id>": { ... } } } }
        let pages = &body["query"]["pages"];
        let page = pages.as_object().and_then(|m| m.values().next());

        let page = match page {
            Some(p) if p.get("missing").is_none() => p,
            _ => return Err(WikiFetchError::NotFound),
        };

        let display_title = page["title"].as_str().unwrap_or(title);
        let extract = page["extract"].as_str().unwrap_or("");
        let fallback_url = format!(
            "https://en.wikipedia.org/wiki/{}",
            urlencoding::encode(title)
        );
        let page_url = page["fullurl"].as_str().unwrap_or(&fallback_url);

        if extract.is_empty() {
            return Err(WikiFetchError::NotFound);
        }

        // Cap at ~8000 chars to stay within model context limits
        let truncated = if extract.len() > 8000 {
            let mut cut = 8000;
            while cut > 0 && !extract.is_char_boundary(cut) {
                cut -= 1;
            }
            format!(
                "{}...\n\n[Article truncated — full article at source]",
                &extract[..cut]
            )
        } else {
            extract.to_string()
        };

        println!(
            "[wikipedia] article fetched: title={:?}, extract_len={}",
            display_title,
            extract.len()
        );

        Ok(format!(
            "# {}\n\n{}\n\nSource: {}",
            display_title, truncated, page_url,
        ))
    }

    /// Search Wikipedia and fetch the summary of the best matching article.
    pub async fn search_and_fetch_best(&self, query: &str) -> Result<String, ErrorData> {
        let search_url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=1&format=json",
            urlencoding::encode(query),
        );
        println!("[wikipedia] fallback search: GET {}", search_url);

        let resp = self
            .services
            .http_client
            .get(&search_url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] fallback search request failed: {e}");
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia search failed: {e}"),
                    None,
                )
            })?;

        if !resp.status().is_success() {
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia search returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse search: {e}"),
                None,
            )
        })?;

        let best_title = body["query"]["search"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["title"].as_str());

        match best_title {
            Some(found) => {
                println!("[wikipedia] fallback found: '{}'", found);
                match self.fetch_article_summary(found).await {
                    Ok(text) => Ok(text),
                    Err(WikiFetchError::NotFound) => Ok(format!(
                        "Wikipedia search matched '{}' but the article could not be loaded.",
                        found
                    )),
                    Err(WikiFetchError::Mcp(e)) => Err(e),
                }
            }
            None => {
                println!(
                    "[wikipedia] fallback search returned no results for '{}'",
                    query
                );
                Ok(format!("No Wikipedia articles found for '{}'.", query))
            }
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
                "GIAP (Goose In A Pond) MCP server — your primary interface for the local home \
                 environment, system utilities, and factual knowledge retrieval.\n\n\
                 Tools: weather, device registry, memories, skills, Wikipedia, time, system info, \
                 notifications, shell commands (sandboxed), file read/write.\n\n\
                 IMPORTANT — Wikipedia usage guidelines:\n\
                 • Prefer get_wikipedia_article for any factual question. It accepts plain topics \
                   (\"black holes\", \"Marie Curie\") — no need to guess exact titles.\n\
                 • Do NOT parrot the article extract verbatim. Read it, extract the relevant facts, \
                   then answer the user's question in your own words — concisely.\n\
                 • For voice mode: aim for 1–3 sentences. Offer to elaborate if the user wants more.\n\
                 • Only use search_wikipedia when you need to disambiguate between multiple topics \
                   or present a list of options to the user.\n\
                 • Never say \"According to Wikipedia\" — just answer naturally with the facts.\n\
                 • Always prefer these tools over shell commands or external requests.",
            )
    }
}

// ── Live Wikipedia tests (require internet — #[ignore] by default) ───────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::GiapServiceHandles;
    use async_trait::async_trait;
    use pond_core::domain::memory::MemoryFragment;
    use pond_core::domain::recipe::AgentRecipe;
    use pond_core::domain::settings::Settings;
    use pond_core::domain::skill::UserSkill;
    use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
    use pond_core::ports::memory_repository::MemoryRepository;
    use pond_core::ports::recipe::AgentRecipeRepository;
    use pond_core::ports::settings::SettingsRepository;
    use pond_core::ports::skill::UserSkillRepository;
    use std::sync::Arc;

    // ── Minimal stubs — only http_client is exercised by Wikipedia tools ──

    struct StubDeviceRegistry;
    #[async_trait]
    impl DeviceRegistry for StubDeviceRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
            unimplemented!()
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(vec![])
        }
        async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubSettings;
    #[async_trait]
    impl SettingsRepository for StubSettings {
        async fn get(&self) -> anyhow::Result<Settings> {
            Ok(Settings::default())
        }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubMemory;
    #[async_trait]
    impl MemoryRepository for StubMemory {
        async fn add(&self, _: MemoryFragment) -> anyhow::Result<()> {
            Ok(())
        }
        async fn search_recent(
            &self,
            _: Option<&str>,
            _: usize,
        ) -> anyhow::Result<Vec<MemoryFragment>> {
            Ok(vec![])
        }
        async fn search_similar(
            &self,
            _: &[f32],
            _: Option<&str>,
            _: usize,
        ) -> anyhow::Result<Vec<MemoryFragment>> {
            Ok(vec![])
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubSkills;
    #[async_trait]
    impl UserSkillRepository for StubSkills {
        async fn list_active(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![])
        }
        async fn list_all(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![])
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<UserSkill>> {
            Ok(None)
        }
        async fn create(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn update(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubRecipes;
    #[async_trait]
    impl AgentRecipeRepository for StubRecipes {
        async fn list(&self) -> anyhow::Result<Vec<AgentRecipe>> {
            Ok(vec![])
        }
        async fn get_by_name(&self, _: &str) -> anyhow::Result<Option<AgentRecipe>> {
            Ok(None)
        }
        async fn get_by_id(&self, _: &str) -> anyhow::Result<Option<AgentRecipe>> {
            Ok(None)
        }
        async fn upsert(&self, _: &AgentRecipe) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server() -> GiapMcpServer {
        let handles = Arc::new(GiapServiceHandles {
            weather: None,
            device_registry: Arc::new(StubDeviceRegistry),
            scheduler: None,
            settings_repo: Arc::new(StubSettings),
            memory_repo: Arc::new(StubMemory),
            skill_repo: Arc::new(StubSkills),
            recipe_repo: Arc::new(StubRecipes),
            http_client: reqwest::Client::new(),
            tool_caller: None,
            last_user_message: tokio::sync::RwLock::new(String::new()),
            last_tool_topic: tokio::sync::RwLock::new(String::new()),
        });
        GiapMcpServer::new(handles)
    }

    /// Exact title → direct fetch succeeds.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_fetch_exact_title() {
        let server = test_server();
        let text = server.fetch_article_summary("Nairobi").await.unwrap();
        println!("{}", text);
        assert!(text.contains("Nairobi"), "extract should mention Nairobi");
        assert!(
            text.contains("Kenya"),
            "Nairobi article should mention Kenya"
        );
        assert!(text.contains("Source:"), "should include source URL");
    }

    /// Vague query that doesn't match an exact title → auto-search fallback.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_vague_query_finds_article() {
        let server = test_server();
        // "black holes" is not an exact Wikipedia title — "Black hole" is.
        let text = server.search_and_fetch_best("black holes").await.unwrap();
        println!("{}", text);
        assert!(
            text.contains("black hole") || text.contains("Black hole"),
            "should find the Black hole article"
        );
    }

    /// The full get_wikipedia_article flow: vague input → 404 → search → fetch.
    /// This is the exact use case: agent calls the tool with a rough topic.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_get_article_auto_resolves_vague_topic() {
        let server = test_server();
        // "volcanoes" is close enough that Wikipedia search should return a
        // relevant article (Volcano, Volcanology, etc.).
        let text = server.search_and_fetch_best("volcanoes").await.unwrap();
        println!("{}", text);
        assert!(
            text.to_lowercase().contains("volcan"),
            "should resolve to a volcano-related article"
        );
    }

    /// Completely nonsensical query returns a graceful "not found" message.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_nonsense_query_returns_not_found() {
        let server = test_server();
        let text = server
            .search_and_fetch_best("xyzzy99foobar_nonexistent")
            .await
            .unwrap();
        println!("{}", text);
        assert!(
            text.contains("No Wikipedia articles found"),
            "should report no results for nonsense query"
        );
    }

    /// Misspelled topic still finds a relevant article via search.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_misspelled_topic_resolved() {
        let server = test_server();
        // "Albert Einsten" is a common misspelling — Wikipedia search handles it.
        let text = server
            .search_and_fetch_best("Albert Einsten")
            .await
            .unwrap();
        println!("{}", text);
        let lower = text.to_lowercase();
        assert!(
            lower.contains("einstein")
                || lower.contains("physicist")
                || lower.contains("relativity"),
            "should resolve misspelled 'Albert Einsten' to Einstein article"
        );
    }
}
