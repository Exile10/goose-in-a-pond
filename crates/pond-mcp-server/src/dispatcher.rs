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

/// All tool (name, description) pairs registered by GIAP's builtin MCP servers.
/// Descriptions are pulled from the `#[tool(description = "...")]` attributes on each handler.
const ALL_TOOLS: &[(&str, &str)] = &[
    // Weather
    ("giap-weather__get_current_weather", "Get current weather conditions for any city. Pass a location name or omit for default."),
    ("giap-weather__get_weather_forecast", "Get multi-day weather forecast. Pass location and number of days."),
    // Knowledge
    ("giap-knowledge__get_wikipedia_article", "Look up factual, encyclopedic information about any topic. Use for people, places, events, science, history."),
    ("giap-knowledge__search_wikipedia", "Search Wikipedia when the exact title is unknown. Returns matching articles."),
    ("giap-knowledge__instant_answer", "Get a quick factual answer from DuckDuckGo instant answers."),
    ("giap-knowledge__define_word", "Look up dictionary definitions, etymology, and usage of a word."),
    ("giap-knowledge__search_books", "Search for books by title, author, or topic via Open Library."),
    // Memory
    ("giap-memory__save_memory", "Save information the user wants remembered (preferences, facts, notes, corrections)."),
    ("giap-memory__recall_memories", "Search saved memories for previously stored information about the user."),
    ("giap-memory__forget_memory", "Delete a specific saved memory by its ID."),
    // Schedule
    ("giap-schedule__create_schedule", "Create a new scheduled task that runs a prompt at a recurring time."),
    ("giap-schedule__list_schedules", "List all scheduled tasks with their cron, timezone, and status."),
    ("giap-schedule__update_schedule", "Update an existing schedule's name, cron, prompt, or timezone."),
    ("giap-schedule__delete_schedule", "Permanently delete a scheduled task by ID."),
    ("giap-schedule__pause_schedule", "Pause a schedule so it stops firing until resumed."),
    ("giap-schedule__resume_schedule", "Resume a previously paused schedule."),
    ("giap-schedule__run_schedule_now", "Manually trigger a scheduled task to execute immediately."),
    ("giap-schedule__get_schedule_runs", "Get the execution history for a scheduled task."),
    ("giap-schedule__world_clock", "Get the current time in one or more timezones."),
    // System
    ("giap-system__get_current_time", "Get the current date, time, and timezone."),
    ("giap-system__get_system_info", "Get system information: OS, hostname, memory, disk usage."),
    ("giap-system__send_notification", "Send a desktop notification popup to the user."),
    ("giap-system__run_shell_command", "Execute a safe, sandboxed shell command (ls, cat, date, uptime, df, etc)."),
    ("giap-system__read_file", "Read the contents of a local file."),
    ("giap-system__write_file", "Write or append content to a local file."),
    // Device
    ("giap-device__list_registered_devices", "List all registered smart home devices and their online status."),
    ("giap-device__get_user_profile", "Get the current user's profile preferences."),
    ("giap-device__get_model_assignments", "Get which AI models are assigned to which roles."),
    ("giap-device__list_skills", "List available agent skills and their descriptions."),
    ("giap-device__get_recipe", "Get details of a named agent recipe/workflow."),
    // News
    ("giap-news__get_top_stories", "Get today's top news stories from Hacker News or other sources."),
    ("giap-news__search_news", "Search for news articles on a specific topic."),
    ("giap-news__get_headlines", "Get current news headlines, optionally filtered by category."),
    // Finance
    ("giap-finance__get_exchange_rate", "Get the current exchange rate between two currencies."),
    ("giap-finance__convert_currency", "Convert an amount from one currency to another."),
    ("giap-finance__get_stock_quote", "Get the current stock price and market data for a ticker symbol."),
    ("giap-finance__get_crypto_price", "Get the current price of a cryptocurrency (Bitcoin, Ethereum, etc)."),
    // Discovery
    ("giap-discovery__get_country_info", "Get information about a country: capital, population, languages, currency."),
    ("giap-discovery__lookup_product", "Look up a product by name or barcode via Open Food Facts."),
    ("giap-discovery__get_product_price", "Get the price of a product from open price databases."),
    ("giap-discovery__search_web", "Search the web via DuckDuckGo or SearXNG for general queries."),
    // Draft
    ("giap-draft__save_draft", "Save a draft response for user approval before executing."),
    ("giap-draft__list_drafts", "List pending drafts awaiting user approval."),
    ("giap-draft__approve_draft", "Approve a pending draft for execution."),
    ("giap-draft__reject_draft", "Reject a pending draft."),
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
        let mut tools: Vec<String> = ALL_TOOLS.iter().map(|(name, _)| name.to_string()).collect();
        if self.schedule.is_none() {
            tools.retain(|t| !t.starts_with(PREFIX_SCHEDULE));
        }
        tools
    }

    async fn available_tools_with_descriptions(&self) -> Vec<(String, String)> {
        let mut tools: Vec<(String, String)> = ALL_TOOLS
            .iter()
            .map(|(name, desc)| (name.to_string(), desc.to_string()))
            .collect();
        if self.schedule.is_none() {
            tools.retain(|(name, _)| !name.starts_with(PREFIX_SCHEDULE));
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
        for (tool, desc) in ALL_TOOLS {
            let has_valid_prefix = valid_prefixes.iter().any(|p| tool.starts_with(p));
            assert!(
                has_valid_prefix,
                "Tool '{}' doesn't start with a known prefix",
                tool
            );
            assert!(
                !desc.is_empty(),
                "Tool '{}' has an empty description",
                tool
            );
        }
    }
}
