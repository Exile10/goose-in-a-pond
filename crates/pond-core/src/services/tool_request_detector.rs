//! Post-inference tool request detection.
//!
//! Scans the LLM's response text for natural language phrases that indicate
//! the model wants a tool to be called (e.g. "Let me look up X for you").
//! Returns a `ToolRequest` that the routes layer can execute and use to
//! revise the response.
//!
//! Pure functions — no I/O, no async, fully testable.

/// A tool the LLM has requested via natural language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    /// Tool to invoke (e.g. "wikipedia", "weather", "save_memory").
    pub tool_name: String,
    /// Query or content for the tool (e.g. "quantum physics"). Empty for
    /// tools that don't need a query (weather).
    pub query: String,
}

/// Scan the LLM's response for a natural language tool request.
///
/// Returns `Some(ToolRequest)` if a tool request phrase is detected,
/// `None` if the response doesn't request any tool.
pub fn detect_tool_request(response: &str) -> Option<ToolRequest> {
    let lower = response.to_lowercase();

    // ── Wikipedia / knowledge lookup ────────────────────────────────
    let wiki_phrases = [
        "let me look up ",
        "let me look that up",
        "i need to look up ",
        "i'll look up ",
        "let me search for ",
        "i should look up ",
        "let me find out about ",
        "let me check on ",
    ];
    for phrase in &wiki_phrases {
        if let Some(idx) = lower.find(phrase) {
            let after = &response[idx + phrase.len()..];
            let query = extract_topic(after);
            if !query.is_empty() {
                return Some(ToolRequest {
                    tool_name: "wikipedia".into(),
                    query,
                });
            }
            // Phrase without extractable topic (e.g. "let me look that up")
            // — still indicates intent, but no query to extract
            if phrase.ends_with("up") && !phrase.ends_with("up ") {
                return None; // "let me look that up" without context
            }
        }
    }

    // ── Weather ─────────────────────────────────────────────────────
    let weather_phrases = [
        "let me check the weather",
        "i'll check the weather",
        "let me get the weather",
        "i need to check the weather",
    ];
    if weather_phrases.iter().any(|p| lower.contains(p)) {
        return Some(ToolRequest {
            tool_name: "weather".into(),
            query: String::new(),
        });
    }

    // ── Save memory ─────────────────────────────────────────────────
    let save_phrases = [
        "i'll save that to memory",
        "let me save that",
        "i'll remember that",
        "let me remember that",
        "i should save this to memory",
    ];
    if save_phrases.iter().any(|p| lower.contains(p)) {
        return Some(ToolRequest {
            tool_name: "save_memory".into(),
            query: String::new(),
        });
    }

    // ── Recall memory ───────────────────────────────────────────────
    let recall_phrases = [
        "let me check what i remember",
        "let me recall",
        "let me check my memory",
        "i'll check my memory",
    ];
    if recall_phrases.iter().any(|p| lower.contains(p)) {
        return Some(ToolRequest {
            tool_name: "recall_memory".into(),
            query: String::new(),
        });
    }

    // ── Schedules ───────────────────────────────────────────────────
    let schedule_phrases = [
        "let me check your schedules",
        "let me check the schedules",
        "i'll check your schedules",
        "let me look at your schedule",
    ];
    if schedule_phrases.iter().any(|p| lower.contains(p)) {
        return Some(ToolRequest {
            tool_name: "schedules".into(),
            query: String::new(),
        });
    }

    // ── Devices ─────────────────────────────────────────────────────
    let device_phrases = [
        "let me check your devices",
        "let me check the devices",
        "i'll check your devices",
    ];
    if device_phrases.iter().any(|p| lower.contains(p)) {
        return Some(ToolRequest {
            tool_name: "devices".into(),
            query: String::new(),
        });
    }

    None
}

