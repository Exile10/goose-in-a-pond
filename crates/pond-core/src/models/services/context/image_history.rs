//! How many historical images get replayed as real pixels (phase F2).
//!
//! # The problem this solves
//!
//! Attachments ARE persisted (`message_attachments`, written by the SQLite
//! session-storage adapter), so a follow-up question about an earlier picture
//! can in principle be answered. But replaying every image a session ever
//! contained is a trap on-device:
//!
//! - Any turn whose conversation contains an image is a multimodal turn, and
//!   multimodal turns bypass the engine's retained KV prompt-session cache
//!   entirely. One image early in a session would therefore make EVERY later
//!   turn in that session pay a full prefill, forever.
//! - Each replayed image re-runs the mmproj vision encoder and re-expands into
//!   its prompt tokens, on a device with a 4096-token context.
//!
//! So the policy is: replay the most recent images up to a small budget, and
//! degrade older ones to a text placeholder. The placeholder matters — without
//! it the model sees a user turn that says "what colour is this?" with nothing
//! attached and confabulates an answer. With it, the model knows an image was
//! there and can ask.
//!
//! This module is the pure decision function so the policy is testable without
//! a database, an engine, or a session.

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

/// Stand-in text for an image that was dropped from the replayed history.
///
/// Phrased so a model reading the transcript understands that something visual
/// existed and is no longer visible, rather than that the user sent nothing.
pub const HISTORY_IMAGE_PLACEHOLDER: &str =
    "[an image was attached here but is no longer available in this context]";

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
}
