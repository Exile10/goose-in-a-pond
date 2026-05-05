//! Memory fragment domain type — stores snippets of conversation or facts
//! with optional embedding vectors for semantic search.
//!
//! Memories are categorised by [`MemorySegment`] (identity, preference, etc.),
//! assigned a [`MemoryTier`] that controls decay, and tracked with importance
//! scoring and access counts for intelligent cleanup.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Memory classification ────────────────────────────────────────────────────

/// Semantic category of a memory (inspired by boop-agent segments).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySegment {
    /// Core facts about the user's identity (name, role, location).
    Identity,
    /// How the user likes things done (style, defaults, preferences).
    Preference,
    /// Corrections the user made to the assistant's knowledge.
    Correction,
    /// People the user knows — family, friends, colleagues.
    Relationship,
    /// Ongoing tasks, goals, work projects.
    Project,
    /// Factual knowledge worth remembering.
    Knowledge,
    /// Transient context (current situation, ongoing state).
    Context,
}

impl MemorySegment {
    /// Default importance for this segment (0.0–1.0).
    pub fn default_importance(&self) -> f32 {
        match self {
            Self::Correction => 0.9,
            Self::Identity => 0.8,
            Self::Preference => 0.7,
            Self::Relationship => 0.7,
            Self::Project => 0.6,
            Self::Knowledge => 0.5,
            Self::Context => 0.3,
        }
    }

    /// Default tier for this segment.
    pub fn default_tier(&self) -> MemoryTier {
        match self {
            Self::Identity => MemoryTier::Permanent,
            Self::Correction => MemoryTier::Long,
            Self::Context => MemoryTier::Short,
            _ => MemoryTier::Long,
        }
    }
}

/// Lifecycle tier controlling decay behaviour.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    /// High decay rate — expected to expire within days.
    Short,
    /// Moderate decay — retained for weeks/months.
    Long,
    /// Never decays, never pruned.
    Permanent,
}

impl MemoryTier {
    /// Default decay rate (lambda) for this tier.
    pub fn default_decay_rate(&self) -> f32 {
        match self {
            Self::Short => 0.10,
            Self::Long => 0.01,
            Self::Permanent => 0.00,
        }
    }
}

/// Lifecycle status for memory management.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLifecycle {
    /// Normal operational state — included in searches.
    Active,
    /// Below archive threshold — hidden from recall but not deleted.
    Archived,
    /// Consolidated into another memory.
    Merged,
}

// ── Memory fragment ──────────────────────────────────────────────────────────

/// A persisted memory fragment.
///
/// `embedding` is stored as a raw f32 BLOB in SQLite and is `#[serde(skip)]`
/// so it never appears in JSON responses (it's binary data, not user-facing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFragment {
    pub id: String,
    /// Profile this memory belongs to (None = global)
    pub profile_id: Option<String>,
    /// Session this memory was extracted from (None = manual/external)
    pub session_id: Option<String>,
    /// The text content of the memory
    pub content: String,
    /// Raw embedding vector (None until an EmbeddingProvider generates it)
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
    /// Source of this fragment: "chat", "note", "sensor_summary", "extraction", "mcp_tool"
    pub source: String,
    /// Optional tags for categorization
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,

    // ── Segment-aware fields (all optional for backward compat) ───────────
    /// Semantic category of this memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<MemorySegment>,
    /// Importance score (0.0–1.0). Higher = more worth retaining.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<f32>,
    /// Decay tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<MemoryTier>,
    /// Decay rate (lambda). Defaults from tier if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay_rate: Option<f32>,
    /// Number of times this memory has been accessed (recalled or injected).
    #[serde(default)]
    pub access_count: u32,
    /// When the memory was last accessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<DateTime<Utc>>,
    /// Lifecycle status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<MemoryLifecycle>,
    /// ID of the memory that superseded this one (via consolidation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

impl MemoryFragment {
    /// Create a fragment sourced from a chat exchange.
    pub fn from_chat(
        id: String,
        profile_id: Option<String>,
        session_id: Option<String>,
        content: String,
    ) -> Self {
        Self {
            id,
            profile_id,
            session_id,
            content,
            embedding: None,
            source: "chat".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            segment: None,
            importance: None,
            tier: None,
            decay_rate: None,
            access_count: 0,
            last_accessed_at: None,
            lifecycle: None,
            superseded_by: None,
        }
    }

    /// Create a fragment from background memory extraction.
    pub fn from_extraction(
        id: String,
        session_id: Option<String>,
        content: String,
        segment: MemorySegment,
        importance: f32,
    ) -> Self {
        let tier = segment.default_tier();
        let decay_rate = tier.default_decay_rate();
        Self {
            id,
            profile_id: None,
            session_id,
            content,
            embedding: None,
            source: "extraction".to_string(),
            tags: vec![],
            created_at: Utc::now(),
            segment: Some(segment),
            importance: Some(importance),
            tier: Some(tier),
            decay_rate: Some(decay_rate),
            access_count: 0,
            last_accessed_at: None,
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
        }
    }
}

