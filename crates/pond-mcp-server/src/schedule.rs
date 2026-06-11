//! Schedule MCP Server — manage cron-based scheduled tasks.
//!
//! Provides 9 tools: `list_schedules`, `create_schedule`, `update_schedule`,
//! `delete_schedule`, `pause_schedule`, `resume_schedule`, `run_schedule_now`,
//! `get_schedule_runs`, `world_clock`.
//! Depends on [`SchedulerPort`] and [`SettingsRepository`].

use pond_core::user_data::domain::schedule::TaskKind;
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use pond_core::user_data::ports::settings::SettingsRepository;
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

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CreateScheduleParams {
    /// Human-readable name for the scheduled task.
    #[serde(default)]
    pub name: String,
    /// 6-field cron expression: sec min hr dom mon dow.
    /// Example: "0 0 8 * * *" = daily at 8:00 AM.
    #[serde(default)]
    pub cron: String,
    /// The prompt to send to the agent on each fire.
    #[serde(default)]
    pub prompt: String,
    /// IANA timezone (e.g. "Africa/Nairobi"). If omitted, uses the user's configured timezone.
    pub timezone: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScheduleIdParam {
    /// The schedule ID to operate on.
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetScheduleRunsParams {
    /// The schedule ID.
    #[serde(default)]
    pub id: String,
    /// Maximum number of runs to return (default 10).
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UpdateScheduleParams {
    /// The schedule ID to update (required).
    #[serde(default)]
    pub id: String,
    /// New name (optional — only provided fields are updated).
    pub name: Option<String>,
    /// New 6-field cron expression (optional). You can also use natural language like "every morning at 9am".
    pub cron: Option<String>,
    /// New prompt/action (optional).
    pub prompt: Option<String>,
    /// New IANA timezone (optional).
    pub timezone: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WorldClockParams {
    /// One or more IANA timezone names (e.g. "America/New_York", "Asia/Tokyo", "Africa/Nairobi"). If omitted, returns the user's configured timezone.
    pub timezones: Option<Vec<String>>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ScheduleMcpServer {
    scheduler: Arc<dyn SchedulerPort>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ScheduleMcpServer {
    pub fn new(
        scheduler: Arc<dyn SchedulerPort>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self {
            scheduler,
            settings_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List all scheduled tasks on this GIAP instance, including their cron schedule, timezone, type, and status."
    )]
    async fn list_schedules(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.scheduler.list_tasks().await {
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
                // Build UI hint with structured schedule data
                let ui_schedules: Vec<serde_json::Value> = tasks
                    .iter()
                    .map(|t| {
                        let kind_label = match &t.kind {
                            TaskKind::AgentPrompt { .. } => "agent",
                            TaskKind::Webhook { .. } => "webhook",
                        };
                        let prompt_preview = match &t.kind {
                            TaskKind::AgentPrompt { prompt } => prompt.clone(),
                            TaskKind::Webhook { webhook_url } => webhook_url.clone(),
                        };
                        let status = if t.currently_running {
                            "running"
                        } else if t.paused {
                            "paused"
                        } else {
                            "active"
                        };
                        serde_json::json!({
                            "id": t.id,
                            "name": t.label,
                            "cron": t.cron,
                            "timezone": t.timezone,
                            "kind": kind_label,
                            "status": status,
                            "prompt": prompt_preview,
                        })
                    })
                    .collect();
                let ui_data = serde_json::json!({ "schedules": ui_schedules });
                let hint = format!("[[[mcp-ui:schedule:{}]]]\n", ui_data);
                let full_result = format!("{}{}", hint, text);
                Ok(CallToolResult::success(vec![Content::text(full_result)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing schedules: {}", e),
                None,
            )),
        }
    }

    #[tool(
        description = "Create a new scheduled task. Accepts natural language like 'every morning at 8am' \
        or 6-field cron: sec min hr dom mon dow. Example: '0 0 8 * * *' = daily at 8 AM."
    )]
    async fn create_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<CreateScheduleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        println!("[schedule] ╔═══ MCP SERVER RECEIVED ═══");
        println!("[schedule] ║ params.name:  {:?}", params.0.name);
        println!("[schedule] ║ params.cron:  {:?}", params.0.cron);
        println!("[schedule] ║ params.prompt: {:?}", params.0.prompt);
        println!("[schedule] ║ params.extra: {:?}", params.0.extra);
        println!("[schedule] ╚═══════════════════════════");

        let user_msg = crate::last_user_message();

        // ── ToolCaller PRIMARY: generate all params from user message ──
        const SCHEDULE_SCHEMA: &str = r#"{"type":"object","properties":{"cron":{"type":"string","description":"6-field cron: sec min hr dom mon dow. Example: 0 0 8 * * * for daily 8 AM"},"prompt":{"type":"string","description":"The action to perform on each fire"},"name":{"type":"string","description":"Short human-readable name"}},"required":["cron","prompt"]}"#;

        let (tc_cron, tc_prompt, tc_name) =
            if let Some(args) = crate::generate_params("create_schedule", SCHEDULE_SCHEMA).await {
                println!("[schedule] ToolCaller generated: {:?}", args);
                (
                    args.get("cron")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                    args.get("prompt")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                    args.get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string()),
                )
            } else {
                (None, None, None)
            };

        // ── Resolve cron: ToolCaller > model param > user message parse > nudge ──
        // Validate cron looks like a real 6-field expression (not garbage from small models)
        let looks_like_cron = |s: &str| {
            let parts: Vec<&str> = s.split_whitespace().collect();
            parts.len() == 6
                && parts.iter().all(|p| {
                    p.chars()
                        .all(|c| c.is_ascii_digit() || c == '*' || c == '/' || c == '-' || c == ',')
                })
        };
        let cron = tc_cron
            .filter(|s| !s.is_empty() && looks_like_cron(s))
            .or_else(|| {
                let c = &params.0.cron;
                if c.is_empty() || !looks_like_cron(c) {
                    None
                } else {
                    Some(c.clone())
                }
            })
            .or_else(|| parse_cron_from_message(&user_msg.to_lowercase()));

        let cron = match cron {
            Some(c) => c,
            None => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Could not parse a schedule from: \"{}\". \
                     Retry with cron (sec min hr dom mon dow). \
                     Examples: '0 0 8 * * *' = daily 8 AM, '0 30 9 * * 1' = Monday 9:30 AM.",
                    user_msg
                ))]));
            }
        };

        // ── Resolve prompt: ToolCaller > model param > user message extract ──
        let prompt = tc_prompt
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let p = &params.0.prompt;
                if p.is_empty() {
                    None
                } else {
                    Some(p.clone())
                }
            })
            .unwrap_or_else(|| extract_prompt_from_message(&user_msg.to_lowercase(), &user_msg));

        // ── Resolve name: ToolCaller > model param > derive from prompt ──
        let name = tc_name
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let n = &params.0.name;
                if n.is_empty() {
                    None
                } else {
                    Some(n.clone())
                }
            })
            .unwrap_or_else(|| {
                if prompt.len() > 40 {
                    format!("{}...", &prompt[..37])
                } else {
                    prompt.clone()
                }
            });

        // Default timezone to user's setting if not provided.
        let timezone = match &params.0.timezone {
            Some(tz) if !tz.is_empty() => tz.clone(),
            _ => self
                .settings_repo
                .get()
                .await
                .map(|s| s.timezone.clone())
                .unwrap_or_else(|_| "UTC".to_string()),
        };

        let id = uuid::Uuid::new_v4().to_string();
        let req = CreateScheduleRequest {
            id: id.clone(),
            label: name,
            cron: cron.clone(),
            timezone: timezone.clone(),
            kind: TaskKind::AgentPrompt {
                prompt: prompt.clone(),
            },
        };

        match self.scheduler.create_task(req).await {
            Ok(schedule) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Schedule created: \"{}\" [{}] — {} {} (agent prompt: \"{}\")",
                schedule.label, schedule.id, schedule.cron, schedule.timezone, prompt,
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to create schedule: {e}. Check the cron expression '{}' is valid 6-field format.",
                cron
            ))])),
        }
    }

    #[tool(description = "Delete a scheduled task by its ID.")]
    async fn delete_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleIdParam>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.scheduler.delete_task(&params.0.id).await {
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
        match self.scheduler.pause_task(&params.0.id).await {
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
        match self.scheduler.resume_task(&params.0.id).await {
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
        match self.scheduler.run_now(&params.0.id).await {
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
        let limit = params.0.limit.unwrap_or(10);
        match self.scheduler.get_runs(&params.0.id, limit).await {
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
                                pond_core::user_data::domain::schedule::RunStatus::Completed => {
                                    let preview = r
                                        .result
                                        .as_deref()
                                        .unwrap_or("")
                                        .chars()
                                        .take(200)
                                        .collect::<String>();
                                    format!("completed{duration}: {preview}")
                                }
                                pond_core::user_data::domain::schedule::RunStatus::Failed => {
                                    let err = r.error.as_deref().unwrap_or("unknown error");
                                    format!("failed{duration}: {err}")
                                }
                                pond_core::user_data::domain::schedule::RunStatus::Running => {
                                    "running...".to_string()
                                }
                            };
                            format!("- [{}] {}", r.started_at.format("%Y-%m-%d %H:%M"), detail)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                // Build UI hint with structured run data
                let ui_runs: Vec<serde_json::Value> = runs
                    .iter()
                    .map(|r| {
                        let status_str = match &r.status {
                            pond_core::user_data::domain::schedule::RunStatus::Completed => "completed",
                            pond_core::user_data::domain::schedule::RunStatus::Failed => "failed",
                            pond_core::user_data::domain::schedule::RunStatus::Running => "running",
                        };
                        serde_json::json!({
                            "started_at": r.started_at.format("%Y-%m-%dT%H:%M:%S").to_string(),
                            "status": status_str,
                            "duration_ms": r.duration_ms,
                            "result": r.result.as_deref().unwrap_or("").chars().take(200).collect::<String>(),
                            "error": r.error.as_deref().unwrap_or(""),
                        })
                    })
                    .collect();
                let ui_data = serde_json::json!({
                    "schedule_id": params.0.id,
                    "runs": ui_runs,
                });
                let hint = format!("[[[mcp-ui:schedule_runs:{}]]]\n", ui_data);
                let full_result = format!("{}{}", hint, text);
                Ok(CallToolResult::success(vec![Content::text(full_result)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to get runs: {}", e),
                None,
            )),
        }
    }

    #[tool(
        description = "Update an existing scheduled task. You can change the name, cron schedule, \
        prompt, or timezone. Only the fields you provide will be updated."
    )]
    async fn update_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<UpdateScheduleParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_msg = crate::last_user_message();

        // ── Resolve ID: model param > extract from user message ──
        let id = if params.0.id.is_empty() {
            // Try to find a UUID-shaped string in the user message
            user_msg
                .split_whitespace()
                .find(|w| uuid::Uuid::parse_str(w).is_ok())
                .map(|s| s.to_string())
                .unwrap_or_default()
        } else {
            params.0.id.clone()
        };

        if id.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Missing schedule ID. Please provide the ID of the schedule to update. \
                 Use list_schedules to see all schedules and their IDs.",
            )]));
        }

        // ── Resolve cron: natural language parse > validated literal ──
        // Same validation as create_schedule — reject strings that aren't valid 6-field cron.
        let looks_like_cron = |s: &str| {
            let parts: Vec<&str> = s.split_whitespace().collect();
            parts.len() == 6
                && parts.iter().all(|p| {
                    p.chars()
                        .all(|c| c.is_ascii_digit() || c == '*' || c == '/' || c == '-' || c == ',')
                })
        };
        let cron = params.0.cron.as_ref().and_then(|c| {
            if c.is_empty() {
                return None;
            }
            // Try natural language first, then validated literal
            parse_cron_from_message(&c.to_lowercase()).or_else(|| {
                if looks_like_cron(c) {
                    Some(c.clone())
                } else {
                    None
                }
            })
        });

        // ── Resolve prompt: pass through if provided ──
        let prompt =
            params.0.prompt.as_ref().and_then(
                |p| {
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.clone())
                    }
                },
            );

        // ── Build TaskKind only if prompt changed ──
        let kind = prompt.map(|p| TaskKind::AgentPrompt { prompt: p });

        let req = UpdateScheduleRequest {
            label: params.0.name.as_ref().and_then(|n| {
                if n.is_empty() {
                    None
                } else {
                    Some(n.clone())
                }
            }),
            cron,
            timezone: params.0.timezone.as_ref().and_then(|tz| {
                if tz.is_empty() {
                    None
                } else {
                    Some(tz.clone())
                }
            }),
            kind,
        };

        match self.scheduler.update_task(&id, req).await {
            Ok(schedule) => {
                let prompt_preview = match &schedule.kind {
                    TaskKind::AgentPrompt { prompt } => {
                        if prompt.len() > 60 {
                            format!("{}...", &prompt[..57])
                        } else {
                            prompt.clone()
                        }
                    }
                    TaskKind::Webhook { webhook_url } => format!("webhook: {webhook_url}"),
                };
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Schedule updated: \"{}\" [{}] — {} {} ({})",
                    schedule.label, schedule.id, schedule.cron, schedule.timezone, prompt_preview,
                ))]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to update schedule '{}': {e}. Check the ID is correct \
                 (use list_schedules to see all schedules).",
                id
            ))])),
        }
    }

    #[tool(
        description = "Get the current time in one or more timezones. Use this before scheduling \
        tasks to confirm the right time across zones. Pass IANA timezone names like \
        'America/New_York', 'Asia/Tokyo', 'Africa/Nairobi'."
    )]
    async fn world_clock(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WorldClockParams>,
    ) -> Result<CallToolResult, ErrorData> {
        use chrono::Offset;
        use chrono_tz::Tz;
        use std::str::FromStr;

        let tz_names: Vec<String> = match params.0.timezones {
            Some(ref tzs) if !tzs.is_empty() => {
                tzs.iter().filter(|s| !s.is_empty()).cloned().collect()
            }
            _ => {
                // Fall back to user's configured timezone
                let tz = self
                    .settings_repo
                    .get()
                    .await
                    .map(|s| s.timezone.clone())
                    .unwrap_or_else(|_| "UTC".to_string());
                vec![tz]
            }
        };

        if tz_names.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No timezones provided. Pass one or more IANA timezone names \
                 like 'America/New_York', 'Europe/London', 'Asia/Tokyo'.",
            )]));
        }

        let now = chrono::Utc::now();
        let mut lines = Vec::with_capacity(tz_names.len());

        for name in &tz_names {
            match Tz::from_str(name) {
                Ok(tz) => {
                    let local = now.with_timezone(&tz);
                    // Format: "Wednesday, 14 May 2026 10:30 AM (EDT, UTC-4)"
                    let abbrev = local.format("%Z").to_string();
                    let offset_secs = local.offset().fix().local_minus_utc();
                    let offset_hours = offset_secs / 3600;
                    let offset_mins = (offset_secs.abs() % 3600) / 60;
                    let utc_offset = if offset_mins == 0 {
                        format!("UTC{offset_hours:+}")
                    } else {
                        let sign = if offset_secs >= 0 { '+' } else { '-' };
                        format!("UTC{sign}{}:{:02}", offset_hours.abs(), offset_mins)
                    };
                    lines.push(format!(
                        "- {}: {} ({}, {})",
                        name,
                        local.format("%A, %d %B %Y %I:%M %p"),
                        abbrev,
                        utc_offset,
                    ));
                }
                Err(_) => {
                    lines.push(format!(
                        "- {}: unknown timezone. Use IANA names like 'America/New_York', \
                         'Europe/London', 'Africa/Nairobi'. See: \
                         https://en.wikipedia.org/wiki/List_of_tz_database_time_zones",
                        name
                    ));
                }
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for ScheduleMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-schedule",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Schedule MCP server — manage cron-based scheduled tasks.\n\n\
                 Tools (9): list_schedules, create_schedule (6-field cron), update_schedule, \
                 delete_schedule, pause_schedule, resume_schedule, run_schedule_now, \
                 get_schedule_runs, world_clock.\n\n\
                 Cron format is 6-field: sec min hr dom mon dow. \
                 Example: '0 0 8 * * *' = daily at 8:00 AM.",
            )
    }
}

