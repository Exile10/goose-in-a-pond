//! Shared formatting utilities for MCP tool results.
//!
//! All tool results must fit within a character budget to avoid
//! overwhelming the LLM's context window.

/// Truncate text to a character budget, respecting UTF-8 char boundaries.
/// Appends a truncation notice if the text was cut.
pub fn truncate_to_budget(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut cut = max_chars;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}...\n\n[Truncated — full content at source]",
        &text[..cut]
    )
}

/// Format a list of items with a header, capped to budget.
pub fn format_list_result(items: &[String], header: &str, max_chars: usize) -> String {
    let mut result = if header.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", header)
    };

    for item in items {
        let line = format!("- {}\n", item);
        if result.len() + line.len() > max_chars {
            result.push_str("...\n[More results available]");
            break;
        }
        result.push_str(&line);
    }

    if result.is_empty() {
        "No results found.".to_string()
    } else {
        result.trim_end().to_string()
    }
}

/// Format a "not configured" guidance message for tools that need an API key.
/// Returns a message the LLM can relay to the user with signup instructions.
pub fn format_not_configured(feature: &str, signup_url: &str) -> String {
    format!(
        "{feature} requires an API key to work. Get one free at {signup_url} — \
         then add it in Settings under 'Knowledge & Discovery'."
    )
}

/// Format an API error as a helpful message (not ErrorData — the LLM reads this).
pub fn format_api_error(service: &str, error: &str) -> String {
    format!(
        "{service} is temporarily unavailable: {error}. Try again in a moment, \
         or ask your question differently."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_within_budget_unchanged() {
        let text = "short text";
        assert_eq!(truncate_to_budget(text, 100), text);
    }

    #[test]
    fn truncate_over_budget_cuts_and_appends_notice() {
        let text = "Hello, world! This is a long string.";
        let result = truncate_to_budget(text, 13);
        assert!(result.starts_with("Hello, world!"));
        assert!(result.contains("[Truncated"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // Multi-byte: "cafe\u0301" — the 'e\u0301' is 2 bytes
        let text = "caf\u{00e9} au lait";
        let result = truncate_to_budget(text, 5);
        // Should not panic or split mid-char
        assert!(result.contains("[Truncated"));
    }

    #[test]
    fn format_list_with_header() {
        let items = vec!["Item 1".into(), "Item 2".into(), "Item 3".into()];
        let result = format_list_result(&items, "Results:", 200);
        assert!(result.starts_with("Results:"));
        assert!(result.contains("- Item 1"));
        assert!(result.contains("- Item 3"));
    }

    #[test]
    fn format_list_truncates_when_over_budget() {
        let items: Vec<String> = (0..100).map(|i| format!("Item number {}", i)).collect();
        let result = format_list_result(&items, "Results:", 100);
        assert!(result.contains("[More results available]"));
    }

    #[test]
    fn format_list_empty_returns_no_results() {
        let items: Vec<String> = vec![];
        let result = format_list_result(&items, "", 200);
        assert_eq!(result, "No results found.");
    }

    #[test]
    fn format_not_configured_includes_url() {
        let msg = format_not_configured("News search", "https://open-platform.theguardian.com");
        assert!(msg.contains("News search"));
        assert!(msg.contains("open-platform.theguardian.com"));
        assert!(msg.contains("API key"));
    }

    #[test]
    fn format_api_error_includes_service_name() {
        let msg = format_api_error("Finnhub", "connection timeout");
        assert!(msg.contains("Finnhub"));
        assert!(msg.contains("connection timeout"));
    }
}
