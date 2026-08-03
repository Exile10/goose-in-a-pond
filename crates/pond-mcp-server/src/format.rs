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
///
/// A header with no items underneath it is a miss, not a result — see
/// [`format_no_results`] for why that distinction has to reach the model.
pub fn format_list_result(items: &[String], header: &str, max_chars: usize) -> String {
    let mut result = if header.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", header)
    };

    let mut wrote_item = false;
    for item in items {
        let line = format!("- {}\n", item);
        if result.len() + line.len() > max_chars {
            result.push_str("...\n[More results available]");
            break;
        }
        result.push_str(&line);
        wrote_item = true;
    }

    // Emptiness is decided by the items, not by the accumulated string. A
    // non-empty header made `result` non-empty before the first item was ever
    // written, so a zero-item search used to return the bare header — which
    // reads to the model as a successful result that happens to have no body.
    if !wrote_item {
        let what = if header.is_empty() {
            "this search".to_string()
        } else {
            header.trim_end().trim_end_matches(':').to_string()
        };
        return format_no_results(&what, &[]);
    }

    result.trim_end().to_string()
}

/// Format an empty result so the model can act on it.
///
/// `<tool-chaining>` in the system prompt licenses a follow-up call when a tool
/// result points at a next step. A bare "No results found." points nowhere, so
/// the model synthesises the dead end into an apology instead of reaching for a
/// sibling tool — which is why retry behaviour tracked whichever tool happened
/// to be called first. Every miss therefore states plainly that it is not an
/// answer, and names what to try instead.
///
/// `alternatives` must be FULLY-QUALIFIED schema names
/// (`giap-discovery__search_web`, not `search_web`). A 2B model does not reliably
/// map a bare suffix onto the prefixed name in its schema — measured on
/// gemma-4-E2B: with a bare name it apologised instead of chaining. Pass an empty
/// slice when the caller cannot name a specific sibling; never name a tool that
/// does not exist, since a fabricated suggestion burns the one retry it makes.
///
/// `what` must be a plain noun phrase with no advice in it. An earlier revision
/// appended "(check the spelling)" and the model complied with the parenthetical
/// instead of the instruction — a competing suggestion beats a buried one.
pub fn format_no_results(what: &str, alternatives: &[&str]) -> String {
    if alternatives.is_empty() {
        return format!(
            "No results for {what}. This is NOT the answer. If another tool in your \
             schema can answer the question, call it now instead of replying."
        );
    }
    format!(
        "No results for {what}. This is NOT the answer — call {} now. \
         Only after those also return nothing may you tell the user you could not find it.",
        join_tool_names(alternatives)
    )
}

/// Format a miss that genuinely has no follow-up, with the reason why.
///
/// Use this instead of [`format_no_results`] where chaining would be wrong — a
/// personal fact that is simply not stored is not something a web search should
/// be sent after. Stating the reason stops the model inventing a next step.
pub fn format_dead_end(what: &str, why: &str) -> String {
    format!("No results: {what}. {why}")
}

/// Join tool names as "a", "a or b", "a, b, or c".
fn join_tool_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_string(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
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
///
/// Steers to a *different* tool rather than to a later retry of this one. The
/// old wording ("try again in a moment") pointed at the only option the model
/// cannot take inside one turn, so a transport failure ended the turn.
pub fn format_api_error(service: &str, error: &str) -> String {
    format!(
        "{service} is temporarily unavailable: {error}. This did not answer the \
         question — if another tool in your schema can answer it, call that tool \
         now rather than retrying this one."
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
        assert!(result.starts_with("No results for this search."));
        assert!(result.contains("NOT the answer"));
    }

    #[test]
    fn format_list_with_header_and_no_items_is_a_miss_not_a_header() {
        // The regression: a non-empty header made the accumulated string
        // non-empty, so zero items returned the bare header and the model read
        // it as a successful result with no body.
        let items: Vec<String> = vec![];
        let result = format_list_result(&items, "Guardian search: \"kenya election\":", 200);
        assert!(result.starts_with("No results for"), "got: {result}");
        assert!(result.contains("Guardian search"));
        assert!(!result.ends_with("election\":"));
    }

    #[test]
    fn no_results_without_alternatives_still_licenses_a_different_tool() {
        let msg = format_no_results("books for 'ubuntu'", &[]);
        assert!(msg.contains("NOT the answer"));
        assert!(msg.contains("another tool in your schema"));
    }

    #[test]
    fn no_results_names_the_alternatives_it_was_given() {
        let msg = format_no_results(
            "Wikipedia articles for 'x'",
            &[
                "giap-discovery__search_web",
                "giap-knowledge__instant_answer",
            ],
        );
        assert!(
            msg.contains("call giap-discovery__search_web or giap-knowledge__instant_answer now"),
            "got: {msg}"
        );
        assert!(
            msg.contains("Only after those also return nothing"),
            "got: {msg}"
        );
    }

    #[test]
    fn join_tool_names_reads_naturally_at_each_arity() {
        assert_eq!(join_tool_names(&["a"]), "a");
        assert_eq!(join_tool_names(&["a", "b"]), "a or b");
        assert_eq!(join_tool_names(&["a", "b", "c"]), "a, b, or c");
    }

    /// A bare suffix is not what the model sees in its schema — gemma-4-E2B
    /// apologised rather than mapping `search_web` onto
    /// `giap-discovery__search_web`. Every suggestion must be callable verbatim,
    /// so scan the real call sites rather than trusting review.
    #[test]
    fn every_suggested_alternative_is_fully_qualified() {
        const SOURCES: &[(&str, &str)] = &[
            ("knowledge.rs", include_str!("knowledge.rs")),
            ("discovery.rs", include_str!("discovery.rs")),
            ("news.rs", include_str!("news.rs")),
            ("finance.rs", include_str!("finance.rs")),
            ("memory.rs", include_str!("memory.rs")),
            ("sensors.rs", include_str!("sensors.rs")),
            ("vision.rs", include_str!("vision.rs")),
        ];
        let mut checked = 0;
        for (name, src) in SOURCES {
            for (i, _) in src.match_indices("format_no_results(") {
                // Bound the argument list to its own matching paren, then the
                // alternatives slice to its own matching bracket. Anything
                // looser reads into the next call's `what` string.
                let args = balanced(src, i + "format_no_results(".len(), '(', ')');
                let Some(open) = args.find("&[") else {
                    continue;
                };
                let slice = balanced(&args, open + 2, '[', ']');
                for quoted in slice.split('"').skip(1).step_by(2) {
                    assert!(
                        quoted.starts_with("giap-") && quoted.contains("__"),
                        "{name}: suggested alternative '{quoted}' is not a \
                         fully-qualified schema name; the model cannot call it"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "the scan matched nothing — parser is broken");
    }

    /// Return the text from `start` up to the delimiter that closes the one
    /// already opened before `start`.
    fn balanced(s: &str, start: usize, open: char, close: char) -> &str {
        let mut depth = 1usize;
        for (offset, ch) in s[start..].char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return &s[start..start + offset];
                }
            }
        }
        &s[start..]
    }

    #[test]
    fn dead_end_states_the_reason_and_names_no_tool() {
        let msg = format_dead_end("memories about the user", "Nothing is stored about this.");
        assert!(msg.contains("Nothing is stored"));
        assert!(!msg.contains("call "));
    }

    #[test]
    fn api_error_steers_to_a_different_tool_not_a_retry() {
        let msg = format_api_error("Finnhub", "connection timeout");
        assert!(msg.contains("another tool in your schema"));
        assert!(!msg.contains("Try again in a moment"));
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
