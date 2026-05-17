//! Shared HTTP client for all Knowledge-family MCP servers.
//!
//! Single client pool with standard timeout, User-Agent, and connection settings.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds_without_panic() {
        let _client = build_http_client();
    }
}
