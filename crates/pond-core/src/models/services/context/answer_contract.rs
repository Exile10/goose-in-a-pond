//! The shape of the answer, restated where the model is about to write one.
//!
//! Every style's system prompt already says to give the result rather than an
//! account of producing it. This restates it in the user message, for one
//! reason: distance. The prompt says it once, at the top of a prefix that is
//! followed by roughly 6,000 tokens of tool schemas and then the whole history.
//! `crates/pond-mcp-server/src/format.rs` records what that costs on the models
//! this runs on -- a competing suggestion beats a buried one -- and an output
//! rule is the kind most exposed to it, because it has to survive all the way to
//! the last token.
//!
//! ## What this is NOT
//!
//! It is not where the rules live. The system prefix stays authoritative and
//! complete, and this is a two-line restatement of the part that decays with
//! distance.
//!
//! Moving the rules here instead would have broken every delegated subagent.
//! `orchestrator.rs` builds a child's whole system prompt from
//! `env.base_system_prefix` plus its own envelope, and `child_user_message`
//! carries no `<system-context>` at all -- so anything that LEFT the prefix would
//! simply not exist for the child, which is the agent that can least afford to
//! lose "an empty result is not an answer". That is why `answer_contract()` is
//! called from `orchestrator.rs` as well: the child gets the restatement in the
//! one place it can reach it.
//!
//! ## Placement, honestly
//!
//! This sits last inside `<system-context>`, immediately before
//! `<user-message>`. That is close to generation, not AT it: the user's own
//! words follow, `EMPTY_TURN_STEER` is appended after `</user-message>` on a
//! re-engagement, and the engine's completeness nudge lands after everything.
//! Nearer is the claim; last is not.
//!
//! ## Cost
//!
//! About 60 tokens of fresh prefill per turn, and no more: the trimmer strips
//! `<system-context>` from every prior user message, so this never accumulates
//! down a conversation. It does not move `prefix_hash`, which covers the
//! rendered template only, so the KV prefix is untouched -- invariant 1 holds.
//!
//! It is deliberately NOT charged against the preamble budget by hand.
//! `turn_trimmer::effective_history_budget` already corrects for a prompt that
//! overshot, using the engine's own reported size, and its doc says in terms
//! that it is the only place the effective budget is computed -- a second
//! subtraction here would be the four-paths-disagree shape PAI-3 exists to
//! remove. `turn_budget_note` rides the same block on the same terms.

/// The answer-shape note to place inside `<system-context>`, after
/// [`super::turn_budget::turn_budget_note`].
///
/// Wrapped in `<answer-contract>` to match the tag skeleton the templates use,
/// so a small model can tell it apart from the request -- and so it is covered
/// by the covertness rule, which says anything in angle brackets is plumbing.
///
/// The worked example is the point, not decoration. A single demonstration moves
/// format compliance far more than a paragraph of rules does on models this
/// size, and this repo found the same effect from the other side: `format.rs`
/// records a parenthetical example being obeyed *instead of* the instruction it
/// was attached to. Here the example and the instruction say the same thing.
pub fn answer_contract() -> String {
    format!(
        "<answer-contract>\n{ANSWER_RULE} Shape: \"It is 24 degrees and cloudy in \
         Nairobi, with rain this evening.\" — the answer, nothing about looking it \
         up.\n</answer-contract>"
    )
}

/// The rule itself, without the wrapper or the example.
///
/// Exists because there are two consumers and they need different presentations:
/// the chat path wants an angle-bracket element inside `<system-context>`, and
/// `orchestrator.rs :: subagent_envelope` wants a line in a Markdown rule list.
/// Sharing the STRING rather than the rendering is what stops the two drifting —
/// which is the defect `BUILTIN_PROMPT_TEMPLATES` was created to fix one layer up.
///
/// The subagent gets no worked example: its envelope already says "Your last
/// message IS the answer", and the example is shaped for a person reading a
/// reply, not for a parent agent reading a finding.
///
/// "the route to it" rather than "your reasoning" is deliberate. Reasoning may be
/// legitimate and is separately gated by `<thinking>`; what this forbids is
/// narrating the retrieval — which tool ran, in what order, how many steps.
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

    /// The instruction and the demonstration must agree, or the demonstration
    /// wins and the instruction is decoration.
    #[test]
    fn it_states_the_rule_and_shows_it() {
        let note = answer_contract();
        let lower = note.to_lowercase();
        assert!(
            lower.contains("not the route"),
            "the rule itself is missing: {note}"
        );
        assert!(
            note.contains('"'),
            "no worked example -- on a 2B model the demonstration is what carries \
             format compliance, not the sentence: {note}"
        );
        assert!(
            lower.contains("nothing about looking it up"),
            "the example does not say what it is an example OF: {note}"
        );
    }

    /// The admission survives here too. A contract that only says "be terse"
    /// teaches a model to drop the part of the answer that failed, which is the
    /// one part the user cannot reconstruct.
    #[test]
    fn a_shortfall_is_still_reportable() {
        let lower = answer_contract().to_lowercase();
        assert!(
            lower.contains("failed") && lower.contains("ordinary words"),
            "the contract suppresses the answer's shape without preserving the \
             admission beside it"
        );
    }

    /// It rides a per-turn message that is re-prefilled every turn, so its size
    /// is a latency cost paid on every request, not once per session.
    #[test]
    fn it_stays_small_enough_to_repeat_every_turn() {
        let chars = answer_contract().chars().count();
        assert!(
            chars <= 320,
            "the answer contract is {chars} chars (~{} tokens), re-prefilled on \
             every turn. Past ~320 it stops being a restatement and becomes a \
             second copy of the rules, which belongs in the cached prefix.",
            chars / 4
        );
    }
}
