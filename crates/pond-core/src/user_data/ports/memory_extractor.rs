//! MemoryExtractor port — extracts durable facts from conversation turns.
//!
//! Called asynchronously after each chat exchange. The adapter uses the LLM
//! to classify facts into segments with importance scores.

use crate::user_data::domain::memory::{MemorySegment, MemoryTier};
use anyhow::Result;
use async_trait::async_trait;

/// A single fact extracted from a conversation turn.
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub content: String,
    pub segment: MemorySegment,
    pub importance: f32,
    pub tier: MemoryTier,
    /// For correction segments: describes the wrong claim being fixed.
    /// Prevents consolidation from accidentally reverting the correction.
    pub corrects: Option<String>,
}

/// Driven port: extract durable facts from a user–assistant exchange.
#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    /// Analyse a conversation turn and return facts worth remembering.
    ///
    /// `existing_content` contains recent memory content strings for dedup.
    /// Implementations should return at most 3 facts per turn.
    async fn extract(
        &self,
        user_message: &str,
        assistant_response: &str,
        existing_content: &[String],
    ) -> Result<Vec<ExtractedFact>>;
}
