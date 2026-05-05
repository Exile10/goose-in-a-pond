//! LLM-based memory consolidator — single-pass merge/prune using the live model.
//!
//! Simplified from boop-agent's 3-phase (proposer/adversary/judge) to a single
//! compact prompt suitable for 3B models.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::domain::memory::{MemoryFragment, MemorySegment};
use pond_core::domain::message::ChatMessage;
use pond_core::ports::memory_consolidator::{ConsolidationAction, MemoryConsolidator};
use pond_core::ports::provider::LlmProvider;
use std::sync::Arc;
use tokio::sync::RwLock;

const CONSOLIDATION_PROMPT: &str = "\
Review these memories and find duplicates or redundancies.
Output a JSON array of actions:
- To merge duplicates: {\"action\":\"merge\",\"ids\":[\"id1\",\"id2\"],\"merged\":\"combined fact\",\"segment\":\"...\",\"importance\":0.0-1.0}
- To prune redundant: {\"action\":\"prune\",\"id\":\"id1\"}
- If nothing to change: []
Be conservative: only merge truly duplicate facts. Keep distinct facts separate.
Output ONLY the JSON array.";

pub struct LlmMemoryConsolidator {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl LlmMemoryConsolidator {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { live_provider }
    }
}

#[async_trait]
impl MemoryConsolidator for LlmMemoryConsolidator {
    async fn consolidate(&self, memories: &[MemoryFragment]) -> Result<Vec<ConsolidationAction>> {
        let provider = {
            let guard = self.live_provider.read().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| anyhow!("no LLM provider"))?
        };

        // Format memories for the prompt (max 20 to stay within context)
        let batch: Vec<_> = memories.iter().take(20).collect();
        let formatted = batch
            .iter()
            .map(|m| {
                let seg = m
                    .segment
                    .as_ref()
                    .map(|s| format!("{:?}", s).to_lowercase())
                    .unwrap_or_else(|| "unknown".to_string());
                let imp = m.importance.unwrap_or(0.5);
                format!("[{}] ({}, {:.1}) {}", m.id, seg, imp, m.content)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let messages = vec![ChatMessage::user(format!("Memories:\n{formatted}"))];
        let response = provider.complete(CONSOLIDATION_PROMPT, messages).await?;

        parse_consolidation_response(&response.content)
    }
}

fn parse_consolidation_response(raw: &str) -> Result<Vec<ConsolidationAction>> {
    let text = raw.trim();

    // Strip thinking tokens
    let cleaned = crate::llm_memory_extractor::strip_thinking(text);

    // Try to find JSON array
    let json_str = if cleaned.starts_with('[') {
        cleaned.to_string()
    } else if let Some(start) = cleaned.find('[') {
        if let Some(end) = cleaned.rfind(']') {
            cleaned[start..=end].to_string()
        } else {
            return Ok(vec![]);
        }
    } else {
        return Ok(vec![]);
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut actions = Vec::new();

    for v in &arr {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        match action {
            "merge" => {
                let ids: Vec<String> = v
                    .get("ids")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|i| i.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let merged = v
                    .get("merged")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let segment = v
                    .get("segment")
                    .and_then(|s| s.as_str())
                    .and_then(|s| crate::llm_memory_extractor::parse_segment_str(s))
                    .unwrap_or(MemorySegment::Knowledge);
                let importance = v
                    .get("importance")
                    .and_then(|i| i.as_f64())
                    .map(|i| (i as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.5);

                if ids.len() >= 2 && !merged.is_empty() {
                    actions.push(ConsolidationAction::Merge {
                        source_ids: ids,
                        merged_content: merged,
                        segment,
                        importance,
                    });
                }
            }
            "prune" => {
                if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                    actions.push(ConsolidationAction::Prune { id: id.to_string() });
                }
            }
            _ => {}
        }
    }

    Ok(actions)
}
