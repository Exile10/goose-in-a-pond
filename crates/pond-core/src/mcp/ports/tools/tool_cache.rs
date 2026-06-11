//! ToolCache port — in-memory cache for deterministic tool results.
//!
//! Reduces redundant API calls (weather, Wikipedia, device queries) by
//! caching results with per-tool TTLs. The cache is purely in-memory
//! with LRU eviction — no persistence across restarts.
//!
//! All methods are synchronous because the cache is in-memory with no I/O.
//! Implementations must be `Send + Sync` for shared ownership across
//! tokio tasks.

use std::time::Duration;

/// Driven Port: tool result cache.
///
/// Caches the string results of tool calls (weather, Wikipedia, etc.)
/// keyed by tool name + normalized query. Entries expire after a
/// tool-specific TTL and are evicted LRU when capacity is reached.
pub trait ToolCache: Send + Sync {
    /// Look up a cached result for the given tool and query.
    ///
    /// Returns `Some(content)` if a non-expired entry exists, `None` otherwise.
    /// A cache hit updates the entry's last-accessed time for LRU tracking.
    fn get(&self, tool_name: &str, query: &str) -> Option<String>;

    /// Store a tool result in the cache with the given TTL.
    ///
    /// If the cache is at capacity, the least-recently-accessed entry is evicted.
    fn put(&self, tool_name: &str, query: &str, result: String, ttl: Duration);

    /// Remove all cached entries for the given tool name.
    ///
    /// Useful when tool configuration changes (e.g. weather location update).
    fn invalidate(&self, tool_name: &str);

    /// Remove all cached entries.
    fn clear(&self);
}