// ── Cron parsing helpers (public for use by other crates) ──────────────────

/// Parse a 6-field cron expression from a natural language time description.
/// Returns `None` if no recognizable time pattern is found.
pub fn parse_cron_from_message(lower: &str) -> Option<String> {
    let hour = extract_hour(lower);

    if lower.contains("every minute") {
        return Some("0 * * * * *".to_string());
    }
    if lower.contains("every hour") || lower.contains("hourly") {
        let min = extract_minute(lower).unwrap_or(0);
        return Some(format!("0 {min} * * * *"));
    }

    // "every morning" / "every day" / "daily"
    if lower.contains("every morning")
        || lower.contains("every day")
        || lower.contains("daily")
        || lower.contains("each morning")
        || lower.contains("each day")
    {
        let h = hour.unwrap_or(8); // default 8 AM for "every morning"
        let m = extract_minute(lower).unwrap_or(0);
        return Some(format!("0 {m} {h} * * *"));
    }

    // "every evening" / "every night"
    if lower.contains("every evening") || lower.contains("every night") {
        let h = hour.unwrap_or(20); // default 8 PM
        let m = extract_minute(lower).unwrap_or(0);
        return Some(format!("0 {m} {h} * * *"));
    }

    // "every week" / "weekly" / "every monday" etc.
    if lower.contains("every week") || lower.contains("weekly") {
        let h = hour.unwrap_or(8);
        let m = extract_minute(lower).unwrap_or(0);
        let dow = extract_day_of_week(lower).unwrap_or(1); // default Monday
        return Some(format!("0 {m} {h} * * {dow}"));
    }

    // Bare "at X am/pm" without frequency -> default to daily
    if let Some(h) = hour {
        let m = extract_minute(lower).unwrap_or(0);
        return Some(format!("0 {m} {h} * * *"));
    }

    None
}

