// ── Modular MCP servers (Phase 1 split) ─────────────────────────────────────
pub mod audit;
pub mod device;
pub mod device_control;
pub mod discovery;
pub mod dispatcher;
pub mod draft;
pub mod finance;
pub mod knowledge;
pub mod memory;
pub mod news;
pub mod schedule;
pub mod system;
pub mod weather;

// ── Shared utilities for Knowledge-family servers ───────────────────────────
pub mod format;
pub mod http;

// ── Shared state for tool param generation ──────────────────────────────────

use pond_core::mcp::ports::tools::tool_caller::ToolCaller;
use std::sync::{Arc, OnceLock, RwLock};

// -- User message and current session: set by GooseAdapter before each turn --
static LAST_USER_MESSAGE: RwLock<String> = RwLock::new(String::new());

// Per-turn request context (session + in-flight tool) and the egress sink now
// live in `pond-core` so outboard adapters (e.g. pond-adapters-weather) can
// report egress into the same store without depending on this crate (#113).
// Re-exported here so existing engine call sites stay unchanged.
pub use pond_core::shared::services::egress::{
    current_session_id, current_tool, set_current_session_id, set_current_tool, set_egress_sink,
};

pub fn set_last_user_message(msg: &str) {
    if let Ok(mut guard) = LAST_USER_MESSAGE.write() {
        *guard = msg.to_string();
    }
}

pub fn last_user_message() -> String {
    LAST_USER_MESSAGE
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// -- ToolCaller specialist: generates structured params for tool calls --
// When configured (e.g. FunctionGemma 270M), ALL tool calls use this to
// generate params. The main LLM decides WHEN to call tools; the ToolCaller
// decides WHAT params to send. Set once at startup, never changes.
static TOOL_CALLER: OnceLock<Option<Arc<dyn ToolCaller>>> = OnceLock::new();

/// Set the tool-calling specialist. Call once at startup.
pub fn set_tool_caller(tc: Option<Arc<dyn ToolCaller>>) {
    let _ = TOOL_CALLER.set(tc);
}

/// Get the tool-calling specialist, if configured.
pub fn tool_caller() -> Option<Arc<dyn ToolCaller>> {
    TOOL_CALLER.get().and_then(|opt| opt.clone())
}

/// Generate tool params via the ToolCaller specialist.
///
/// When a ToolCaller is configured, this is the PRIMARY param generator —
/// the main LLM's params are ignored. Returns `None` if no ToolCaller is
/// configured or if the user message is empty.
pub async fn generate_params(
    tool_name: &str,
    schema: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let tc = tool_caller()?;
    let user_msg = last_user_message();
    if user_msg.is_empty() {
        println!(
            "[tool-caller] {} skipped: no user message available",
            tool_name
        );
        return None;
    }
    println!("[tool-caller] ╔═══ ToolCaller Request ═══");
    println!("[tool-caller] ║ tool:   {}", tool_name);
    println!("[tool-caller] ║ query:  {:?}", user_msg);
    println!("[tool-caller] ║ schema: {}", schema);
    println!("[tool-caller] ╚═════════════════════════");

    match tc.generate_tool_call(tool_name, schema, &user_msg).await {
        Ok(args) if !args.is_empty() => {
            println!("[tool-caller] ╔═══ ToolCaller Response ═══");
            println!("[tool-caller] ║ status: SUCCESS");
            for (k, v) in &args {
                println!("[tool-caller] ║ {}: {}", k, v);
            }
            println!("[tool-caller] ╚══════════════════════════");
            Some(args)
        }
        Ok(_) => {
            println!("[tool-caller] ╔═══ ToolCaller Response ═══");
            println!("[tool-caller] ║ status: EMPTY (no args returned)");
            println!("[tool-caller] ╚══════════════════════════");
            None
        }
        Err(e) => {
            println!("[tool-caller] ╔═══ ToolCaller Response ═══");
            println!("[tool-caller] ║ status: FAILED");
            println!("[tool-caller] ║ error:  {e}");
            println!("[tool-caller] ╚══════════════════════════");
            None
        }
    }
}

// Re-export key types for downstream crates
pub use audit::AuditMcpServer;
pub use device::DeviceMcpServer;
pub use device_control::DeviceControlMcpServer;
pub use discovery::DiscoveryMcpServer;
pub use draft::DraftMcpServer;
pub use finance::FinanceMcpServer;
pub use knowledge::{clean_query_for_search, KnowledgeMcpServer};
pub use memory::{auto_classify_segment, parse_memory_segment, parse_memory_tier, MemoryMcpServer};
pub use news::NewsMcpServer;
pub use schedule::{try_upcoming_schedules_context, ScheduleMcpServer};
pub use system::SystemMcpServer;
pub use weather::WeatherMcpServer;

// Re-export init + spawn functions for Goose builtin extension registration
pub use audit::{init_audit_deps, spawn_audit_server};
pub use device::{init_device_deps, spawn_device_server};
pub use device_control::{init_device_control_deps, spawn_device_control_server};
pub use discovery::{init_discovery_deps, spawn_discovery_server};
pub use draft::{init_draft_deps, spawn_draft_server};
pub use finance::{init_finance_deps, spawn_finance_server};
pub use knowledge::{init_knowledge_deps, spawn_knowledge_server};
pub use memory::{init_memory_deps, spawn_memory_server};
pub use news::{init_news_deps, spawn_news_server};
pub use schedule::{init_schedule_deps, spawn_schedule_server};
pub use system::spawn_system_server;
pub use weather::{init_weather_deps, spawn_weather_server, WEATHER_APP_URI};

// ── MCP App resources ─────────────────────────────────────────────────────
// Collects all embedded HTML resources from MCP servers that support UI apps.

/// Returns all `(uri, html_content)` pairs from every MCP server that provides
/// an embedded MCP App resource. Used by pond-api to populate the static
/// resource registry in AppState.
pub fn all_app_resources() -> Vec<(&'static str, &'static str)> {
    let mut resources = Vec::new();
    resources.extend(weather::app_resources());
    // Future MCP servers with apps add their resources here:
    // resources.extend(schedule::app_resources());
    resources
}

// Re-export shared utilities for downstream Knowledge-family servers
pub use format::{format_api_error, format_list_result, format_not_configured, truncate_to_budget};
pub use http::{build_http_client, traced_get, traced_get_with};

// Re-export the direct tool dispatcher
pub use dispatcher::McpToolDispatcher;
