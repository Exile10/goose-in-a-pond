//! Memory decay and cleanup service.
//!
//! Computes effective scores for memories based on importance, decay rate,
//! time since last access, and access count.  Memories below threshold are
//! archived or pruned.  Runs periodically as a background task.
//!
//! Adapted from boop-agent's decay formula.

use crate::domain::memory::{MemoryFragment, MemoryLifecycle, MemoryTier};
use crate::ports::memory_repository::MemoryRepository;
use anyhow::Result;

/// Memories with effective score below this are deleted.
const PRUNE_THRESHOLD: f32 = 0.05;
/// Memories with effective score below this (but above prune) are archived.
const ARCHIVE_THRESHOLD: f32 = 0.15;

/// Compute the effective score of a memory after time-based decay.
///
/// Formula: `importance * exp(-decay_rate * days_since_access) * (1 + ln(access_count + 1) * 0.1)`
///
/// - Frequently accessed memories resist decay via the reinforcement term.
/// - The decay_rate is per-tier: short=0.1, long=0.01, permanent=0.0.
/// - Permanent-tier memories always return their raw importance (no decay).
pub fn effective_score(fragment: &MemoryFragment) -> f32 {
    let importance = fragment.importance.unwrap_or(0.5);
    let decay_rate = fragment.decay_rate.unwrap_or(0.01);

    // Permanent tier: no decay
    if fragment.tier.as_ref() == Some(&MemoryTier::Permanent) {
        return importance;
    }

    let days_since_access = fragment
        .last_accessed_at
        .map(|at| {
            let diff = chrono::Utc::now() - at;
            (diff.num_seconds() as f64 / 86400.0).max(0.0) as f32
        })
        .unwrap_or_else(|| {
            // Never accessed: use time since creation
            let diff = chrono::Utc::now() - fragment.created_at;
            (diff.num_seconds() as f64 / 86400.0).max(0.0) as f32
        });

    let decayed = importance * (-decay_rate * days_since_access).exp();
    let reinforcement = 1.0 + (fragment.access_count as f32 + 1.0).ln() * 0.1;
    (decayed * reinforcement).clamp(0.0, 1.0)
}

/// Run one cleanup pass: compute scores, archive/prune low-value memories.
///
/// Returns (scanned, archived, pruned) counts.
pub async fn run_cleanup(repo: &dyn MemoryRepository) -> Result<(usize, usize, usize)> {
    let memories = repo.search_scoreable(None).await?;
    let mut updates: Vec<(String, MemoryLifecycle)> = Vec::new();
    let mut pruned = 0;
    let mut archived = 0;

    for mem in &memories {
        // Skip memories without decay fields (legacy, pre-segment)
        if mem.importance.is_none() {
            continue;
        }

        // Permanent tier is exempt
        if mem.tier.as_ref() == Some(&MemoryTier::Permanent) {
            continue;
        }

        let score = effective_score(mem);

        if score < PRUNE_THRESHOLD {
            updates.push((mem.id.clone(), MemoryLifecycle::Archived)); // soft delete
            pruned += 1;
        } else if score < ARCHIVE_THRESHOLD {
            updates.push((mem.id.clone(), MemoryLifecycle::Archived));
            archived += 1;
        }
    }

    if !updates.is_empty() {
        repo.batch_update_lifecycle(&updates).await?;
    }

    let scanned = memories.len();
    if pruned > 0 || archived > 0 {
        tracing::info!(
            "[memory-cleanup] scanned={scanned}, archived={archived}, pruned={pruned}"
        );
    }

    Ok((scanned, archived, pruned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::memory::{MemorySegment, MemoryTier};

    fn make_memory(importance: f32, decay_rate: f32, days_old: f64, access_count: u32, tier: MemoryTier) -> MemoryFragment {
        let created = chrono::Utc::now() - chrono::Duration::seconds((days_old * 86400.0) as i64);
        MemoryFragment {
            id: "test".to_string(),
            profile_id: None,
            session_id: None,
            content: "test memory".to_string(),
            embedding: None,
            source: "test".to_string(),
            tags: vec![],
            created_at: created,
            segment: Some(MemorySegment::Knowledge),
            importance: Some(importance),
            tier: Some(tier),
            decay_rate: Some(decay_rate),
            access_count,
            last_accessed_at: None, // uses created_at fallback
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
        }
    }

    #[test]
    fn permanent_never_decays() {
        let mem = make_memory(0.8, 0.0, 365.0, 0, MemoryTier::Permanent);
        let score = effective_score(&mem);
        assert!((score - 0.8).abs() < 0.01, "permanent score should be raw importance, got {score}");
    }

    #[test]
    fn short_tier_decays_fast() {
        let mem = make_memory(0.5, 0.1, 30.0, 0, MemoryTier::Short);
        let score = effective_score(&mem);
        // 0.5 * exp(-0.1 * 30) ≈ 0.5 * 0.05 ≈ 0.025
        assert!(score < PRUNE_THRESHOLD, "short-tier 30-day-old memory should be below prune threshold, got {score}");
    }

    #[test]
    fn long_tier_retains_well() {
        let mem = make_memory(0.7, 0.01, 30.0, 0, MemoryTier::Long);
        let score = effective_score(&mem);
        // 0.7 * exp(-0.01 * 30) ≈ 0.7 * 0.74 ≈ 0.52
        assert!(score > 0.4, "long-tier 30-day memory should still be strong, got {score}");
    }

    #[test]
    fn access_count_reinforces() {
        let mem_no_access = make_memory(0.5, 0.05, 20.0, 0, MemoryTier::Long);
        let mem_accessed = make_memory(0.5, 0.05, 20.0, 10, MemoryTier::Long);
        let score_no = effective_score(&mem_no_access);
        let score_yes = effective_score(&mem_accessed);
        assert!(score_yes > score_no, "accessed memory should score higher: {score_yes} vs {score_no}");
    }

    #[test]
    fn fresh_memory_high_score() {
        let mem = make_memory(0.8, 0.01, 0.0, 0, MemoryTier::Long);
        let score = effective_score(&mem);
        assert!(score > 0.7, "fresh memory should have high score, got {score}");
    }
}
