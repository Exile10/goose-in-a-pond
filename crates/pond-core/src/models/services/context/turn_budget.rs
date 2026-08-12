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
        // Capped. The budget is a CEILING, not a target to economise against,
        // and this note used to read as the latter: "Pace yourself: if you are
        // running out of steps, stop gathering and answer with what you have."
        //
        // That is written into every default install, because the default
        // `agent_max_turns` is 50 and only `0` takes the uncapped branch. So the
        // shipped configuration told the model that a partial answer was
        // acceptable, while the non-default one told it the opposite. Measured
        // 2026-08-12 on "what time is it in the first 10 states alphabetically?":
        // gemma-4-E2B made ZERO tool calls and asserted one time for all ten
        // states, and nothing in the pipeline objected.
        //
        // Two things this must still do, which is why it is not simply the
        // uncapped text:
        //
        // * Not promise room the engine will not give. The cap is real; a model
        //   cut off mid-plan at step 50 with no instruction produces nothing
        //   usable.
        // * Make an incomplete answer SAY it is incomplete. That is the actual
        //   remedy for the failure above -- the danger was never that the answer
        //   was partial, it was that a partial answer read as a whole one.
        Some(steps) => format!(
            "You may take up to {steps} tool-calling steps for this request. Use as \
             many as the task genuinely needs — do not stop early, and do not ask \
             whether you should keep going. If you actually reach the limit, answer \
             with what you have AND say plainly which parts you could not complete. \
             Never present a partial answer as a complete one."
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

    /// Neither branch may tell the model that stopping short is acceptable.
    ///
    /// The capped branch used to, and it is the branch every default install
    /// takes — `agent_max_turns` defaults to 50, and only `0` reaches the
    /// uncapped text. So the shipped prompt said "if you are running out of
    /// steps, stop gathering and answer with what you have" while the
    /// non-default one said "finish the task, then answer". Measured on
    /// 2026-08-12: asked for the time in the first ten US states alphabetically,
    /// gemma-4-E2B made zero tool calls and asserted a single time for all ten.
    ///
    /// Quantified over BOTH branches on purpose. A guard written against only
    /// the capped one would pass again the moment somebody "helpfully" restored
    /// the pacing language to the other.
    #[test]
    fn no_budget_note_licenses_a_partial_answer() {
        for (label, note) in [
            ("uncapped", turn_budget_note(None)),
            ("capped", turn_budget_note(Some(50))),
            ("capped-small", turn_budget_note(Some(8))),
        ] {
            let lower = note.to_lowercase();
            for banned in ["stop gathering", "pace yourself"] {
                assert!(
                    !lower.contains(banned),
                    "the {label} turn-budget note tells the model to {banned:?}. The budget is a \
                     ceiling, not a target to economise against — this exact phrasing shipped in \
                     every default install and a 2B model answered a ten-item question with zero \
                     tool calls. Note: {note}"
                );
            }
            assert!(
                lower.contains("do not stop early"),
                "the {label} note does not push against stopping early. Note: {note}"
            );
        }
    }

    /// A capped budget can genuinely run out, and when it does the answer must
    /// announce what is missing. This is the half the old wording got right and
    /// the replacement must not lose: the danger was never that an answer was
    /// partial, it was that a partial answer read as a whole one.
    #[test]
    fn the_capped_note_requires_an_incomplete_answer_to_say_so() {
        let note = turn_budget_note(Some(50)).to_lowercase();
        assert!(
            note.contains("could not complete"),
            "the capped note no longer asks the model to name the parts it could not finish; \
             an unlabelled partial answer is indistinguishable from a complete one. Note: {note}"
        );
        assert!(
            note.contains("never present a partial answer as a complete one"),
            "the capped note lost the instruction not to pass off a partial answer as whole. \
             Note: {note}"
        );
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