/// Extract hour from patterns like "at 10 am", "at 8pm", "at 14:30".
pub fn extract_hour(s: &str) -> Option<u32> {
    let patterns = ["at ", "by "];
    for pat in patterns {
        if let Some(idx) = s.find(pat) {
            let after = &s[idx + pat.len()..];
            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(mut h) = num_str.parse::<u32>() {
                let rest = after[num_str.len()..].trim_start();
                if rest.starts_with("pm") || rest.starts_with("p.m") {
                    if h < 12 {
                        h += 12;
                    }
                } else if rest.starts_with("am") || rest.starts_with("a.m") {
                    if h == 12 {
                        h = 0;
                    }
                }
                if h <= 23 {
                    return Some(h);
                }
            }
        }
    }
    // Pattern: "N o'clock"
    if let Some(idx) = s.find("o'clock").or_else(|| s.find("o clock")) {
        let before = s[..idx].trim_end();
        let num_str: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if let Ok(h) = num_str.parse::<u32>() {
            if h <= 23 {
                return Some(h);
            }
        }
    }
    None
}

/// Extract minute from "HH:MM" or ":MM".
pub fn extract_minute(s: &str) -> Option<u32> {
    for (i, _) in s.match_indices(':') {
        if i > 0 && s.as_bytes()[i - 1].is_ascii_digit() {
            let after = &s[i + 1..];
            let num_str: String = after
                .chars()
                .take(2)
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(m) = num_str.parse::<u32>() {
                if m < 60 {
                    return Some(m);
                }
            }
        }
    }
    None
}

