//! Shared HTTP client for all Knowledge-family MCP servers.
//!
//! Single client pool with standard timeout, User-Agent, and connection settings.
//! Use [`traced_get`] instead of bare `client.get().send()` so outbound requests
//! are recorded in the structured event trace (queryable by session).

use reqwest::Client;
use std::time::Duration;

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

/// Perform a GET request and emit a `giap::trace` event recording the
/// destination host, tool name, status code, and wall-clock latency.
/// The current session ID is read from the MCP-server global so the event
/// can be correlated with the turn that triggered the tool call.
pub async fn traced_get(
    client: &Client,
    url: &str,
    tool: &str,
) -> reqwest::Result<reqwest::Response> {
    let host = extract_host(url);
    let session_id = crate::current_session_id();
    let start = std::time::Instant::now();
    let result = client.get(url).send().await;
    let latency_ms = start.elapsed().as_millis() as u64;
    match &result {
        Ok(resp) => {
            tracing::info!(
                target: "giap::trace",
                kind = "mcp_http",
                session_id = %session_id,
                tool = %tool,
                host = %host,
                status = resp.status().as_u16(),
                latency_ms,
            );
        }
        Err(e) => {
            tracing::warn!(
                target: "giap::trace",
                kind = "mcp_http_error",
                session_id = %session_id,
                tool = %tool,
                host = %host,
                error = %e,
                latency_ms,
            );
        }
    }
    result
}

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
}
