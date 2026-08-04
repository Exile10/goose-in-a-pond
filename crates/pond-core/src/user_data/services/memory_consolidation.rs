//! Memory consolidation service.
//!
//! Two responsibilities, both shared by every consolidation mode:
//!
//! - [`select_batch`] picks the bounded slice of the memory store that a run is
//!   allowed to reason about, so prompts stay inside a 3B model's context.
//! - [`apply_actions`] is the single home for turning accepted
//!   [`ConsolidationAction`]s into repository writes. Both the single-pass mode
//!   ([`run_consolidation`], below) and the three-stage adversarial mode in
//!   `pond-server` funnel through it, so the correction-safety guards cannot
//!   drift between the two paths.
//!
//! Safety guards enforced in [`apply_actions`]:
//! - Correction memories are never pruned.
//! - Merges involving correction sources preserve the `corrects` metadata and
//!   force the merged segment to `Correction`.
//! - Splits propagate `corrects` onto the first `Correction`-segment entry.
//! - A merge or split that fails to insert its replacement aborts before the
//!   sources are marked superseded, so a write error can never orphan memories.
//! - Every lifecycle change is logged as a `MemoryEvent` for auditability.

use crate::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::ports::memory_consolidator::{ConsolidationAction, MemoryConsolidator};
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::services::consolidation_schedule::MIN_MEMORIES_TO_CONSOLIDATE;
use anyhow::Result;
use std::collections::HashMap;

/// Mode label written to the `consolidation_runs` audit table for a single-pass
/// run. Mirrors the `memory_consolidation_mode` setting value.
pub const MODE_SINGLE: &str = "single";
/// Mode label for a three-stage Proposer/Adversary/Judge run.
pub const MODE_ADVERSARIAL: &str = "adversarial";

/// Which consolidation pipeline a run should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationMode {
    /// One LLM call. The default.
    Single,
    /// Three sequential LLM calls: Proposer -> Adversary -> Judge.
    Adversarial,
}

impl ConsolidationMode {
    /// Stable label for logs and the `consolidation_runs.mode` column.
    pub fn as_str(self) -> &'static str {
        match self {
            ConsolidationMode::Single => MODE_SINGLE,
            ConsolidationMode::Adversarial => MODE_ADVERSARIAL,
        }
    }

    /// True when this mode costs three LLM calls instead of one.
    pub fn is_adversarial(self) -> bool {
        matches!(self, ConsolidationMode::Adversarial)
    }
}

/// Parse the `memory_consolidation_mode` setting.
///
/// Anything unrecognised resolves to [`ConsolidationMode::Single`], not
/// adversarial. A typo or a value from a future version must not silently opt
/// the user into triple the inference cost on an 8GB device — the cheap path is
/// the safe default, and it is what `Settings::default()` asks for anyway.
/// Matching is case-insensitive and tolerates surrounding whitespace.
pub fn mode_from_setting(setting: &str) -> ConsolidationMode {
    if setting.trim().eq_ignore_ascii_case(MODE_ADVERSARIAL) {
        ConsolidationMode::Adversarial
    } else {
        ConsolidationMode::Single
    }
}

/// The bounded slice of the memory store a single consolidation run may see,
/// plus how much was left behind.
#[derive(Debug, Clone)]
pub struct BatchSelection {
    /// Memories this run may reason about, oldest first.
    pub batch: Vec<MemoryFragment>,
    /// How many scoreable, segmented memories were available in total.
    pub considered: usize,
    /// How many were left for a future run because of the batch cap. Callers
    /// should log this rather than dropping it silently.
    pub deferred: usize,
}

impl BatchSelection {
    /// True when there are too few memories for consolidation to be worthwhile.
    pub fn below_minimum(&self) -> bool {
        self.batch.len() < MIN_MEMORIES_TO_CONSOLIDATE
    }
}

