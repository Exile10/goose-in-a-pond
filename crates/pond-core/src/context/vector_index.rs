//! The personal-context vector index — domain types and port. One retrieval surface over three
//! corpora (memories, context items, summaries) that keep their own stores: an index is
//! rebuildable, a merged store is not. It holds no text — under WAL a source row and its vector
//! cannot be deleted atomically, and an orphan holding a snippet is data that survived deletion.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::profile::ProfileScope;

/// Which corpus a vector belongs to, so retrieval can label provenance.
///
/// Declaration order IS the tie-break policy: `Ord` is derived, so memory outranks context
/// outranks summary. Reordering these variants changes what a member is told; a test pins it.
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
    /// Which passage of the row this vector describes: 0 when embedded whole, 0..n when chunked.
    ///
    /// Part of the identity — a row's vectors are `(corpus, row_id, chunk_ix)` — so writing
    /// chunk 3 does not overwrite chunk 2.
    pub chunk_ix: i64,
    /// Byte span of the passage within the source text; `None` means the whole text.
    ///
    /// A span rather than the words: an orphan span resolves to nothing, where an orphan
    /// snippet would be deleted data that survived its deletion.
    pub chunk_span: Option<(i64, i64)>,
    /// The embedder that produced `vector`. Never inferred at read time.
    pub model_id: String,
    pub vector: Vec<f32>,
    /// Freshness marker copied from the source row, when it has one. A summary
    /// is overwritten in place, so this is what tells a sweep the stored vector
    /// now describes an older conversation.
    pub source_rev: Option<String>,
}

impl VectorEntry {
    /// A vector describing the WHOLE of a row's text.
    ///
    /// The right shape for memories and summaries, which are short. Chunked corpora build
    /// entries directly so the span is never accidentally omitted.
    pub fn whole(
        corpus: Corpus,
        row_id: String,
        model_id: String,
        vector: Vec<f32>,
        source_rev: Option<String>,
    ) -> Self {
        Self {
            corpus,
            row_id,
            chunk_ix: 0,
            chunk_span: None,
            model_id,
            vector,
            source_rev,
        }
    }
}

/// One retrieval result: identity and score, never content.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub corpus: Corpus,
    pub row_id: String,
    /// Cosine similarity against the query, in `[-1, 1]`.
    pub score: f32,
}

/// A hit with its text resolved from the live source row.
///
/// The index stores no text; this is read back through the same `JOIN` that enforces scope and
/// liveness, so a caller always sees the CURRENT row, never a copy that outlived a deletion.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHit {
    pub corpus: Corpus,
    pub row_id: String,
    pub score: f32,
    /// The live text. For a memory its content, for a context item its title and
    /// body, for a summary the rolling summary itself.
    pub text: String,
}

/// How much of ONE corpus is usable for a given model.
///
/// The totals on [`IndexHealth`] cannot show a corpus that is structurally at zero: two healthy
/// corpora average the dead one away, so a row per corpus is the only shape that answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusHealth {
    pub corpus: Corpus,
    /// Source rows that qualify — that pass this corpus's liveness predicate — not the raw
    /// table count, so coverage means "of the rows that COULD be indexed, how many are".
    /// A corpus whose predicate excludes everything reports `rows == 0`, which
    /// [`source_rows`](Self::source_rows) is there to distinguish from an empty table.
    pub rows: u64,
    /// Every row in the source table, ignoring the liveness predicate.
    ///
    /// The denominator of coverage is [`rows`](Self::rows), never this; this answers "is the
    /// corpus empty, or excluded?". `rows == 0 && source_rows > 0` is the structural failure.
    pub source_rows: u64,
    /// Of `rows`, those carrying a vector from the model asked about — the only
    /// ones retrieval can actually return.
    pub indexed_rows: u64,
    /// Of `rows`, those with no vector at all. Repaired by embedding.
    pub missing_rows: u64,
    /// Of `rows`, those whose vector came from a DIFFERENT model: present,
    /// excluded from retrieval by design, and repaired by RE-embedding. Kept
    /// apart from `missing_rows` because "absent" and "present but unusable"
    /// look identical in a coverage percentage and are not the same problem.
    pub mismatched: u64,
}

