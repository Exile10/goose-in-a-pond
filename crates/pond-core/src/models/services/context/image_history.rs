//! How many historical images are carried as real pixels (phase F2).
//!
//! # The problem this solves
//!
//! Attachments ARE persisted (`message_attachments`, written by the SQLite
//! session-storage adapter), so a follow-up question about an earlier picture
//! can in principle be answered. But keeping every image a session ever
//! contained is a trap on-device:
//!
//! - Any turn whose conversation contains an image is a multimodal turn, and
//!   multimodal turns bypass the engine's retained KV prompt-session cache
//!   entirely. One image early in a session would therefore make EVERY later
//!   turn in that session pay a full prefill, forever.
//! - Each image re-runs the mmproj vision encoder and re-expands into its
//!   prompt tokens, on a device with a 4096-token context. Measured on a real
//!   four-turn conversation: 1 then 2 then 3 encodes per turn (0.7-2.7s each),
//!   prefill climbing 28s to 37s. The cost compounds with the transcript.
//!
//! So the policy is: keep the most recent images up to a small budget, and
//! degrade older ones to a text placeholder. The placeholder matters — without
//! it the model sees a user turn that says "what colour is this?" with nothing
//! attached and confabulates an answer. With it, the model knows an image was
//! there and can ask.
//!
//! # Both history paths use this, and must agree
//!
//! - **Hydration replay** rebuilds a fresh engine session from durable GIAP
//!   history after a restart.
//! - **The live in-turn trimmer** rewrites the conversation the engine already
//!   holds, before every turn (when `hybrid_compaction_enabled`).
//!
//! They see the same conversation from different sides, so a session that
//! survives a restart must not gain or lose images in the process. Both call
//! [`plan_history_images`] rather than choosing their own budget.
//!
//! In both paths the image on the turn ABOUT to be sent is outside this budget:
//! hydration drops the trailing user message, and the live trimmer runs before
//! the new message is appended. Neither is history yet.
//!
//! This module is the pure decision function so the policy is testable without
//! a database, an engine, or a session.

use std::borrow::Cow;

/// Maximum number of historical images replayed as real image content.
///
/// One is intentional rather than timid: the overwhelmingly common shape is
/// "here is a photo" followed by two or three questions about that same photo,
/// which this serves exactly. Raising it multiplies the per-turn prefill cost
/// of every subsequent turn in the session.
///
/// The image on the CURRENT turn is not drawn from this budget — it is not
/// history yet.
pub const MAX_HISTORY_REPLAY_IMAGES: usize = 1;

/// Opening marker shared by every placeholder wording.
///
/// Detection and removal key off this rather than an exact string, so a
/// reworded placeholder — or one written by an earlier build and reloaded from
/// the engine's own session store — is still recognised and replaced instead of
/// accumulating alongside the new one.
pub const HISTORY_IMAGE_PLACEHOLDER_MARKER: &str = "[image attachment removed";

/// Stand-in text for a message whose images were ALL dropped.
///
/// Phrased so a model reading the transcript understands that something visual
/// existed and is no longer visible, rather than that the user sent nothing.
pub const HISTORY_IMAGE_PLACEHOLDER: &str =
    "[image attachment removed: an image attached here is no longer available in this context]";

/// Stand-in text for a message that lost SOME images and still shows at least
/// one.
///
/// A separate wording because the obvious one is a lie in that state: a message
/// rendered as [text, image, "an image was attached here but is no longer
/// available"] contradicts the image sitting right next to it, which is exactly
/// the confusion the `<vision>` prompt section exists to prevent. It also has to
/// read correctly whichever side the surviving image lands on — the live cap
/// appends the placeholder after the images, the hydration replay writes it
/// before them — so it says "still shown in this message", never "above".
///
/// Count-free on purpose: capping is staged (a message can be capped again on a
/// later turn), and a number written on the first pass cannot be kept true on
/// the second without parsing it back out of the transcript.
pub const HISTORY_IMAGE_PLACEHOLDER_PARTIAL: &str =
    "[image attachment removed: at least one other image attached here is no longer available in \
     this context; the image still shown in this message is unaffected]";

