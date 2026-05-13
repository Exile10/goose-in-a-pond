//! Adversarial memory consolidator — two-agent protocol using the live LLM.
//!
//! Step 1 (Prosecutor): proposes prune/merge actions with rationale.
//! Step 2 (Defender): challenges each proposal, agreeing or denying.
//! Only proposals the Defender agrees with are accepted.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::domain::memory::{MemoryFragment, MemorySegment};
use pond_core::domain::message::ChatMessage;
use pond_core::ports::adversarial_consolidator::AdversarialConsolidator;
use pond_core::ports::memory_consolidator::{
    AdversarialConsolidationResult, AdversarialExchange, ConsolidationAction, ConsolidationEvent,
    ConsolidationProposal, DefenderVerdict,
};
use pond_core::ports::provider::LlmProvider;
use std::sync::Arc;
use tokio::sync::RwLock;

const PROSECUTOR_PROMPT: &str = "\
Respond with a JSON array ONLY. No explanation, no thinking, no preamble.

You are reviewing user memories for cleanup. For each redundant or outdated memory, add an entry:
[{\"action\":\"prune\",\"id\":\"MEMORY_ID\",\"reason\":\"brief reason\"},{\"action\":\"merge\",\"ids\":[\"ID1\",\"ID2\"],\"merged\":\"combined text\",\"segment\":\"preference\",\"importance\":0.7,\"reason\":\"brief reason\"}]

Rules: Keep identity facts. Keep unique preferences. Prune expired temporal items. Merge duplicates.
If nothing to remove, respond with exactly: []";

const DEFENDER_PROMPT: &str = "\
Respond with a JSON array ONLY. No explanation, no thinking, no preamble.

For each prosecutor proposal, decide AGREE or DENY:
[{\"index\":0,\"agreed\":true,\"reason\":\"brief reason\"},{\"index\":1,\"agreed\":false,\"reason\":\"brief reason\"}]

Rules: DENY if unique info, user identity, or active. AGREE only if truly redundant/obsolete.
If no proposals, respond with exactly: []";

pub struct AdversarialMemoryConsolidator {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl AdversarialMemoryConsolidator {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { live_provider }
    }

    /// Acquire a clone of the current LLM provider.
    async fn get_provider(&self) -> Result<Arc<dyn LlmProvider>> {
        let guard = self.live_provider.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("no LLM provider available"))
    }
}

#[async_trait]
impl AdversarialConsolidator for AdversarialMemoryConsolidator {
    async fn consolidate(
        &self,
        memories: &[MemoryFragment],
        event_tx: Option<tokio::sync::mpsc::Sender<ConsolidationEvent>>,
    ) -> Result<AdversarialConsolidationResult> {
        let provider = self.get_provider().await?;

        // Notify: started
        if let Some(ref tx) = event_tx {
            let _ = tx
                .send(ConsolidationEvent::Started {
                    memory_count: memories.len(),
                })
                .await;
        }

        // ── Format memories ────────────────────────────────────────────────
        let batch: Vec<&MemoryFragment> = memories.iter().take(20).collect();
        let formatted = format_memories(&batch);

        // ── Step 1: Prosecutor ─────────────────────────────────────────────
        // Combine instructions + data in a single user message with a minimal
        // system prompt.  This prevents thinking-capable models (Gemma 4) from
        // entering a reasoning-only mode that consumes all generation tokens
        // and never outputs the JSON array.
        let prosecutor_input = format!("{}\n\nMemories:\n{formatted}", PROSECUTOR_PROMPT);
        let prosecutor_messages = vec![ChatMessage::user(prosecutor_input.clone())];
        // Use complete_raw() to get the full model output INCLUDING thinking
        // blocks.  Thinking-capable models (Gemma 4) often put structured JSON
        // inside their reasoning tags; strip_thinking_tokens() would delete it.
        let prosecutor_response = provider
            .complete_raw("You output JSON arrays only.", prosecutor_messages)
            .await?;

        tracing::info!(
            response_len = prosecutor_response.content.len(),
            response_preview = %prosecutor_response.content.chars().take(800).collect::<String>(),
            "Prosecutor LLM response"
        );

        let proposals = parse_prosecutor_response(&prosecutor_response.content)?;
        tracing::info!(count = proposals.len(), "Prosecutor proposals parsed");

        // Emit prosecutor proposals
        if let Some(ref tx) = event_tx {
            for proposal in &proposals {
                let _ = tx
                    .send(ConsolidationEvent::ProsecutorProposal {
                        proposal: proposal.clone(),
                    })
                    .await;
            }
        }

        if proposals.is_empty() {
            let result = AdversarialConsolidationResult {
                exchanges: vec![],
                accepted_count: 0,
                rejected_count: 0,
            };
            if let Some(ref tx) = event_tx {
                let _ = tx
                    .send(ConsolidationEvent::Completed {
                        result: result.clone(),
                    })
                    .await;
            }
            return Ok(result);
        }

        // ── Step 2: Defender ───────────────────────────────────────────────
        let proposals_text = format_proposals(&proposals);
        let defender_input = format!(
            "{}\n\nMemories:\n{formatted}\n\nProsecutor proposals:\n{proposals_text}",
            DEFENDER_PROMPT
        );
        let defender_messages = vec![ChatMessage::user(defender_input)];
        let defender_response = provider
            .complete_raw("You output JSON arrays only.", defender_messages)
            .await?;

        tracing::info!(
            response_len = defender_response.content.len(),
            response_preview = %defender_response.content.chars().take(800).collect::<String>(),
            "Defender LLM response"
        );

        let verdicts = parse_defender_response(&defender_response.content, proposals.len())?;
        tracing::info!(count = verdicts.len(), "Defender verdicts parsed");

        // ── Build exchanges ────────────────────────────────────────────────
        let mut exchanges = Vec::with_capacity(proposals.len());
        let mut accepted_count = 0;
        let mut rejected_count = 0;

        for (i, proposal) in proposals.into_iter().enumerate() {
            let verdict = verdicts.get(i).cloned().unwrap_or(DefenderVerdict {
                agreed: false,
                rationale: "No verdict received — defaulting to deny".to_string(),
            });

            let accepted = verdict.agreed;
            if accepted {
                accepted_count += 1;
            } else {
                rejected_count += 1;
            }

            let exchange = AdversarialExchange {
                proposal,
                verdict,
                accepted,
            };

            if let Some(ref tx) = event_tx {
                let _ = tx
                    .send(ConsolidationEvent::DefenderVerdict {
                        exchange: exchange.clone(),
                    })
                    .await;
            }

            exchanges.push(exchange);
        }

        let result = AdversarialConsolidationResult {
            exchanges,
            accepted_count,
            rejected_count,
        };

        if let Some(ref tx) = event_tx {
            let _ = tx
                .send(ConsolidationEvent::Completed {
                    result: result.clone(),
                })
                .await;
        }

        tracing::info!(
            accepted = accepted_count,
            rejected = rejected_count,
            "[adversarial-consolidation] complete"
        );

        Ok(result)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Format memory fragments for inclusion in LLM prompts.
fn format_memories(memories: &[&MemoryFragment]) -> String {
    memories
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
        .join("\n")
}

/// Format proposals for the Defender prompt.
fn format_proposals(proposals: &[ConsolidationProposal]) -> String {
    proposals
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let action_desc = match &p.action {
                ConsolidationAction::Prune { id } => format!("PRUNE [{}]", id),
                ConsolidationAction::Merge {
                    source_ids,
                    merged_content,
                    ..
                } => format!(
                    "MERGE [{}] -> \"{}\"",
                    source_ids.join(", "),
                    merged_content
                ),
            };
            format!("#{}: {} — Reason: {}", i, action_desc, p.rationale)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the content INSIDE thinking tags (the model's reasoning).
/// Returns None if no thinking block is found.
fn extract_thinking_content(text: &str) -> Option<String> {
    // Gemma 4: <|channel>thought...<channel|>
    if let Some(start) = text.find("<|channel>") {
        let after = &text[start + 10..];
        let end = after.find("<channel|>").unwrap_or(after.len());
        let content = after[..end].trim();
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }
    // Qwen/DeepSeek: <think>...</think>
    if let Some(start) = text.find("<think>") {
        let after = &text[start + 7..];
        let end = after.find("</think>").unwrap_or(after.len());
        let content = after[..end].trim();
        if !content.is_empty() {
            return Some(content.to_string());
        }
    }
    None
}

/// Parse the Prosecutor's JSON response into proposals.
///
/// Some models (Gemma 4) wrap their entire output in thinking tags
/// and may or may not produce a JSON array. We look for JSON in:
/// 1. The text after stripping thinking blocks
/// 2. The raw text (which includes tag delimiters)
/// 3. Inside the thinking block content itself
fn parse_prosecutor_response(raw: &str) -> Result<Vec<ConsolidationProposal>> {
    let text = raw.trim();
    let cleaned = crate::llm_memory_extractor::strip_thinking(text);

    // Try cleaned text, then raw text, then thinking content
    let json_str = extract_json_array(&cleaned)
        .or_else(|| extract_json_array(text))
        .or_else(|| extract_thinking_content(text).and_then(|c| extract_json_array(&c)));
    let json_str = match json_str {
        Some(s) => s,
        None => {
            tracing::info!("Prosecutor produced no JSON array — model may have only reasoned without outputting structured data");
            return Ok(vec![]);
        }
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut proposals = Vec::new();

    for v in &arr {
        let action_str = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("no reason given")
            .to_string();

        match action_str {
            "prune" => {
                if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                    proposals.push(ConsolidationProposal {
                        action: ConsolidationAction::Prune { id: id.to_string() },
                        rationale: reason,
                    });
                }
            }
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
                    proposals.push(ConsolidationProposal {
                        action: ConsolidationAction::Merge {
                            source_ids: ids,
                            merged_content: merged,
                            segment,
                            importance,
                        },
                        rationale: reason,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(proposals)
}

/// Parse the Defender's JSON response into verdicts.
fn parse_defender_response(raw: &str, proposal_count: usize) -> Result<Vec<DefenderVerdict>> {
    let text = raw.trim();
    let cleaned = crate::llm_memory_extractor::strip_thinking(text);

    // Try cleaned text, then raw text, then thinking content
    let json_str = extract_json_array(&cleaned)
        .or_else(|| extract_json_array(text))
        .or_else(|| extract_thinking_content(text).and_then(|c| extract_json_array(&c)));
    let json_str = match json_str {
        Some(s) => s,
        None => {
            // No valid JSON — deny all proposals (conservative default)
            return Ok(vec![
                DefenderVerdict {
                    agreed: false,
                    rationale: "Could not parse defender response — defaulting to deny".to_string(),
                };
                proposal_count
            ]);
        }
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();

    // Build a map from index to verdict for out-of-order responses
    let mut verdict_map: Vec<Option<DefenderVerdict>> = vec![None; proposal_count];

    for v in &arr {
        let index = v.get("index").and_then(|i| i.as_u64()).map(|i| i as usize);
        let agreed = v.get("agreed").and_then(|a| a.as_bool()).unwrap_or(false);
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("no reason given")
            .to_string();

        if let Some(idx) = index {
            if idx < proposal_count {
                verdict_map[idx] = Some(DefenderVerdict {
                    agreed,
                    rationale: reason,
                });
            }
        }
    }

    // Fill missing verdicts with deny (conservative)
    let verdicts: Vec<DefenderVerdict> = verdict_map
        .into_iter()
        .map(|v| {
            v.unwrap_or(DefenderVerdict {
                agreed: false,
                rationale: "No verdict for this proposal — defaulting to deny".to_string(),
            })
        })
        .collect();

    Ok(verdicts)
}

/// Extract a JSON array substring from text that may contain preamble/thinking.
fn extract_json_array(text: &str) -> Option<String> {
    if text.starts_with('[') {
        if let Some(end) = text.rfind(']') {
            return Some(text[..=end].to_string());
        }
    }
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_prosecutor_prune_proposal() {
        let json = r#"[{"action":"prune","id":"mem-1","reason":"Outdated context information"}]"#;
        let proposals = parse_prosecutor_response(json).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].rationale, "Outdated context information");
        match &proposals[0].action {
            ConsolidationAction::Prune { id } => assert_eq!(id, "mem-1"),
            _ => panic!("Expected Prune action"),
        }
    }

    #[test]
    fn parse_prosecutor_merge_proposal() {
        let json = r#"[{"action":"merge","ids":["mem-1","mem-2"],"merged":"Combined fact","segment":"knowledge","importance":0.7,"reason":"Duplicate information"}]"#;
        let proposals = parse_prosecutor_response(json).unwrap();
        assert_eq!(proposals.len(), 1);
        match &proposals[0].action {
            ConsolidationAction::Merge {
                source_ids,
                merged_content,
                importance,
                ..
            } => {
                assert_eq!(source_ids.len(), 2);
                assert_eq!(merged_content, "Combined fact");
                assert!((importance - 0.7).abs() < 0.01);
            }
            _ => panic!("Expected Merge action"),
        }
    }

    #[test]
    fn parse_prosecutor_with_thinking_tokens() {
        let json = "<think>Let me analyze...</think>[{\"action\":\"prune\",\"id\":\"x\",\"reason\":\"old\"}]";
        let proposals = parse_prosecutor_response(json).unwrap();
        assert_eq!(proposals.len(), 1);
    }

    #[test]
    fn parse_prosecutor_empty_array() {
        let proposals = parse_prosecutor_response("[]").unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn parse_prosecutor_invalid_json_returns_empty() {
        let proposals = parse_prosecutor_response("not json at all").unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn parse_defender_verdicts() {
        let json = r#"[{"index":0,"agreed":true,"reason":"Truly redundant"},{"index":1,"agreed":false,"reason":"Contains unique info"}]"#;
        let verdicts = parse_defender_response(json, 2).unwrap();
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts[0].agreed);
        assert!(!verdicts[1].agreed);
    }

    #[test]
    fn parse_defender_missing_verdict_defaults_to_deny() {
        let json = r#"[{"index":0,"agreed":true,"reason":"OK"}]"#;
        let verdicts = parse_defender_response(json, 3).unwrap();
        assert_eq!(verdicts.len(), 3);
        assert!(verdicts[0].agreed);
        assert!(!verdicts[1].agreed); // missing → deny
        assert!(!verdicts[2].agreed); // missing → deny
    }

    #[test]
    fn parse_defender_no_json_denies_all() {
        let verdicts = parse_defender_response("I cannot process this", 2).unwrap();
        assert_eq!(verdicts.len(), 2);
        assert!(!verdicts[0].agreed);
        assert!(!verdicts[1].agreed);
    }

    #[test]
    fn parse_defender_with_thinking_tokens() {
        let json =
            "<think>reasoning</think>[{\"index\":0,\"agreed\":false,\"reason\":\"keep it\"}]";
        let verdicts = parse_defender_response(json, 1).unwrap();
        assert_eq!(verdicts.len(), 1);
        assert!(!verdicts[0].agreed);
    }

    #[test]
    fn extract_json_array_from_clean_input() {
        let result = extract_json_array("[1,2,3]");
        assert_eq!(result, Some("[1,2,3]".to_string()));
    }

    #[test]
    fn extract_json_array_from_preamble() {
        let result = extract_json_array("Here: [1,2,3] done");
        assert_eq!(result, Some("[1,2,3]".to_string()));
    }

    #[test]
    fn extract_json_array_no_array() {
        let result = extract_json_array("no array here");
        assert!(result.is_none());
    }

    #[test]
    fn format_memories_output() {
        use chrono::Utc;
        use pond_core::domain::memory::{MemoryLifecycle, MemorySegment, MemoryTier};

        let frag = MemoryFragment {
            id: "test-1".to_string(),
            profile_id: None,
            session_id: None,
            content: "User likes coffee".to_string(),
            embedding: None,
            source: "extraction".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            segment: Some(MemorySegment::Preference),
            importance: Some(0.7),
            tier: Some(MemoryTier::Long),
            decay_rate: Some(0.01),
            access_count: 0,
            last_accessed_at: None,
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
        };
        let refs = vec![&frag];
        let output = format_memories(&refs);
        assert!(output.contains("[test-1]"));
        assert!(output.contains("preference"));
        assert!(output.contains("0.7"));
        assert!(output.contains("User likes coffee"));
    }

    #[test]
    fn format_proposals_output() {
        let proposals = vec![
            ConsolidationProposal {
                action: ConsolidationAction::Prune {
                    id: "mem-1".to_string(),
                },
                rationale: "outdated".to_string(),
            },
            ConsolidationProposal {
                action: ConsolidationAction::Merge {
                    source_ids: vec!["a".to_string(), "b".to_string()],
                    merged_content: "combined".to_string(),
                    segment: MemorySegment::Knowledge,
                    importance: 0.6,
                },
                rationale: "duplicates".to_string(),
            },
        ];
        let output = format_proposals(&proposals);
        assert!(output.contains("#0: PRUNE [mem-1]"));
        assert!(output.contains("#1: MERGE [a, b]"));
        assert!(output.contains("outdated"));
        assert!(output.contains("duplicates"));
    }
}
