//! Network-egress tracker (#113) — shared, cross-crate.
//!
//! Records every outbound HTTP call built-in tools make into the unified event
//! store and classifies its privacy sensitivity, so the activity API (#114) can
//! answer "what did the system phone home to, and when?".
//!
//! This lives in `pond-core` (rather than `pond-mcp-server`) so that *any*
//! adapter — the MCP servers AND outboard adapters like `pond-adapters-weather`
//! — can report egress into one place without depending on each other. Adapters
//! report; core owns the policy.
//!
//! ## Request context
//! `current_session_id` / `current_tool` are process-global, set by the engine
//! before/around each turn and tool call; the recorder reads them so every
//! egress event is correlated to the turn and attributed to the tool that
//! triggered it. (Same shape as the rest of GIAP's per-turn context.)

use std::sync::{Arc, OnceLock, RwLock};

use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::security::ports::event_log::EventLog;

// ── Request context ──────────────────────────────────────────────────────────

static CURRENT_SESSION_ID: RwLock<String> = RwLock::new(String::new());
static CURRENT_TOOL: RwLock<String> = RwLock::new(String::new());

/// Record the session ID for the turn currently in flight.
pub fn set_current_session_id(sid: &str) {
    if let Ok(mut guard) = CURRENT_SESSION_ID.write() {
        *guard = sid.to_string();
    }
}

