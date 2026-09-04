//! Retrieval — the second `<system-context>` corpus (PAI-8 P2). Context items are not written into
//! `memories`; a mailbox would drown a curated store. [`split_preamble_budget`] SPLITS the memory
//! budget rather than adding, so no block escapes `CompactionProfile`'s arithmetic. The blend is
//! not memory's: an item has no importance, and its `occurred_at` may be in the future.

use chrono::{DateTime, Utc};

use crate::context::domain::ContextItem;

/// Weight of semantic similarity. The same 0.5 the memory blend uses.
pub const SIMILARITY_WEIGHT: f32 = 0.5;
/// Weight of how near the item's own moment is to now. Occupies the slot
/// memory gives to stored importance.
pub const PROXIMITY_WEIGHT: f32 = 0.3;
/// Weight of how recently the pond ingested it. Memory's recency slot.
pub const INGEST_RECENCY_WEIGHT: f32 = 0.2;

/// Half-life, in days, of both time terms. Matches
/// `memory_relevance::RECENCY_HALF_LIFE_DAYS`: an item from today scores 1.0,
/// one a fortnight away scores 0.5.
pub const HALF_LIFE_DAYS: f32 = 14.0;

/// Share of the preamble's memory budget that goes to context when there is any context to show.
///
/// A third, not a half: memory is the curated store. This is the number to turn down first when
/// the preamble has to shrink.
pub const CONTEXT_BUDGET_SHARE: f32 = 1.0 / 3.0;

/// The XML tag the context block rides in, inside the user message's `<system-context>`.
///
/// Distinct from `<memories>` so the model can tell what it was told from what arrived. Whoever
/// wires the block into the adapter also owes this tag an entry in `prompts.rs`.
pub const CONTEXT_BLOCK_TAG: &str = "personal-context";

/// Decay in `[0, 1]` for a gap of `days`, in either direction.
fn decay(days: f32) -> f32 {
    0.5_f32.powf(days.abs() / HALF_LIFE_DAYS)
}

/// How near this item's own moment is to `now` — **symmetric**, so a meeting
/// three weeks away decays exactly like a message three weeks old.
pub fn proximity_score(item: &ContextItem, now: DateTime<Utc>) -> f32 {
    decay((now - item.occurred_at()).num_seconds() as f32 / 86_400.0)
}

/// How recently the pond learned of this item. One-sided: an item cannot be
/// ingested in the future, and if a clock skew says it was, the answer is 1.0.
pub fn ingest_recency_score(item: &ContextItem, now: DateTime<Utc>) -> f32 {
    let days = (now - item.ingested_at()).num_seconds() as f32 / 86_400.0;
    if days <= 0.0 {
        1.0
    } else {
        decay(days)
    }
}

/// Blended score for one candidate.
///
/// `similarity` is `None` for a candidate that arrived by recency alone, and forfeits the term
/// rather than taking a neutral value; see `memory_relevance::relevance_score`.
pub fn relevance_score(item: &ContextItem, similarity: Option<f32>, now: DateTime<Utc>) -> f32 {
    let sim = similarity.unwrap_or(0.0).clamp(0.0, 1.0);
    SIMILARITY_WEIGHT * sim
        + PROXIMITY_WEIGHT * proximity_score(item, now)
        + INGEST_RECENCY_WEIGHT * ingest_recency_score(item, now)
}

/// Sort candidates best-first. Ties break on id so the rendered block — and
/// therefore the KV prefix beyond it — is reproducible for identical inputs.
pub fn rank_by_relevance(candidates: &mut [(ContextItem, Option<f32>)], now: DateTime<Utc>) {
    candidates.sort_by(|a, b| {
        let sa = relevance_score(&a.0, a.1, now);
        let sb = relevance_score(&b.0, b.1, now);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.id().cmp(b.0.id()))
    });
}

/// How the preamble's memory budget is divided this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSplit {
    pub memory_tokens: usize,
    pub context_tokens: usize,
}

