//! Tool result cache domain types.
//!
//! Defines the cached tool result entry and default TTL configuration
//! for each tool type. Used by the `ToolCache` port to avoid redundant
//! tool calls for identical queries within the TTL window.

use std::time::{Duration, Instant};

/// Maximum number of entries in the tool cache before LRU eviction.
pub const DEFAULT_CACHE_CAPACITY: usize = 64;

/// Default TTL for weather tool results (5 minutes).
pub const TTL_WEATHER: Duration = Duration::from_secs(5 * 60);

/// Default TTL for Wikipedia tool results (1 hour).
pub const TTL_WIKIPEDIA: Duration = Duration::from_secs(60 * 60);

/// Default TTL for device registry queries (30 seconds).
pub const TTL_DEVICES: Duration = Duration::from_secs(30);

/// Default TTL for schedule queries (10 seconds).
pub const TTL_SCHEDULES: Duration = Duration::from_secs(10);

/// Default TTL for unknown tool types (2 minutes).
pub const TTL_DEFAULT: Duration = Duration::from_secs(2 * 60);

/// A cached tool result entry.
#[derive(Debug, Clone)]
pub struct CachedToolResult {
    /// The tool result content (the string returned by the tool).
    pub content: String,
    /// When this entry was cached.
    pub cached_at: Instant,
    /// How long this entry is valid.
    pub ttl: Duration,
    /// The tool name (e.g. "weather", "wikipedia").
    pub tool_name: String,
    /// The normalized cache key (e.g. "weather:what is the weather").
    pub cache_key: String,
    /// When this entry was last accessed (for LRU eviction).
    pub last_accessed: Instant,
}

impl CachedToolResult {
    /// Returns true if this entry has expired.
    pub fn is_expired(&self) -> bool {
        self.cached_at.elapsed() > self.ttl
    }
}

/// Returns true if a tool's results are safe to cache.
///
/// Mutation tools (save_memory, create_schedule) and personalized reads
/// (recall_memory) are excluded from caching because their results
/// depend on side effects or change with every invocation.
pub fn is_cacheable_tool(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "save_memory" | "recall_memory" | "forget_memory" | "create_schedule"
    )
}

/// Returns the default TTL for a given tool name.
///
/// Only called for cacheable tools (see [`is_cacheable_tool`]).
pub fn default_ttl_for_tool(tool_name: &str) -> Duration {
    match tool_name {
        "weather" => TTL_WEATHER,
        "wikipedia" => TTL_WIKIPEDIA,
        "devices" => TTL_DEVICES,
        "schedules" => TTL_SCHEDULES,
        _ => TTL_DEFAULT,
    }
}

/// Build a normalized cache key from tool name and query.
///
/// The query is lowercased and trimmed to ensure case-insensitive
/// matching and ignore leading/trailing whitespace.
pub fn make_cache_key(tool_name: &str, query: &str) -> String {
    format!("{}:{}", tool_name, query.to_lowercase().trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cacheable_tools() {
        assert!(is_cacheable_tool("weather"));
        assert!(is_cacheable_tool("wikipedia"));
        assert!(is_cacheable_tool("devices"));
        assert!(is_cacheable_tool("schedules"));
    }

    #[test]
    fn non_cacheable_tools() {
        assert!(!is_cacheable_tool("save_memory"));
        assert!(!is_cacheable_tool("recall_memory"));
        assert!(!is_cacheable_tool("forget_memory"));
        assert!(!is_cacheable_tool("create_schedule"));
    }

    #[test]
    fn default_ttls_match_spec() {
        assert_eq!(default_ttl_for_tool("weather"), Duration::from_secs(300));
        assert_eq!(default_ttl_for_tool("wikipedia"), Duration::from_secs(3600));
        assert_eq!(default_ttl_for_tool("devices"), Duration::from_secs(30));
        assert_eq!(default_ttl_for_tool("schedules"), Duration::from_secs(10));
        assert_eq!(default_ttl_for_tool("unknown"), Duration::from_secs(120));
    }

    #[test]
    fn cache_key_normalized() {
        assert_eq!(
            make_cache_key("weather", "What Is The Weather?"),
            "weather:what is the weather?"
        );
        assert_eq!(
            make_cache_key("wikipedia", "  Rust Language  "),
            "wikipedia:rust language"
        );
    }

    #[test]
    fn cached_result_expiry() {
        let entry = CachedToolResult {
            content: "sunny".to_string(),
            cached_at: Instant::now(),
            ttl: Duration::from_secs(300),
            tool_name: "weather".to_string(),
            cache_key: "weather:test".to_string(),
            last_accessed: Instant::now(),
        };
        assert!(!entry.is_expired());

        // Entry with zero TTL should be expired immediately
        let expired_entry = CachedToolResult {
            content: "old".to_string(),
            cached_at: Instant::now() - Duration::from_secs(10),
            ttl: Duration::from_secs(0),
            tool_name: "test".to_string(),
            cache_key: "test:query".to_string(),
            last_accessed: Instant::now(),
        };
        assert!(expired_entry.is_expired());
    }
}