/// The session ID for the in-flight turn, or an empty string if none is set.
pub fn current_session_id() -> String {
    CURRENT_SESSION_ID
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Record the built-in tool about to run, so its egress can be attributed.
pub fn set_current_tool(tool: &str) {
    if let Ok(mut guard) = CURRENT_TOOL.write() {
        *guard = tool.to_string();
    }
}

/// The tool currently in flight, or an empty string if none is set.
pub fn current_tool() -> String {
    CURRENT_TOOL.read().map(|g| g.clone()).unwrap_or_default()
}

// ── Sink ─────────────────────────────────────────────────────────────────────

/// The unified event store every outbound call is recorded into. Set once at
/// startup; when unset (tests, standalone adapter use) the tracker is a silent
/// no-op.
static EGRESS_SINK: OnceLock<Arc<dyn EventLog>> = OnceLock::new();

/// Install the durable sink the egress tracker writes to. Call once at startup;
/// later calls are ignored (the first wins).
pub fn set_egress_sink(sink: Arc<dyn EventLog>) {
    let _ = EGRESS_SINK.set(sink);
}

fn egress_sink() -> Option<Arc<dyn EventLog>> {
    EGRESS_SINK.get().cloned()
}

// ── Recording ──────────────────────────────────────────────────────────────--

/// Record one outbound HTTP call (any caller, any built-in tool). Reads the
/// in-flight tool + session from the request context, classifies the host, and
/// appends a `Network` event to the sink fire-and-forget so a slow observability
/// write never adds latency to the call. No-op when no sink is installed.
pub fn record_egress(url: &str, method: &str, status: Option<u16>, latency_ms: u64) {
    let Some(sink) = egress_sink() else {
        return;
    };
    let host = extract_host(url).to_string();
    let tool = current_tool();
    let session_id = current_session_id();
    let event = egress_event(&host, &tool, &session_id, method, status, latency_ms);
    tokio::spawn(async move {
        if let Err(e) = sink.append(event).await {
            tracing::warn!(target: "giap::trace", error = %e, "failed to record egress event");
        }
    });
}

/// Build the `Network` event describing one outbound call. Pure and total so the
/// classification + shape can be unit-tested without a live client or sink.
pub fn egress_event(
    host: &str,
    tool: &str,
    session_id: &str,
    method: &str,
    status: Option<u16>,
    latency_ms: u64,
) -> Event {
    let mut event = Event::new(EventCategory::Network, "egress.http")
        .attr("host", host)
        .attr("method", method)
        .attr("latency_ms", latency_ms as i64)
        .sensitivity(classify_host(host));
    if !tool.is_empty() {
        event = event.attr("tool", tool);
    }
    if !session_id.is_empty() {
        event = event.session(session_id);
    }
    if let Some(code) = status {
        event = event.attr("status", code as i64);
    }
    event
}

// ── Privacy classification ─────────────────────────────────────────────────--

/// Curated allowlist of public, read-only informational APIs the built-in tools
/// call. Matched as exact host or sub-domain. Kept narrow on purpose: anything
/// not on it (including user-configured search backends) is treated as
/// privacy-`Sensitive` by default.
const KNOWN_PUBLIC_SUFFIXES: &[&str] = &[
    "wikipedia.org",
    "wikimedia.org",
    "duckduckgo.com",
    "dictionaryapi.dev",
    "openlibrary.org",
    "restcountries.com",
    "openfoodfacts.org",
    "frankfurter.dev",
    "finnhub.io",
    "coingecko.com",
    "finance.yahoo.com",
    "hacker-news.firebaseio.com",
    "ycombinator.com",
    "guardianapis.com",
    "theguardian.com",
    "gnews.io",
    "open-meteo.com",
];

/// Classify a destination host's privacy sensitivity:
/// loopback → `Internal`, known public API → `Public`, anything else →
/// `Sensitive` (the safe default for unknown third parties).
pub fn classify_host(host: &str) -> PrivacySensitivity {
    let h = host.trim().to_ascii_lowercase();
    if is_loopback(&h) {
        return PrivacySensitivity::Internal;
    }
    if KNOWN_PUBLIC_SUFFIXES
        .iter()
        .any(|s| h == *s || h.ends_with(&format!(".{s}")))
    {
        return PrivacySensitivity::Public;
    }
    PrivacySensitivity::Sensitive
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host == "127.0.0.1"
        || host.starts_with("127.")
        || host.ends_with(".localhost")
}

/// Extract the bare host from a URL (no scheme, port, or path).
pub fn extract_host(url: &str) -> &str {
    url.find("://")
        .map(|i| &url[i + 3..])
        .and_then(|s| s.split('/').next())
        .and_then(|h| h.split(':').next())
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_host_from_url() {
        assert_eq!(
            extract_host("https://en.wikipedia.org/w/api.php?x=1"),
            "en.wikipedia.org"
        );
        assert_eq!(extract_host("http://127.0.0.1:8080/foo"), "127.0.0.1");
        assert_eq!(extract_host("not a url"), "unknown");
    }

    #[test]
    fn classifies_loopback_as_internal() {
        assert_eq!(classify_host("localhost"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("127.0.0.1"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("::1"), PrivacySensitivity::Internal);
        assert_eq!(
            classify_host("searxng.localhost"),
            PrivacySensitivity::Internal
        );
    }

    #[test]
    fn classifies_known_apis_as_public() {
        for host in [
            "en.wikipedia.org",
            "api.duckduckgo.com",
            "api.coingecko.com",
            "hacker-news.firebaseio.com",
            "api.open-meteo.com",
            "geocoding-api.open-meteo.com",
        ] {
            assert_eq!(
                classify_host(host),
                PrivacySensitivity::Public,
                "{host} should be Public"
            );
        }
        assert_eq!(
            classify_host("EN.WIKIPEDIA.ORG"),
            PrivacySensitivity::Public
        );
    }

    #[test]
    fn classifies_unknown_hosts_as_sensitive() {
        assert_eq!(
            classify_host("tracker.example.com"),
            PrivacySensitivity::Sensitive
        );
        // A look-alike suffix must not match (no substring/false-positive).
        assert_eq!(
            classify_host("notwikipedia.org.evil.com"),
            PrivacySensitivity::Sensitive
        );
    }

    #[test]
    fn egress_event_shape_and_classification() {
        let ev = egress_event(
            "en.wikipedia.org",
            "search_wikipedia",
            "sess-1",
            "GET",
            Some(200),
            42,
        );
        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.action, "egress.http");
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(ev.session_id.as_deref(), Some("sess-1"));
        assert_eq!(ev.attributes.get("host"), Some(&"en.wikipedia.org".into()));
        assert_eq!(ev.attributes.get("tool"), Some(&"search_wikipedia".into()));
        assert_eq!(ev.attributes.get("method"), Some(&"GET".into()));
        assert_eq!(ev.attributes.get("status"), Some(&200_i64.into()));
        assert_eq!(ev.attributes.get("latency_ms"), Some(&42_i64.into()));
    }

    #[test]
    fn egress_event_omits_empty_fields_and_missing_status() {
        let ev = egress_event("tracker.example.com", "", "", "GET", None, 5);
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Sensitive);
        assert!(ev.session_id.is_none());
        assert!(!ev.attributes.contains_key("tool"));
        assert!(!ev.attributes.contains_key("status"));
    }

    #[tokio::test]
    async fn record_egress_appends_to_sink_with_context() {
        use crate::security::domain::event::EventQuery;
        use crate::security::ports::event_log::EventLog;
        use async_trait::async_trait;
        use std::sync::Mutex;

        // Capturing sink.
        struct CapturingLog(Arc<Mutex<Vec<Event>>>);
        #[async_trait]
        impl EventLog for CapturingLog {
            async fn append(&self, event: Event) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
            async fn query(&self, _q: EventQuery) -> anyhow::Result<Vec<Event>> {
                Ok(self.0.lock().unwrap().clone())
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        set_egress_sink(Arc::new(CapturingLog(captured.clone())));
        set_current_session_id("sess-xyz");
        set_current_tool("get_current_weather");

        record_egress(
            "https://api.open-meteo.com/v1/forecast?x=1",
            "GET",
            Some(200),
            12,
        );

        // record_egress spawns the append; give it a moment to land.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.lock().unwrap().clone();
        let ev = events
            .iter()
            .find(|e| e.attributes.get("host") == Some(&"api.open-meteo.com".into()))
            .expect("egress event recorded");
        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(ev.session_id.as_deref(), Some("sess-xyz"));
        assert_eq!(
            ev.attributes.get("tool"),
            Some(&"get_current_weather".into())
        );
        assert_eq!(ev.attributes.get("status"), Some(&200_i64.into()));
    }
}
