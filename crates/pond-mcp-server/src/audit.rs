//! Audit / Privacy-Audit MCP Server (#115, Q2-38) — "what did you do?".
//!
//! Read-only tools over the unified `events` store (#109) so the agent can give
//! the user a grounded account of its own activity — including which external
//! hosts it contacted — instead of guessing. It NEVER mutates the store, and
//! `Secret`-classified events (credentials, tokens) are never surfaced.
//!
//! Provides 3 tools: `get_recent_activity`, `summarize_activity`,
//! `list_privacy_risks`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use pond_core::security::domain::event::{
    AttributeValue, Event, EventCategory, EventQuery, PrivacySensitivity,
};
use pond_core::security::ports::event_log::EventLog;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

// ── Constants ────────────────────────────────────────────────────────────────

const DEFAULT_RECENT_LIMIT: usize = 50;
const MAX_RECENT_LIMIT: usize = 200;
/// Bound on rows scanned for aggregation tools (summary / privacy report).
const SCAN_LIMIT: usize = 1000;
/// The highest sensitivity these tools ever surface. `Secret` (credentials,
/// tokens) is excluded in the store query itself — not just post-filtered —
/// so row limits count only visible events (#157 review follow-up).
const MAX_SURFACEABLE: PrivacySensitivity = PrivacySensitivity::Sensitive;

