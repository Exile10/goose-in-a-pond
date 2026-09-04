//! Pins the wording of the user nudges goose appends last in the context (completeness check,
//! grind, `/goal` kickoff): on gemma-4 E2B/E4B a bolded `**Goal:**` there leaked into answers
//! despite the prompt's ban (`every_style_forbids_narrating_the_harness` covers that half). Lives
//! here, not in `pond-core`: `include_str!` over the goose submodule would cost it CI's fast set.

#![cfg(test)]

/// The fork file that builds the engine's steering messages.
const AGENT_RS: &str = include_str!("../../../goose/crates/goose/src/agents/agent.rs");

/// Below this the extractor has broken, not the nudges vanished. There are three
/// goal/grind/kickoff injections plus the stop-hook one.
const FLOOR_NUDGES: usize = 4;

/// The source with `//` line comments removed, string literals intact. Load-bearing: comments
/// in `agent.rs` quote the old wording verbatim, so without the strip this guard would fail
/// on the documentation of its own fix.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut cut = line.len();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The text of every message the engine appends to the model's context, returned as
/// `(anchor, literal_text)` with `{...}` interpolations removed (`{goal}` is a value, not a word).
/// Anchored on the bindings, not `Message::user()`: the text is built one statement earlier,
/// and a window around the constructor would miss it or drag in the neighbouring notification.
fn injected_message_texts(src: &str) -> Vec<(String, String)> {
    // `fn goal_nudge` is anchored because the fork moved the completeness nudge's text into
    // that helper; its body is a single `format!`, so the statement-to-first-semicolon walk
    // below captures exactly its string.
    const ANCHORS: &[&str] = &[
        "let nudge = format!(",
        "let kickoff = Message::user()",
        "fn goal_nudge",
    ];
    let code = strip_line_comments(src);
    let mut found = Vec::new();

    for anchor in ANCHORS {
        for (at, _) in code.match_indices(anchor) {
            // Take the statement: from the anchor to the first `;` that is not
            // inside a string literal.
            let tail = &code[at..];
            let mut in_string = false;
            let mut end = tail.len();
            let bytes = tail.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' if in_string => i += 1,
                    b'"' => in_string = !in_string,
                    b';' if !in_string => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
                i += 1;
            }
            let stmt = &tail[..end];

            // Concatenate every string literal in the statement, then drop the
            // `{...}` interpolations so a variable named `goal` is not mistaken
            // for the word.
            let mut text = String::new();
            for (n, piece) in stmt.split('"').enumerate() {
                if n % 2 == 1 {
                    text.push_str(piece);
                }
            }
            let mut cleaned = String::with_capacity(text.len());
            let mut depth = 0usize;
            for ch in text.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ if depth == 0 => cleaned.push(ch),
                    _ => {}
                }
            }
            found.push(((*anchor).to_string(), cleaned));
        }
    }
    found
}

/// No injected message hands the model a label to read back.
#[test]
fn no_injected_nudge_carries_a_quotable_label() {
    let nudges = injected_message_texts(AGENT_RS);
    assert!(
        nudges.len() >= FLOOR_NUDGES,
        "found {} injected messages (floor {FLOOR_NUDGES}) — the extractor is \
         broken, so an empty result proves nothing",
        nudges.len()
    );

    for (anchor, text) in &nudges {
        let lower = text.to_lowercase();
        assert!(
            !lower.contains("goal"),
            "an injected message still says \"goal\":\n  anchor: {anchor}\n  text: {text:?}\n\
             This message is appended as an invisible USER message at the END of the \
             context. E2B and E4B read that noun straight back to the household — \
             \"I could not fully meet your goal\" — and the system prompt cannot \
             outrank it from thousands of tokens away. Say what was asked, not what \
             the harness calls it."
        );
        assert!(
            !text.contains("**"),
            "an injected message carries Markdown emphasis:\n  anchor: {anchor}\n  \
             text: {text:?}\nEvery GIAP prompt style forbids Markdown in output, so a \
             bolded label in the model's last input is both a quotable label and a \
             formatting instruction that contradicts the prompt."
        );
    }
}

/// The exact string that produced the measured leak, pinned by itself so the specific
/// regression has a test that names it rather than one that catches it as a side effect.
#[test]
fn the_bolded_goal_label_is_gone_from_the_whole_file() {
    let code = strip_line_comments(AGENT_RS);
    assert!(
        !code.contains("**Goal:"),
        "`**Goal:` is back in agent.rs. This is the literal wording measured \
         leaking into household-facing answers on both gemma-4-E2B and E4B."
    );
}
