//! LLM-based memory extractor — uses the live LLM provider to extract
//! durable facts from conversation turns.
//!
//! The extraction prompt is the whole per-turn prefill of a background job that
//! runs after *every* turn, on the same single-slot local model that is serving
//! chat — so its size is a latency cost, not just a context cost. It is kept
//! near ~400 tokens (1589 chars) and spends that budget on the two rules a
//! small model gets wrong unprompted: write in the third person, and write a
//! sentence that still means something with the conversation removed.
//! `pond-core`'s extraction service enforces both regardless — see
//! `fact_defect`.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::memory::{fact_defect, normalise_fact_content, MemorySegment};
use pond_core::user_data::ports::memory_extractor::{ExtractedFact, MemoryExtractor};
use pond_core::user_data::services::memory_relevance::is_duplicate_content;
use std::sync::Arc;
use tokio::sync::RwLock;

const EXTRACTION_PROMPT: &str = "\
Extract durable facts about the USER from this conversation turn.
Return JSON: {\"facts\":[{\"content\":\"one sentence fact\",\"segment\":\"identity|preference|correction|relationship|project|knowledge|context\",\"importance\":0.0-1.0}]}

Each content is stored forever and shown with no conversation around it:
- Third person. Never \"I\", \"me\", \"my\", \"we\".
- Self-contained: name every person and place. Never \"there\", \"that place\",
  \"the latter\", \"the former\", or an opening \"He/She/It/They\".
- One plain sentence, no label prefix.
\"my mom florence lives in kisumu, i moved there in 2019\" ->
{\"facts\":[{\"content\":\"The user's mother Florence lives in Kisumu.\",\"segment\":\"relationship\",\"importance\":0.7},{\"content\":\"The user moved to Kisumu in 2019.\",\"segment\":\"identity\",\"importance\":0.8}]}

Segments (importance):
- identity 0.80 name, role, home city
- correction 0.90 the user fixes something; add \"corrects\":\"the wrong claim\".
  Save it even when it restates a fact already stored.
- preference 0.70 style, defaults, likes and dislikes
- relationship 0.70 named people and pets
- project 0.60 ongoing work the user returns to across days. A task the user
  asked for this turn is NOT a project.
- knowledge 0.50 facts the user taught, not common knowledge
- context 0.35 transient, useful for a few days

Max 3 facts, fewer is better. Never save a request already carried out
(\"write a prime function\", \"set a reminder\"), anything the assistant supplied
or the system prompt already states, guesses (\"I think\", \"maybe\"), filler, or
vague content. If nothing is worth keeping, return {\"facts\":[]}.";

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
        return Ok(parse_fact_array(&arr, existing_content, max_facts));
    }

    // Try legacy format: bare JSON array [...]
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&cleaned) {
        return Ok(parse_fact_array(&arr, existing_content, max_facts));
    }

    // Try extracting JSON from within the text (model may add preamble)
    // First try object format, then array format
    if let Some(start) = cleaned.find('{') {
        if let Some(end) = cleaned.rfind('}') {
            let slice = &cleaned[start..=end];
            if let Some(arr) = extract_facts_from_object(slice) {
                return Ok(parse_fact_array(&arr, existing_content, max_facts));
            }
        }
    }

    if let Some(start) = cleaned.find('[') {
        if let Some(end) = cleaned.rfind(']') {
            let slice = &cleaned[start..=end];
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                return Ok(parse_fact_array(&arr, existing_content, max_facts));
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
///
/// Rejected facts do not consume the `max_facts` budget, and each accepted fact
/// joins the dedup set so one response cannot emit the same fact twice in two
/// wordings. The extraction service re-applies both checks before writing —
/// this pass only stops junk from crowding out good facts here.
fn parse_fact_array(
    arr: &[serde_json::Value],
    existing_content: &[String],
    max_facts: usize,
) -> Vec<ExtractedFact> {
    let mut seen: Vec<String> = existing_content.to_vec();
    let mut facts: Vec<ExtractedFact> = Vec::new();
    for value in arr {
        if facts.len() >= max_facts {
            break;
        }
        if let Some(fact) = parse_fact_json(value, &seen) {
            seen.push(fact.content.to_lowercase());
            facts.push(fact);
        }
    }
    facts
}

fn parse_fact_json(v: &serde_json::Value, existing: &[String]) -> Option<ExtractedFact> {
    // Accept both new format key ("content") and legacy key ("fact")
    let raw = v
        .get("content")
        .or_else(|| v.get("fact"))
        .and_then(|f| f.as_str())?;
    let content = normalise_fact_content(raw);

    // Unusable content: first person, a reference nothing can resolve, or
    // nothing at all. Dropped here so it never reaches the store.
    if let Some(defect) = fact_defect(&content) {
        tracing::debug!(defect = %defect, "[memory-extraction] rejected fact: {content:?}");
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

    // Skip if an existing memory already says this — unless it is a
    // correction, which restates the claim it overturns almost word for word
    // and would be dropped in favour of the stale row. Read after the segment
    // so the exemption can see it; `pond-core` applies the same rule at the
    // write gate.
    let is_correction = segment == MemorySegment::Correction || corrects.is_some();
    if !is_correction && existing.iter().any(|e| is_duplicate_content(e, &content)) {
        return None;
    }

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

    // ── quality gate ─────────────────────────────────────────────────────────

    #[test]
    fn defective_facts_are_dropped_at_parse_time() {
        let json = r#"{"facts": [
            {"content": "The user's mother lives in the latter city.", "segment": "relationship", "importance": 0.7},
            {"content": "My mom's name is Florence", "segment": "relationship", "importance": 0.7},
            {"content": "The user's mother is named Florence and lives in Kisumu.", "segment": "relationship", "importance": 0.7}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(
            facts[0].content,
            "The user's mother is named Florence and lives in Kisumu."
        );
    }

    #[test]
    fn a_rejected_fact_does_not_consume_the_budget() {
        // Two junk facts ahead of two good ones, max 2: both good ones survive.
        let json = r#"{"facts": [
            {"content": "She lives in Kisumu.", "segment": "relationship", "importance": 0.7},
            {"content": "My birthday is in March.", "segment": "identity", "importance": 0.8},
            {"content": "The user's mother is named Florence.", "segment": "relationship", "importance": 0.7},
            {"content": "The user works at Jarida.", "segment": "identity", "importance": 0.8}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 2).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].content, "The user's mother is named Florence.");
        assert_eq!(facts[1].content, "The user works at Jarida.");
    }

    #[test]
    fn a_label_prefix_is_stripped_from_content() {
        let json = r#"{"facts": [
            {"content": "Active Project: The user is porting the dashboard to the Jetson.", "segment": "project", "importance": 0.6}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(
            facts[0].content,
            "The user is porting the dashboard to the Jetson."
        );
    }

    #[test]
    fn one_response_cannot_emit_the_same_fact_twice() {
        let json = r#"{"facts": [
            {"content": "The user's mother's name is Florence.", "segment": "relationship", "importance": 0.7},
            {"content": "Florence is the name of the user's mother.", "segment": "relationship", "importance": 0.7}
        ]}"#;
        let facts = parse_extraction_response(json, &[], 3).unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn the_prompt_states_the_rules_the_gate_enforces() {
        // The gate is silent when it fires, so the prompt has to carry the same
        // rules or every turn pays for facts that are thrown away.
        for clue in [
            "Third person",
            "Self-contained",
            "the latter",
            "NOT a project",
            "restates a fact already stored",
        ] {
            assert!(
                EXTRACTION_PROMPT.contains(clue),
                "extraction prompt lost: {clue:?}"
            );
        }
    }

    /// This prompt is prefilled after *every* turn, on the same single-slot
    /// local model that is serving chat, so growth here is felt as latency on
    /// the next user message. It reached 2466 chars once by accretion; this
    /// ceiling makes the next accretion a failing test rather than a silent
    /// regression.
    const EXTRACTION_PROMPT_CEILING: usize = 1800;

    #[test]
    fn the_prompt_stays_within_its_prefill_budget() {
        assert!(
            EXTRACTION_PROMPT.len() <= EXTRACTION_PROMPT_CEILING,
            "extraction prompt is {} chars, ceiling is {EXTRACTION_PROMPT_CEILING}",
            EXTRACTION_PROMPT.len()
        );
    }

    #[test]
    fn a_correction_is_not_deduplicated_away_at_parse_time() {
        // The stale row is already in the store; the correction changes one
        // word of it, in the same order. Deliberately *not* an argument
        // reversal: the order guard exempts those anyway, so a reversal pair
        // would pass this test even with the correction exemption deleted. Here
        // the lexical measure genuinely reads a duplicate (Jaccard 0.75,
        // containment 0.86), so only the exemption keeps the fix.
        let stale = "The user's daughter Aisha started school in Nakuru last year";
        let fixed = "The user's daughter Aisha started school in Nairobi last year";
        assert!(
            is_duplicate_content(stale, fixed),
            "test is vacuous unless the lexical measure flags this pair"
        );

        // Both the segment and the bare `corrects` field must exempt it, since
        // a small model sets one without the other often enough.
        let existing = vec![stale.to_lowercase()];
        for segment in ["correction", "preference"] {
            let json = format!(
                r#"{{"facts": [{{"content": "{fixed}", "segment": "{segment}", "importance": 0.9, "corrects": "{stale}"}}]}}"#
            );
            let facts = parse_extraction_response(&json, &existing, 3).unwrap();
            assert_eq!(
                facts.len(),
                1,
                "correction dropped with segment {segment:?}"
            );
        }

        // …while an ordinary restatement of the same row is still dropped.
        let json = format!(
            r#"{{"facts": [{{"content": "{fixed}", "segment": "preference", "importance": 0.7}}]}}"#
        );
        assert!(parse_extraction_response(&json, &existing, 3)
            .unwrap()
            .is_empty());
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
