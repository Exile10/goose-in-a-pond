//! In-memory LRU tool result cache.
//!
//! Implements `ToolCache` using a `Mutex<HashMap>` with LRU eviction
//! based on `last_accessed` timestamps. Suitable for pond-core since
//! it uses only `std` types (no external crate dependencies).

use crate::domain::tool_cache::{make_cache_key, CachedToolResult, DEFAULT_CACHE_CAPACITY};
use crate::ports::tool_cache::ToolCache;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// In-memory LRU tool result cache.
///
/// Thread-safe via `Mutex`. The lock is held only for HashMap operations
/// (microseconds per call). With a max capacity of 64 entries, LRU
/// eviction scans are trivially fast.
pub struct InMemoryToolCache {
    entries: Mutex<HashMap<String, CachedToolResult>>,
    capacity: usize,
}

impl InMemoryToolCache {
    /// Create a new cache with the default capacity (64 entries).
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    /// Create a new cache with a custom capacity.
    ///
    /// Primarily useful for testing LRU eviction with small capacities.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::with_capacity(capacity)),
            capacity,
        }
    }
}

impl Default for InMemoryToolCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCache for InMemoryToolCache {
    fn get(&self, tool_name: &str, query: &str) -> Option<String> {
        let key = make_cache_key(tool_name, query);
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        let entry = map.get_mut(&key)?;

        if entry.is_expired() {
            // Expired — remove and return miss
            map.remove(&key);
            return None;
        }

        // Update last_accessed for LRU tracking
        entry.last_accessed = Instant::now();
        Some(entry.content.clone())
    }

    fn put(&self, tool_name: &str, query: &str, result: String, ttl: Duration) {
        let key = make_cache_key(tool_name, query);
        let now = Instant::now();
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        // If the key already exists, update in place
        if map.contains_key(&key) {
            if let Some(entry) = map.get_mut(&key) {
                entry.content = result;
                entry.cached_at = now;
                entry.ttl = ttl;
                entry.last_accessed = now;
            }
            return;
        }

        // Evict expired entries first
        map.retain(|_, v| !v.is_expired());

        // If still at capacity, evict the least-recently-accessed entry
        if map.len() >= self.capacity {
            if let Some(lru_key) = map
                .iter()
                .min_by_key(|(_, v)| v.last_accessed)
                .map(|(k, _)| k.clone())
            {
                map.remove(&lru_key);
            }
        }

        map.insert(
            key.clone(),
            CachedToolResult {
                content: result,
                cached_at: now,
                ttl,
                tool_name: tool_name.to_string(),
                cache_key: key,
                last_accessed: now,
            },
        );
    }

