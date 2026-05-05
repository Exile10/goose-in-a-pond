//! Background memory extraction service.
//!
//! After each conversation turn, calls the `MemoryExtractor` port to pull
//! durable facts, deduplicates against existing memories, and stores them.
//! Runs asynchronously — must never block the SSE chat stream.

use crate::domain::memory::MemoryFragment;
use crate::ports::memory_extractor::MemoryExtractor;
use crate::ports::memory_repository::MemoryRepository;
use std::sync::Arc;
use tokio::sync::Mutex;

const MIN_MESSAGE_LEN: usize = 15;

pub struct MemoryExtractionService {
    last_run: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
    /// Minimum seconds between extraction runs.
    interval_secs: i64,
}

impl MemoryExtractionService {
    pub fn new(interval_secs: u32) -> Self {
        Self {
            last_run: Mutex::new(None),
            interval_secs: interval_secs as i64,
        }
    }

    /// Run extraction for a conversation turn.
    pub async fn run(
        &self,
        extractor: &dyn MemoryExtractor,
        repo: &dyn MemoryRepository,
        user_message: &str,
        assistant_response: &str,
        session_id: Option<&str>,
    ) {
        if user_message.len() < MIN_MESSAGE_LEN && assistant_response.len() < MIN_MESSAGE_LEN {
            return;
        }

        // Rate limit
        {
            let mut last = self.last_run.lock().await;
            let now = chrono::Utc::now();
            if let Some(prev) = *last {
                if (now - prev).num_seconds() < self.interval_secs {
                    tracing::debug!("[memory-extraction] rate-limited, skipping");
                    return;
                }
            }
            *last = Some(now);
        }

        // Fetch recent memories for dedup
        let existing: Vec<String> = repo
            .search_recent(None, 20)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.content.to_lowercase())
            .collect();

        // Extract facts
        let facts = match extractor
            .extract(user_message, assistant_response, &existing)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("[memory-extraction] extraction failed: {e}");
                return;
            }
        };

        if facts.is_empty() {
            tracing::debug!("[memory-extraction] no facts extracted");
            return;
        }

        // Deduplicate and store
        let mut stored = 0;
        for fact in facts {
            // Skip if content already exists (case-insensitive substring match)
            let lower = fact.content.to_lowercase();
            if existing
                .iter()
                .any(|e| e.contains(&lower) || lower.contains(e.as_str()))
            {
                tracing::debug!("[memory-extraction] dedup skipped: {:?}", fact.content);
                continue;
            }

            let fragment = MemoryFragment::from_extraction(
                uuid::Uuid::new_v4().to_string(),
                session_id.map(|s| s.to_string()),
                fact.content.clone(),
                fact.segment,
                fact.importance,
            );

            if let Err(e) = repo.add(fragment).await {
                tracing::warn!("[memory-extraction] failed to store fact: {e}");
            } else {
                stored += 1;
                tracing::info!("[memory-extraction] stored: {:?}", fact.content);
            }
        }

        if stored > 0 {
            tracing::info!("[memory-extraction] {stored} new memories from this turn");
        }
    }
}
