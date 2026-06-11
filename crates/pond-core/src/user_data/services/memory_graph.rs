//! Causal graph traversal for memory retrieval.
//!
//! Given a set of memory fragments and edges (a DAG), this module provides
//! a pure, synchronous function that performs BFS from a set of root IDs
//! and returns the most causally relevant memories scored by proximity.
//!
//! No I/O, no async — fully testable in isolation.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::user_data::domain::memory::{MemoryEdge, MemoryFragment};

/// Bonus score added per hop *closer* to the root.
///
/// A node at depth 0 (root) gets `MAX_DEPTH * HOP_BONUS`, depth 1 gets
/// `(MAX_DEPTH - 1) * HOP_BONUS`, etc.
const HOP_BONUS: f32 = 0.2;

/// Maximum BFS traversal depth (inclusive).
const MAX_DEPTH: u32 = 3;

/// Retrieve the most causally relevant memories via BFS graph traversal.
///
/// # Algorithm
/// 1. Build an adjacency list from `edges` (both directions — the graph is
///    explored bidirectionally so that both causes and effects are discovered).
/// 2. BFS from each id in `recent_ids` up to [`MAX_DEPTH`] hops.
/// 3. Score each discovered node:
///    `score = base_importance + HOP_BONUS * (MAX_DEPTH - depth)`
///    where `base_importance` comes from the fragment's `importance` field
///    (defaulting to 0.5 if unset).
/// 4. Deduplicate (keep the highest score if reached via multiple paths).
/// 5. Sort descending by score and return the top `max_results`.
///
/// # Returns
/// A `Vec<MemoryFragment>` ordered by descending relevance score, at most
/// `max_results` entries. Memories not reachable from `recent_ids` within
/// `MAX_DEPTH` hops are excluded.
pub fn retrieve_relevant_memories(
    all_memories: &[MemoryFragment],
    edges: &[MemoryEdge],
    recent_ids: &[String],
    max_results: usize,
) -> Vec<MemoryFragment> {
    if recent_ids.is_empty() || all_memories.is_empty() {
        return vec![];
    }

    // Index memories by ID for O(1) lookup.
    let memory_map: HashMap<&str, &MemoryFragment> =
        all_memories.iter().map(|m| (m.id.as_str(), m)).collect();

    // Build bidirectional adjacency list.
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        adjacency
            .entry(edge.from_id.as_str())
            .or_default()
            .push(edge.to_id.as_str());
        adjacency
            .entry(edge.to_id.as_str())
            .or_default()
            .push(edge.from_id.as_str());
    }

    // BFS: (memory_id, depth)
    let mut visited: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<(&str, u32)> = VecDeque::new();
    // Map from memory_id -> best (lowest) depth discovered.
    let mut best_depth: HashMap<&str, u32> = HashMap::new();

    // Seed the BFS with root IDs that actually exist in the memory set.
    for id in recent_ids {
        if memory_map.contains_key(id.as_str()) {
            queue.push_back((id.as_str(), 0));
            visited.insert(id.as_str());
            best_depth.insert(id.as_str(), 0);
        }
    }

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH {
            continue;
        }

        if let Some(neighbors) = adjacency.get(current_id) {
            for &neighbor_id in neighbors {
                // Only visit nodes that exist in our memory set.
                if !memory_map.contains_key(neighbor_id) {
                    continue;
                }

                let next_depth = depth + 1;

                // Record the best (shallowest) depth for scoring.
                let entry = best_depth.entry(neighbor_id).or_insert(next_depth);
                if next_depth < *entry {
                    *entry = next_depth;
                }

                // Only enqueue if not previously visited (cycle protection).
                if visited.insert(neighbor_id) {
                    queue.push_back((neighbor_id, next_depth));
                }
            }
        }
    }

    // Score and collect results.
    let mut scored: Vec<(&MemoryFragment, f32)> = best_depth
        .iter()
        .filter_map(|(&id, &depth)| {
            let frag = memory_map.get(id)?;
            let base = frag.importance.unwrap_or(0.5);
            let proximity_bonus = HOP_BONUS * (MAX_DEPTH.saturating_sub(depth) as f32);
            Some((*frag, base + proximity_bonus))
        })
        .collect();

    // Sort descending by score, then by ID for determinism on ties.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.id.cmp(&b.0.id))
    });

    scored
        .into_iter()
        .take(max_results)
        .map(|(frag, _)| frag.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::memory::{EdgeRelation, MemoryFragment};

    /// Helper: create a minimal MemoryFragment with given id and importance.
    fn mem(id: &str, importance: f32) -> MemoryFragment {
        let mut frag =
            MemoryFragment::from_chat(id.to_string(), None, None, format!("content of {id}"));
        frag.importance = Some(importance);
        frag
    }

    /// Helper: create a directed edge.
    fn edge(from: &str, to: &str, relation: EdgeRelation) -> MemoryEdge {
        MemoryEdge {
            from_id: from.to_string(),
            to_id: to.to_string(),
            relation,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn single_root_finds_connected_memories() {
        //  A --Caused--> B --Caused--> C
        let memories = vec![mem("A", 0.8), mem("B", 0.6), mem("C", 0.4)];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("B", "C", EdgeRelation::Caused),
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 10);
        assert_eq!(result.len(), 3);
        // All three should be present
        let ids: Vec<&str> = result.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"A"));
        assert!(ids.contains(&"B"));
        assert!(ids.contains(&"C"));
    }

    #[test]
    fn depth_limit_is_respected() {
        // Chain: A -> B -> C -> D -> E (depth 4 from A)
        let memories = vec![
            mem("A", 0.5),
            mem("B", 0.5),
            mem("C", 0.5),
            mem("D", 0.5),
            mem("E", 0.5),
        ];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("B", "C", EdgeRelation::Caused),
            edge("C", "D", EdgeRelation::Caused),
            edge("D", "E", EdgeRelation::Caused),
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 10);
        let ids: Vec<&str> = result.iter().map(|m| m.id.as_str()).collect();
        // A(0), B(1), C(2), D(3) should be found; E(4) should NOT (MAX_DEPTH=3)
        assert!(ids.contains(&"A"));
        assert!(ids.contains(&"B"));
        assert!(ids.contains(&"C"));
        assert!(ids.contains(&"D"));
        assert!(!ids.contains(&"E"), "E is at depth 4, beyond MAX_DEPTH=3");
    }

    #[test]
    fn closer_nodes_score_higher() {
        // A -> B -> C, all same base importance
        let memories = vec![mem("A", 0.5), mem("B", 0.5), mem("C", 0.5)];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("B", "C", EdgeRelation::Caused),
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 10);
        // A at depth 0: 0.5 + 0.2*3 = 1.1
        // B at depth 1: 0.5 + 0.2*2 = 0.9
        // C at depth 2: 0.5 + 0.2*1 = 0.7
        assert_eq!(result[0].id, "A");
        assert_eq!(result[1].id, "B");
        assert_eq!(result[2].id, "C");
    }

    #[test]
    fn disconnected_memories_are_excluded() {
        let memories = vec![mem("A", 0.8), mem("B", 0.6), mem("X", 0.9)];
        let edges = vec![edge("A", "B", EdgeRelation::Caused)];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 10);
        let ids: Vec<&str> = result.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"A"));
        assert!(ids.contains(&"B"));
        assert!(
            !ids.contains(&"X"),
            "X is disconnected and should be excluded"
        );
    }

    #[test]
    fn empty_graph_returns_empty() {
        let result = retrieve_relevant_memories(&[], &[], &["A".into()], 10);
        assert!(result.is_empty());
    }

    #[test]
    fn empty_recent_ids_returns_empty() {
        let memories = vec![mem("A", 0.8)];
        let result = retrieve_relevant_memories(&memories, &[], &[], 10);
        assert!(result.is_empty());
    }

    #[test]
    fn cycle_handling_does_not_loop() {
        // A -> B -> C -> A (cycle)
        let memories = vec![mem("A", 0.5), mem("B", 0.5), mem("C", 0.5)];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("B", "C", EdgeRelation::Caused),
            edge("C", "A", EdgeRelation::Caused), // back-edge forming cycle
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 10);
        // Should find all 3 without hanging
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn max_results_caps_output() {
        // A -> B -> C, but max_results = 2
        let memories = vec![mem("A", 0.5), mem("B", 0.5), mem("C", 0.5)];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("B", "C", EdgeRelation::Caused),
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 2);
        assert_eq!(result.len(), 2);
        // Highest scoring (closest to root) should be kept
        assert_eq!(result[0].id, "A");
        assert_eq!(result[1].id, "B");
    }

    #[test]
    fn multiple_roots_expand_search() {
        // A -> B, C -> D (two disconnected subgraphs, both rooted)
        let memories = vec![mem("A", 0.5), mem("B", 0.5), mem("C", 0.5), mem("D", 0.5)];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("C", "D", EdgeRelation::Referenced),
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into(), "C".into()], 10);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn importance_differentiates_same_depth() {
        // A -> B, A -> C where B has higher importance than C
        let memories = vec![mem("A", 0.5), mem("B", 0.9), mem("C", 0.3)];
        let edges = vec![
            edge("A", "B", EdgeRelation::Caused),
            edge("A", "C", EdgeRelation::Caused),
        ];

        let result = retrieve_relevant_memories(&memories, &edges, &["A".into()], 10);
        // A depth 0: 0.5 + 0.6 = 1.1
        // B depth 1: 0.9 + 0.4 = 1.3
        // C depth 1: 0.3 + 0.4 = 0.7
        assert_eq!(result[0].id, "B");
        assert_eq!(result[1].id, "A");
        assert_eq!(result[2].id, "C");
    }

    #[test]
    fn nonexistent_root_ids_are_ignored() {
        let memories = vec![mem("A", 0.5)];
        let result = retrieve_relevant_memories(&memories, &[], &["nonexistent".into()], 10);
        assert!(result.is_empty());
    }

    #[test]
    fn default_importance_used_when_none() {
        let mut frag =
            MemoryFragment::from_chat("A".to_string(), None, None, "no importance set".to_string());
        frag.importance = None; // explicitly None

        let result = retrieve_relevant_memories(&[frag], &[], &["A".into()], 10);
        assert_eq!(result.len(), 1);
        // Score should be 0.5 (default) + 0.2 * 3 (depth 0) = 1.1
    }
}
