use std::time::Duration;

/// Configuration for the HTTP device controller.
#[derive(Debug, Clone)]
pub struct HttpDeviceConfig {
    /// Per-request timeout. Defaults to 5 s — long enough for a slow plug, short
    /// enough to report "unreachable" quickly.
    pub timeout: Duration,
    /// Optional Bearer token added to every request (`Authorization: Bearer <token>`).
    pub bearer_token: Option<String>,
}

impl Default for HttpDeviceConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            bearer_token: None,
        }
    }
}

impl HttpDeviceConfig {
    /// Read config from env vars. Unset vars fall back to defaults.
    ///
    /// Variables: HTTP_DEVICE_TIMEOUT_SECS, HTTP_DEVICE_TOKEN
    pub fn from_env() -> Self {
        let timeout_secs = std::env::var("HTTP_DEVICE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);
        let bearer_token =
            std::env::var("HTTP_DEVICE_TOKEN").ok().filter(|s| !s.is_empty());
        Self {
            timeout: Duration::from_secs(timeout_secs),
            bearer_token,
        }
    }
}