// ── Memory graph (causal DAG) ───────────────────────────────────────────────

/// The kind of causal or structural relationship between two memories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    /// This memory *led to* the creation of the target memory.
    Caused,
    /// This memory was *injected into context* when the target was created.
    Referenced,
    /// This memory *replaces* the target (e.g. consolidation, correction).
    Superseded,
}

/// A directed edge between two [`MemoryFragment`]s in the causal graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEdge {
    /// Source memory ID (the *from* end of the directed edge).
    pub from_id: String,
    /// Target memory ID (the *to* end of the directed edge).
    pub to_id: String,
    /// Semantic type of the relationship.
    pub relation: EdgeRelation,
    /// ISO-8601 timestamp when this edge was created.
    pub created_at: String,
}

/// A subgraph of the memory DAG — a set of nodes and the edges that connect them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryGraph {
    /// The memory fragments in this subgraph.
    pub nodes: Vec<MemoryFragment>,
    /// The edges connecting nodes in this subgraph.
    pub edges: Vec<MemoryEdge>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_serde_round_trip() {
        let seg = MemorySegment::Correction;
        let json = serde_json::to_string(&seg).unwrap();
        assert_eq!(json, "\"correction\"");
        let back: MemorySegment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemorySegment::Correction);
    }

    #[test]
    fn tier_serde_round_trip() {
        let tier = MemoryTier::Permanent;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"permanent\"");
        let back: MemoryTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemoryTier::Permanent);
    }

    #[test]
    fn lifecycle_serde_round_trip() {
        let lc = MemoryLifecycle::Archived;
        let json = serde_json::to_string(&lc).unwrap();
        assert_eq!(json, "\"archived\"");
        let back: MemoryLifecycle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MemoryLifecycle::Archived);
    }

    #[test]
    fn fragment_backward_compat_deser() {
        // Old fragments without new fields should deserialize fine.
        let json = r#"{
            "id": "old-1",
            "profile_id": null,
            "session_id": null,
            "content": "User likes coffee",
            "source": "chat",
            "tags": [],
            "created_at": "2024-01-01T00:00:00Z"
        }"#;
        let frag: MemoryFragment = serde_json::from_str(json).unwrap();
        assert_eq!(frag.id, "old-1");
        assert!(frag.segment.is_none());
        assert!(frag.importance.is_none());
        assert_eq!(frag.access_count, 0);
        assert!(frag.lifecycle.is_none());
    }

    #[test]
    fn from_extraction_sets_defaults() {
        let frag = MemoryFragment::from_extraction(
            "ext-1".to_string(),
            None,
            "User's name is Jerry".to_string(),
            MemorySegment::Identity,
            0.85,
        );
        assert_eq!(frag.segment, Some(MemorySegment::Identity));
        assert_eq!(frag.importance, Some(0.85));
        assert_eq!(frag.tier, Some(MemoryTier::Permanent));
        assert_eq!(frag.decay_rate, Some(0.0));
        assert_eq!(frag.lifecycle, Some(MemoryLifecycle::Active));
        assert_eq!(frag.source, "extraction");
    }

    #[test]
    fn segment_defaults() {
        assert_eq!(MemorySegment::Correction.default_importance(), 0.9);
        assert_eq!(MemorySegment::Context.default_importance(), 0.3);
        assert_eq!(
            MemorySegment::Identity.default_tier(),
            MemoryTier::Permanent
        );
        assert_eq!(MemorySegment::Context.default_tier(), MemoryTier::Short);
    }

    #[test]
    fn edge_relation_serde_round_trip() {
        let rel = EdgeRelation::Caused;
        let json = serde_json::to_string(&rel).unwrap();
        assert_eq!(json, "\"caused\"");
        let back: EdgeRelation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EdgeRelation::Caused);
    }

    #[test]
    fn memory_edge_serde_round_trip() {
        let edge = MemoryEdge {
            from_id: "a".to_string(),
            to_id: "b".to_string(),
            relation: EdgeRelation::Referenced,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&edge).unwrap();
        let back: MemoryEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(back, edge);
    }

    #[test]
    fn memory_graph_contains_nodes_and_edges() {
        let graph = MemoryGraph {
            nodes: vec![MemoryFragment::from_chat(
                "n1".to_string(),
                None,
                None,
                "test".to_string(),
            )],
            edges: vec![MemoryEdge {
                from_id: "n1".to_string(),
                to_id: "n2".to_string(),
                relation: EdgeRelation::Superseded,
                created_at: "2025-06-01T00:00:00Z".to_string(),
            }],
        };
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].relation, EdgeRelation::Superseded);
    }
}