/// The placeholder that is TRUE for a message left holding `kept` of its images.
///
/// `kept` must be the number of images the message ACTUALLY carries after the
/// caller has finished attaching them — never the number
/// [`plan_history_images`] asked for. The two diverge whenever the bytes behind
/// a planned image cannot be loaded, and passing the plan produces a message
/// that reads "the image still shown in this message" while carrying none,
/// which is the precise contradiction this whole module exists to prevent.
#[must_use]
pub fn history_image_placeholder(kept: usize) -> &'static str {
    if kept == 0 {
        HISTORY_IMAGE_PLACEHOLDER
    } else {
        HISTORY_IMAGE_PLACEHOLDER_PARTIAL
    }
}

/// Whether `text` already carries a placeholder from an earlier capping pass.
#[must_use]
pub fn contains_history_image_placeholder(text: &str) -> bool {
    text.contains(HISTORY_IMAGE_PLACEHOLDER_MARKER)
}

/// Remove every placeholder from `text`, so exactly one can be re-emitted for
/// the message's CURRENT state.
///
/// This is what makes repeated capping converge. A message with two images and
/// a budget of one is capped twice — 2 to 1 when it becomes history, 1 to 0 when
/// a newer image turn arrives — and by the second pass the first pass's
/// placeholder is part of the message text, so appending another would leave the
/// model reading two contradictory stand-ins for the same attachment.
///
/// Removal spans the marker through its closing `]`, so it works for any
/// wording; an unterminated marker takes the rest of the string, which is the
/// safe direction (a truncated placeholder is not worth preserving).
///
/// # Two known imprecisions, both left alone deliberately
///
/// - A user who types the literal marker text loses everything from it to the
///   next `]`. Narrowing the match to the exact wordings we emit would break the
///   one job this function has — recognising a placeholder written by an EARLIER
///   build, which by definition uses a wording this build does not know.
/// - The result is trimmed only when something was removed; text with no
///   placeholder is returned borrowed and untouched. The trim exists for the
///   case where the message was ONLY a placeholder, so that the caller's
///   `format!("{stripped}\n{placeholder}")` does not open with a blank line. It
///   is not a general promise to trim.
#[must_use]
pub fn strip_history_image_placeholders(text: &str) -> Cow<'_, str> {
    if !contains_history_image_placeholder(text) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(HISTORY_IMAGE_PLACEHOLDER_MARKER) {
        out.push_str(&rest[..start]);
        let after = &rest[start + HISTORY_IMAGE_PLACEHOLDER_MARKER.len()..];
        match after.find(']') {
            Some(end) => rest = &after[end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out.trim().to_string())
}

/// Decide, for each historical message, how many of its images to replay.
///
/// `image_counts` is the number of stored images per message in CHRONOLOGICAL
/// order (0 for text-only messages). The returned vector is the same length and
/// holds the number of images to replay for that message; `image_counts[i] -
/// returned[i]` images were dropped and should be represented by
/// [`HISTORY_IMAGE_PLACEHOLDER`].
///
/// Budget is spent newest-first, and within a message the LEADING images are
/// kept so ordinal 0 stays ordinal 0 (a partially-replayed message keeps its
/// first image, which is the one users refer to).
#[must_use]
pub fn plan_image_replay(image_counts: &[usize], budget: usize) -> Vec<usize> {
    let mut plan = vec![0usize; image_counts.len()];
    let mut remaining = budget;
    for (slot, count) in plan.iter_mut().zip(image_counts.iter()).rev() {
        if remaining == 0 {
            break;
        }
        let take = (*count).min(remaining);
        *slot = take;
        remaining -= take;
    }
    plan
}

/// [`plan_image_replay`] at the default [`MAX_HISTORY_REPLAY_IMAGES`] budget.
///
/// Both history paths call this rather than passing their own budget, so the
/// live trimmer and the hydration replay cannot drift apart (see the module
/// docs for why that matters).
#[must_use]
pub fn plan_history_images(image_counts: &[usize]) -> Vec<usize> {
    plan_image_replay(image_counts, MAX_HISTORY_REPLAY_IMAGES)
}

/// How many images a plan drops — i.e. how many [`HISTORY_IMAGE_PLACEHOLDER`]
/// stand-ins are owed, and whether there is any rewriting to do at all.
///
/// Zero is the signal a caller uses to leave the conversation byte-identical.
#[must_use]
pub fn dropped_image_count(image_counts: &[usize], plan: &[usize]) -> usize {
    image_counts
        .iter()
        .zip(plan.iter())
        .map(|(had, keep)| had.saturating_sub(*keep))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_plans_nothing() {
        assert!(plan_image_replay(&[], MAX_HISTORY_REPLAY_IMAGES).is_empty());
    }

    #[test]
    fn text_only_history_replays_nothing() {
        assert_eq!(plan_image_replay(&[0, 0, 0], 2), vec![0, 0, 0]);
    }

    #[test]
    fn budget_is_spent_on_the_newest_images_first() {
        // Three image-bearing turns, budget of one: only the last keeps pixels.
        assert_eq!(plan_image_replay(&[1, 0, 1, 0, 1], 1), vec![0, 0, 0, 0, 1]);
    }

    #[test]
    fn budget_spills_backwards_when_the_newest_turn_is_smaller() {
        // Newest turn has one image, budget is three -> reach further back.
        assert_eq!(plan_image_replay(&[2, 0, 1], 3), vec![2, 0, 1]);
        assert_eq!(plan_image_replay(&[2, 0, 1], 2), vec![1, 0, 1]);
    }

    #[test]
    fn a_message_is_replayed_partially_rather_than_not_at_all() {
        // Budget 2 against a single 4-image turn keeps the first two.
        assert_eq!(plan_image_replay(&[4], 2), vec![2]);
    }

    #[test]
    fn zero_budget_drops_everything() {
        assert_eq!(plan_image_replay(&[3, 2, 1], 0), vec![0, 0, 0]);
    }

    #[test]
    fn plan_never_exceeds_what_a_message_actually_has() {
        let counts = [1usize, 2, 0, 1];
        let plan = plan_image_replay(&counts, 100);
        assert_eq!(plan, counts);
    }

    #[test]
    fn default_budget_keeps_only_the_latest_image() {
        let plan = plan_image_replay(&[1, 1], MAX_HISTORY_REPLAY_IMAGES);
        assert_eq!(plan, vec![0, 1]);
    }

    #[test]
    fn plan_history_images_uses_the_shared_budget() {
        assert_eq!(
            plan_history_images(&[1, 0, 1, 0, 1]),
            plan_image_replay(&[1, 0, 1, 0, 1], MAX_HISTORY_REPLAY_IMAGES)
        );
    }

    /// A conversation already inside the budget must report nothing dropped —
    /// that is what lets both callers skip rewriting entirely.
    #[test]
    fn nothing_is_dropped_when_history_already_fits() {
        for counts in [vec![], vec![0, 0, 0], vec![0, 1, 0]] {
            let plan = plan_history_images(&counts);
            assert_eq!(dropped_image_count(&counts, &plan), 0, "counts={counts:?}");
            assert_eq!(plan, counts, "counts={counts:?}");
        }
    }

    #[test]
    fn dropped_count_is_every_image_beyond_the_budget() {
        let counts = vec![2, 0, 3, 1];
        let plan = plan_history_images(&counts);
        assert_eq!(plan, vec![0, 0, 0, 1]);
        assert_eq!(dropped_image_count(&counts, &plan), 5);
    }

    /// Defensive: a caller that hands over mismatched lengths gets a count over
    /// the overlap rather than a panic. `zip` stops at the shorter side.
    #[test]
    fn dropped_count_tolerates_a_short_plan() {
        assert_eq!(dropped_image_count(&[1, 1, 1], &[0]), 1);
    }

    // ── Placeholder wording and convergence ───────────────────────────────

    /// Both wordings must be findable by the marker, or a second capping pass
    /// cannot tell that a placeholder is already there and appends another.
    #[test]
    fn every_wording_carries_the_marker() {
        for text in [HISTORY_IMAGE_PLACEHOLDER, HISTORY_IMAGE_PLACEHOLDER_PARTIAL] {
            assert!(text.starts_with(HISTORY_IMAGE_PLACEHOLDER_MARKER), "{text}");
            assert!(text.ends_with(']'), "{text}");
            assert!(contains_history_image_placeholder(text), "{text}");
        }
        assert_eq!(history_image_placeholder(0), HISTORY_IMAGE_PLACEHOLDER);
        for kept in 1..4 {
            assert_eq!(
                history_image_placeholder(kept),
                HISTORY_IMAGE_PLACEHOLDER_PARTIAL
            );
        }
    }

    /// The partial wording exists because the all-dropped one is FALSE while an
    /// image is still attached. It must not claim the message has no image, and
    /// it must not say "above" — the live cap appends it after the surviving
    /// image, the hydration replay writes it before.
    #[test]
    fn the_partial_wording_does_not_contradict_the_surviving_image() {
        let partial = HISTORY_IMAGE_PLACEHOLDER_PARTIAL.to_lowercase();
        assert!(
            partial.contains("other image"),
            "must describe the DROPPED images, not the kept one"
        );
        assert!(
            partial.contains("still shown"),
            "must acknowledge the image that is still attached"
        );
        for positional in ["above", "below", "preceding", "following"] {
            assert!(
                !partial.contains(positional),
                "'{positional}' is wrong on one of the two callers"
            );
        }
    }

    #[test]
    fn text_without_a_placeholder_is_borrowed_unchanged() {
        let text = "what colour is this?";
        let stripped = strip_history_image_placeholders(text);
        assert!(matches!(stripped, Cow::Borrowed(_)));
        assert_eq!(stripped, text);
    }

    #[test]
    fn stripping_removes_every_placeholder_and_leaves_the_real_text() {
        let text = format!("look at this\n{HISTORY_IMAGE_PLACEHOLDER}");
        assert_eq!(strip_history_image_placeholders(&text), "look at this");

        // Both wordings, in either order, and more than one of them.
        let doubled = format!(
            "{HISTORY_IMAGE_PLACEHOLDER_PARTIAL}\nwhat colour is this?\n{HISTORY_IMAGE_PLACEHOLDER}"
        );
        let stripped = strip_history_image_placeholders(&doubled);
        assert_eq!(stripped, "what colour is this?");
        assert!(!contains_history_image_placeholder(&stripped));
    }

    #[test]
    fn stripping_a_message_that_was_only_a_placeholder_leaves_nothing() {
        assert_eq!(
            strip_history_image_placeholders(HISTORY_IMAGE_PLACEHOLDER),
            ""
        );
    }

    /// Defensive: a truncated placeholder (a transcript cut mid-bracket) must
    /// not leave half a stand-in behind for the model to read.
    #[test]
    fn an_unterminated_placeholder_is_dropped_whole() {
        let text = format!("look\n{HISTORY_IMAGE_PLACEHOLDER_MARKER}: an image was att");
        assert_eq!(strip_history_image_placeholders(&text), "look");
    }

    /// Strip-then-emit is the convergence rule the capping path relies on:
    /// applying it repeatedly must reach a fixed point of exactly one
    /// placeholder rather than one per pass.
    #[test]
    fn repeated_strip_and_emit_converges_to_one_placeholder() {
        let mut text = "what colour is this?".to_string();
        for kept in [1usize, 0, 0] {
            text = format!(
                "{}\n{}",
                strip_history_image_placeholders(&text),
                history_image_placeholder(kept)
            );
            assert_eq!(
                text.matches(HISTORY_IMAGE_PLACEHOLDER_MARKER).count(),
                1,
                "one placeholder per state, never one per pass: {text}"
            );
        }
        assert!(text.starts_with("what colour is this?"));
        assert!(text.ends_with(HISTORY_IMAGE_PLACEHOLDER));
    }
}
