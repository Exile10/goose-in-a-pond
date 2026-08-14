//! The engine's own nudges must not be quotable.
//!
//! Goose appends invisible USER messages to steer a turn: a completeness check
//! when a turn ends without a tool call, a grind reminder, and a kickoff for the
//! `/goal` command. "Invisible" means invisible to the *person* — the model sees
//! them as ordinary user text, appended after the system prompt, the whole tool
//! schema and the entire history. They are the last thing in the context before
//! generation.
//!
//! That position is the problem. Measured 2026-08-12 on gemma-4-E2B and E4B, the
//! models answered in the harness's vocabulary: *"I could not fully meet your
//! goal"*, *"The goal has not been fully met"*. The wording handed them the noun:
//! `**Goal:** {goal}`, bolded, with "goal" said three more times around it.
//!
//! GIAP's system prompt forbids exactly this, and the prohibition was not enough.
//! `crates/pond-mcp-server/src/format.rs` had already written down why, from an
//! unrelated measurement on the same models: *"a competing suggestion beats a
//! buried one."* The prohibition sits in the static prefix, thousands of tokens
//! back; the nudge sits at position −1.
//!
//! So the fix is the nudge's wording, and this guard pins it. **It is deliberately
//! not a test of GIAP's prompt** — `every_style_forbids_narrating_the_harness` in
//! `pond-core` covers that half, and its doc records that the half it could not
//! cover was this one.
//!
//! WHY THIS CRATE. `include_str!` over the submodule makes the including crate
//! unbuildable without `git submodule update --init`. `pond-core` is in CI's
//! "fast crates" set precisely because it does not pull goose, and putting this
//! guard there would quietly end that split. `pond-adapters-goose` already owns
//! the goose relationship and is already excluded from the fast set.
//!
//! WHAT IS NOT GUARDED, and why. `stop_hook_denial_context_message` builds a
//! nudge naming a plugin and a "policy hook denial". It is the same category of
//! harness vocabulary, and it is left alone: GIAP never arms a stop hook, so no
//! measurement here could show the wording mattering, and every line changed in
//! the fork is a line to re-apply at every upstream rebase. It is included in the
//! scan below and passes on its own merits — it happens to name no goal.

#![cfg(test)]

/// The fork file that builds the engine's steering messages.
const AGENT_RS: &str = include_str!("../../../goose/crates/goose/src/agents/agent.rs");

/// Below this the extractor has broken, not the nudges vanished. There are three
/// goal/grind/kickoff injections plus the stop-hook one.
const FLOOR_NUDGES: usize = 4;

/// The source with `//` line comments removed, string literals intact.
///
/// Load-bearing: the comments this change added to `agent.rs` quote the old
/// wording verbatim in order to explain it, so without the strip this guard
/// would fail on the documentation of its own fix.
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

/// The text of every message the engine appends to the model's context.
///
/// Anchored on the bindings rather than on `Message::user()`, because the text is
/// built into a variable one statement earlier and a windowed search around the
/// constructor would either miss it or drag in the neighbouring notification.
///
/// Returns `(anchor, literal_text)` with `{...}` interpolations removed — the
/// variable is *named* `goal`, and `{goal}` is a value, not vocabulary.
fn injected_message_texts(src: &str) -> Vec<(String, String)> {
    const ANCHORS: &[&str] = &["let nudge = format!(", "let kickoff = Message::user()"];
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

/// The exact string that produced the measured leak, pinned by itself.
///
/// Separate from the loop above because it is the specific regression, and a
/// specific regression deserves a test that names it rather than one that
/// catches it as a side effect of a general rule.
#[test]
fn the_bolded_goal_label_is_gone_from_the_whole_file() {
    let code = strip_line_comments(AGENT_RS);
    assert!(
        !code.contains("**Goal:"),
        "`**Goal:` is back in agent.rs. This is the literal wording measured \
         leaking into household-facing answers on both gemma-4-E2B and E4B."
    );
}