/// Split the turn's memory budget between the two corpora.
///
/// `have_context` is "is there anything to put in the block", not "is the
/// feature on": a pond with no context sources must not pay a token for this.
pub fn split_preamble_budget(memory_token_budget: usize, have_context: bool) -> BudgetSplit {
    if !have_context {
        return BudgetSplit {
            memory_tokens: memory_token_budget,
            context_tokens: 0,
        };
    }
    let context_tokens = (memory_token_budget as f32 * CONTEXT_BUDGET_SHARE) as usize;
    BudgetSplit {
        memory_tokens: memory_token_budget.saturating_sub(context_tokens),
        context_tokens,
    }
}

/// Estimated prompt cost of one rendered item. The same chars/4 heuristic the
/// memory injection loop uses, over the same text that will be rendered.
pub fn estimated_tokens(item: &ContextItem) -> usize {
    render_line(item).len() / 4 + 1
}

/// Take items best-first until the budget is spent.
///
/// Keeps the first item even when it alone exceeds the budget, as the memory loop does: one
/// over-long item beats an empty block.
pub fn select_within_budget<'a>(
    ranked: &'a [(ContextItem, Option<f32>)],
    token_budget: usize,
) -> Vec<&'a ContextItem> {
    let mut used = 0usize;
    let mut kept: Vec<&ContextItem> = Vec::new();
    for (item, _) in ranked {
        let cost = estimated_tokens(item);
        if used + cost > token_budget && !kept.is_empty() {
            break;
        }
        used += cost;
        kept.push(item);
    }
    kept
}

/// One item as the model sees it.
///
/// The date is written out because "tomorrow" in an ingested body is relative to a moment the
/// model cannot see.
pub fn render_line(item: &ContextItem) -> String {
    let when = item.occurred_at().format("%Y-%m-%d %H:%M");
    let who = if item.participants().is_empty() {
        String::new()
    } else {
        format!(" ({})", item.participants().join(", "))
    };
    let body = item.body().trim();
    if body.is_empty() {
        format!(
            "- [{} {}] {}{}",
            item.source_kind().as_str(),
            when,
            item.title().trim(),
            who
        )
    } else {
        format!(
            "- [{} {}] {}{}: {}",
            item.source_kind().as_str(),
            when,
            item.title().trim(),
            who,
            body
        )
    }
}

