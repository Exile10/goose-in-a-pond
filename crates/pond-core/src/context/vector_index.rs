//! The personal-context vector index — domain types and port (phase A).
//!
//! One semantic retrieval surface over the three things this pond knows about a
//! household: extracted **memories**, ingested **context items**, and
//! conversation **summaries**. They keep their own stores and their own
//! lifecycles; only the *index* is shared.
//!
//! # One index, separate stores
//!
//! An index is rebuildable; a merged store cannot be unmerged. The three corpora
//! genuinely differ — context items are redacted at construction and mirror an
//! external system by `external_id`, memories are a workspace with consolidation
//! and decay, summaries are overwritten in place — so merging them would destroy
//! guarantees that took real work to establish. Sharing retrieval costs nothing.
//!
//! # What this port deliberately does not do
//!
//! It stores no text. See the migration for why: under WAL a source row and its
//! vector cannot be deleted atomically, so orphans are inevitable, and an orphan
//! that holds a snippet is deleted data that survived a deletion promise.
//! [`VectorHit`] therefore returns identity and score, never content — the
//! caller reads the live row from its own store, which is also what keeps scope
//! and sensitivity filtering honest.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::profile::ProfileScope;

/// Which corpus a vector belongs to.
///
/// Carried on every row and every hit so retrieval can label provenance — "you
/// told me" versus "your calendar says" are different claims and a member is
/// entitled to know which one they are being given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Corpus {
    /// `memory_fragments` — things extracted from conversation.
    Memory,
    /// `context_items` — things ingested from a source (sensor, camera, later
    /// connectors). Redacted before storage.
    Context,
    /// `sessions.rolling_summary` — a model's compression of a conversation.
    Summary,
}

impl Corpus {
    pub fn as_str(self) -> &'static str {
        match self {
            Corpus::Memory => "memory",
            Corpus::Context => "context",
            Corpus::Summary => "summary",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "memory" => Some(Corpus::Memory),
            "context" => Some(Corpus::Context),
            "summary" => Some(Corpus::Summary),
            _ => None,
        }
    }

    /// Every corpus, so a sweep cannot silently forget one.
    pub const ALL: [Corpus; 3] = [Corpus::Memory, Corpus::Context, Corpus::Summary];
}

/// One vector on its way into the index.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorEntry {
    pub corpus: Corpus,
    /// The source row's primary key in its own database.
    pub row_id: String,
    /// The embedder that produced `vector`. Never inferred at read time.
    pub model_id: String,
    pub vector: Vec<f32>,
    /// Freshness marker copied from the source row, when it has one. A summary
    /// is overwritten in place, so this is what tells a sweep the stored vector
    /// now describes an older conversation.
    pub source_rev: Option<String>,
}

/// One retrieval result: identity and score, never content.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub corpus: Corpus,
    pub row_id: String,
    /// Cosine similarity against the query, in `[-1, 1]`.
    pub score: f32,
}

/// How much of the index is usable for a given model.
///
/// Exists so "the index is silently incomplete" is a number somebody can read
/// rather than a thing that has to be inferred from bad answers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexHealth {
    /// Vectors produced by the model currently configured.
    pub matching: u64,
    /// Vectors produced by some other model — present, and unusable, until they
    /// are re-embedded.
    pub mismatched: u64,
    /// Rows in the source stores with no vector at all.
    pub missing: u64,
}

/// Driven port: the shared vector index.
///
/// Writes go to this file; reads `JOIN` against the authoritative databases, so
/// scope and sensitivity are filtered against **live** rows rather than against
/// a denormalised copy that can go stale.
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// Insert or replace the vector for `(corpus, row_id)`.
    ///
    /// Upsert rather than append, because a rolling summary is rewritten in
    /// place: appending would leave a vector describing a conversation that no
    /// longer exists, scoring against a query as if it were current.
    async fn upsert(&self, entry: &VectorEntry) -> Result<()>;

    /// Drop the vector for a source row that has been deleted.
    ///
    /// Best-effort by nature — this cannot be atomic with the source delete, so
    /// [`Self::prune_orphans`] is the reconciliation that makes it eventually
    /// true.
    async fn remove(&self, corpus: Corpus, row_id: &str) -> Result<()>;

    /// Read one vector back. Roundtrip verification, and the cheap way to assert
    /// a write landed without a full search.
    async fn get(&self, corpus: Corpus, row_id: &str) -> Result<Option<VectorEntry>>;

    /// Top-`limit` hits for `query`, restricted to `model_id` and to what
    /// `scope` may see.
    ///
    /// **Vectors from another model are excluded, not scored.** Cosine over two
    /// different embedding spaces returns a plausible number that means nothing,
    /// and a plausible wrong answer is worse than a missing one. Scope is
    /// applied in the SQL — never by filtering the results afterwards, which
    /// would let invisible rows consume the top-K slots and quietly degrade
    /// retrieval for the members who share a pond.
    async fn search(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<VectorHit>>;

    /// Source rows that have no vector for `model_id`, or whose vector is stale.
    ///
    /// A `LEFT JOIN` rather than a durable queue, deliberately: it is crash-safe,
    /// restartable and self-healing, and the index file can be deleted and
    /// rebuilt from it. A queue fails the other way — a dropped entry is an item
    /// that is never searchable, with nothing left to notice.
    async fn needs_embedding(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<String>>;

    /// Delete index rows whose source row is gone. Returns how many.
    async fn prune_orphans(&self) -> Result<u64>;

    /// Counts for a health surface. See [`IndexHealth`].
    async fn health(&self, model_id: &str) -> Result<IndexHealth>;
}