/// Choose which memories a consolidation run may reason about.
///
/// Only segmented memories are eligible — an unsegmented row has not been
/// through extraction's categoriser yet and there is nothing to reason about.
///
/// Ordering is **oldest first** by `created_at`. Two reasons:
///
/// 1. Duplicates cluster in time. Extraction re-derives the same fact across
///    nearby turns, so a *contiguous* window is far more likely to contain both
///    halves of a duplicate pair than a score-ranked selection, which would
///    scatter the pair across different runs and never let the model see them
///    together.
/// 2. The oldest facts are the stalest, so they are the most likely to have
///    already been superseded or to have decayed into noise.
///
/// Known limitation: the window does not rotate. With a store larger than
/// `batch_size` and a model that proposes nothing, the same oldest slice is
/// re-examined every run and newer memories are never reached. Advancing a
/// persisted cursor would need a schema column; until then the deferred count
/// is logged so the shortfall is visible rather than silent.
pub fn select_batch(memories: Vec<MemoryFragment>, batch_size: usize) -> BatchSelection {
    // A zero/absurd setting must not disable consolidation outright.
    let batch_size = batch_size.max(MIN_MEMORIES_TO_CONSOLIDATE);

    let mut eligible: Vec<MemoryFragment> = memories
        .into_iter()
        .filter(|m| m.segment.is_some())
        .collect();
    eligible.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let considered = eligible.len();
    let deferred = considered.saturating_sub(batch_size);
    eligible.truncate(batch_size);

    BatchSelection {
        batch: eligible,
        considered,
        deferred,
    }
}

/// How many of each action a run actually applied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub merged: usize,
    pub pruned: usize,
    pub split: usize,
    pub recategorized: usize,
    /// Actions refused by a correction-safety guard.
    pub blocked: usize,
}

impl ApplyOutcome {
    /// Total number of actions that changed the store.
    pub fn changed(&self) -> usize {
        self.merged + self.pruned + self.split + self.recategorized
    }
}