/// Extract day of week (0=Sun, 1=Mon, ... 6=Sat) for tokio-cron-scheduler.
pub fn extract_day_of_week(s: &str) -> Option<u32> {
    if s.contains("sunday") || s.contains("sun") {
        return Some(0);
    }
    if s.contains("monday") || s.contains("mon") {
        return Some(1);
    }
    if s.contains("tuesday") || s.contains("tue") {
        return Some(2);
    }
    if s.contains("wednesday") || s.contains("wed") {
        return Some(3);
    }
    if s.contains("thursday") || s.contains("thu") {
        return Some(4);
    }
    if s.contains("friday") || s.contains("fri") {
        return Some(5);
    }
    if s.contains("saturday") || s.contains("sat") {
        return Some(6);
    }
    None
}

/// Extract the action prompt from a scheduling message.
/// Strips scheduling-related prefixes to get the core task description.
pub fn extract_prompt_from_message(lower: &str, original: &str) -> String {
    let strip_patterns = [
        "schedule to ",
        "schedule ",
        "set up a ",
        "set up ",
        "every morning ",
        "every day ",
        "every evening ",
        "every night ",
        "every hour ",
        "every week ",
        "every minute ",
        "daily ",
        "hourly ",
        "weekly ",
        "remind me to ",
        "remind me ",
        "at ",
        "by ",
    ];

    let mut work = lower.to_string();

    // Strip leading scheduling/time words
    for pat in &strip_patterns {
        if let Some(rest) = work.strip_prefix(pat) {
            work = rest.to_string();
        }
    }

    // Strip "at HH am/pm" and "every X" from middle
    let time_re_patterns = [
        "at ",
        "every morning",
        "every day",
        "every evening",
        "every night",
        "every hour",
        "every week",
        "every minute",
        "daily",
        "hourly",
        "weekly",
    ];
    for pat in &time_re_patterns {
        work = work.replace(pat, " ");
    }

    // Strip am/pm and digits that look like times
    let cleaned: String = work
        .split_whitespace()
        .filter(|w| {
            !w.chars().all(|c| c.is_ascii_digit() || c == ':')
                && *w != "am"
                && *w != "pm"
                && *w != "a.m."
                && *w != "p.m."
        })
        .collect::<Vec<_>>()
        .join(" ");

    let trimmed = cleaned.trim().to_string();

    // If extraction left nothing useful, use the original message as the prompt
    if trimmed.len() < 5 {
        return original.trim().to_string();
    }

    // Capitalize first letter
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.collect::<String>()),
        None => original.trim().to_string(),
    }
}

