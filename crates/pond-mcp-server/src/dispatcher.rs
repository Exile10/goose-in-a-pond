//! McpToolDispatcher — routes tool calls directly to GIAP's builtin MCP servers.
//!
//! Implements [`ToolDispatcher`] by holding instances of all MCP servers and
//! routing calls by tool name prefix (e.g. "giap-weather__get_current_weather").
//! This bypasses Goose's extension manager, enabling the PondAgent to call
//! GIAP tools without a running Goose session.
//!
//! # Peer Construction
//!
//! rmcp's `RequestContext<RoleServer>` requires a `Peer` which can only be
//! obtained from a running service. We use `serve_directly_with_ct` with a
//! DuplexStream to create a minimal background service, extract the peer,
//! and reuse it for all tool dispatch calls. Since GIAP's MCP handlers never
//! access `ctx.peer`, the peer's transport being a no-op is safe.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_adapters_weather::WeatherProvider;
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::draft::DraftRepository;
use pond_core::ports::embedding::EmbeddingProvider;
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::recipe::AgentRecipeRepository;
use pond_core::ports::scheduler::SchedulerPort;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use pond_core::ports::tool_dispatcher::{ToolCallResult, ToolDispatcher};
use rmcp::model::{CallToolRequestParams, CallToolResult as RmcpCallToolResult, RequestId};
use rmcp::service::{Peer, RequestContext, RunningService};
use rmcp::{RoleServer, ServerHandler};
use std::sync::Arc;

use crate::{
    DeviceMcpServer, DiscoveryMcpServer, DraftMcpServer, FinanceMcpServer, KnowledgeMcpServer,
    MemoryMcpServer, NewsMcpServer, ScheduleMcpServer, SystemMcpServer, WeatherMcpServer,
};

// ── Tool name constants ──────────────────────────────────────────────────────

const PREFIX_WEATHER: &str = "giap-weather__";
const PREFIX_KNOWLEDGE: &str = "giap-knowledge__";
const PREFIX_MEMORY: &str = "giap-memory__";
const PREFIX_SCHEDULE: &str = "giap-schedule__";
const PREFIX_SYSTEM: &str = "giap-system__";
const PREFIX_DEVICE: &str = "giap-device__";
const PREFIX_NEWS: &str = "giap-news__";
const PREFIX_FINANCE: &str = "giap-finance__";
const PREFIX_DISCOVERY: &str = "giap-discovery__";
const PREFIX_DRAFT: &str = "giap-draft__";

/// All tool names registered by GIAP's builtin MCP servers.
const ALL_TOOLS: &[&str] = &[
    // Weather
    "giap-weather__get_current_weather",
    "giap-weather__get_weather_forecast",
    // Knowledge
    "giap-knowledge__get_wikipedia_article",
    "giap-knowledge__search_wikipedia",
    "giap-knowledge__instant_answer",
    "giap-knowledge__define_word",
    "giap-knowledge__search_books",
    // Memory
    "giap-memory__save_memory",
    "giap-memory__recall_memories",
    "giap-memory__forget_memory",
    // Schedule
    "giap-schedule__create_schedule",
    "giap-schedule__list_schedules",
    "giap-schedule__update_schedule",
    "giap-schedule__delete_schedule",
    "giap-schedule__pause_schedule",
    "giap-schedule__resume_schedule",
    "giap-schedule__run_schedule_now",
    "giap-schedule__get_schedule_runs",
    "giap-schedule__world_clock",
    // System
    "giap-system__get_current_time",
    "giap-system__get_system_info",
    "giap-system__send_notification",
    "giap-system__run_shell_command",
    "giap-system__read_file",
    "giap-system__write_file",
    // Device
    "giap-device__list_registered_devices",
    "giap-device__get_user_profile",
    "giap-device__get_model_assignments",
    "giap-device__list_skills",
    "giap-device__get_recipe",
    // News
    "giap-news__get_top_stories",
    "giap-news__search_news",
    "giap-news__get_headlines",
    // Finance
    "giap-finance__get_exchange_rate",
    "giap-finance__convert_currency",
    "giap-finance__get_stock_quote",
    "giap-finance__get_crypto_price",
    // Discovery
    "giap-discovery__get_country_info",
    "giap-discovery__lookup_product",
    "giap-discovery__get_product_price",
    "giap-discovery__search_web",
    // Draft
    "giap-draft__save_draft",
    "giap-draft__list_drafts",
    "giap-draft__approve_draft",
    "giap-draft__reject_draft",
];

