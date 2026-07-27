//! The per-request reasoning-budget note shown to the model.
//!
//! The agent engine caps a request at N provider calls ("turns") but never
//! tells the model — and GIAP's provider shim strips the engine's own
//! turn-context block, because that block is not byte-stable across turns and
//! would break KV prefix reuse. So GIAP states the budget itself.
//!
//! Placement is the whole reason this is a per-REQUEST note and not a per-turn
//! one: it rides the `<system-context>` block of the user message, which the
//! trimmer strips from prior turns, so the system prefix stays byte-identical
//! across a session. The adapter builds the user message once, before the agent
//! loop starts, so a live "turn 7 of 50" counter is not available here without
//! a seam inside the engine's loop.

/// The note to place inside `<system-context>` for a request whose budget is
/// `max_steps` tool-calling steps, or `None` when reasoning is uncapped.
///
/// Wrapped in a `<turn-budget>` element to match the v2 tag skeleton the
/// prompt templates use, so a small model can tell it apart from the request.
pub fn turn_budget_note(max_steps: Option<u32>) -> String {
    let body = match max_steps {
        // Uncapped: the failure mode to guard against is the model stopping
        // early and asking permission to continue, so say so explicitly.
        None => "Your reasoning budget for this request is unbounded. Take as many \
                 tool-calling steps as the task genuinely needs and do not stop \
                 early to ask whether you should keep going — finish the task, \
                 then answer."
            .to_string(),
        // Capped: the model should pace itself and, crucially, still produce an
        // answer rather than being cut off mid-plan.
        Some(steps) => format!(
            "You may take up to {steps} tool-calling steps for this request. Pace \
             yourself: if you are running out of steps, stop gathering and answer \
             with what you have."
        ),
    };
    format!("<turn-budget>\n{body}\n</turn-budget>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_note_states_the_step_count() {
        let note = turn_budget_note(Some(50));
        assert!(note.starts_with("<turn-budget>\n"));
        assert!(note.ends_with("\n</turn-budget>"));
        assert!(note.contains("up to 50 tool-calling steps"), "{note}");
        // A capped budget must not tell the model its reasoning is unbounded.
        assert!(!note.contains("unbounded"));
    }

    #[test]
    fn uncapped_note_never_quotes_the_sentinel() {
        let note = turn_budget_note(None);
        assert!(note.contains("unbounded"), "{note}");
        // The sentinel (100_000) must never leak into the prompt as a number.
        assert!(!note.contains("100000"));
        assert!(!note.contains("100_000"));
        // And it must push against stopping early to ask permission.
        assert!(note.contains("do not stop"), "{note}");
    }

    /// The note is one self-contained element: the adapter concatenates it into
    /// `<system-context>` next to `<memories>`, so it must not leak newlines at
    /// the edges or nest another `<system-context>`.
    #[test]
    fn note_is_a_single_well_formed_element() {
        for note in [turn_budget_note(None), turn_budget_note(Some(8))] {
            assert_eq!(note.matches("<turn-budget>").count(), 1);
            assert_eq!(note.matches("</turn-budget>").count(), 1);
            assert!(!note.contains("<system-context>"));
            assert_eq!(note.trim(), note);
        }
    }
}
