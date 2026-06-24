//! Shared HTTP client for all Knowledge-family MCP servers.
//!
//! Single client pool with standard timeout, User-Agent, and connection settings.
//!
//! Every outbound request from a built-in tool MUST go through [`traced_get`] /
//! [`traced_get_with`] rather than `client.get(...).send()` directly. These
//! helpers are the egress choke point (#113): they record each call — host,
//! tool, HTTP method, status, latency — into the unified event store and
//! classify its privacy sensitivity, so the activity API (#114) can answer
//! "what did the system phone home to, and when?".

use reqwest::Client;
use std::time::Duration;

use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};

const USER_AGENT: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Build a shared HTTP client with standard GIAP configuration.
pub fn build_http_client() -> Client {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(DEFAULT_TIMEOUT)
        .pool_max_idle_per_host(4)
        .build()
        .expect("Failed to build HTTP client")
}

/// Perform a traced `GET` request. The destination host, the in-flight tool and
/// session, the status, and the wall-clock latency are recorded to the event
/// store (see module docs).
pub async fn traced_get(client: &Client, url: &str) -> reqwest::Result<reqwest::Response> {
    traced_get_with(client, url, |b| b).await
}

/// Like [`traced_get`] but lets the caller customise the request builder
/// (headers, per-call timeout, query, …) while still recording the egress.
/// Use this for sites that chain `.header(...)` / `.timeout(...)` etc.
pub async fn traced_get_with<F>(
    client: &Client,
    url: &str,
    customize: F,
) -> reqwest::Result<reqwest::Response>
where
    F: FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
{
    let builder = customize(client.get(url));
    send_traced(builder, "GET", url).await
}

/// Send a prepared request builder, recording the egress regardless of outcome.
async fn send_traced(
    builder: reqwest::RequestBuilder,
    method: &str,
    url: &str,
) -> reqwest::Result<reqwest::Response> {
    let host = extract_host(url).to_string();
    let tool = crate::current_tool();
    let session_id = crate::current_session_id();

    let start = std::time::Instant::now();
    let result = builder.send().await;
    let latency_ms = start.elapsed().as_millis() as u64;
    let status = result.as_ref().ok().map(|r| r.status().as_u16());

    // Operator-facing log line (unchanged behaviour).
    match &result {
        Ok(resp) => tracing::info!(
            target: "giap::trace",
            kind = "mcp_http",
            session_id = %session_id,
            tool = %tool,
            host = %host,
            status = resp.status().as_u16(),
            latency_ms,
        ),
        Err(e) => tracing::warn!(
            target: "giap::trace",
            kind = "mcp_http_error",
            session_id = %session_id,
            tool = %tool,
            host = %host,
            error = %e,
            latency_ms,
        ),
    }

    // Durable, queryable egress record (#113). Fire-and-forget so the tool's
    // own latency is never coupled to the observability write.
    if let Some(sink) = crate::egress_sink() {
        let event = egress_event(&host, &tool, &session_id, method, status, latency_ms);
        tokio::spawn(async move {
            if let Err(e) = sink.append(event).await {
                tracing::warn!(target: "giap::trace", error = %e, "failed to record egress event");
            }
        });
    }

    result
}

/// Build the `Network` event describing one outbound call. Pure and total so
/// the classification + shape can be unit-tested without a live client.
pub(crate) fn egress_event(
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
];

/// Classify a destination host's privacy sensitivity:
/// loopback → `Internal`, known public API → `Public`, anything else →
/// `Sensitive` (the safe default for unknown third parties).
fn classify_host(host: &str) -> PrivacySensitivity {
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
fn extract_host(url: &str) -> &str {
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
    fn client_builds_without_panic() {
        let _client = build_http_client();
    }

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
        ] {
            assert_eq!(
                classify_host(host),
                PrivacySensitivity::Public,
                "{host} should be Public"
            );
        }
        // Case-insensitive.
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
}
