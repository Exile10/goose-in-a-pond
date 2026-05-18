//! LLM-based memory extractor — uses the live LLM provider to extract
//! durable facts from conversation turns.
//!
//! The extraction prompt is intentionally compact (~150 tokens of instruction)
//! to work well with 3B parameter models.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::domain::memory::{MemorySegment, MemoryTier};
use pond_core::domain::message::ChatMessage;
use pond_core::ports::memory_extractor::{ExtractedFact, MemoryExtractor};
use pond_core::ports::provider::LlmProvider;
use std::sync::Arc;
use tokio::sync::RwLock;

const EXTRACTION_PROMPT: &str = "\
Extract durable facts about the USER from this conversation turn.
Return JSON: {\"facts\":[{\"content\":\"one sentence fact\",\"segment\":\"identity|preference|correction|relationship|project|knowledge|context\",\"importance\":0.0-1.0}]}

Segment guide:
- identity: core user facts (name, role, location). importance 0.80-0.85
- correction: user explicitly corrects something. importance 0.85-0.90. Add \"corrects\":\"the wrong claim being fixed\" field.
- preference: style, defaults, likes/dislikes. importance 0.65-0.75
- relationship: people, pets, connections. importance 0.65-0.75
- project: ongoing work, deadlines, goals. importance 0.55-0.65
- knowledge: facts the user taught (not common knowledge). importance 0.45-0.55
- context: current situation, transient. importance 0.30-0.40

Max 3 facts per turn. Prefer fewer, higher-quality facts.
NEVER save:
- General knowledge or facts the assistant provided (weather, Wikipedia, etc.)
- Info already in the system prompt (assistant name, timezone, personality)
- Speculative or unverified claims (\"I think\", \"maybe\", \"probably\")
- Transient conversational filler (\"OK\", \"thanks\", greetings)
- Empty or vague content without concrete information
ONLY save facts about the user that would be lost if forgotten.
If nothing is worth remembering, return {\"facts\":[]}.";

pub struct LlmMemoryExtractor {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
    max_facts: usize,
}

impl LlmMemoryExtractor {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>, max_facts: u32) -> Self {
        Self {
            live_provider,
            max_facts: max_facts as usize,
        }
    }
}

#[async_trait]
impl MemoryExtractor for LlmMemoryExtractor {
    async fn extract(
        &self,
        user_message: &str,
        assistant_response: &str,
        existing_content: &[String],
    ) -> Result<Vec<ExtractedFact>> {
        let provider = {
            let guard = self.live_provider.read().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| anyhow!("no LLM provider available"))?
        };

        // Build the user message for extraction
        // Truncate long responses to stay within small-model context
        let asst_truncated = if assistant_response.len() > 500 {
            format!("{}...", &assistant_response[..500])
        } else {
            assistant_response.to_string()
        };

        let input = format!("User: {user_message}\nAssistant: {asst_truncated}");

        let messages = vec![ChatMessage::user(input)];
        let response = provider.complete(EXTRACTION_PROMPT, messages).await?;

        parse_extraction_response(&response.content, existing_content, self.max_facts)
    }
}

/// Parse the LLM's extraction response into `ExtractedFact` objects.
///
/// Accepts multiple formats for robustness with small models:
/// - New format: `{"facts": [...]}`
/// - Legacy format: bare JSON array `[...]`
/// - Preamble: text before the JSON (model may add explanation)
/// - Wrapped in thinking tags
fn parse_extraction_response(
    raw: &str,
    existing_content: &[String],
    max_facts: usize,
) -> Result<Vec<ExtractedFact>> {
    let text = raw.trim();

    // Strip thinking tokens if present
    let cleaned = strip_thinking(text);

    // Try new format: {"facts": [...]}
    if let Some(arr) = extract_facts_from_object(&cleaned) {
        let facts = parse_fact_array(&arr, existing_content, max_facts);
        return Ok(facts);
    }

    // Try legacy format: bare JSON array [...]
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&cleaned) {
        let facts: Vec<ExtractedFact> = arr
            .iter()
            .filter_map(|v| parse_fact_json(v, existing_content))
            .take(max_facts)
            .collect();
        return Ok(facts);
    }

    // Try extracting JSON from within the text (model may add preamble)
    // First try object format, then array format
    if let Some(start) = cleaned.find('{') {
        if let Some(end) = cleaned.rfind('}') {
            let slice = &cleaned[start..=end];
            if let Some(arr) = extract_facts_from_object(slice) {
                let facts = parse_fact_array(&arr, existing_content, max_facts);
                return Ok(facts);
            }
        }
    }

    if let Some(start) = cleaned.find('[') {
        if let Some(end) = cleaned.rfind(']') {
            let slice = &cleaned[start..=end];
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                let facts: Vec<ExtractedFact> = arr
                    .iter()
                    .filter_map(|v| parse_fact_json(v, existing_content))
                    .take(max_facts)
                    .collect();
                return Ok(facts);
            }
        }
    }

    // Empty array/object is valid — no facts to extract
    if cleaned == "[]"
        || cleaned.is_empty()
        || cleaned == "{\"facts\":[]}"
        || cleaned == r#"{"facts": []}"#
    {
        return Ok(vec![]);
    }

    Ok(vec![])
}