// ── Dispatcher ───────────────────────────────────────────────────────────────

/// Concrete dispatcher that routes tool calls to GIAP's builtin MCP servers.
///
/// Holds instances of all MCP servers and dispatches by parsing the tool name
/// prefix. Each server is constructed once and reused for all calls.
///
/// A background "peer service" is kept alive to provide a valid `Peer<RoleServer>`
/// for constructing `RequestContext` values. This is a no-op service — no real
/// MCP transport is active.
pub struct McpToolDispatcher {
    weather: WeatherMcpServer,
    knowledge: KnowledgeMcpServer,
    memory: MemoryMcpServer,
    schedule: Option<ScheduleMcpServer>,
    system: SystemMcpServer,
    device: DeviceMcpServer,
    news: NewsMcpServer,
    finance: FinanceMcpServer,
    discovery: DiscoveryMcpServer,
    draft: DraftMcpServer,
    /// Cloned peer from a minimal running service — used to construct RequestContext.
    peer: Peer<RoleServer>,
    /// Keeps the background peer-provider service alive. Dropped on dispatcher drop.
    _peer_service: RunningService<RoleServer, SystemMcpServer>,
}

impl McpToolDispatcher {
    /// Construct the dispatcher with the same dependencies used by
    /// `register_giap_extensions()`.
    ///
    /// This spawns a minimal background task to maintain a valid rmcp `Peer`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        memory_repo: Arc<dyn MemoryRepository>,
        weather: Option<Arc<dyn WeatherProvider>>,
        scheduler: Option<Arc<dyn SchedulerPort>>,
        settings_repo: Arc<dyn SettingsRepository>,
        device_registry: Arc<dyn DeviceRegistry>,
        skill_repo: Arc<dyn UserSkillRepository>,
        recipe_repo: Arc<dyn AgentRecipeRepository>,
        draft_repo: Arc<dyn DraftRepository>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        let http_client = crate::build_http_client();

        let weather_server = WeatherMcpServer::new(weather);
        let knowledge_server = KnowledgeMcpServer::new(http_client.clone());
        let memory_server = MemoryMcpServer::new(memory_repo, embedding_provider);
        let schedule_server = scheduler.map(|s| ScheduleMcpServer::new(s, settings_repo.clone()));
        let system_server = SystemMcpServer::new();
        let device_server = DeviceMcpServer::new(
            device_registry,
            settings_repo.clone(),
            skill_repo,
            recipe_repo,
        );
        let news_server = NewsMcpServer::new(http_client.clone(), settings_repo.clone());
        let finance_server = FinanceMcpServer::new(http_client.clone(), settings_repo.clone());
        let discovery_server = DiscoveryMcpServer::new(http_client, settings_repo);
        let draft_server = DraftMcpServer::new(draft_repo);

        // Create a dummy peer via serve_directly on a DuplexStream.
        // The system server is lightweight (no deps) — we use it as the service
        // backing the peer. The DuplexStream client side is immediately dropped
        // so no real traffic flows; we just need the Peer for RequestContext.
        let (_client_stream, server_stream) = tokio::io::duplex(64);
        let running = rmcp::service::serve_directly(SystemMcpServer::new(), server_stream, None);
        let peer = running.peer().clone();

        Self {
            weather: weather_server,
            knowledge: knowledge_server,
            memory: memory_server,
            schedule: schedule_server,
            system: system_server,
            device: device_server,
            news: news_server,
            finance: finance_server,
            discovery: discovery_server,
            draft: draft_server,
            peer,
            _peer_service: running,
        }
    }

    /// Build a `RequestContext<RoleServer>` for dispatching a tool call.
    ///
    /// Uses the shared peer. GIAP handlers never access the peer so the
    /// closed transport is irrelevant.
    fn make_context(&self) -> RequestContext<RoleServer> {
        RequestContext::new(RequestId::Number(0), self.peer.clone())
    }
}

#[async_trait]
impl ToolDispatcher for McpToolDispatcher {
    async fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult> {
        // Strip the server prefix to get the bare tool name for rmcp dispatch
        let (server_prefix, bare_name) = parse_tool_name(tool_name)?;

        // Convert arguments to the format rmcp expects
        let args_map = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            _ => Some(serde_json::Map::from_iter([(
                "input".to_string(),
                arguments,
            )])),
        };