// ── Parameter structs ──────────────────────────────────────────────────────--

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecentActivityParams {
    /// "hour" | "day" (default) | "week".
    pub window: Option<String>,
    /// snake_case, e.g. "network", "sensor", "device", "tool".
    pub category: Option<String>,
    /// Default 50, max 200.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WindowParams {
    /// "hour" | "day" (default) | "week".
    pub window: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuditMcpServer {
    event_log: Arc<dyn EventLog>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AuditMcpServer {
    pub fn new(event_log: Arc<dyn EventLog>) -> Self {
        Self {
            event_log,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
List the assistant's own recent activity (sensor/device/tool/network events) from the local \
event log. Answers \"what did you do?\". Never guess.")]
    async fn get_recent_activity(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecentActivityParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let window = normalize_window(params.0.window.as_deref());
        let category = params.0.category.as_deref().and_then(parse_category);
        let limit = params
            .0
            .limit
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_RECENT_LIMIT)
            .clamp(1, MAX_RECENT_LIMIT);
        let since = chrono::Utc::now() - window_span(window);

        // Secret events are excluded by the store itself (max_sensitivity), so
        // `limit` counts only surfaceable rows — no over-fetch needed. Query
        // one extra row purely to detect truncation for the "showing N most
        // recent" indicator; `recent_lines` caps to `limit`.
        let events = match self
            .event_log
            .query(EventQuery {
                category,
                since: Some(since),
                max_sensitivity: Some(MAX_SURFACEABLE),
                limit: Some(limit + 1),
                ..Default::default()
            })
            .await
        {
            Ok(e) => e,
            Err(e) => return Ok(read_error("recent activity", &e)),
        };
        Ok(CallToolResult::success(vec![Content::text(recent_lines(
            &events, window, limit,
        ))]))
    }

    #[tool(description = "\
Summarize recent activity as counts per category (overview, not a full list). Flags outbound \
network call count.")]
    async fn summarize_activity(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WindowParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let window = normalize_window(params.0.window.as_deref());
        let since = chrono::Utc::now() - window_span(window);

        let events = match self
            .event_log
            .query(EventQuery {
                since: Some(since),
                max_sensitivity: Some(MAX_SURFACEABLE),
                limit: Some(SCAN_LIMIT),
                ..Default::default()
            })
            .await
        {
            Ok(e) => e,
            Err(e) => return Ok(read_error("activity summary", &e)),
        };
        Ok(CallToolResult::success(vec![Content::text(summary_text(
            &events, window,
        ))]))
    }

    #[tool(description = "\
Privacy report: external hosts contacted (and via which tool) plus count of events touching \
personal/sensitive data.")]
    async fn list_privacy_risks(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WindowParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let window = normalize_window(params.0.window.as_deref());
        let since = chrono::Utc::now() - window_span(window);

        let network = match self
            .event_log
            .query(EventQuery {
                category: Some(EventCategory::Network),
                since: Some(since),
                max_sensitivity: Some(MAX_SURFACEABLE),
                limit: Some(SCAN_LIMIT),
                ..Default::default()
            })
            .await
        {
            Ok(e) => e,
            Err(e) => return Ok(read_error("privacy report", &e)),
        };
        let all = match self
            .event_log
            .query(EventQuery {
                since: Some(since),
                max_sensitivity: Some(MAX_SURFACEABLE),
                limit: Some(SCAN_LIMIT),
                ..Default::default()
            })
            .await
        {
            Ok(e) => e,
            Err(e) => return Ok(read_error("privacy report", &e)),
        };
        Ok(CallToolResult::success(vec![Content::text(
            privacy_report(&network, &all, window),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for AuditMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("giap-audit", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "GIAP Audit MCP server — answer the user's questions about the assistant's OWN \
                 activity, grounded in the local, on-device event log (never guess).\n\n\
                 Tools: get_recent_activity (list what happened), summarize_activity (counts per \
                 category), list_privacy_risks (external hosts contacted + sensitive-data touches).\n\n\
                 All data is local. Credentials/tokens (Secret-classified events) are never shown.",
            )
    }
}

// ── Pure helpers (unit-tested without a live client/context) ──────────────────

/// Clamp a free-form window string to one of the three supported windows.
fn normalize_window(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("hour") => "hour",
        Some("week") => "week",
        _ => "day",
    }
}

fn window_span(window: &str) -> chrono::Duration {
    match window {
        "hour" => chrono::Duration::hours(1),
        "week" => chrono::Duration::weeks(1),
        _ => chrono::Duration::days(1),
    }
}

/// Parse an `EventCategory` from its snake_case wire form.
fn parse_category(s: &str) -> Option<EventCategory> {
    serde_json::from_value(serde_json::Value::String(s.trim().to_string())).ok()
}

/// The snake_case string for a category (matches the serde representation).
fn category_str(category: EventCategory) -> String {
    serde_json::to_value(category)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Read a text attribute, if present and string-typed.
fn attr_text<'a>(event: &'a Event, key: &str) -> Option<&'a str> {
    match event.attributes.get(key) {
        Some(AttributeValue::Text(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Events safe to surface: `Secret` (credentials/tokens) is always excluded.
/// The store query already excludes Secret via `max_sensitivity` — this is
/// defense in depth so a future `EventLog` impl that ignores the filter can
/// never leak credentials through these tools.
fn is_visible(event: &Event) -> bool {
    event.privacy_sensitivity != PrivacySensitivity::Secret
}

/// Render a newest-first list of activity lines, capped to `limit`. The caller
/// queries `limit + 1` store-filtered rows, so one extra visible event here
/// means there are more than `limit` and the output flags the truncation.
fn recent_lines(events: &[Event], window: &str, limit: usize) -> String {
    let mut visible: Vec<&Event> = events.iter().filter(|e| is_visible(e)).collect();
    if visible.is_empty() {
        return format!("No activity recorded in the last {window}.");
    }
    // Cap to the requested number of *visible* events; flag if we trimmed.
    let truncated = visible.len() > limit;
    visible.truncate(limit);
    let mut out = if truncated {
        format!(
            "Activity in the last {window} (showing {} most recent):\n",
            visible.len()
        )
    } else {
        format!(
            "Activity in the last {window} ({} event{}):\n",
            visible.len(),
            if visible.len() == 1 { "" } else { "s" }
        )
    };
    for e in visible {
        let when = e.timestamp.format("%Y-%m-%d %H:%M");
        let mut line = format!("• {} {} · {}", when, category_str(e.category), e.action);
        // Surface the destination host for network calls.
        if let Some(host) = attr_text(e, "host") {
            line.push_str(&format!(" → {host}"));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Counts per category over the window.
fn summary_text(events: &[Event], window: &str) -> String {
    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    for e in events.iter().filter(|e| is_visible(e)) {
        *by_category.entry(category_str(e.category)).or_default() += 1;
        total += 1;
    }
    if total == 0 {
        return format!("No activity recorded in the last {window}.");
    }
    let mut out = format!(
        "In the last {window}: {total} event{}.\n",
        if total == 1 { "" } else { "s" }
    );
    for (cat, n) in &by_category {
        out.push_str(&format!("- {cat}: {n}\n"));
    }
    if let Some(net) = by_category.get("network") {
        out.push_str(&format!(
            "\n↳ {net} external network call{} — use list_privacy_risks for destinations.",
            if *net == 1 { "" } else { "s" }
        ));
    }
    out.trim_end().to_string()
}

/// Group network egress by destination host, plus a sensitive-data tally.
fn privacy_report(network: &[Event], all: &[Event], window: &str) -> String {
    // host -> (count, set of tools that made the calls)
    let mut by_host: BTreeMap<String, (usize, BTreeSet<String>)> = BTreeMap::new();
    for e in network.iter().filter(|e| is_visible(e)) {
        let host = attr_text(e, "host").unwrap_or("(unknown host)").to_string();
        let entry = by_host.entry(host).or_default();
        entry.0 += 1;
        if let Some(tool) = attr_text(e, "tool") {
            if !tool.is_empty() {
                entry.1.insert(tool.to_string());
            }
        }
    }

    let sensitive = all
        .iter()
        .filter(|e| e.privacy_sensitivity == PrivacySensitivity::Sensitive)
        .count();

    let mut out = format!("Privacy report — last {window}:\n\n");
    if by_host.is_empty() {
        out.push_str("No external network calls recorded.\n");
    } else {
        out.push_str("External network calls:\n");
        for (host, (count, tools)) in &by_host {
            let via = if tools.is_empty() {
                String::new()
            } else {
                format!(
                    " via {}",
                    tools.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            out.push_str(&format!(
                "- {host}: {count} call{}{via}\n",
                if *count == 1 { "" } else { "s" }
            ));
        }
    }
    out.push_str(&format!(
        "\n{sensitive} event{} handled personal/sensitive data.",
        if sensitive == 1 { "" } else { "s" }
    ));
    out.trim_end().to_string()
}

/// Generic, non-leaky failure response; the real error is logged server-side.
fn read_error(what: &str, err: &anyhow::Error) -> CallToolResult {
    tracing::warn!(error = %err, "audit: {what} query failed");
    CallToolResult::success(vec![Content::text(format!(
        "Sorry, I couldn't read the {what} right now."
    ))])
}

// ── Static deps + spawn function for the Goose builtin registry ───────────────

use rmcp::ServiceExt;
use tokio::io::DuplexStream;

struct AuditDeps {
    event_log: Arc<dyn EventLog>,
}

static AUDIT_DEPS: OnceLock<AuditDeps> = OnceLock::new();

/// Install the audit server's event-log handle. Call once at startup, before any
/// chat session loads the extension.
pub fn init_audit_deps(event_log: Arc<dyn EventLog>) {
    let _ = AUDIT_DEPS.set(AuditDeps { event_log });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_audit_server(reader: DuplexStream, writer: DuplexStream) {
    // Degrade gracefully instead of panicking: if an entry point registered the
    // giap-audit extension without calling `init_audit_deps` first, we simply do
    // not start the server. Goose sees the pipe close and treats the extension as
    // unavailable for the session rather than crashing the whole process.
    let Some(deps) = AUDIT_DEPS.get() else {
        tracing::error!(
            "giap-audit: init_audit_deps() was never called for this entry point; \
             audit MCP server not started (the audit tool is unavailable this session)"
        );
        return;
    };
    let server = AuditMcpServer::new(deps.event_log.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-audit MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────--

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Minimal no-op EventLog so the server can be constructed in tests.
    struct StubLog;
    #[async_trait]
    impl EventLog for StubLog {
        async fn append(&self, _event: Event) -> anyhow::Result<()> {
            Ok(())
        }
        async fn query(&self, _query: EventQuery) -> anyhow::Result<Vec<Event>> {
            Ok(vec![])
        }
        async fn purge(&self, _query: EventQuery) -> anyhow::Result<u64> {
            Ok(0)
        }
    }

    fn ev(category: EventCategory, action: &str) -> Event {
        Event::new(category, action)
    }

    #[test]
    fn server_constructs() {
        let _server = AuditMcpServer::new(Arc::new(StubLog));
    }

    #[test]
    fn normalize_window_clamps() {
        assert_eq!(normalize_window(Some("hour")), "hour");
        assert_eq!(normalize_window(Some("WEEK")), "week");
        assert_eq!(normalize_window(Some("decade")), "day");
        assert_eq!(normalize_window(None), "day");
    }

    #[test]
    fn recent_lines_hides_secret_and_shows_host() {
        let events = vec![
            ev(EventCategory::Network, "egress.http").attr("host", "en.wikipedia.org"),
            ev(EventCategory::Sensor, "sensor.reading"),
            ev(EventCategory::Auth, "auth.token_minted").sensitivity(PrivacySensitivity::Secret),
        ];
        let out = recent_lines(&events, "day", MAX_RECENT_LIMIT);
        assert!(
            out.contains("2 events"),
            "Secret excluded from count: {out}"
        );
        assert!(out.contains("en.wikipedia.org"));
        assert!(out.contains("sensor.reading"));
        assert!(!out.contains("auth.token_minted"), "Secret action leaked");
    }

    #[test]
    fn recent_lines_caps_after_secret_filter() {
        // The newest scanned row is Secret; with limit=2 the user should still
        // see 2 visible events, not 1 (the bug: capping before the filter).
        let events = vec![
            ev(EventCategory::Auth, "auth.token_minted").sensitivity(PrivacySensitivity::Secret),
            ev(EventCategory::Sensor, "sensor.reading"),
            ev(EventCategory::Device, "device.state_changed"),
        ];
        let out = recent_lines(&events, "day", 2);
        assert!(out.contains("sensor.reading"));
        assert!(out.contains("device.state_changed"));
        assert!(!out.contains("auth.token_minted"));
    }

    #[test]
    fn recent_lines_truncates_with_indicator() {
        let events = vec![
            ev(EventCategory::System, "system.a"),
            ev(EventCategory::System, "system.b"),
            ev(EventCategory::System, "system.c"),
        ];
        let out = recent_lines(&events, "day", 2);
        assert!(out.contains("showing 2 most recent"), "{out}");
        // Only the first 2 (newest-first input order) are rendered.
        assert!(out.contains("system.a") && out.contains("system.b"));
        assert!(!out.contains("system.c"));
    }

    #[test]
    fn recent_lines_empty() {
        assert_eq!(
            recent_lines(&[], "hour", MAX_RECENT_LIMIT),
            "No activity recorded in the last hour."
        );
    }

    #[test]
    fn summary_counts_by_category_excluding_secret() {
        let events = vec![
            ev(EventCategory::Sensor, "sensor.reading"),
            ev(EventCategory::Sensor, "sensor.reading"),
            ev(EventCategory::Network, "egress.http"),
            ev(EventCategory::Auth, "auth.token").sensitivity(PrivacySensitivity::Secret),
        ];
        let out = summary_text(&events, "day");
        assert!(out.contains("3 events"), "secret excluded: {out}");
        assert!(out.contains("- sensor: 2"));
        assert!(out.contains("- network: 1"));
        assert!(!out.contains("auth"));
        assert!(out.contains("1 external network call"));
    }

    #[test]
    fn privacy_report_groups_hosts_and_counts_sensitive() {
        let network = vec![
            ev(EventCategory::Network, "egress.http")
                .attr("host", "en.wikipedia.org")
                .attr("tool", "search_wikipedia"),
            ev(EventCategory::Network, "egress.http")
                .attr("host", "en.wikipedia.org")
                .attr("tool", "search_wikipedia"),
            ev(EventCategory::Network, "egress.http")
                .attr("host", "api.open-meteo.com")
                .attr("tool", "get_current_weather"),
        ];
        let all = vec![
            ev(EventCategory::Sensor, "sensor.reading").sensitivity(PrivacySensitivity::Sensitive),
            ev(EventCategory::System, "system.tick"),
        ];
        let out = privacy_report(&network, &all, "day");
        assert!(
            out.contains("en.wikipedia.org: 2 calls via search_wikipedia"),
            "{out}"
        );
        assert!(
            out.contains("api.open-meteo.com: 1 call via get_current_weather"),
            "{out}"
        );
        assert!(
            out.contains("1 event handled personal/sensitive data"),
            "{out}"
        );
    }

    #[test]
    fn privacy_report_no_calls() {
        let out = privacy_report(&[], &[], "week");
        assert!(out.contains("No external network calls recorded"));
        assert!(out.contains("0 events handled personal/sensitive data"));
    }
}