    fn invalidate(&self, tool_name: &str) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, v| v.tool_name != tool_name);
    }

    fn clear(&self) {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_miss_returns_none() {
        let cache = InMemoryToolCache::new();
        assert!(cache.get("weather", "what is the weather").is_none());
    }

    #[test]
    fn cache_hit_returns_stored_result() {
        let cache = InMemoryToolCache::new();
        cache.put(
            "weather",
            "what is the weather",
            "Sunny, 25C".to_string(),
            Duration::from_secs(300),
        );
        let result = cache.get("weather", "what is the weather");
        assert_eq!(result, Some("Sunny, 25C".to_string()));
    }

    #[test]
    fn cache_key_is_case_insensitive_and_trimmed() {
        let cache = InMemoryToolCache::new();
        cache.put(
            "wikipedia",
            "Rust Language",
            "Rust is a systems language.".to_string(),
            Duration::from_secs(3600),
        );

        // Same query with different casing and whitespace should hit
        assert_eq!(
            cache.get("wikipedia", "rust language"),
            Some("Rust is a systems language.".to_string())
        );
        assert_eq!(
            cache.get("wikipedia", "  RUST LANGUAGE  "),
            Some("Rust is a systems language.".to_string())
        );
    }

    #[test]
    fn ttl_expiry_invalidates_entry() {
        let cache = InMemoryToolCache::new();

        // Insert with zero TTL — immediately expired
        cache.put(
            "weather",
            "test",
            "old data".to_string(),
            Duration::from_secs(0),
        );

        // Should miss because TTL expired
        assert!(cache.get("weather", "test").is_none());
    }

    #[test]
    fn lru_eviction_when_capacity_exceeded() {
        let cache = InMemoryToolCache::with_capacity(3);

        // Fill cache to capacity
        cache.put("a", "q1", "r1".to_string(), Duration::from_secs(3600));
        cache.put("b", "q2", "r2".to_string(), Duration::from_secs(3600));
        cache.put("c", "q3", "r3".to_string(), Duration::from_secs(3600));

        // Access "a" and "c" to bump their last_accessed timestamps.
        // "b" remains the only entry that was never re-accessed, making it
        // the deterministic LRU victim regardless of Instant resolution.
        assert_eq!(cache.get("a", "q1"), Some("r1".to_string()));
        assert_eq!(cache.get("c", "q3"), Some("r3".to_string()));

        // Insert 4th entry — should evict "b" (the only entry never re-accessed)
        cache.put("d", "q4", "r4".to_string(), Duration::from_secs(3600));

        // "a" should still be present (recently accessed)
        assert_eq!(cache.get("a", "q1"), Some("r1".to_string()));
        // "d" should be present (just inserted)
        assert_eq!(cache.get("d", "q4"), Some("r4".to_string()));
        // "c" should still be present (recently accessed)
        assert_eq!(cache.get("c", "q3"), Some("r3".to_string()));
        // "b" should be evicted (least recently accessed — never re-accessed)
        assert!(cache.get("b", "q2").is_none());
    }

    #[test]
    fn invalidate_removes_tool_specific_entries() {
        let cache = InMemoryToolCache::new();

        cache.put("weather", "q1", "w1".to_string(), Duration::from_secs(300));
        cache.put("weather", "q2", "w2".to_string(), Duration::from_secs(300));
        cache.put("wikipedia", "q3", "wp1".to_string(), Duration::from_secs(3600));

        // Invalidate weather
        cache.invalidate("weather");

        // Weather entries should be gone
        assert!(cache.get("weather", "q1").is_none());
        assert!(cache.get("weather", "q2").is_none());

        // Wikipedia should still be present
        assert_eq!(cache.get("wikipedia", "q3"), Some("wp1".to_string()));
    }

    #[test]
    fn clear_empties_everything() {
        let cache = InMemoryToolCache::new();

        cache.put("weather", "q1", "w1".to_string(), Duration::from_secs(300));
        cache.put("wikipedia", "q2", "wp1".to_string(), Duration::from_secs(3600));

        cache.clear();

        assert!(cache.get("weather", "q1").is_none());
        assert!(cache.get("wikipedia", "q2").is_none());
    }

    #[test]
    fn put_updates_existing_entry() {
        let cache = InMemoryToolCache::new();

        cache.put("weather", "q1", "old".to_string(), Duration::from_secs(300));
        cache.put("weather", "q1", "new".to_string(), Duration::from_secs(300));

        assert_eq!(cache.get("weather", "q1"), Some("new".to_string()));
    }

    #[test]
    fn expired_entries_evicted_before_lru() {
        let cache = InMemoryToolCache::with_capacity(2);

        // Insert two entries, one expired
        cache.put("a", "q1", "r1".to_string(), Duration::from_secs(0)); // immediately expired
        cache.put("b", "q2", "r2".to_string(), Duration::from_secs(3600));

        // Insert 3rd — expired entry should be cleaned up first, no LRU eviction needed
        cache.put("c", "q3", "r3".to_string(), Duration::from_secs(3600));

        // "b" should survive (not evicted)
        assert_eq!(cache.get("b", "q2"), Some("r2".to_string()));
        // "c" should be present
        assert_eq!(cache.get("c", "q3"), Some("r3".to_string()));
    }

    #[test]
    fn different_tools_same_query_cached_separately() {
        let cache = InMemoryToolCache::new();

        cache.put("weather", "test", "sunny".to_string(), Duration::from_secs(300));
        cache.put("wikipedia", "test", "article".to_string(), Duration::from_secs(3600));

        assert_eq!(cache.get("weather", "test"), Some("sunny".to_string()));
        assert_eq!(cache.get("wikipedia", "test"), Some("article".to_string()));
    }
}
