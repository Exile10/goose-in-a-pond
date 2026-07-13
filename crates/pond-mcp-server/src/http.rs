//! Shared HTTP client for all Knowledge-family MCP servers.
//!
//! Single client pool with standard timeout, User-Agent, and connection settings.
//!
//! Every outbound request from a built-in tool MUST go through [`traced_get`] /
//! [`traced_get_with`] rather than `client.get(...).send()` directly. These
//! helpers are the egress choke point (#113): each call is reported to the
//! shared egress tracker in `pond_core::shared::services::egress`, which records
//! host / tool / method / status / latency into the unified event store and
//! classifies privacy sensitivity, so the activity API (#114) can answer
//! "what did the system phone home to, and when?".

use reqwest::Client;
use std::time::Duration;

use pond_core::shared::services::egress;

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
    let host = egress::extract_host(url);
    let tool = egress::current_tool();
    let session_id = egress::current_session_id();

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

    // Durable, queryable egress record (#113).
    egress::record_egress(url, method, status, latency_ms);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds_without_panic() {
        let _client = build_http_client();
    }
}