/// Apply accepted consolidation actions to the repository.
///
/// `batch` is the set of memories the actions were proposed against; it is used
/// to look up source metadata (`corrects`, segment) for the correction guards.
/// Actions naming an id outside the batch still apply, but cannot benefit from
/// the guards — which is exactly why callers must pass the batch the model saw.
pub async fn apply_actions(
    repo: &dyn MemoryRepository,
    batch: &[MemoryFragment],
    actions: impl IntoIterator<Item = ConsolidationAction>,
) -> Result<ApplyOutcome> {
    let memory_map: HashMap<&str, &MemoryFragment> =
        batch.iter().map(|m| (m.id.as_str(), m)).collect();

    let mut outcome = ApplyOutcome::default();

    for action in actions {
        match action {
            ConsolidationAction::Merge {
                source_ids,
                merged_content,
                segment,
                importance,
            } => {
                // Guard: if any source is a correction, the merged memory
                // inherits correction status so a later pass cannot prune it.
                let any_correction = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .any(|m| m.is_correction());

                let effective_segment = if any_correction {
                    MemorySegment::Correction
                } else {
                    segment
                };

                let corrects = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .filter_map(|m| m.corrects.clone())
                    .next();

                if any_correction {
                    tracing::info!(
                        "[consolidation] merge includes correction source — forcing segment=Correction"
                    );
                }

                let new_frag = MemoryFragment::from_extraction(
                    uuid::Uuid::new_v4().to_string(),
                    None,
                    merged_content,
                    effective_segment,
                    importance,
                    corrects,
                );
                let new_id = new_frag.id.clone();
                // Propagate: if the replacement cannot be written, the sources
                // must NOT be superseded.
                repo.add(new_frag).await?;

                for src_id in &source_ids {
                    let _ = repo.mark_superseded(src_id, &new_id).await;
                    let _ = repo
                        .log_event(MemoryEventKind::Superseded, src_id, None, Some(&new_id))
                        .await;
                }
                let _ = repo
                    .log_event(MemoryEventKind::Consolidated, &new_id, None, None)
                    .await;
                outcome.merged += 1;
            }
            ConsolidationAction::Prune { id } => {
                // Guard: never prune correction memories.
                if let Some(mem) = memory_map.get(id.as_str()) {
                    if mem.is_correction() {
                        tracing::warn!("[consolidation] blocked prune of correction memory {id}");
                        outcome.blocked += 1;
                        continue;
                    }
                }

                let _ = repo.update_lifecycle(&id, MemoryLifecycle::Archived).await;
                let _ = repo
                    .log_event(MemoryEventKind::Pruned, &id, None, None)
                    .await;
                outcome.pruned += 1;
            }
            ConsolidationAction::Recategorize {
                id,
                new_segment,
                new_importance,
            } => {
                let _ = repo.update_segment(&id, new_segment, new_importance).await;
                let _ = repo
                    .log_event(
                        MemoryEventKind::Consolidated,
                        &id,
                        None,
                        Some("recategorized"),
                    )
                    .await;
                outcome.recategorized += 1;
            }
            ConsolidationAction::Split {
                source_id,
                new_memories,
            } => {
                // If the source is a correction, propagate `corrects` to the
                // first Correction-segment entry so the fix survives the split.
                let source_corrects = memory_map
                    .get(source_id.as_str())
                    .and_then(|m| m.corrects.clone());

                let mut first_new_id = String::new();
                let mut corrects_assigned = false;

                for entry in &new_memories {
                    let entry_corrects = if !corrects_assigned
                        && source_corrects.is_some()
                        && entry.segment == MemorySegment::Correction
                    {
                        corrects_assigned = true;
                        source_corrects.clone()
                    } else {
                        None
                    };

                    let new_frag = MemoryFragment::from_extraction(
                        uuid::Uuid::new_v4().to_string(),
                        None,
                        entry.content.clone(),
                        entry.segment.clone(),
                        entry.importance,
                        entry_corrects,
                    );
                    let nid = new_frag.id.clone();
                    if first_new_id.is_empty() {
                        first_new_id = nid.clone();
                    }
                    repo.add(new_frag).await?;
                    let _ = repo
                        .log_event(MemoryEventKind::Consolidated, &nid, None, None)
                        .await;
                }
                if !first_new_id.is_empty() {
                    let _ = repo.mark_superseded(&source_id, &first_new_id).await;
                    let _ = repo
                        .log_event(
                            MemoryEventKind::Superseded,
                            &source_id,
                            None,
                            Some(&first_new_id),
                        )
                        .await;
                    outcome.split += 1;
                }
            }
        }
    }

    Ok(outcome)
}