/// Extract the topic/query from text after a trigger phrase.
///
/// Takes the text immediately after the phrase (e.g. after "let me look up ")
/// and extracts the topic up to the first sentence-ending punctuation or
/// common connecting words.
fn extract_topic(after: &str) -> String {
    let trimmed = after.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Find the end boundary: period, comma, exclamation, question mark,
    // newline, or connecting phrases like "for you", "right now"
    let end_markers = ['.', ',', '!', '?', '\n', '\r'];
    let connecting = [
        " for you",
        " right now",
        " real quick",
        " quickly",
        " first",
    ];

    let mut end = trimmed.len();

    // Check for punctuation
    for (i, ch) in trimmed.char_indices() {
        if end_markers.contains(&ch) {
            end = i;
            break;
        }
    }

    let candidate = &trimmed[..end];

    // Trim connecting phrases from the end
    let mut result = candidate.to_string();
    for suffix in &connecting {
        if let Some(stripped) = result.to_lowercase().strip_suffix(suffix) {
            result = trimmed[..stripped.len()].to_string();
        }
    }

    // Remove surrounding quotes if present
    let result = result.trim();
    let result = result
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('"')
        .trim_matches('"');

    result.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_wikipedia_let_me_look_up() {
        let r = detect_tool_request("Let me look up quantum physics for you.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "wikipedia".into(),
                query: "quantum physics".into(),
            })
        );
    }

    #[test]
    fn detect_wikipedia_ill_look_up() {
        let r = detect_tool_request("I'll look up John Cena.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "wikipedia".into(),
                query: "John Cena".into(),
            })
        );
    }

    #[test]
    fn detect_wikipedia_with_quotes() {
        let r = detect_tool_request("Let me look up \"black holes\" for you.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "wikipedia".into(),
                query: "black holes".into(),
            })
        );
    }

    #[test]
    fn detect_wikipedia_embedded_in_response() {
        let r = detect_tool_request(
            "That's a great question! Let me look up photosynthesis for you. I want to make sure I give you accurate information."
        );
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "wikipedia".into(),
                query: "photosynthesis".into(),
            })
        );
    }

    #[test]
    fn detect_weather() {
        let r = detect_tool_request("Let me check the weather for you.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "weather".into(),
                query: String::new(),
            })
        );
    }

    #[test]
    fn detect_save_memory() {
        let r = detect_tool_request("Got it! I'll save that to memory.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "save_memory".into(),
                query: String::new(),
            })
        );
    }

    #[test]
    fn detect_recall_memory() {
        let r = detect_tool_request("Let me check what I remember about that.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "recall_memory".into(),
                query: String::new(),
            })
        );
    }

    #[test]
    fn detect_schedules() {
        let r = detect_tool_request("Sure, let me check your schedules.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "schedules".into(),
                query: String::new(),
            })
        );
    }

    #[test]
    fn detect_devices() {
        let r = detect_tool_request("I'll check your devices to see what's online.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "devices".into(),
                query: String::new(),
            })
        );
    }

    #[test]
    fn no_detection_for_normal_response() {
        assert_eq!(
            detect_tool_request("Hello! How can I help you today?"),
            None
        );
    }

    #[test]
    fn no_detection_for_empty() {
        assert_eq!(detect_tool_request(""), None);
    }

    #[test]
    fn no_detection_for_past_tense() {
        // "I looked up" is past tense — the tool was already called, don't re-trigger
        assert_eq!(
            detect_tool_request("I looked up the information earlier."),
            None
        );
    }

    #[test]
    fn extract_topic_strips_for_you() {
        assert_eq!(extract_topic("quantum physics for you."), "quantum physics");
    }

    #[test]
    fn extract_topic_stops_at_period() {
        assert_eq!(
            extract_topic("Mars. I think you'll find it interesting."),
            "Mars"
        );
    }

    #[test]
    fn extract_topic_empty_input() {
        assert_eq!(extract_topic(""), "");
    }

    #[test]
    fn case_insensitive_detection() {
        let r = detect_tool_request("LET ME LOOK UP the solar system.");
        assert_eq!(
            r,
            Some(ToolRequest {
                tool_name: "wikipedia".into(),
                query: "the solar system".into(),
            })
        );
    }
}