        // Build rmcp CallToolRequestParams — must own the tool name
        let bare_name_owned = bare_name.to_string();
        let params = if let Some(args) = args_map {
            CallToolRequestParams::new(bare_name_owned).with_arguments(args)
        } else {
            CallToolRequestParams::new(bare_name_owned)
        };

        let ctx = self.make_context();

        // Route to the correct server and call via ServerHandler::call_tool
        let result = match server_prefix {
            PREFIX_WEATHER => self.weather.call_tool(params, ctx).await,
            PREFIX_KNOWLEDGE => self.knowledge.call_tool(params, ctx).await,
            PREFIX_MEMORY => self.memory.call_tool(params, ctx).await,
            PREFIX_SCHEDULE => {
                if let Some(ref sched) = self.schedule {
                    sched.call_tool(params, ctx).await
                } else {
                    return Ok(ToolCallResult {
                        content: "Schedule service is not configured.".to_string(),
                        success: false,
                    });
                }
            }
            PREFIX_SYSTEM => self.system.call_tool(params, ctx).await,
            PREFIX_DEVICE => self.device.call_tool(params, ctx).await,
            PREFIX_NEWS => self.news.call_tool(params, ctx).await,
            PREFIX_FINANCE => self.finance.call_tool(params, ctx).await,
            PREFIX_DISCOVERY => self.discovery.call_tool(params, ctx).await,
            PREFIX_DRAFT => self.draft.call_tool(params, ctx).await,
            _ => {
                return Err(anyhow!("Unknown tool server prefix: {}", server_prefix));
            }
        };

        // Convert rmcp result to our domain type
        match result {
            Ok(rmcp_result) => Ok(convert_rmcp_result(rmcp_result)),
            Err(error_data) => Ok(ToolCallResult {
                content: error_data.message.to_string(),
                success: false,
            }),
        }
    }

    async fn available_tools(&self) -> Vec<String> {
        let mut tools: Vec<String> = ALL_TOOLS.iter().map(|s| s.to_string()).collect();
        // Remove schedule tools if scheduler is not configured
        if self.schedule.is_none() {
            tools.retain(|t| !t.starts_with(PREFIX_SCHEDULE));
        }
        tools
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a tool name like "giap-weather__get_current_weather" into
/// (prefix = "giap-weather__", bare_name = "get_current_weather").
fn parse_tool_name(tool_name: &str) -> Result<(&str, &str)> {
    if let Some(pos) = tool_name.find("__") {
        let prefix = &tool_name[..pos + 2]; // include the "__"
        let bare = &tool_name[pos + 2..];
        if bare.is_empty() {
            return Err(anyhow!("Empty tool name after prefix: {}", tool_name));
        }
        Ok((prefix, bare))
    } else {
        Err(anyhow!(
            "Invalid tool name format (expected 'giap-<server>__<tool>'): {}",
            tool_name
        ))
    }
}

/// Extract text content from rmcp's CallToolResult.
fn convert_rmcp_result(result: RmcpCallToolResult) -> ToolCallResult {
    let success = !result.is_error.unwrap_or(false);
    let content = result
        .content
        .into_iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");

    ToolCallResult { content, success }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_weather_tool_name() {
        let (prefix, bare) = parse_tool_name("giap-weather__get_current_weather").unwrap();
        assert_eq!(prefix, "giap-weather__");
        assert_eq!(bare, "get_current_weather");
    }

    #[test]
    fn parse_schedule_tool_name() {
        let (prefix, bare) = parse_tool_name("giap-schedule__create_schedule").unwrap();
        assert_eq!(prefix, "giap-schedule__");
        assert_eq!(bare, "create_schedule");
    }

    #[test]
    fn parse_invalid_tool_name() {
        let result = parse_tool_name("no_prefix_tool");
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_bare_name() {
        let result = parse_tool_name("giap-weather__");
        assert!(result.is_err());
    }

    #[test]
    fn all_tools_have_valid_prefixes() {
        let valid_prefixes = [
            PREFIX_WEATHER,
            PREFIX_KNOWLEDGE,
            PREFIX_MEMORY,
            PREFIX_SCHEDULE,
            PREFIX_SYSTEM,
            PREFIX_DEVICE,
            PREFIX_NEWS,
            PREFIX_FINANCE,
            PREFIX_DISCOVERY,
            PREFIX_DRAFT,
        ];
        for tool in ALL_TOOLS {
            let has_valid_prefix = valid_prefixes.iter().any(|p| tool.starts_with(p));
            assert!(
                has_valid_prefix,
                "Tool '{}' doesn't start with a known prefix",
                tool
            );
        }
    }
}