/// How much of the index is usable for a given model, so "silently incomplete" is a number
/// somebody can read rather than a thing inferred from bad answers.
///
/// The totals sum [`per_corpus`](Self::per_corpus) over qualifying rows; orphans count nowhere.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexHealth {
    /// Vectors produced by the model currently configured.
    pub matching: u64,
    /// Vectors produced by some other model — present, and unusable, until they
    /// are re-embedded.
    pub mismatched: u64,
    /// Rows in the source stores with no vector at all.
    pub missing: u64,
    /// The same counts again, one row per corpus, in [`Corpus::ALL`] order.
    ///
    /// Alongside the totals rather than instead: the totals are what a maintenance pass logs,
    /// the rows are what says WHICH corpus moved.
    pub per_corpus: Vec<CorpusHealth>,
}

/// Driven port: the shared vector index.
///
/// Writes go to this file; reads `JOIN` against the authoritative databases, so scope and
/// sensitivity are filtered against live rows rather than a denormalised copy that can go stale.
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// Insert or replace the vector for `(corpus, row_id)`.
    ///
    /// Upsert rather than append: a rolling summary is rewritten in place, so appending would
    /// leave a vector describing a conversation that no longer exists but still scores.
    async fn upsert(&self, entry: &VectorEntry) -> Result<()>;

    /// Drop the vector for a source row that has been deleted.
    ///
    /// Best-effort: this cannot be atomic with the source delete, so [`Self::prune_orphans`] is
    /// the reconciliation that makes it eventually true.
    async fn remove(&self, corpus: Corpus, row_id: &str) -> Result<()>;

    /// Read one vector back. Roundtrip verification, and the cheap way to assert
    /// a write landed without a full search.
    async fn get(&self, corpus: Corpus, row_id: &str) -> Result<Option<VectorEntry>>;

    /// Top-`limit` hits for `query`, restricted to `model_id` and to what `scope` may see.
    ///
    /// Vectors from another model are excluded, not scored: cosine across two embedding spaces
    /// is meaningless. Scope is applied in the SQL, or invisible rows take the top-K slots.
    async fn search(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<VectorHit>>;

    /// Like [`Self::search`], but with each hit's text read back from its live source row in the
    /// same query. The `ATTACH` puts the text in the same `JOIN` that filters scope and liveness,
    /// so no row can be archived or deleted between being scored and being read.
    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ResolvedHit>>;

    /// Source rows that have no vector for `model_id`, or whose vector is stale.
    ///
    /// A `LEFT JOIN` rather than a durable queue: crash-safe and restartable, and the index file
    /// can be rebuilt from it. A dropped queue entry would be an item nothing is left to notice.
    async fn needs_embedding(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<String>>;

    /// Like [`Self::needs_embedding`], but with each row's text, so a sweep can embed without a
    /// second read through another port. Context items otherwise have no embedding path at all:
    /// backfill only copies vectors that already exist, so an item that arrived without one is
    /// never indexed and the assistant answers "no recorded activity" — wrong, not missing.
    async fn needs_embedding_with_text(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>>;

    /// Copy existing source-store vectors into the index without re-embedding; returns the count.
    /// What makes it rebuildable: the sweeps minting those vectors fire only on a NULL column, so
    /// deleting this file loses already-embedded rows forever. `expected_dims` is a filter, not a
    /// hint — another width came from another model and must not be restamped as this one.
    async fn backfill_from_source(
        &self,
        corpus: Corpus,
        model_id: &str,
        expected_dims: usize,
    ) -> Result<u64>;

    /// Delete index rows whose source row is gone. Returns how many.
    async fn prune_orphans(&self) -> Result<u64>;

    /// Counts for a health surface, globally and per corpus. See [`IndexHealth`] and
    /// [`CorpusHealth`]. Every corpus must be represented, including one with no qualifying
    /// rows: a corpus absent from the answer is a corpus nobody can see is broken.
    async fn health(&self, model_id: &str) -> Result<IndexHealth>;
}