/// Run one single-pass consolidation: select a batch, ask the consolidator for
/// actions, apply them, and record an audit row.
///
/// This is the `"single"` value of `memory_consolidation_mode` — one LLM call
/// instead of the three an adversarial run costs, which matters on a 3B
/// on-device model. `pond-server` owns the three-stage variant because it needs
/// a cancellation token and an SSE event sink; both share [`apply_actions`].
///
/// `batch_size` — max memories to reason about, from
/// `memory_consolidation_batch_size`.
///
/// Returns (merged, pruned) counts.
pub async fn run_consolidation(
    consolidator: &dyn MemoryConsolidator,
    repo: &dyn MemoryRepository,
    batch_size: usize,
) -> Result<(usize, usize)> {
    let started = std::time::Instant::now();
    let selection = select_batch(
        repo.search_scoreable(&ProfileScope::Household).await?,
        batch_size,
    );

    if selection.below_minimum() {
        return Ok((0, 0));
    }
    if selection.deferred > 0 {
        tracing::info!(
            mode = MODE_SINGLE,
            considered = selection.considered,
            batch = selection.batch.len(),
            deferred = selection.deferred,
            "[memory-consolidation] batch cap applied — remaining memories deferred to the next run"
        );
    }

    let actions = consolidator.consolidate(&selection.batch).await?;
    let proposed = actions.len();
    let outcome = apply_actions(repo, &selection.batch, actions).await?;

    if outcome.changed() > 0 {
        tracing::info!(
            merged = outcome.merged,
            pruned = outcome.pruned,
            split = outcome.split,
            recategorized = outcome.recategorized,
            blocked = outcome.blocked,
            "[memory-consolidation] single-pass complete"
        );
    }

    let _ = repo
        .log_consolidation_run(
            MODE_SINGLE,
            selection.batch.len(),
            outcome.changed(),
            proposed.saturating_sub(outcome.changed()),
            started.elapsed().as_millis() as u64,
            None,
        )
        .await;

    Ok((outcome.merged + outcome.recategorized, outcome.pruned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use crate::user_data::ports::memory_consolidator::SplitEntry;
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};

    fn frag(id: &str, segment: MemorySegment, age_secs: i64) -> MemoryFragment {
        let mut f = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            format!("content of {id}"),
            segment,
            0.5,
            None,
        );
        f.created_at = Utc::now() - ChronoDuration::seconds(age_secs);
        f
    }

    fn correction(id: &str, corrects: Option<&str>) -> MemoryFragment {
        MemoryFragment::from_extraction(
            id.to_string(),
            None,
            format!("correction {id}"),
            MemorySegment::Correction,
            0.9,
            corrects.map(str::to_string),
        )
    }

    // ── mode dispatch (E2) ──────────────────────────────────────────────────

    #[test]
    fn mode_setting_selects_the_pipeline() {
        assert_eq!(mode_from_setting("single"), ConsolidationMode::Single);
        assert_eq!(
            mode_from_setting("adversarial"),
            ConsolidationMode::Adversarial
        );
    }

    #[test]
    fn mode_setting_is_case_and_whitespace_tolerant() {
        assert_eq!(
            mode_from_setting("  Adversarial "),
            ConsolidationMode::Adversarial
        );
        assert_eq!(mode_from_setting("SINGLE"), ConsolidationMode::Single);
    }

    /// An unrecognised value must fall back to the CHEAP path — a typo must not
    /// silently triple inference cost on an 8GB device.
    #[test]
    fn unknown_mode_falls_back_to_single_not_adversarial() {
        for bogus in ["", "three-stage", "adversarial-v2", "yes", "true"] {
            assert_eq!(
                mode_from_setting(bogus),
                ConsolidationMode::Single,
                "{bogus:?} must resolve to the cheap path"
            );
        }
    }

    #[test]
    fn mode_labels_match_the_setting_values() {
        assert_eq!(ConsolidationMode::Single.as_str(), MODE_SINGLE);
        assert_eq!(ConsolidationMode::Adversarial.as_str(), MODE_ADVERSARIAL);
        // Round-trips through the setting string.
        for mode in [ConsolidationMode::Single, ConsolidationMode::Adversarial] {
            assert_eq!(mode_from_setting(mode.as_str()), mode);
        }
    }

    /// The shipped default must be the cheap mode.
    #[test]
    fn the_default_setting_resolves_to_single() {
        let defaults = crate::user_data::domain::settings::Settings::default();
        assert_eq!(
            mode_from_setting(&defaults.memory_consolidation_mode),
            ConsolidationMode::Single
        );
    }

    // ── select_batch (E3) ───────────────────────────────────────────────────

    #[test]
    fn select_batch_caps_and_reports_the_remainder() {
        let memories: Vec<MemoryFragment> = (0..25)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();

        let selection = select_batch(memories, 10);

        assert_eq!(selection.batch.len(), 10, "batch must respect the cap");
        assert_eq!(selection.considered, 25);
        assert_eq!(
            selection.deferred, 15,
            "the remainder must be reported, not silently dropped"
        );
    }

    #[test]
    fn select_batch_takes_the_oldest_first() {
        // m0 is newest (age 0), m24 is oldest (age 24).
        let memories: Vec<MemoryFragment> = (0..25)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();

        let selection = select_batch(memories, 6);

        let ids: Vec<&str> = selection.batch.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m24", "m23", "m22", "m21", "m20", "m19"]);
    }

    #[test]
    fn select_batch_skips_unsegmented_memories() {
        let mut memories = vec![frag("a", MemorySegment::Knowledge, 1)];
        let mut bare = frag("b", MemorySegment::Knowledge, 2);
        bare.segment = None;
        memories.push(bare);

        let selection = select_batch(memories, 50);
        assert_eq!(selection.considered, 1);
        assert_eq!(selection.batch[0].id, "a");
    }

    #[test]
    fn select_batch_reports_no_remainder_when_under_the_cap() {
        let memories: Vec<MemoryFragment> = (0..4)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();
        let selection = select_batch(memories, 50);
        assert_eq!(selection.deferred, 0);
        assert!(
            selection.below_minimum(),
            "4 memories is under the useful minimum"
        );
    }

    #[test]
    fn select_batch_floors_an_absurd_batch_size() {
        let memories: Vec<MemoryFragment> = (0..20)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();
        // A stored 0 must not mean "consolidate nothing forever".
        let selection = select_batch(memories, 0);
        assert_eq!(selection.batch.len(), MIN_MEMORIES_TO_CONSOLIDATE);
    }

    // ── apply_actions correction safety (E4) ────────────────────────────────

    #[tokio::test]
    async fn prune_of_a_correction_is_blocked() {
        let repo = MockMemoryRepository::new();
        let batch = vec![correction("c1", Some("name is not Jeremy"))];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "c1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.pruned, 0);
        assert_eq!(outcome.blocked, 1);
        assert!(
            repo.lifecycle_updates().await.is_empty(),
            "a blocked prune must not touch lifecycle"
        );
    }

    #[tokio::test]
    async fn prune_of_a_correction_by_corrects_field_is_blocked() {
        let repo = MockMemoryRepository::new();
        // Correction status can come from `corrects` alone, not just the segment.
        let mut mem = frag("p1", MemorySegment::Preference, 10);
        mem.corrects = Some("used to say coffee".to_string());
        let batch = vec![mem];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "p1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.blocked, 1);
        assert!(repo.lifecycle_updates().await.is_empty());
    }

    #[tokio::test]
    async fn ordinary_prune_archives_and_logs() {
        let repo = MockMemoryRepository::new();
        let batch = vec![frag("k1", MemorySegment::Knowledge, 10)];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "k1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.pruned, 1);
        assert_eq!(
            repo.lifecycle_updates().await,
            vec![("k1".to_string(), MemoryLifecycle::Archived)]
        );
        assert!(repo
            .events()
            .await
            .iter()
            .any(|(kind, id, _)| matches!(kind, MemoryEventKind::Pruned) && id == "k1"));
    }

    #[tokio::test]
    async fn merge_with_a_correction_source_forces_correction_and_keeps_corrects() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            correction("c1", Some("name is not Jeremy")),
            frag("k1", MemorySegment::Knowledge, 5),
        ];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["c1".to_string(), "k1".to_string()],
                merged_content: "name is Jerry".to_string(),
                // The model asked for Knowledge; the guard must override.
                segment: MemorySegment::Knowledge,
                importance: 0.6,
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.merged, 1);
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "name is Jerry")
            .expect("merged memory was written");
        assert_eq!(created.segment, Some(MemorySegment::Correction));
        assert_eq!(created.corrects.as_deref(), Some("name is not Jeremy"));

        // Both sources superseded by the new memory.
        let superseded = repo.superseded().await;
        assert_eq!(superseded.len(), 2);
        assert!(superseded.iter().all(|(_, by)| by == &created.id));
    }

    #[tokio::test]
    async fn merge_without_a_correction_source_keeps_the_proposed_segment() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            frag("k1", MemorySegment::Knowledge, 5),
            frag("k2", MemorySegment::Knowledge, 6),
        ];

        apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["k1".to_string(), "k2".to_string()],
                merged_content: "prefers dark roast".to_string(),
                segment: MemorySegment::Preference,
                importance: 0.7,
            }],
        )
        .await
        .unwrap();

        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "prefers dark roast")
            .unwrap();
        assert_eq!(created.segment, Some(MemorySegment::Preference));
        assert!(created.corrects.is_none());
    }

    #[tokio::test]
    async fn split_propagates_corrects_to_the_first_correction_entry() {
        let repo = MockMemoryRepository::new();
        let batch = vec![correction("c1", Some("not a vegetarian"))];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Split {
                source_id: "c1".to_string(),
                new_memories: vec![
                    SplitEntry {
                        content: "eats fish".to_string(),
                        segment: MemorySegment::Preference,
                        importance: 0.5,
                    },
                    SplitEntry {
                        content: "is not a vegetarian".to_string(),
                        segment: MemorySegment::Correction,
                        importance: 0.9,
                    },
                    SplitEntry {
                        content: "also not vegan".to_string(),
                        segment: MemorySegment::Correction,
                        importance: 0.8,
                    },
                ],
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.split, 1);
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();

        let carrier = stored
            .iter()
            .find(|m| m.content == "is not a vegetarian")
            .unwrap();
        assert_eq!(
            carrier.corrects.as_deref(),
            Some("not a vegetarian"),
            "the first Correction-segment entry inherits corrects"
        );

        // Exactly one entry carries it — not the preference, not the second correction.
        let carriers = stored.iter().filter(|m| m.corrects.is_some()).count();
        assert_eq!(carriers, 1);

        // The source is superseded by the first new memory.
        let superseded = repo.superseded().await;
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].0, "c1");
    }

    #[tokio::test]
    async fn recategorize_updates_segment_and_logs() {
        let repo = MockMemoryRepository::new();
        let batch = vec![frag("k1", MemorySegment::Knowledge, 5)];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Recategorize {
                id: "k1".to_string(),
                new_segment: MemorySegment::Identity,
                new_importance: 0.95,
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.recategorized, 1);
        assert_eq!(
            repo.segment_updates().await,
            vec![("k1".to_string(), MemorySegment::Identity, 0.95)]
        );
        assert!(repo
            .events()
            .await
            .iter()
            .any(|(_, id, data)| id == "k1" && data.as_deref() == Some("recategorized")));
    }

    // ── run_consolidation orchestration ─────────────────────────────────────

    struct FixedConsolidator {
        actions: Vec<ConsolidationAction>,
        /// How many memories the consolidator was handed.
        seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl MemoryConsolidator for FixedConsolidator {
        async fn consolidate(
            &self,
            memories: &[MemoryFragment],
        ) -> Result<Vec<ConsolidationAction>> {
            self.seen
                .store(memories.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(self.actions.clone())
        }
    }

    #[tokio::test]
    async fn run_consolidation_respects_the_batch_cap_and_audits() {
        let repo = MockMemoryRepository::new();
        for i in 0..30 {
            repo.add(frag(&format!("m{i}"), MemorySegment::Knowledge, i))
                .await
                .unwrap();
        }

        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consolidator = FixedConsolidator {
            actions: vec![ConsolidationAction::Prune {
                id: "m29".to_string(),
            }],
            seen: seen.clone(),
        };

        let (merged, pruned) = run_consolidation(&consolidator, &repo, 8).await.unwrap();

        assert_eq!(merged, 0);
        assert_eq!(pruned, 1);
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            8,
            "the consolidator must never see more than batch_size memories"
        );

        let runs = repo.consolidation_runs().await;
        assert_eq!(runs.len(), 1, "one audit row per run");
        assert_eq!(runs[0].0, MODE_SINGLE);
        assert_eq!(runs[0].1, 8, "audit records the batch size actually used");
    }

    #[tokio::test]
    async fn run_consolidation_skips_a_tiny_store() {
        let repo = MockMemoryRepository::new();
        for i in 0..3 {
            repo.add(frag(&format!("m{i}"), MemorySegment::Knowledge, i))
                .await
                .unwrap();
        }

        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consolidator = FixedConsolidator {
            actions: vec![],
            seen: seen.clone(),
        };

        let (merged, pruned) = run_consolidation(&consolidator, &repo, 50).await.unwrap();
        assert_eq!((merged, pruned), (0, 0));
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no LLM call for a store below the minimum"
        );
        assert!(repo.consolidation_runs().await.is_empty());
    }
}