/// Build a context block of upcoming scheduled tasks for system prompt injection.
///
/// Takes an explicit `SchedulerPort` reference instead of using a global.
/// Returns `Some("## Upcoming Scheduled Tasks\n- ...")` or `None` if no active tasks.
pub async fn try_upcoming_schedules_context(scheduler: &dyn SchedulerPort) -> Option<String> {
    let tasks = scheduler.list_upcoming(5).await.ok()?;
    if tasks.is_empty() {
        return None;
    }
    let lines: Vec<String> = tasks
        .iter()
        .map(|t| {
            let kind_label = match &t.kind {
                TaskKind::AgentPrompt { prompt } => {
                    let preview = if prompt.len() > 60 {
                        format!("{}...", &prompt[..57])
                    } else {
                        prompt.clone()
                    };
                    format!("agent: \"{preview}\"")
                }
                TaskKind::Webhook { webhook_url } => {
                    format!("webhook: {webhook_url}")
                }
            };
            format!(
                "- \"{}\" — {} {} ({})",
                t.label, t.cron, t.timezone, kind_label
            )
        })
        .collect();
    Some(format!("## Upcoming Scheduled Tasks\n{}", lines.join("\n")))
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ScheduleDeps {
    scheduler: Arc<dyn SchedulerPort>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
}

static SCHEDULE_DEPS: OnceLock<ScheduleDeps> = OnceLock::new();

/// Initialize schedule server dependencies. Call once at startup.
pub fn init_schedule_deps(
    scheduler: Arc<dyn SchedulerPort>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
) {
    let _ = SCHEDULE_DEPS.set(ScheduleDeps {
        scheduler,
        settings_repo,
    });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_schedule_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = SCHEDULE_DEPS
        .get()
        .expect("init_schedule_deps() not called");
    let server = ScheduleMcpServer::new(deps.scheduler.clone(), deps.settings_repo.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-schedule MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_daily_at_8am() {
        assert_eq!(
            parse_cron_from_message("every morning at 8 am"),
            Some("0 0 8 * * *".to_string())
        );
    }

    #[test]
    fn parse_daily_default_morning() {
        assert_eq!(
            parse_cron_from_message("every morning"),
            Some("0 0 8 * * *".to_string())
        );
    }

    #[test]
    fn parse_evening() {
        assert_eq!(
            parse_cron_from_message("every evening at 9 pm"),
            Some("0 0 21 * * *".to_string())
        );
    }

    #[test]
    fn parse_every_minute() {
        assert_eq!(
            parse_cron_from_message("every minute"),
            Some("0 * * * * *".to_string())
        );
    }

    #[test]
    fn parse_hourly() {
        assert_eq!(
            parse_cron_from_message("every hour"),
            Some("0 0 * * * *".to_string())
        );
    }

    #[test]
    fn parse_weekly_monday() {
        assert_eq!(
            parse_cron_from_message("every week on monday at 10 am"),
            Some("0 0 10 * * 1".to_string())
        );
    }

    #[test]
    fn parse_bare_time_defaults_daily() {
        assert_eq!(
            parse_cron_from_message("at 3 pm"),
            Some("0 0 15 * * *".to_string())
        );
    }

    #[test]
    fn parse_with_minutes() {
        assert_eq!(
            parse_cron_from_message("daily at 10:30 am"),
            Some("0 30 10 * * *".to_string())
        );
    }

    #[test]
    fn extract_hour_am_pm() {
        assert_eq!(extract_hour("at 10 am"), Some(10));
        assert_eq!(extract_hour("at 3 pm"), Some(15));
        assert_eq!(extract_hour("at 12 am"), Some(0));
        assert_eq!(extract_hour("at 12 pm"), Some(12));
    }

    #[test]
    fn extract_day_of_week_values() {
        assert_eq!(extract_day_of_week("monday"), Some(1));
        assert_eq!(extract_day_of_week("friday"), Some(5));
        assert_eq!(extract_day_of_week("sunday"), Some(0));
    }

    #[test]
    fn extract_prompt_strips_prefixes() {
        let lower = "schedule to get weather at 10 am every morning";
        let original = "Schedule to get weather at 10 am every morning";
        let prompt = extract_prompt_from_message(lower, original);
        assert!(!prompt.to_lowercase().contains("schedule to"));
    }

    #[test]
    fn no_pattern_returns_none() {
        assert_eq!(parse_cron_from_message("hello world"), None);
    }
}
