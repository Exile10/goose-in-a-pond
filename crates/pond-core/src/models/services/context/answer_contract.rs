//! Restates the answer rule last in the turn message; stated once ~6k tokens up, it decays.
//! Keep the rules in the system prefix too: subagents never see `<system-context>`.

/// Answer-shape note, last in `<system-context>`. The rule alone: its worked example (about 30
/// tokens) went on 2026-10-14 so a short turn fits one 128-token LiteRT prefill chunk, and an
/// example is also what a small model copies (see the domain test below).
pub fn answer_contract() -> String {
    format!("<answer-contract>\n{ANSWER_RULE}\n</answer-contract>")
}

/// The bare rule, shared with `orchestrator.rs :: subagent_envelope` so the two can't drift.
/// "The route to it" means narrating retrieval, not reasoning (`<thinking>` gates that).
pub const ANSWER_RULE: &str = "Reply with the result, not the route to it. If part of it \
                               failed, name that part in ordinary words.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_is_a_balanced_element() {
        let note = answer_contract();
        assert!(note.starts_with("<answer-contract>\n"), "{note}");
        assert!(note.ends_with("\n</answer-contract>"), "{note}");
        assert_eq!(note.matches("<answer-contract>").count(), 1);
        assert_eq!(note.matches("</answer-contract>").count(), 1);
    }

    #[test]
    fn it_states_the_rule_with_no_example_to_copy() {
        let note = answer_contract();
        assert!(
            note.to_lowercase().contains("not the route"),
            "the rule itself is missing: {note}"
        );
        assert!(
            !note.contains('"'),
            "a quoted example is back. It costs a short turn a second LiteRT prefill chunk, \
             and small models copy examples: {note}"
        );
    }

    /// Should an example return: one in a domain the pond serves gets copied as a fake reading.
    #[test]
    fn the_example_cannot_be_mistaken_for_a_real_result() {
        let note = answer_contract().to_lowercase();

        // Domains the pond has tools for.
        for word in [
            "degrees",
            "celsius",
            "fahrenheit",
            "cloudy",
            "overcast",
            "sunny",
            "raining",
            "weather",
            "temperature",
            "stock",
            "usd",
            "bitcoin",
        ] {
            assert!(
                !note.contains(word),
                "the answer-contract example mentions '{word}', a domain this pond \
                 serves with a real tool — a model that copies the example then \
                 fabricates a plausible reading instead of using the tool result: \
                 {note}"
            );
        }

        assert!(
            !note.contains("nairobi"),
            "the example names this pond's own city: {note}"
        );
    }

    /// "Be terse" alone teaches dropping the failed part, the one part the user can't reconstruct.
    #[test]
    fn a_shortfall_is_still_reportable() {
        let lower = answer_contract().to_lowercase();
        assert!(
            lower.contains("failed") && lower.contains("ordinary words"),
            "the contract suppresses the answer's shape without preserving the \
             admission beside it"
        );
    }

    #[test]
    fn it_stays_small_enough_to_repeat_every_turn() {
        let chars = answer_contract().chars().count();
        assert!(
            chars <= 160,
            "the answer contract is {chars} chars (~{} tokens), re-prefilled on \
             every turn. A short turn's whole envelope has to fit one 128-token LiteRT \
             prefill chunk; the rules themselves belong in the cached prefix.",
            chars / 4
        );
    }
}