/// Try to parse `{"facts": [...]}` wrapper and return the inner array.
fn extract_facts_from_object(text: &str) -> Option<Vec<serde_json::Value>> {
    let obj: serde_json::Value = serde_json::from_str(text).ok()?;
    let arr = obj.get("facts")?.as_array()?;
    Some(arr.clone())
}

/// Parse a JSON array of fact objects into `ExtractedFact` values.
fn parse_fact_array(
    arr: &[serde_json::Value],
    existing_content: &[String],
    max_facts: usize,
) -> Vec<ExtractedFact> {
    arr.iter()
        .filter_map(|v| parse_fact_json(v, existing_content))
        .take(max_facts)
        .collect()
}

fn parse_fact_json(v: &serde_json::Value, existing: &[String]) -> Option<ExtractedFact> {
    // Accept both new format key ("content") and legacy key ("fact")
    let content = v
        .get("content")
        .or_else(|| v.get("fact"))
        .and_then(|f| f.as_str())?
        .trim()
        .to_string();
    if content.len() < 5 {
        return None;
    }

    // Skip if already exists
    let lower = content.to_lowercase();
    if existing
        .iter()
        .any(|e| e.contains(&lower) || lower.contains(e.as_str()))
    {
        return None;
    }

    let segment = v
        .get("segment")
        .and_then(|s| s.as_str())
        .and_then(parse_segment_str)
        .unwrap_or(MemorySegment::Knowledge);

    let importance = v
        .get("importance")
        .and_then(|i| i.as_f64())
        .map(|i| (i as f32).clamp(0.0, 1.0))
        .unwrap_or_else(|| segment.default_importance());

    let tier = segment.default_tier();

    // For correction segments, read what wrong claim is being corrected
    let corrects = v
        .get("corrects")
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some(ExtractedFact {
        content,
        segment,
        importance,
        tier,
        corrects,
    })
}

pub fn parse_segment_str(s: &str) -> Option<MemorySegment> {
    match s.to_lowercase().as_str() {
        "identity" => Some(MemorySegment::Identity),
        "preference" => Some(MemorySegment::Preference),
        "correction" => Some(MemorySegment::Correction),
        "relationship" => Some(MemorySegment::Relationship),
        "project" => Some(MemorySegment::Project),
        "knowledge" => Some(MemorySegment::Knowledge),
        "context" => Some(MemorySegment::Context),
        _ => None,
    }
}