/// The whole block, or an empty string when there is nothing to say.
///
/// Empty rather than an empty tag pair: a `<personal-context></personal-context>`
/// in every prompt costs tokens on every turn and tells the model nothing.
pub fn render_block(items: &[&ContextItem]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = items.iter().map(|i| render_line(i)).collect();
    format!(
        "<{tag}>\n{}\n</{tag}>",
        lines.join("\n"),
        tag = CONTEXT_BLOCK_TAG
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::{ItemKind, ItemParts, SourceKind};
    use crate::security::domain::redaction::RedactionKind;
    use crate::security::mocks::mock_redactor::MockRedactor;
    use chrono::Duration;

    fn item(id: &str, occurred_at: DateTime<Utc>, body: &str) -> ContextItem {
        let redactor = MockRedactor::replacing("never-matches", RedactionKind::ApiKey);
        ContextItem::from_parts(
            &redactor,
            ItemParts {
                id: id.into(),
                source_id: "src-1".into(),
                external_id: id.into(),
                profile_id: "jerry".into(),
                source_kind: SourceKind::Voice,
                kind: ItemKind::Message,
                occurred_at,
                ingested_at: occurred_at,
                title: format!("title {id}"),
                body: body.into(),
                participants: vec![],
                stored_sensitivity: None,
                embedding: None,
            },
        )
        .expect("valid item")
    }

    #[test]
    fn the_weights_are_a_partition() {
        let total = SIMILARITY_WEIGHT + PROXIMITY_WEIGHT + INGEST_RECENCY_WEIGHT;
        assert!(
            (total - 1.0).abs() < 1e-6,
            "the blend's weights sum to {total}, so a score is no longer in [0, 1]"
        );
    }

    /// The reason this file does not reuse `memory_relevance::recency_score`.
    /// A calendar event a year out must not score like one this afternoon.
    #[test]
    fn a_far_future_item_decays_like_a_far_past_one() {
        let now = Utc::now();
        let soon = item("a", now + Duration::hours(2), "standup");
        let far = item("b", now + Duration::days(365), "some day");
        let old = item("c", now - Duration::days(365), "long ago");

        assert!(proximity_score(&soon, now) > 0.9);
        assert!(
            proximity_score(&far, now) < 0.05,
            "a meeting a year away scored {}",
            proximity_score(&far, now)
        );
        assert!(
            (proximity_score(&far, now) - proximity_score(&old, now)).abs() < 1e-6,
            "the proximity term must be symmetric about now"
        );
    }

    #[test]
    fn ranking_puts_the_near_and_topical_item_first() {
        let now = Utc::now();
        let mut candidates = vec![
            (item("old", now - Duration::days(40), "old news"), None),
            (
                item("near", now + Duration::hours(1), "the dentist"),
                Some(0.8),
            ),
            (item("mid", now - Duration::days(3), "something"), None),
        ];
        rank_by_relevance(&mut candidates, now);
        assert_eq!(candidates[0].0.id(), "near");
        assert_eq!(candidates[2].0.id(), "old");
    }

    /// Ties break on id, so the block is byte-identical for identical inputs and
    /// the KV prefix beyond it does not move.
    #[test]
    fn an_exact_tie_breaks_deterministically() {
        let now = Utc::now();
        let a = (item("aaa", now, "same"), Some(0.5));
        let b = (item("bbb", now, "same"), Some(0.5));
        let mut one = vec![a.clone(), b.clone()];
        let mut two = vec![b, a];
        rank_by_relevance(&mut one, now);
        rank_by_relevance(&mut two, now);
        assert_eq!(one[0].0.id(), "aaa");
        assert_eq!(two[0].0.id(), "aaa");
    }

    /// A pond with no context sources must pay nothing: the memory block keeps
    /// every token it has today.
    #[test]
    fn an_empty_corpus_costs_the_memory_block_nothing() {
        let split = split_preamble_budget(3000, false);
        assert_eq!(split.memory_tokens, 3000);
        assert_eq!(split.context_tokens, 0);
    }

    /// The budget is a split of what the turn already had, never an addition:
    /// a block that appears in the prompt but not in the profile's arithmetic
    /// is how the history budget starts lying.
    #[test]
    fn the_split_never_adds_tokens_to_the_preamble() {
        for budget in [0usize, 1, 7, 500, 3000, 100_000] {
            let split = split_preamble_budget(budget, true);
            assert_eq!(
                split.memory_tokens + split.context_tokens,
                budget,
                "the split of {budget} does not add up"
            );
            assert!(
                split.memory_tokens >= split.context_tokens,
                "context took more of the preamble than memory at budget {budget}"
            );
        }
        assert!(
            split_preamble_budget(3000, true).context_tokens > 0,
            "with a budget of 3000 and something to show, context got nothing"
        );
    }

    #[test]
    fn the_budget_cuts_the_block_and_keeps_at_least_one() {
        let now = Utc::now();
        let long = "x".repeat(4000);
        let mut ranked = vec![
            (item("a", now, &long), Some(0.9)),
            (item("b", now, &long), Some(0.8)),
        ];
        rank_by_relevance(&mut ranked, now);

        let kept = select_within_budget(&ranked, 400);
        assert_eq!(kept.len(), 1, "the token budget was not applied");
        assert_eq!(kept[0].id(), "a");

        // A budget of zero still yields the single best item rather than an
        // empty block -- the same choice the memory loop makes.
        assert_eq!(select_within_budget(&ranked, 0).len(), 1);
        assert!(select_within_budget(&[], 1000).is_empty());
    }

    #[test]
    fn the_block_is_empty_when_there_is_nothing_to_say() {
        assert_eq!(render_block(&[]), "");
        let now = Utc::now();
        let one = item("a", now, "the boiler engineer comes at four");
        let block = render_block(&[&one]);
        assert!(block.starts_with("<personal-context>"));
        assert!(block.ends_with("</personal-context>"));
        assert!(block.contains("the boiler engineer comes at four"));
        assert!(
            block.contains(&now.format("%Y-%m-%d").to_string()),
            "the model cannot date an item it is not given a date for: {block}"
        );
        assert!(
            block.contains("voice"),
            "the block does not say where the item came from: {block}"
        );
    }
}
