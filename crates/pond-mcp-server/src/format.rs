//! Shared formatting utilities for MCP tool results.
//!
//! All tool results must fit within a BYTE budget to avoid overwhelming the
//! LLM's context window. See [`truncate_to_budget`] for why bytes and not chars.

/// Truncate text to a byte budget, respecting UTF-8 char boundaries.
/// Appends a truncation notice if the text was cut.
///
/// **Bytes, deliberately, and the parameter used to be called `max_chars` while
/// the body measured `text.len()`.** The name was the bug, not the arithmetic:
/// this budget exists to bound PROMPT TOKENS, and for that bytes are the better
/// proxy. A BPE tokenizer working over UTF-8 spends more tokens per character on
/// non-Latin script than on ASCII, so charging by characters would hand a
/// Cyrillic or Devanagari result roughly twice the token budget of an English
/// one for the same nominal number. Charging by bytes tracks the real cost.
///
/// `pond-core` already had the honest version of this next door —
/// `context_budget::truncate_at_byte_budget(content, max_bytes)` — doing exactly
/// the same thing under a name that says so. This now agrees with it.
///
/// The cut still lands on a char boundary, so the output is always valid UTF-8;
/// only the accounting is in bytes.
pub fn truncate_to_budget(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
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
/// `max_bytes`, on the same reasoning as [`truncate_to_budget`] — the accumulator
/// is compared with `String::len()`, which is bytes.
///
/// A header with no items underneath it is a miss, not a result — see
/// [`format_no_results`] for why that distinction has to reach the model.
pub fn format_list_result(items: &[String], header: &str, max_bytes: usize) -> String {
    let mut result = if header.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", header)
    };

    let mut wrote_item = false;
    for item in items {
        let line = format!("- {}\n", item);
        if result.len() + line.len() > max_bytes {
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
/// (`giap-knowledge__search_wikipedia`, not `search_wikipedia`). A 2B model does not reliably
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

/// Format a "not configured" guidance message for tools that CANNOT work
/// without an API key.
///
/// Returns a message the LLM can relay to the user with signup instructions.
/// Only for a hard stop — `giap-wolfram` is the shape, since there is no keyless
/// way to compute an answer. A tool that merely gets *worse* without a key wants
/// [`format_degraded`]: telling the user a working tool "requires an API key to
/// work" is false, and a false explanation for a real result is worse than no
/// explanation.
pub fn format_not_configured(feature: &str, signup_url: &str) -> String {
    format!(
        "{feature} requires an API key to work. Get one free at {signup_url} — \
         then add it in Settings under 'Knowledge & Discovery'."
    )
}

/// Note that a result came from a keyless fallback, and is therefore worse than
/// the tool can do.
///
/// The counterpart to [`format_not_configured`], for the far more common case:
/// `search_news` without a Guardian key still answers, from the Wikimedia
/// featured feed rather than a keyword search; `get_stock_quote` without a
/// Finnhub key still answers, from an unofficial Yahoo endpoint. Both are real
/// answers and both are quietly worse, and nothing said so — the degradation was
/// an `eprintln!` on the server's stderr, which no user sees.
///
/// Appended AFTER the result rather than replacing it, and phrased as a fact
/// about the source rather than an instruction, because the answer is the
/// answer: a model told to relay setup advice tends to lead with it.
pub fn format_degraded(result: &str, what_is_missing: &str, signup_url: &str) -> String {
    format!(
        "{result}\n\n[Source note: this came from a free fallback because no \
         {what_is_missing} is configured. Better results are available with one — \
         free at {signup_url}, added in Settings. Mention this only if asked \
         about the source or the quality.]"
    )
}

/// Append [`format_degraded`]'s note to a tool result that already succeeded.
///
/// Takes and returns a `CallToolResult` so a degraded path is one line at the
/// call site — the alternative is every caller unwrapping content, formatting,
/// and rebuilding, which is how three call sites ended up saying nothing at all.
///
/// A result with no text content is returned untouched: there is nothing to
/// annotate, and inventing a body to hang a note on would turn "no answer" into
/// "an answer plus advice".
pub fn degrade_result(
    result: rmcp::model::CallToolResult,
    what_is_missing: &str,
    signup_url: &str,
) -> rmcp::model::CallToolResult {
    use rmcp::model::{Content, RawContent};
    let existing: String = result
        .content
        .iter()
        .filter_map(|c| match &c.raw {
            RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if existing.trim().is_empty() {
        return result;
    }
    rmcp::model::CallToolResult::success(vec![Content::text(format_degraded(
        &existing,
        what_is_missing,
        signup_url,
    ))])
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

    /// The budget is BYTES, and that is a decision rather than an accident.
    ///
    /// The parameter was called `max_chars` while the body compared
    /// `text.len()`, so which unit it meant was anyone's guess and the module
    /// doc asserted the wrong one. Bytes is right: the budget bounds prompt
    /// TOKENS, and a BPE tokenizer over UTF-8 spends more tokens per character
    /// on non-Latin script than on ASCII — so charging per character would hand
    /// a Cyrillic result roughly twice the token budget of an English one for
    /// the same nominal number. `pond-core`'s `truncate_at_byte_budget` had
    /// already made the same choice under an honest name.
    ///
    /// Pinned with a string whose two counts differ by exactly 2x, so a silent
    /// switch to `chars().count()` cannot pass.
    #[test]
    fn the_budget_is_counted_in_bytes_not_characters() {
        let cyrillic = "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440}";
        assert_eq!(cyrillic.chars().count(), 10, "fixture is not 10 chars");
        assert_eq!(cyrillic.len(), 19, "fixture is not 19 bytes");

        // Fits its byte budget exactly: returned untouched.
        assert_eq!(truncate_to_budget(cyrillic, 19), cyrillic);

        // Must be cut at 12 bytes. A char-counting implementation would see
        // 10 <= 12 and hand the whole string back.
        let cut = truncate_to_budget(cyrillic, 12);
        assert!(
            cut.contains("[Truncated"),
            "a 19-byte string survived a 12-byte budget, so the budget is being \
             counted in characters: {cut:?}"
        );

        // Bytes are the accounting, never the slicing — the cut still lands on a
        // char boundary and the result is valid UTF-8.
        assert!(
            cut.starts_with("\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}"),
            "cut mid-character: {cut:?}"
        );
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
                "giap-knowledge__search_wikipedia",
                "giap-knowledge__get_wikipedia_article",
            ],
        );
        assert!(
            msg.contains(
                "call giap-knowledge__search_wikipedia or giap-knowledge__get_wikipedia_article now"
            ),
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

    /// Every builtin server is served through the one supervised helper.
    ///
    /// All seventeen `spawn_*_server` functions carried the same seven lines,
    /// and all seventeen dropped the exit: `Ok(running) => { let _ =
    /// running.waiting().await; }`. `waiting()` returns when the server has
    /// STOPPED — a panicked handler, a closed transport, a peer that went away —
    /// and nothing was logged, so a dead extension presented to everyone
    /// downstream as "the model stopped using that tool".
    ///
    /// A source scan because the alternative is asserting on seventeen spawn
    /// sites individually, which is the duplication this replaced.
    #[test]
    fn no_server_is_spawned_outside_the_supervised_helper() {
        let mut offenders: Vec<String> = Vec::new();
        let mut served = 0usize;

        for (name, src) in TOOL_SOURCES {
            let code = strip_line_comments(src);
            if code.contains("crate::serve_builtin(") {
                served += 1;
            }
            // The shape that discards the exit, in any spacing.
            if code.contains("running.waiting()") {
                offenders.push((*name).to_string());
            }
            if code.contains("tokio::spawn(") && !code.contains("crate::serve_builtin(") {
                offenders.push(format!("{name} (raw tokio::spawn)"));
            }
        }

        // Vacuity control: if the helper were renamed, `served` would be 0 and
        // an empty offender list would read as success.
        assert!(
            served >= 15,
            "only {served} servers go through serve_builtin — the scan is looking \
             for the wrong name, so an empty offender list proves nothing"
        );
        assert!(
            offenders.is_empty(),
            "these spawn a server without supervision, so its death is silent: {offenders:?}"
        );
    }

    /// A bare suffix is not what the model sees in its schema — gemma-4-E2B
    /// apologised rather than mapping `search_wikipedia` onto
    /// `giap-knowledge__search_wikipedia`. Every suggestion must be callable verbatim,
    /// so scan the real call sites rather than trusting review.
    #[test]
    fn every_suggested_alternative_is_fully_qualified() {
        let mut checked = 0;
        for (name, src) in suggestion_sources() {
            for quoted in suggestions_in(src) {
                assert!(
                    quoted.starts_with("giap-") && quoted.contains("__"),
                    "{name}: suggested alternative '{quoted}' is not a \
                     fully-qualified schema name; the model cannot call it"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "the scan matched nothing — parser is broken");
    }

    /// The files whose `format_no_results` call sites are scanned. Every server
    /// that can report a miss belongs here; one left out is simply unchecked.
    fn suggestion_sources() -> &'static [(&'static str, &'static str)] {
        &[
            ("knowledge.rs", include_str!("knowledge.rs")),
            ("wolfram.rs", include_str!("wolfram.rs")),
            ("discovery.rs", include_str!("discovery.rs")),
            ("news.rs", include_str!("news.rs")),
            ("finance.rs", include_str!("finance.rs")),
            ("memory.rs", include_str!("memory.rs")),
            ("sensors.rs", include_str!("sensors.rs")),
            ("vision.rs", include_str!("vision.rs")),
        ]
    }

    /// Every quoted alternative passed to a `format_no_results` call in `src`.
    ///
    /// Bounds the argument list to its own matching paren, then the alternatives
    /// slice to its own matching bracket. Anything looser reads on into the next
    /// call's `what` string and starts asserting about prose.
    fn suggestions_in(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, _) in src.match_indices("format_no_results(") {
            let args = balanced(src, i + "format_no_results(".len(), '(', ')');
            let Some(open) = args.find("&[") else {
                continue;
            };
            let slice = balanced(&args, open + 2, '[', ']');
            out.extend(slice.split('"').skip(1).step_by(2).map(str::to_string));
        }
        out
    }

    /// Every `giap-*` tool this crate registers, as `extension__tool`.
    ///
    /// Parsed from source because there is no other honest input here:
    /// constructing all eighteen servers needs the goose submodule and a dozen
    /// repositories, and the registration list is a `OnceLock` that no-ops on a
    /// second call in-process. `registration_matches_the_catalog.rs` reaches the
    /// same conclusion about the extension list for the same reasons.
    ///
    /// **Line comments are stripped first.** `discovery.rs` carries a disabled
    /// tool whose doc comment shows the `#[tool(...)]` line it would need to
    /// come back; counting that would report a tool the model is never offered,
    /// which is the exact failure this inventory exists to prevent.
    ///
    /// `wolfram.rs` maps to `giap-knowledge`, not to a `giap-wolfram`: its tools
    /// are a second router composed onto the knowledge server, so the file name
    /// is not the extension name.
    /// Every MCP server source in this crate, keyed by the extension it
    /// registers. Module-level so the tool inventory and the supervision guard
    /// read one list — a second copy is the drift both of them exist to catch.
    ///
    /// `wolfram.rs` maps to `giap-knowledge`, not `giap-wolfram`: its tools are
    /// a second router composed onto the knowledge server, so the file name is
    /// not the extension name.
    const TOOL_SOURCES: &[(&str, &str)] = &[
        ("giap-audit", include_str!("audit.rs")),
        ("giap-context", include_str!("context.rs")),
        ("giap-device", include_str!("device.rs")),
        ("giap-device-control", include_str!("device_control.rs")),
        ("giap-discovery", include_str!("discovery.rs")),
        ("giap-draft", include_str!("draft.rs")),
        ("giap-finance", include_str!("finance.rs")),
        ("giap-knowledge", include_str!("knowledge.rs")),
        ("giap-knowledge", include_str!("wolfram.rs")),
        ("giap-memory", include_str!("memory.rs")),
        ("giap-news", include_str!("news.rs")),
        ("giap-orchestrator", include_str!("orchestrator.rs")),
        ("giap-schedule", include_str!("schedule.rs")),
        ("giap-sensors", include_str!("sensors.rs")),
        ("giap-system", include_str!("system.rs")),
        ("giap-toolkit", include_str!("toolkit.rs")),
        ("giap-vision", include_str!("vision.rs")),
        ("giap-weather", include_str!("weather.rs")),
    ];

    fn registered_tools() -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for (ext, src) in TOOL_SOURCES {
            let code = strip_line_comments(src);
            for (i, _) in code.match_indices("#[tool(") {
                if let Some(name) = next_fn_ident(&code[i + "#[tool(".len()..]) {
                    out.insert(format!("{ext}__{name}"));
                }
            }
        }
        out
    }

    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The name of the first `fn <ident>(` at or after `s`. A `#[tool]`
    /// attribute always immediately precedes the method it annotates.
    fn next_fn_ident(s: &str) -> Option<String> {
        let mut idx = 0;
        while let Some(rel) = s[idx..].find("fn ") {
            let at = idx + rel;
            let boundary = s[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if boundary {
                let after = &s[at + 3..];
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && after[name.len()..].trim_start().starts_with('(') {
                    return Some(name);
                }
            }
            idx = at + 3;
        }
        None
    }

    /// The inventory parser must not be quietly reading nothing.
    #[test]
    fn the_tool_inventory_parser_sees_the_tools_that_are_there() {
        let tools = registered_tools();
        assert!(
            tools.contains("giap-weather__get_current_weather"),
            "{tools:?}"
        );
        // The composed second router on giap-knowledge — the case a file-name
        // to extension-name assumption gets wrong.
        assert!(
            tools.contains("giap-knowledge__compute_answer"),
            "{tools:?}"
        );
        // Disabled 2026-08-13 by removing its #[tool] attribute. Its doc comment
        // still shows that attribute, so this is also the comment-stripping test.
        assert!(!tools.contains("giap-discovery__search_web"), "{tools:?}");
        assert_eq!(
            tools.len(),
            68,
            "the tool inventory changed. Update this count in the same commit as \
             the tool — the number is the record, and a stale one is how a count \
             read 64 for months while the real number was 65. This assertion has \
             been wrong twice itself: once written as 65 on a branch 17 commits \
             behind main, and once pointing at an AGENTS.md that had since been \
             taken out of the tree entirely (b971f6ab), which is why it no longer \
             names a file to go and edit."
        );
    }

    /// A suggestion must name a tool that EXISTS, not merely one that is
    /// spelled like a tool.
    ///
    /// `format_no_results`' own doc says never to name a tool that does not
    /// exist, because a fabricated suggestion burns the one retry the model
    /// makes. Nothing enforced it: when `search_web` was disabled, seventeen
    /// call sites across four files went on pointing at it, and the sibling
    /// guard below was happy because `giap-discovery__search_web` is
    /// well-formed. Well-formed and callable are different claims.
    #[test]
    fn every_suggested_alternative_is_a_tool_that_exists() {
        let tools = registered_tools();
        let mut checked = 0;
        for (name, src) in suggestion_sources() {
            for quoted in suggestions_in(src) {
                assert!(
                    tools.contains(&quoted),
                    "{name}: suggests '{quoted}', which no server registers. \
                     Either the tool was removed or renamed and this call site \
                     was missed, or the name is a typo the model will burn a \
                     retry on."
                );
                checked += 1;
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

    /// A working tool that got a worse answer is not an unconfigured tool.
    ///
    /// `format_not_configured` says "requires an API key to work". For
    /// `search_news` and `get_stock_quote` that is false — both answer without
    /// one, from the Wikimedia feed and an unofficial Yahoo endpoint. Saying
    /// they do not work would be a false explanation attached to a real result,
    /// which is worse than the silence it replaced.
    #[test]
    fn a_degraded_result_keeps_its_answer_and_names_what_is_missing() {
        let out = format_degraded(
            "Top story: Kenya election",
            "Guardian API key",
            "https://x.test",
        );
        assert!(
            out.starts_with("Top story: Kenya election"),
            "the answer must come first — a note that displaces the result is a \
             regression, not a warning: {out}"
        );
        assert!(out.contains("Guardian API key"), "{out}");
        assert!(out.contains("https://x.test"), "{out}");
        assert!(
            !out.contains("requires an API key to work"),
            "a degraded result must not claim the tool is non-functional: {out}"
        );
    }

    /// Nothing to annotate means nothing is invented.
    #[test]
    fn degrading_an_empty_result_changes_nothing() {
        use rmcp::model::CallToolResult;
        let empty = CallToolResult::success(vec![]);
        let out = degrade_result(empty, "Some key", "https://x.test");
        assert!(
            out.content.is_empty(),
            "a note was hung on a result with no body, turning 'no answer' into \
             'an answer plus advice'"
        );
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