/// Strip `<think>…</think>` and `<|channel>…<channel|>` tokens.
pub fn strip_thinking(text: &str) -> String {
    let mut result = text.to_string();

    // Strip <think>…</think>
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result.find("</think>") {
            result = format!("{}{}", &result[..start], &result[end + 8..]);
        } else {
            // Unclosed think block — strip from <think> to end
            result = result[..start].to_string();
            break;
        }
    }

    // Strip <|channel>…<channel|>
    while let Some(start) = result.find("<|channel>") {
        if let Some(end) = result.find("<channel|>") {
            result = format!("{}{}", &result[..start], &result[end + 10..]);
        } else {
            result = result[..start].to_string();
            break;
        }
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_json_array() {
        let json = r#"[
            {"fact": "User's name is Jerry", "segment": "identity", "importance": 0.85},
            {"fact": "User prefers dark mode", "segment": "preference", "importance": 0.7}
        ]"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].content, "User's name is Jerry");
        assert_eq!(facts[0].segment, MemorySegment::Identity);
        assert!((facts[0].importance - 0.85).abs() < 0.01);
        assert_eq!(facts[1].segment, MemorySegment::Preference);
    }

    #[test]
    fn parse_empty_array() {
        let facts = parse_extraction_response("[]", &[], 3).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn parse_json_with_preamble() {
        let text = "Here are the facts:\n[{\"fact\": \"Lives in Nairobi\", \"segment\": \"identity\", \"importance\": 0.8}]";
        let facts = parse_extraction_response(text, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn dedup_skips_existing() {
        let existing = vec!["user's name is jerry".to_string()];
        let json =
            r#"[{"fact": "User's name is Jerry", "segment": "identity", "importance": 0.85}]"#;
        let facts = parse_extraction_response(json, &existing, 3).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn max_three_facts() {
        let json = r#"[
            {"fact": "Fact one", "segment": "knowledge", "importance": 0.5},
            {"fact": "Fact two", "segment": "knowledge", "importance": 0.5},
            {"fact": "Fact three", "segment": "knowledge", "importance": 0.5},
            {"fact": "Fact four", "segment": "knowledge", "importance": 0.5}
        ]"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 3);
    }

    #[test]
    fn strip_thinking_tokens() {
        let text = "<think>reasoning here</think>[{\"fact\": \"Test fact\", \"segment\": \"knowledge\", \"importance\": 0.5}]";
        let stripped = strip_thinking(text);
        assert!(
            stripped.contains("[{"),
            "stripped should contain JSON, got: {stripped:?}"
        );
        let facts = parse_extraction_response(text, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn missing_segment_defaults_to_knowledge() {
        let json = r#"[{"fact": "Some fact", "importance": 0.6}]"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].segment, MemorySegment::Knowledge);
    }

    #[test]
    fn importance_clamped() {
        let json = r#"[{"fact": "High importance", "segment": "identity", "importance": 1.5}]"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts[0].importance, 1.0);
    }

    // ── New format tests ({"facts": [...]}) ──────────────────────────────────

    #[test]
    fn parse_new_format_with_content_key() {
        let json = r#"{"facts": [
            {"content": "User's name is Jerry", "segment": "identity", "importance": 0.82},
            {"content": "User prefers dark mode", "segment": "preference", "importance": 0.7}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].content, "User's name is Jerry");
        assert_eq!(facts[0].segment, MemorySegment::Identity);
        assert!((facts[0].importance - 0.82).abs() < 0.01);
    }

    #[test]
    fn parse_new_format_empty() {
        let json = r#"{"facts": []}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn parse_new_format_with_preamble() {
        let text = r#"Here are the extracted facts:
{"facts": [{"content": "Lives in Nairobi", "segment": "identity", "importance": 0.8}]}"#;
        let facts = parse_extraction_response(text, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "Lives in Nairobi");
    }

    #[test]
    fn parse_new_format_with_thinking() {
        let text = r#"<think>analyzing conversation</think>{"facts": [{"content": "User works at Jarida", "segment": "identity", "importance": 0.83}]}"#;
        let facts = parse_extraction_response(text, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].segment, MemorySegment::Identity);
    }

    #[test]
    fn parse_new_format_respects_max_facts() {
        let json = r#"{"facts": [
            {"content": "Fact one", "segment": "knowledge", "importance": 0.5},
            {"content": "Fact two", "segment": "knowledge", "importance": 0.5},
            {"content": "Fact three", "segment": "knowledge", "importance": 0.5},
            {"content": "Fact four", "segment": "knowledge", "importance": 0.5}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 3);
    }

    #[test]
    fn parse_new_format_dedup_works() {
        let existing = vec!["user works at jarida".to_string()];
        let json = r#"{"facts": [{"content": "User works at Jarida", "segment": "identity", "importance": 0.83}]}"#;
        let facts = parse_extraction_response(json, &existing, 3).unwrap();
        assert!(facts.is_empty());
    }

    #[test]
    fn parse_correction_with_corrects_field() {
        let json = r#"{"facts": [
            {"content": "User's name is Jerry, not John", "segment": "correction", "importance": 0.9, "corrects": "User's name is John"}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].segment, MemorySegment::Correction);
        assert_eq!(facts[0].corrects, Some("User's name is John".to_string()));
    }

    #[test]
    fn parse_non_correction_has_no_corrects() {
        let json = r#"{"facts": [
            {"content": "User likes dark mode", "segment": "preference", "importance": 0.7}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
        assert!(facts[0].corrects.is_none());
    }
}
