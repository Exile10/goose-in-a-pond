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
///
/// **Declaration order is the tie-break policy.** `Ord` is derived, and equal
/// scores are broken by this order: a memory outranks a context item outranks a
/// summary. That is the design's "memory wins ties" — a summary is a model's
/// compression, not a claim, so it must never outrank the precise version of the
/// same thing. Reordering these variants silently changes what the assistant
/// prefers to tell a member, which is why a test pins it.
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

/// A hit with its text resolved from the live source row.
///
/// The index stores no text; this is read back through the same `JOIN` that
/// enforces scope and liveness, so what a caller sees is always the CURRENT row
/// — never a denormalised copy that outlived a deletion or an archive.
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
/// The three totals on [`IndexHealth`] cannot show a corpus that is structurally
/// at zero, and that is not hypothetical. On a live pond the global figure read
/// about 2% missing while the summary corpus could never populate at all — its
/// liveness predicate requires an attributed session and every session's
/// `profile_id` was NULL — because two healthy corpora averaged the dead one
/// away. An average over corpora is the wrong shape for a question that is
/// really "is each of these three working"; a row per corpus is the right one.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusHealth {
    pub corpus: Corpus,
    /// Source rows that **qualify** — that pass this corpus's liveness
    /// predicate — not the raw table count.
    ///
    /// This choice is the whole point of the type, so it is stated rather than
    /// left to be discovered: coverage here means "of the rows that COULD be
    /// indexed, how many are". Counting raw table rows instead would report
    /// every archived memory and every unattributed session as permanently
    /// missing, so a perfectly healthy pond could never reach full coverage and
    /// the number would rightly be ignored.
    ///
    /// The cost of the choice is that a corpus whose predicate excludes
    /// everything reports `rows == 0` rather than a large missing count. That
    /// is the signal wanted, not a loss of one: zero qualifying rows against a
    /// table full of sessions says "nothing here can ever be indexed", which is
    /// a different defect from "nothing here has been indexed yet" and needs a
    /// different fix.
    ///
    /// That argument only works if the reader can SEE the table is full, which
    /// is what [`source_rows`](Self::source_rows) is for. Without it this field
    /// reproduces the very blindness it was added to remove — measured on a live
    /// pond, `rows == 0` for the summary corpus while 27 sessions carried a real
    /// rolling summary, and the surface reported the index 100% covered.
    pub rows: u64,
    /// Every row in the source table, ignoring the liveness predicate.
    ///
    /// The denominator of coverage is [`rows`](Self::rows), never this. It
    /// exists for one question that `rows` alone cannot answer: **is this
    /// corpus empty, or is it excluded?** Both report zero qualifying rows, and
    /// they need opposite fixes — one waits for data, the other is a predicate
    /// bug that no amount of embedding will repair.
    ///
    /// `source_rows > rows` is normal and healthy in itself: archived and merged
    /// memories are meant to fall out. It is `rows == 0 && source_rows > 0` that
    /// says something is structurally wrong.
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

/// How much of the index is usable for a given model.
///
/// Exists so "the index is silently incomplete" is a number somebody can read
/// rather than a thing that has to be inferred from bad answers.
///
/// The three totals are the sums of [`per_corpus`](Self::per_corpus), so they
/// describe rows that qualify for indexing — an orphaned vector, or one hanging
/// off an archived memory, is counted nowhere here. That is deliberate: those
/// are the prune sweep's business, and adding them to a coverage figure makes it
/// describe the index file rather than what retrieval can reach.
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
    /// Alongside the totals rather than instead of them: the totals are what a
    /// maintenance pass logs and what a caller compares between runs, and the
    /// rows are what tells anybody WHICH corpus moved.
    pub per_corpus: Vec<CorpusHealth>,
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

    /// Like [`Self::search`], but with each hit's text read back from its live
    /// source row in the same query.
    ///
    /// One round trip rather than N: the `ATTACH` makes the text available to
    /// the same `JOIN` that already filters scope and liveness, so there is no
    /// window in which a row could be archived or deleted between being scored
    /// and being read.
    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ResolvedHit>>;

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

    /// Like [`Self::needs_embedding`], but with each row's text, so a sweep can
    /// embed without a second read through another port.
    ///
    /// Exists because **context items had no embedding path at all**: adoption
    /// only copies vectors that already exist, and nothing ever embedded an item
    /// that arrived without one. A probe against the live agent found it -- a
    /// planted sensor event was never indexed and the assistant answered "no
    /// recorded activity", which is a wrong answer rather than a missing one.
    async fn needs_embedding_with_text(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>>;

    /// Copy vectors that already exist in a source store into the index,
    /// without re-embedding anything. Returns how many were copied.
    ///
    /// **This is what makes the index genuinely rebuildable.** Memories and
    /// context items keep their own vector in their own table, and the sweeps
    /// that produce those vectors are driven by *that* column being NULL — so
    /// once a row is embedded, nothing ever calls the write-through for it
    /// again. Delete this file and those rows would be absent from the index
    /// forever, silently, while the store looks perfectly healthy. Found by
    /// deleting `pond_vectors.db` on a live pond: the summary corpus came back
    /// and the memories did not.
    ///
    /// `expected_dims` is a filter, not a hint: a stored vector of another width
    /// came from another model and must NOT be stamped with this one, which
    /// would launder a stale vector into the current space where nothing could
    /// detect it.
    ///
    /// Corpora with no vector of their own (summaries) copy nothing and are
    /// served by their own embedding sweep instead.
    async fn backfill_from_source(
        &self,
        corpus: Corpus,
        model_id: &str,
        expected_dims: usize,
    ) -> Result<u64>;

    /// Delete index rows whose source row is gone. Returns how many.
    async fn prune_orphans(&self) -> Result<u64>;

    /// Counts for a health surface, globally and per corpus. See
    /// [`IndexHealth`] and [`CorpusHealth`].
    ///
    /// Every corpus must be represented, including one with no qualifying rows:
    /// a corpus that is absent from the answer is a corpus nobody can see is
    /// broken, which is the failure the per-corpus shape exists to end.
    async fn health(&self, model_id: &str) -> Result<IndexHealth>;
}
