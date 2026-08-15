//! SQLite-backed [`VectorIndex`] over `pond_vectors.db` (phase A).
//!
//! # The ATTACH, and why reads join rather than copy
//!
//! Every pooled connection here `ATTACH`es the authoritative system database as
//! `sys` (see [`SqliteVectorIndex::connect`]). That buys real `JOIN`s, so a
//! search filters scope, lifecycle and existence against **live** rows instead
//! of against denormalised copies in the index — copies that would go stale
//! exactly when it matters, which is after a member is removed or a memory is
//! archived.
//!
//! Only cross-database *transactions* lose atomicity under WAL; queries are
//! fine. So writes go to this file alone and the joins stay read-only, which is
//! also why orphans are expected and [`prune_orphans`] exists.
//!
//! # Similarity is brute force, on purpose
//!
//! No `sqlite-vec`, no HNSW. At household scale — thousands of rows — a cosine
//! over 768-dim f32 in Rust is sub-millisecond, and irrelevant beside a flat
//! ~30 tok/s decode. A compiled extension would buy nothing and add a native
//! dependency to a build already fighting llama.cpp, ONNX Runtime, candle and
//! aarch64 cross-compilation. The bottleneck is the embedder, never this.
//!
//! [`prune_orphans`]: VectorIndex::prune_orphans

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use pond_core::context::vector_index::{
    Corpus, CorpusHealth, IndexHealth, ResolvedHit, VectorEntry, VectorHit, VectorIndex,
};
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;

pub struct SqliteVectorIndex {
    pool: Pool<Sqlite>,
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// Cosine similarity, refusing incomparable widths.
///
/// Returns `None` rather than `0.0` for a mismatch. `0.0` is a legitimate score
/// — it means "orthogonal, judged" — and using it for "not comparable" is the
/// exact defect that made a mixed-dimension memory store return an arbitrary
/// page of rows ranked as if they had been judged, while suppressing the keyword
/// fallback that should have run instead.
fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na * nb))
}

/// The `sys.<table>` a corpus's rows live in, and the column holding the id.
///
/// Stated once so a corpus added to one query and not another is a compile
/// error rather than a silently empty result.
fn source_table(corpus: Corpus) -> (&'static str, &'static str) {
    match corpus {
        Corpus::Memory => ("sys.memory_fragments", "id"),
        Corpus::Context => ("sys.context_items", "id"),
        Corpus::Summary => ("sys.sessions", "id"),
    }
}

/// Which rows of a corpus are LIVE, as a SQL fragment on the joined source row.
///
/// Stated once and used by `search`, `needs_embedding`, `health` and
/// `backfill_from_source`, because a corpus that is filtered in one and not
/// another is how an archived memory stays searchable through the index while
/// every direct read of the store correctly hides it. That was a real defect
/// here: `search` had no liveness predicate at all.
///
/// The Summary rule is the sharp one. `sessions.profile_id` is NULL for a
/// session nobody was identified in -- a guest, or a legacy row -- and the idle
/// loop summarises EVERY session it lists. Unlike `memory_fragments`, where NULL
/// means "shared household context", a NULL here means "we do not know whose
/// this is", so an unattributed summary must never enter household retrieval.
/// Same column, opposite meaning; that asymmetry is the whole reason this is
/// written out per corpus rather than shared.
fn liveness_sql(corpus: Corpus) -> &'static str {
    match corpus {
        Corpus::Memory => "AND (s.lifecycle IS NULL OR s.lifecycle = 'active')",
        Corpus::Context => "",
        Corpus::Summary => {
            "AND s.rolling_summary IS NOT NULL AND s.rolling_summary != '' \
             AND s.profile_id IS NOT NULL"
        }
    }
}

/// Scope predicate against the JOINED source row, as SQL.
///
/// In the `WHERE`, never applied to the results afterwards: post-filtering lets
/// rows the caller may not see occupy top-K slots, so a guest sharing a pond
/// silently degrades everyone else's retrieval instead of simply seeing nothing.
///
/// `Summary` has no owner column today — a session's identity binding lives
/// elsewhere — so an owner-scoped summary search matches nothing rather than
/// guessing. Narrower than necessary is the safe direction, and phase C is where
/// that gets a real answer.
fn scope_sql(corpus: Corpus, scope: &ProfileScope) -> (String, Option<String>) {
    match (corpus, scope) {
        (_, ProfileScope::Guest) => ("AND 1 = 0".into(), None),
        (_, ProfileScope::Household) => (String::new(), None),
        (Corpus::Memory, ProfileScope::Owner(id)) => (
            // Mirrors `sqlite_memory::scope_sql`: unattributed rows are shared
            // household context and stay visible to their owner-scoped reads.
            "AND (s.profile_id = ? OR s.profile_id IS NULL)".into(),
            Some(id.clone()),
        ),
        (Corpus::Context, ProfileScope::Owner(id)) => {
            // `context_items.profile_id` is NOT NULL, so there is no IS NULL
            // limb to add and adding one would create a hiding place.
            ("AND s.profile_id = ?".into(), Some(id.clone()))
        }
        // `sessions.profile_id` exists (migration 0003) and is written by the
        // identification chain. NO `IS NULL` limb, deliberately, and this is the
        // opposite of the Memory arm above: an unattributed session is a guest
        // or a legacy row, not shared household context.
        (Corpus::Summary, ProfileScope::Owner(id)) => {
            ("AND s.profile_id = ?".into(), Some(id.clone()))
        }
    }
}

/// The SQL expression yielding a corpus's live text.
///
/// Read back through the JOIN rather than stored in the index: the index holds
/// no text on purpose (an orphan carrying a snippet would be deleted data that
/// survived a deletion promise), so this is where the words come from.
///
/// Context mirrors `ContextItem::embedding_text` — title and body joined — so
/// what a caller reads is what was embedded.
fn text_sql(corpus: Corpus) -> &'static str {
    match corpus {
        Corpus::Memory => "s.content",
        Corpus::Context => {
            "CASE WHEN s.title = '' THEN s.body \
                            WHEN s.body = '' THEN s.title \
                            ELSE s.title || char(10) || s.body END"
        }
        Corpus::Summary => "s.rolling_summary",
    }
}

/// The freshness column a corpus compares against, if it has one.
fn source_rev_sql(corpus: Corpus) -> Option<&'static str> {
    match corpus {
        // A summary is rewritten in place, so its vector can describe an older
        // conversation while still being present. This is what makes that
        // detectable.
        Corpus::Summary => Some("s.rolling_summary_updated_at"),
        // A memory's content is stable once extracted; consolidation supersedes
        // rather than edits, which shows up as a new row.
        Corpus::Memory => None,
        // A context item is re-synced by external_id, which rewrites the row.
        Corpus::Context => Some("s.ingested_at"),
    }
}

impl SqliteVectorIndex {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    /// Open `pond_vectors.db`, ATTACHing the system database on every pooled
    /// connection.
    ///
    /// `after_connect` rather than a one-off: an `ATTACH` is per-connection, so
    /// doing it once would work until the pool opened its second connection and
    /// then fail with "no such table: sys.memory_fragments" under exactly the
    /// concurrency that makes it hardest to reproduce.
    pub async fn connect(vectors_path: &Path, system_path: &Path) -> Result<Pool<Sqlite>> {
        let opts =
            SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", vectors_path.display()))?
                .create_if_missing(true)
                .pragma("journal_mode", "WAL")
                .pragma("synchronous", "NORMAL")
                .pragma("cache_size", "2000")
                .pragma("mmap_size", "33554432");

        let system = system_path.display().to_string();
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .after_connect(move |conn, _meta| {
                let system = system.clone();
                Box::pin(async move {
                    conn.execute(format!("ATTACH DATABASE '{system}' AS sys").as_str())
                        .await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await
            .with_context(|| format!("opening {}", vectors_path.display()))?;
        Ok(pool)
    }
}

#[async_trait]
impl VectorIndex for SqliteVectorIndex {
    async fn upsert(&self, entry: &VectorEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO vectors (corpus, row_id, model_id, dims, vector, source_rev, embedded_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(corpus, row_id) DO UPDATE SET \
               model_id = excluded.model_id, dims = excluded.dims, vector = excluded.vector, \
               source_rev = excluded.source_rev, embedded_at = excluded.embedded_at",
        )
        .bind(entry.corpus.as_str())
        .bind(&entry.row_id)
        .bind(&entry.model_id)
        .bind(entry.vector.len() as i64)
        .bind(vec_to_blob(&entry.vector))
        .bind(&entry.source_rev)
        .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn remove(&self, corpus: Corpus, row_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM vectors WHERE corpus = ? AND row_id = ?")
            .bind(corpus.as_str())
            .bind(row_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get(&self, corpus: Corpus, row_id: &str) -> Result<Option<VectorEntry>> {
        let row: Option<(String, Vec<u8>, Option<String>)> = sqlx::query_as(
            "SELECT model_id, vector, source_rev FROM vectors WHERE corpus = ? AND row_id = ?",
        )
        .bind(corpus.as_str())
        .bind(row_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(model_id, blob, source_rev)| VectorEntry {
            corpus,
            row_id: row_id.to_string(),
            model_id,
            vector: blob_to_vec(&blob),
            source_rev,
        }))
    }

    async fn search(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        if query.is_empty() || limit == 0 || scope.excludes_everything() {
            return Ok(vec![]);
        }

        let mut scored: Vec<VectorHit> = Vec::new();
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let (scope_pred, bind) = scope_sql(corpus, scope);
            let live = liveness_sql(corpus);
            // The JOIN is the existence check: an orphan whose source row is
            // gone simply does not match, which is what makes a vector with no
            // text harmless rather than a leak.
            let sql = format!(
                "SELECT v.row_id, v.vector FROM vectors v \
                 JOIN {table} s ON s.{id_col} = v.row_id \
                 WHERE v.corpus = ? AND v.model_id = ? {scope_pred} {live}"
            );
            let mut q = sqlx::query_as::<_, (String, Vec<u8>)>(&sql)
                .bind(corpus.as_str())
                .bind(model_id);
            if let Some(b) = bind {
                q = q.bind(b);
            }
            let rows = q.fetch_all(&self.pool).await?;
            for (row_id, blob) in rows {
                // `cosine` answers None for an incomparable width. Skip rather
                // than score: a stored vector of the wrong width is not a bad
                // match, it is not a match at all.
                if let Some(score) = cosine(query, &blob_to_vec(&blob)) {
                    scored.push(VectorHit {
                        corpus,
                        row_id,
                        score,
                    });
                }
            }
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Stable tie-break, so equal scores do not reorder between runs
                // and a test can assert on the result.
                .then_with(|| a.row_id.cmp(&b.row_id))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ResolvedHit>> {
        if query.is_empty() || limit == 0 || scope.excludes_everything() {
            return Ok(vec![]);
        }
        let mut scored: Vec<ResolvedHit> = Vec::new();
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let (scope_pred, bind) = scope_sql(corpus, scope);
            let live = liveness_sql(corpus);
            let text = text_sql(corpus);
            let sql = format!(
                "SELECT v.row_id, v.vector, {text} FROM vectors v \
                 JOIN {table} s ON s.{id_col} = v.row_id \
                 WHERE v.corpus = ? AND v.model_id = ? {scope_pred} {live}"
            );
            let mut q = sqlx::query_as::<_, (String, Vec<u8>, String)>(&sql)
                .bind(corpus.as_str())
                .bind(model_id);
            if let Some(b) = bind {
                q = q.bind(b);
            }
            for (row_id, blob, text) in q.fetch_all(&self.pool).await? {
                if let Some(score) = cosine(query, &blob_to_vec(&blob)) {
                    scored.push(ResolvedHit {
                        corpus,
                        row_id,
                        score,
                        text,
                    });
                }
            }
        }
        // Score first; then the corpus order, which IS the "memory wins ties"
        // policy (see `Corpus`); then the id, so equal rows never reorder
        // between runs and a test can assert on the result.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.corpus.cmp(&b.corpus))
                .then_with(|| a.row_id.cmp(&b.row_id))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    async fn needs_embedding(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        let (table, id_col) = source_table(corpus);
        let rev = source_rev_sql(corpus);

        // LEFT JOIN from the SOURCE, so a row that has never been embedded and a
        // row whose vector is stale come back from one query. `v.row_id IS NULL`
        // is "never embedded"; the model comparison catches a provider switch;
        // the rev comparison catches a source row rewritten in place.
        // `v.source_rev IS NOT NULL` is load-bearing: without it, a row whose
        // stored rev is NULL compares unequal to a non-NULL column and is
        // reported stale on EVERY sweep, so the sweep re-embeds the same rows
        // forever and never converges. A NULL rev means "this corpus does not
        // track revisions", not "out of date".
        let staleness = match rev {
            Some(col) => format!("OR (v.source_rev IS NOT NULL AND v.source_rev IS NOT {col})"),
            None => String::new(),
        };
        // Summaries only exist where the column is populated, and an empty
        // summary is not a document.
        let extra = liveness_sql(corpus);
        let sql = format!(
            "SELECT s.{id_col} FROM {table} s \
             LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
             WHERE (v.row_id IS NULL OR v.model_id != ? {staleness}) {extra} \
             LIMIT ?"
        );
        let rows: Vec<(String,)> = sqlx::query_as(&sql)
            .bind(corpus.as_str())
            .bind(model_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn needs_embedding_with_text(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let (table, id_col) = source_table(corpus);
        let rev = source_rev_sql(corpus);
        let staleness = match rev {
            Some(col) => format!("OR (v.source_rev IS NOT NULL AND v.source_rev IS NOT {col})"),
            None => String::new(),
        };
        let extra = liveness_sql(corpus);
        let text = text_sql(corpus);
        let sql = format!(
            "SELECT s.{id_col}, {text} FROM {table} s \
             LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
             WHERE (v.row_id IS NULL OR v.model_id != ? {staleness}) {extra} \
             LIMIT ?"
        );
        let rows: Vec<(String, String)> = sqlx::query_as(&sql)
            .bind(corpus.as_str())
            .bind(model_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn backfill_from_source(
        &self,
        corpus: Corpus,
        model_id: &str,
        expected_dims: usize,
    ) -> Result<u64> {
        // Pure SQL across the ATTACH: no round trips, no inference, no decoding
        // of a single blob. The vectors already exist -- this only teaches the
        // index about them.
        let (table, id_col) = source_table(corpus);
        // Nothing to copy for summaries: a session has no vector of its own.
        // The summary embedding sweep owns that corpus.
        if corpus == Corpus::Summary {
            return Ok(0);
        }
        let extra = liveness_sql(corpus);
        // `length(embedding) = dims * 4` is the width filter: a vector from a
        // different model must not be restamped with this one.
        let expected_bytes = (expected_dims * std::mem::size_of::<f32>()) as i64;
        let rev = match source_rev_sql(corpus) {
            Some(col) => col,
            None => "NULL",
        };
        let sql = format!(
            "INSERT INTO vectors (corpus, row_id, model_id, dims, vector, source_rev, embedded_at) \
             SELECT ?, s.{id_col}, ?, ?, s.embedding, {rev}, ? \
             FROM {table} s \
             LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
             WHERE s.embedding IS NOT NULL AND length(s.embedding) = ? \
             AND v.row_id IS NULL {extra}"
        );
        let copied = sqlx::query(&sql)
            .bind(corpus.as_str())
            .bind(model_id)
            .bind(expected_dims as i64)
            .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
            .bind(corpus.as_str())
            .bind(expected_bytes)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if copied > 0 {
            tracing::info!(
                corpus = corpus.as_str(),
                copied,
                "adopted existing vectors into the index"
            );
        }
        Ok(copied)
    }

    async fn prune_orphans(&self) -> Result<u64> {
        let mut removed = 0u64;
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let sql = format!(
                "DELETE FROM vectors WHERE corpus = ? AND row_id NOT IN \
                 (SELECT s.{id_col} FROM {table} s)"
            );
            let result = sqlx::query(&sql)
                .bind(corpus.as_str())
                .execute(&self.pool)
                .await?;
            removed += result.rows_affected();
        }
        if removed > 0 {
            tracing::info!(removed, "pruned vector index orphans");
        }
        Ok(removed)
    }

    /// Counted per corpus, FROM the source side, with the totals derived as the
    /// sums.
    ///
    /// The previous version asked the index file two whole-table questions
    /// ("how many vectors carry this model", "how many carry another") and then
    /// summed `missing` across corpora into a third. Three global numbers cannot
    /// express a corpus that is at zero: on a live pond that reported roughly 2%
    /// missing while the summary corpus was structurally empty and could never
    /// populate, because the two working corpora were large enough to swamp it.
    ///
    /// Counting from the source side is what makes the difference. An index-side
    /// count can only ever describe rows that already have a vector, so it can
    /// tell you the index is small but never that it is missing something — and
    /// "of the rows that qualify, how many are indexed" is the only question a
    /// coverage figure can honestly answer.
    ///
    /// The totals therefore no longer include index rows with no qualifying
    /// source row (orphans, archived memories, unattributed sessions). Those are
    /// [`prune_orphans`](VectorIndex::prune_orphans)'s business; counting them
    /// as coverage describes the file rather than what retrieval can reach.
    async fn health(&self, model_id: &str) -> Result<IndexHealth> {
        let mut totals = IndexHealth::default();
        let mut per_corpus = Vec::with_capacity(Corpus::ALL.len());

        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let live = liveness_sql(corpus);
            // `WHERE 1 = 1` because `liveness_sql` yields a leading `AND` and is
            // empty for a corpus with no predicate.
            //
            // The corpus filter belongs in the JOIN condition, never the WHERE:
            // moved into the WHERE it turns this LEFT JOIN into an inner one and
            // every un-indexed row silently drops out of the count -- which
            // would report a corpus with no vectors at all as perfectly healthy,
            // the exact blindness this rewrite is here to remove.
            //
            // Bind order follows the order the `?`s appear in the SQL text, so
            // `model_id` (inside the SELECT list) is bound BEFORE the corpus.
            let sql = format!(
                "SELECT COUNT(*), \
                 COALESCE(SUM(CASE WHEN v.model_id = ? THEN 1 ELSE 0 END), 0), \
                 COALESCE(SUM(CASE WHEN v.row_id IS NULL THEN 1 ELSE 0 END), 0) \
                 FROM {table} s \
                 LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
                 WHERE 1 = 1 {live}"
            );
            let (rows, indexed, missing): (i64, i64, i64) = sqlx::query_as(&sql)
                .bind(model_id)
                .bind(corpus.as_str())
                .fetch_one(&self.pool)
                .await?;

            // A second, deliberately separate count: the whole table, with no
            // liveness predicate. It cannot ride the query above, whose
            // predicate lives in the WHERE, and it is what tells a reader
            // whether `rows == 0` means "this corpus is empty" or "this corpus
            // is excluded". On a live pond the summary corpus reported zero
            // qualifying rows while 27 sessions held a real rolling summary, and
            // with only the first number the surface called that 100% covered.
            let (source,): (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {table} s"))
                .fetch_one(&self.pool)
                .await?;

            let rows = rows.max(0) as u64;
            let source_rows = source.max(0) as u64;
            let indexed_rows = indexed.max(0) as u64;
            let missing_rows = missing.max(0) as u64;
            // Subtracted rather than counted by a fourth CASE, so the parts can
            // never fail to add up to `rows`. `(corpus, row_id)` is the vectors
            // primary key, so a source row joins at most one vector and every
            // qualifying row falls into exactly one of the three buckets.
            let mismatched = rows
                .saturating_sub(indexed_rows)
                .saturating_sub(missing_rows);

            totals.matching += indexed_rows;
            totals.mismatched += mismatched;
            totals.missing += missing_rows;
            per_corpus.push(CorpusHealth {
                corpus,
                rows,
                source_rows,
                indexed_rows,
                missing_rows,
                mismatched,
            });
        }

        totals.per_corpus = per_corpus;
        Ok(totals)
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::add_session;
    use super::*;
    use tempfile::TempDir;

    /// A pond with the real system schema plus a vector index attached to it.
    async fn wire() -> (TempDir, SqliteVectorIndex) {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index = SqliteVectorIndex::new(db.vectors.clone());
        (tmp, index)
    }

    /// `memory_fragments.profile_id` really is a foreign key to `profiles`, and
    /// `db.rs` enables `foreign_keys` on every connection — so a fixture that
    /// invents a member id is rejected. That is the schema working; create the
    /// member first.
    async fn add_profile(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn add_memory(pool: &Pool<Sqlite>, id: &str, profile: Option<&str>) {
        sqlx::query(
            "INSERT INTO memory_fragments (id, profile_id, content, source, tags, created_at, \
             access_count, lifecycle) VALUES (?, ?, 'x', 'chat', '[]', datetime('now'), 0, 'active')",
        )
        .bind(id)
        .bind(profile)
        .execute(pool)
        .await
        .unwrap();
    }

    fn entry(id: &str, v: Vec<f32>) -> VectorEntry {
        VectorEntry {
            corpus: Corpus::Memory,
            row_id: id.to_string(),
            model_id: "nomic-embed-text-v1.5".into(),
            vector: v,
            source_rev: None,
        }
    }

    #[tokio::test]
    async fn a_vector_roundtrips() {
        let (_tmp, index) = wire().await;
        let e = entry("m1", vec![0.1, 0.2, 0.3]);
        index.upsert(&e).await.unwrap();

        let got = index.get(Corpus::Memory, "m1").await.unwrap().unwrap();
        assert_eq!(got, e, "what came back is not what went in");
        assert!(index.get(Corpus::Memory, "nope").await.unwrap().is_none());
    }

    /// The identity is `(corpus, row_id)`, so a re-embed REPLACES. A rolling
    /// summary is rewritten in place; appending would leave a vector describing
    /// a conversation that no longer exists, still scoring against queries.
    #[tokio::test]
    async fn re_embedding_replaces_rather_than_appends() {
        let (_tmp, index) = wire().await;
        index.upsert(&entry("m1", vec![1.0, 0.0])).await.unwrap();
        index.upsert(&entry("m1", vec![0.0, 1.0])).await.unwrap();

        let got = index.get(Corpus::Memory, "m1").await.unwrap().unwrap();
        assert_eq!(got.vector, vec![0.0, 1.0], "the old vector survived");
    }

    /// Phase A's stated acceptance test: the index is DERIVED, so losing the
    /// file must be recoverable rather than fatal.
    #[tokio::test]
    async fn deleting_the_index_file_rebuilds_it_empty_and_usable() {
        let tmp = TempDir::new().unwrap();
        {
            let db = crate::db::Database::init(tmp.path()).await.unwrap();
            let index = SqliteVectorIndex::new(db.vectors.clone());
            add_memory(&db.system, "m1", None).await;
            index.upsert(&entry("m1", vec![1.0, 0.0])).await.unwrap();
            assert!(index.get(Corpus::Memory, "m1").await.unwrap().is_some());
        }

        // Delete the derived file (and its WAL sidecars) the way a user or a
        // support step would.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(tmp.path().join(format!("pond_vectors.db{suffix}")));
        }
        assert!(!tmp.path().join("pond_vectors.db").exists());

        // Reopening must recreate and re-migrate it, and the source row must
        // still be there asking to be re-embedded.
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index = SqliteVectorIndex::new(db.vectors.clone());
        assert!(
            index.get(Corpus::Memory, "m1").await.unwrap().is_none(),
            "the index came back populated, so it was not actually rebuilt"
        );
        let todo = index
            .needs_embedding(Corpus::Memory, "nomic-embed-text-v1.5", 10)
            .await
            .unwrap();
        assert_eq!(
            todo,
            vec!["m1".to_string()],
            "a rebuilt index must report the source row as needing embedding"
        );
    }

    /// The ATTACH is what makes the join possible at all. If it were done once
    /// per pool rather than per connection, this would pass until the pool
    /// opened a second connection.
    #[tokio::test]
    async fn search_joins_against_live_rows_and_an_orphan_matches_nothing() {
        let (_tmp, index) = wire().await;

        // 'ghost' has a vector but no source row: an orphan.
        index.upsert(&entry("ghost", vec![1.0, 0.0])).await.unwrap();
        let hits = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "an orphan matched; the JOIN is not filtering by existence"
        );

        // And it is prunable.
        assert_eq!(index.prune_orphans().await.unwrap(), 1);
        assert!(index.get(Corpus::Memory, "ghost").await.unwrap().is_none());
    }

    /// A vector from another model must be EXCLUDED, not scored. Cosine across
    /// two embedding spaces returns a plausible number that means nothing.
    #[tokio::test]
    async fn a_vector_from_another_model_is_never_returned() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_memory(&db.system, "m1", None).await;

        let mut stale = entry("m1", vec![1.0, 0.0]);
        stale.model_id = "all-MiniLM-L6-v2".into();
        index.upsert(&stale).await.unwrap();

        let hits = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert!(hits.is_empty(), "a foreign-model vector was scored");

        // And the sweep must offer it for re-embedding rather than leaving it.
        let todo = index
            .needs_embedding(Corpus::Memory, "nomic-embed-text-v1.5", 10)
            .await
            .unwrap();
        assert_eq!(todo, vec!["m1".to_string()]);

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        assert_eq!(health.matching, 0);
        assert_eq!(health.mismatched, 1);
    }

    /// The per-corpus row for `corpus`, failing loudly if it is absent.
    ///
    /// Absence is itself the defect: a corpus that reports nothing is a corpus
    /// nobody can see is broken, which is exactly how the summary corpus stayed
    /// invisible behind a single global percentage.
    fn corpus_health(health: &IndexHealth, corpus: Corpus) -> &CorpusHealth {
        health
            .per_corpus
            .iter()
            .find(|c| c.corpus == corpus)
            .unwrap_or_else(|| panic!("{corpus:?} is missing from the health surface entirely"))
    }

    /// A corpus with rows that QUALIFY and no vectors at all must report
    /// `rows > 0` beside `indexed_rows == 0`.
    ///
    /// This is the exact shape the single global number could not express, so it
    /// is asserted directly rather than inferred from a percentage. On the live
    /// pond the global figure read about 2% missing while one whole corpus sat
    /// at zero coverage: the two healthy corpora averaged the dead one away, the
    /// operator saw a number that looked fine, and retrieval was quietly
    /// answering from two thirds of what it was supposed to have.
    #[tokio::test]
    async fn a_corpus_with_qualifying_rows_and_no_vectors_reports_zero_coverage() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_memory(&db.system, "m1", None).await;
        add_memory(&db.system, "m2", None).await;

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        let memory = corpus_health(&health, Corpus::Memory);
        assert_eq!(
            memory.rows, 2,
            "the qualifying source rows were not counted, so coverage has no denominator"
        );
        assert_eq!(
            memory.indexed_rows, 0,
            "nothing was ever embedded, yet the corpus claims coverage"
        );
        assert_eq!(memory.missing_rows, 2);
        assert_eq!(memory.mismatched, 0);
    }

    /// A vector written by ANOTHER model is `mismatched`, never `indexed_rows`.
    ///
    /// Counting it as coverage is the failure this surface exists to prevent:
    /// the row is present, it is excluded from retrieval by design, and calling
    /// it indexed would report a corpus as healthy at the precise moment it
    /// stopped working. It is kept out of `missing_rows` too, because the repair
    /// differs -- one needs an embed, the other a re-embed of something already
    /// sitting there.
    #[tokio::test]
    async fn a_vector_from_another_model_counts_as_mismatched_not_indexed() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_memory(&db.system, "mine", None).await;
        add_memory(&db.system, "theirs", None).await;

        index.upsert(&entry("mine", vec![1.0, 0.0])).await.unwrap();
        let mut foreign = entry("theirs", vec![1.0, 0.0]);
        foreign.model_id = "all-MiniLM-L6-v2".into();
        index.upsert(&foreign).await.unwrap();

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        let memory = corpus_health(&health, Corpus::Memory);
        assert_eq!(memory.rows, 2);
        assert_eq!(
            memory.indexed_rows, 1,
            "a foreign-model vector was counted as coverage"
        );
        assert_eq!(memory.mismatched, 1);
        assert_eq!(
            memory.missing_rows, 0,
            "a row that HAS a vector was reported as never embedded"
        );
    }

    /// The per-corpus rows must ADD UP to the totals the maintenance pass still
    /// reads, over a store deliberately full of the awkward cases.
    ///
    /// The orphan and the archived memory are the reason this is not trivial:
    /// both are index rows a whole-file `COUNT` would add to the totals, and
    /// neither is a row retrieval can ever return, so neither may appear in a
    /// coverage figure. A total that counts them describes the index FILE rather
    /// than what the assistant can reach.
    #[tokio::test]
    async fn the_per_corpus_numbers_sum_to_the_global_totals() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();

        // Memory: one indexed, one from another model, one bare.
        add_memory(&db.system, "indexed", None).await;
        add_memory(&db.system, "foreign", None).await;
        add_memory(&db.system, "bare", None).await;
        index
            .upsert(&entry("indexed", vec![1.0, 0.0]))
            .await
            .unwrap();
        let mut foreign = entry("foreign", vec![1.0, 0.0]);
        foreign.model_id = "all-MiniLM-L6-v2".into();
        index.upsert(&foreign).await.unwrap();

        // An archived memory that IS indexed, and an orphan with no source row.
        add_memory(&db.system, "archived", None).await;
        index
            .upsert(&entry("archived", vec![1.0, 0.0]))
            .await
            .unwrap();
        sqlx::query("UPDATE memory_fragments SET lifecycle = 'archived' WHERE id = 'archived'")
            .execute(&db.system)
            .await
            .unwrap();
        index.upsert(&entry("ghost", vec![1.0, 0.0])).await.unwrap();

        // A summary that qualifies, and one the predicate refuses because nobody
        // was identified in the session.
        add_session(
            &db.system,
            "owned",
            Some("we discussed the garden"),
            "2026-08-13 10:00:00",
        )
        .await;
        add_session(
            &db.system,
            "guest",
            Some("a guest chatted"),
            "2026-08-13 10:00:00",
        )
        .await;
        sqlx::query("UPDATE sessions SET profile_id = NULL WHERE id = 'guest'")
            .execute(&db.system)
            .await
            .unwrap();

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        assert_eq!(
            health.per_corpus.len(),
            Corpus::ALL.len(),
            "a corpus vanished from the health surface"
        );

        let sum = |f: fn(&CorpusHealth) -> u64| health.per_corpus.iter().map(f).sum::<u64>();
        assert_eq!(
            health.matching,
            sum(|c| c.indexed_rows),
            "the matching total is not the sum of the per-corpus coverage"
        );
        assert_eq!(health.mismatched, sum(|c| c.mismatched));
        assert_eq!(health.missing, sum(|c| c.missing_rows));

        // And each corpus's parts add up to its own qualifying row count, which
        // is what makes `indexed_rows / rows` a percentage rather than a ratio
        // of two unrelated numbers.
        for c in &health.per_corpus {
            assert_eq!(
                c.rows,
                c.indexed_rows + c.missing_rows + c.mismatched,
                "{:?} does not add up: {c:?}",
                c.corpus
            );
        }

        // Pinned values, so the sums above cannot be satisfied by three zeros.
        let memory = corpus_health(&health, Corpus::Memory);
        assert_eq!(
            (
                memory.rows,
                memory.indexed_rows,
                memory.mismatched,
                memory.missing_rows
            ),
            (3, 1, 1, 1),
            "the archived memory or the orphan leaked into the memory corpus"
        );
        let summary = corpus_health(&health, Corpus::Summary);
        assert_eq!(
            summary.rows, 1,
            "the unattributed session was counted as indexable"
        );
        assert_eq!(summary.missing_rows, 1);
    }

    /// The SQL predicate is pinned DIRECTLY, because the behavioural test below
    /// cannot see it: `search` early-returns on `scope.excludes_everything()`
    /// before any query runs, so making this predicate permissive leaves that
    /// test green. Verified by mutation — the first version of this guard passed
    /// with `Guest` opened up, which is precisely the vacuous-test shape this
    /// codebase keeps paying for.
    ///
    /// Two layers is the intent (early return, then a predicate that fails
    /// closed if a future caller reaches the SQL another way); the point is that
    /// each is pinned by something, rather than each being assumed by the other.
    #[test]
    fn the_scope_predicate_refuses_a_guest_for_every_corpus() {
        for corpus in Corpus::ALL {
            let (pred, bind) = scope_sql(corpus, &ProfileScope::Guest);
            assert_eq!(
                pred, "AND 1 = 0",
                "{corpus:?} does not refuse a guest in SQL"
            );
            assert!(bind.is_none());
        }
        // Household is unrestricted, and an owner is restricted with a bind.
        assert_eq!(scope_sql(Corpus::Memory, &ProfileScope::Household).0, "");
        let (pred, bind) = scope_sql(Corpus::Memory, &ProfileScope::Owner("jerry".into()));
        assert!(pred.contains("profile_id = ?"), "got: {pred}");
        assert_eq!(bind.as_deref(), Some("jerry"));
    }

    /// Scope is applied in the SQL. A guest sees nothing from any corpus.
    #[tokio::test]
    async fn a_guest_sees_nothing_and_an_owner_sees_only_their_own() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_profile(&db.system, "jerry").await;
        add_profile(&db.system, "sam").await;
        add_memory(&db.system, "mine", Some("jerry")).await;
        add_memory(&db.system, "theirs", Some("sam")).await;
        add_memory(&db.system, "shared", None).await;
        for id in ["mine", "theirs", "shared"] {
            index.upsert(&entry(id, vec![1.0, 0.0])).await.unwrap();
        }

        let guest = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Guest,
                10,
            )
            .await
            .unwrap();
        assert!(guest.is_empty(), "a guest reached the household's index");

        let owner = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Owner("jerry".into()),
                10,
            )
            .await
            .unwrap();
        let ids: Vec<&str> = owner.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mine", "shared"],
            "an owner must see their own rows and unattributed household ones, \
             and never another member's"
        );
    }
}

/// Phase B: write-through. These live here rather than beside each adapter
/// because what they assert is the INDEX's contents after a normal store
/// operation -- the thing a member would experience as "it can find this".
#[cfg(test)]
mod write_through_tests {
    use super::tests_support::*;
    use super::*;
    use pond_core::context::vector_index::VectorIndex as _;
    use pond_core::user_data::domain::memory::MemoryFragment;
    use pond_core::user_data::ports::memory_repository::MemoryRepository;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Storing a memory that carries a vector must make it searchable, without
    /// any separate indexing step.
    #[tokio::test]
    async fn a_stored_memory_is_immediately_searchable() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), Some("nomic-embed-text-v1.5".into()));

        let mut frag =
            MemoryFragment::from_chat("m1".into(), None, None, "the spare key is out back".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();

        let hits = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "a stored memory was not searchable");
        assert_eq!(hits[0].row_id, "m1");
        assert_eq!(hits[0].corpus, Corpus::Memory);
    }

    /// The redactor drops a fragment's vector when the content held a secret,
    /// because that vector is a durable derivative of text the store refuses to
    /// keep. Write-through must mirror the STORED row, so such a fragment
    /// contributes nothing to the index -- writing through ABOVE the redactor
    /// would put exactly that derivative somewhere it cannot be audited.
    #[tokio::test]
    async fn a_fragment_whose_vector_was_dropped_contributes_nothing_to_the_index() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), Some("nomic-embed-text-v1.5".into()));

        // A sibling WITH a vector, so a failure here cannot be "indexing is
        // simply not working" -- the mechanism has to be demonstrably live.
        let mut ok = MemoryFragment::from_chat("kept".into(), None, None, "harmless".into());
        ok.embedding = Some(vec![1.0, 0.0]);
        repo.add(ok).await.unwrap();

        // And the one the redactor stripped: same path, no vector.
        let stripped =
            MemoryFragment::from_chat("secret".into(), None, None, "[redacted:api-key]".into());
        assert!(stripped.embedding.is_none());
        repo.add(stripped).await.unwrap();

        assert!(
            index.get(Corpus::Memory, "kept").await.unwrap().is_some(),
            "the write-through is not live, so this test proves nothing"
        );
        assert!(
            index.get(Corpus::Memory, "secret").await.unwrap().is_none(),
            "a fragment the redactor stripped still reached the index"
        );
    }

    /// A process with no embedder configured (the `pond memories add` CLI) must
    /// not strip entries the server wrote correctly. It cannot attribute a
    /// vector to a model, so it leaves the index alone for the sweep.
    #[tokio::test]
    async fn an_unattributable_vector_is_left_alone_rather_than_removed() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        // The server already indexed this row.
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Memory,
                row_id: "m1".into(),
                model_id: "nomic-embed-text-v1.5".into(),
                vector: vec![1.0, 0.0],
                source_rev: None,
            })
            .await
            .unwrap();

        // Now a second process stores the row itself, carrying a vector it
        // cannot name a model for.
        let no_model = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), None);
        let mut frag = MemoryFragment::from_chat("m1".into(), None, None, "kept".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        no_model.add(frag).await.unwrap();

        assert!(
            index.get(Corpus::Memory, "m1").await.unwrap().is_some(),
            "a write with no embedder stripped an entry the server had written"
        );
    }

    /// Deleting a memory removes its vector, so the common case needs no sweep.
    #[tokio::test]
    async fn deleting_a_memory_drops_its_vector() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), Some("m".into()));

        let mut frag = MemoryFragment::from_chat("m1".into(), None, None, "x".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();
        repo.delete("m1").await.unwrap();

        assert!(index.get(Corpus::Memory, "m1").await.unwrap().is_none());
    }

    /// An ARCHIVED memory must not be searchable. Every direct read of the
    /// store excludes it; the index must agree, or archiving becomes a lie the
    /// moment retrieval goes through the index instead.
    #[tokio::test]
    async fn an_archived_memory_is_not_returned_by_search() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_memory_with_vector(&db.system, "live", &[1.0, 0.0]).await;
        add_memory_with_vector(&db.system, "archived", &[1.0, 0.0]).await;
        index
            .backfill_from_source(Corpus::Memory, "m", 2)
            .await
            .unwrap();
        sqlx::query("UPDATE memory_fragments SET lifecycle = 'archived' WHERE id = 'archived'")
            .execute(&db.system)
            .await
            .unwrap();

        let hits = index
            .search(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["live"],
            "an archived memory came back from the index"
        );
    }

    /// A summary of a session nobody was identified in must never enter
    /// household retrieval.
    ///
    /// The idle loop summarises EVERY session it lists, and it takes no scope.
    /// `sessions.profile_id IS NULL` means "we do not know whose this is" --
    /// the OPPOSITE of `memory_fragments.profile_id IS NULL`, which means shared
    /// household context. Same column name, inverted meaning; conflating them
    /// would surface a guest's conversation to the household.
    #[tokio::test]
    async fn an_unattributed_session_summary_is_never_surfaced() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_profile(&db.system, "jerry").await;
        add_session(
            &db.system,
            "owned",
            Some("jerry chat"),
            "2026-08-13 10:00:00",
        )
        .await;
        add_session(
            &db.system,
            "guest",
            Some("guest chat"),
            "2026-08-13 10:00:00",
        )
        .await;
        sqlx::query("UPDATE sessions SET profile_id = 'jerry' WHERE id = 'owned'")
            .execute(&db.system)
            .await
            .unwrap();
        // The guest case: nobody was identified in this session.
        sqlx::query("UPDATE sessions SET profile_id = NULL WHERE id = 'guest'")
            .execute(&db.system)
            .await
            .unwrap();

        for id in ["owned", "guest"] {
            index
                .upsert(&VectorEntry {
                    corpus: Corpus::Summary,
                    row_id: id.into(),
                    model_id: "m".into(),
                    vector: vec![1.0, 0.0],
                    source_rev: Some("2026-08-13 10:00:00".into()),
                })
                .await
                .unwrap();
        }

        let hits = index
            .search(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["owned"],
            "an unattributed (guest) session summary reached a household read"
        );

        // And the sweep must not even offer it for embedding -- no point paying
        // inference for a row retrieval will always refuse.
        let todo = index
            .needs_embedding(Corpus::Summary, "other", 10)
            .await
            .unwrap();
        assert_eq!(todo, vec!["owned".to_string()]);
    }

    /// `search_resolved` must return the LIVE text of each hit, per corpus, and
    /// must obey exactly the same scope and liveness rules as `search`.
    ///
    /// This is what phase C's retrieval service consumes, so a divergence
    /// between the two queries would mean the thing users actually hit behaves
    /// differently from the thing the isolation tests cover.
    #[tokio::test]
    async fn resolved_search_returns_live_text_and_obeys_the_same_rules() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_memory_with_vector(&db.system, "m1", &[1.0, 0.0]).await;
        sqlx::query(
            "UPDATE memory_fragments SET content = 'the bill is eighty pounds' WHERE id='m1'",
        )
        .execute(&db.system)
        .await
        .unwrap();
        add_memory_with_vector(&db.system, "gone", &[1.0, 0.0]).await;
        index
            .backfill_from_source(Corpus::Memory, "m", 2)
            .await
            .unwrap();

        // A summary, which resolves from a different column entirely.
        add_session(
            &db.system,
            "s1",
            Some("we discussed the garden"),
            "2026-08-13 10:00:00",
        )
        .await;
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                model_id: "m".into(),
                vector: vec![1.0, 0.0],
                source_rev: Some("2026-08-13 10:00:00".into()),
            })
            .await
            .unwrap();

        // Archive one: it must vanish from the resolved search too.
        sqlx::query("UPDATE memory_fragments SET lifecycle='archived' WHERE id='gone'")
            .execute(&db.system)
            .await
            .unwrap();

        let hits = index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        let texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
        assert!(
            texts.contains(&"the bill is eighty pounds"),
            "memory text was not resolved: {texts:?}"
        );
        assert!(
            texts.contains(&"we discussed the garden"),
            "summary text was not resolved: {texts:?}"
        );
        assert!(
            !hits.iter().any(|h| h.row_id == "gone"),
            "an archived memory came back from the resolved search"
        );

        // Equal scores: the memory must come first. This is "memory wins ties",
        // and it is the enum's declaration order doing the work.
        assert_eq!(
            hits[0].corpus,
            Corpus::Memory,
            "a summary outranked a memory on a tie"
        );

        // And a guest still gets nothing through this path.
        assert!(index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Guest, 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// Phase E's stated acceptance: a bus event produces an index entry.
    ///
    /// No separate subscriber was built for this, and that is the finding rather
    /// than a shortcut. The chain a sensor or camera event already takes --
    /// `BusIngest::absorb` -> `IngestPipeline::ingest` -> `save_item` -> the
    /// write-through -- ends at the same terminal write phase B instrumented, so
    /// a second bus subscriber beside `BusIngest` would be a parallel path to
    /// the same row with its own way of going wrong. This exercises the real
    /// pipeline end to end instead.
    #[tokio::test]
    async fn an_ingested_item_reaches_the_index_and_disconnecting_removes_it() {
        use pond_core::context::domain::{SourceKind, SourceParts, SourceStatus};
        use pond_core::context::ingest::{IngestPipeline, RawItem};
        use pond_core::context::ports::ContextRepository;
        use pond_core::security::domain::redaction::RedactionKind;
        use pond_core::security::mocks::mock_redactor::MockRedactor;

        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        add_profile(&db.system, "jerry").await;

        let redactor = Arc::new(MockRedactor::replacing("nothing", RedactionKind::ApiKey));
        let repo = Arc::new(
            crate::sqlite_context::SqliteContextRepository::new(
                db.system.clone(),
                redactor.clone(),
            )
            .with_vector_index(index.clone(), Some("m".into())),
        );

        let source = pond_core::context::domain::ContextSource::from_parts(SourceParts {
            id: "src-sensor".into(),
            kind: SourceKind::Sensor,
            provider: "pond".into(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: chrono::Utc::now(),
        })
        .expect("valid source");
        repo.upsert_source(&source).await.unwrap();

        // The embedder the pipeline would have: any vector will do, the point is
        // that it reaches the index.
        struct E;
        #[async_trait]
        impl pond_core::models::ports::embedding::EmbeddingProvider for E {
            async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
                Ok(vec![1.0, 0.0])
            }
            fn dimensions(&self) -> usize {
                2
            }
            fn model_id(&self) -> String {
                "m".into()
            }
        }
        let pipeline = IngestPipeline::new(repo.clone(), redactor).with_embedder(Some(
            Arc::new(E) as Arc<dyn pond_core::models::ports::embedding::EmbeddingProvider>
        ));

        pipeline
            .ingest(
                &source,
                RawItem {
                    external_id: "evt-1".into(),
                    kind: pond_core::context::domain::ItemKind::Event,
                    occurred_at: chrono::Utc::now(),
                    title: "back door".into(),
                    body: "the back door opened".into(),
                    participants: vec![],
                },
                chrono::Utc::now(),
            )
            .await
            .expect("ingest");

        let hits = index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "an ingested event never reached the index");
        assert_eq!(hits[0].corpus, Corpus::Context);
        assert!(
            hits[0].text.contains("the back door opened"),
            "resolved text was wrong: {}",
            hits[0].text
        );

        // Disconnecting the source is a deletion promise: the vectors go too,
        // now, not at the next maintenance sweep.
        repo.disconnect_source("src-sensor", &ProfileScope::Household)
            .await
            .unwrap();
        assert!(
            index
                .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
                .await
                .unwrap()
                .is_empty(),
            "a disconnected source left its vectors behind"
        );
    }

    /// Phase C's stated acceptance: two profiles and a guest, across corpora.
    #[tokio::test]
    async fn three_way_isolation_two_members_and_a_guest() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_profile(&db.system, "jerry").await;
        add_profile(&db.system, "sam").await;
        add_memory_with_vector(&db.system, "jerry-own", &[1.0, 0.0]).await;
        add_memory_with_vector(&db.system, "sam-own", &[1.0, 0.0]).await;
        add_memory_with_vector(&db.system, "shared", &[1.0, 0.0]).await;
        sqlx::query("UPDATE memory_fragments SET profile_id='jerry' WHERE id='jerry-own'")
            .execute(&db.system)
            .await
            .unwrap();
        sqlx::query("UPDATE memory_fragments SET profile_id='sam' WHERE id='sam-own'")
            .execute(&db.system)
            .await
            .unwrap();
        index
            .backfill_from_source(Corpus::Memory, "m", 2)
            .await
            .unwrap();

        let ids = |hits: Vec<VectorHit>| {
            let mut v: Vec<String> = hits.into_iter().map(|h| h.row_id).collect();
            v.sort();
            v
        };

        let jerry = ids(index
            .search(&[1.0, 0.0], "m", &ProfileScope::Owner("jerry".into()), 10)
            .await
            .unwrap());
        assert_eq!(jerry, vec!["jerry-own", "shared"], "jerry's view is wrong");

        let sam = ids(index
            .search(&[1.0, 0.0], "m", &ProfileScope::Owner("sam".into()), 10)
            .await
            .unwrap());
        assert_eq!(sam, vec!["sam-own", "shared"], "sam's view is wrong");

        let guest = index
            .search(&[1.0, 0.0], "m", &ProfileScope::Guest, 10)
            .await
            .unwrap();
        assert!(guest.is_empty(), "a guest reached the household's index");
    }

    /// Deleting the index must be recoverable for memory too, not just for
    /// summaries. **This is a regression test for a real bug**: the memory
    /// sweeps are driven by `memory_fragments.embedding IS NULL`, so an
    /// already-embedded row never reaches the write-through again. Found by
    /// deleting `pond_vectors.db` on a live pond — the summary came back and
    /// five memories did not, silently, with the store looking healthy.
    #[tokio::test]
    async fn an_already_embedded_memory_is_adopted_when_the_index_is_rebuilt() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        // A store that is fully embedded, and an index that knows nothing.
        add_memory_with_vector(&db.system, "m1", &vec![0.5f32; 8]).await;
        add_memory_with_vector(&db.system, "m2", &vec![0.25f32; 8]).await;
        assert!(index.get(Corpus::Memory, "m1").await.unwrap().is_none());

        let copied = index
            .backfill_from_source(Corpus::Memory, "m", 8)
            .await
            .unwrap();
        assert_eq!(copied, 2, "existing vectors were not adopted");
        let got = index.get(Corpus::Memory, "m1").await.unwrap().unwrap();
        assert_eq!(
            got.vector,
            vec![0.5f32; 8],
            "the adopted vector is not the stored one"
        );
        assert_eq!(got.model_id, "m");

        // Idempotent: a second pass must not duplicate or re-copy.
        assert_eq!(
            index
                .backfill_from_source(Corpus::Memory, "m", 8)
                .await
                .unwrap(),
            0,
            "adoption ran twice over the same rows"
        );
    }

    /// A stored vector of a DIFFERENT width came from a different model and must
    /// not be restamped with the current one — that would launder a stale vector
    /// into the live space where nothing could ever detect it.
    #[tokio::test]
    async fn adoption_refuses_a_vector_of_the_wrong_width() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_memory_with_vector(&db.system, "current", &vec![0.5f32; 8]).await;
        add_memory_with_vector(&db.system, "legacy", &vec![0.5f32; 4]).await;

        let copied = index
            .backfill_from_source(Corpus::Memory, "m", 8)
            .await
            .unwrap();
        assert_eq!(copied, 1);
        assert!(index
            .get(Corpus::Memory, "current")
            .await
            .unwrap()
            .is_some());
        assert!(
            index.get(Corpus::Memory, "legacy").await.unwrap().is_none(),
            "a 4-wide vector was adopted as if this model had produced it"
        );
    }

    /// The sweep must CONVERGE. A vector stored without a revision stamp
    /// compares unequal to a non-NULL column, so without the `IS NOT NULL` limb
    /// in the staleness clause it is reported stale on every sweep forever --
    /// the pond re-embeds the same summaries indefinitely, burning the one
    /// scarce resource on the device and never finishing.
    ///
    /// A NULL rev means "this entry does not track revisions", not "out of
    /// date". Mutation-tested: dropping that limb fails this and nothing else.
    #[tokio::test]
    async fn a_vector_stored_without_a_revision_does_not_re_embed_forever() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_session(&db.system, "s1", Some("a summary"), "2026-08-13 10:00:00").await;
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                model_id: "m".into(),
                vector: vec![1.0, 0.0],
                // No stamp -- what a writer that does not track revisions leaves.
                source_rev: None,
            })
            .await
            .unwrap();

        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert!(
            todo.is_empty(),
            "an un-stamped vector was reported stale, so the sweep would re-embed \
             it on every pass and never converge"
        );
    }

    /// Phase B's stated acceptance criterion for the summary corpus: a
    /// re-summarised session's vector CHANGES.
    ///
    /// The sweep detects it with no help from the writer, because the stored
    /// `source_rev` no longer matches `rolling_summary_updated_at`.
    #[tokio::test]
    async fn a_resummarised_session_is_reported_stale_and_its_vector_changes() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_session(
            &db.system,
            "s1",
            Some("first summary"),
            "2026-08-13 10:00:00",
        )
        .await;

        // Not yet indexed -> the sweep offers it.
        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert_eq!(todo, vec!["s1".to_string()]);

        // Index it at the revision the sweep would have read.
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                model_id: "m".into(),
                vector: vec![1.0, 0.0],
                source_rev: Some("2026-08-13 10:00:00".into()),
            })
            .await
            .unwrap();

        // Now it is current: the sweep must NOT keep re-embedding it. This is
        // the convergence property -- a NULL or mismatched rev here would make
        // every sweep redo the same work forever.
        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert!(todo.is_empty(), "an up-to-date summary was reported stale");

        // Re-summarise: the column is rewritten in place with a new stamp.
        set_summary(&db.system, "s1", "a better summary", "2026-08-13 11:00:00").await;
        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert_eq!(
            todo,
            vec!["s1".to_string()],
            "a re-summarised session was not reported stale, so its vector would never change"
        );

        // And re-indexing REPLACES rather than appends.
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                model_id: "m".into(),
                vector: vec![0.0, 1.0],
                source_rev: Some("2026-08-13 11:00:00".into()),
            })
            .await
            .unwrap();
        let got = index.get(Corpus::Summary, "s1").await.unwrap().unwrap();
        assert_eq!(
            got.vector,
            vec![0.0, 1.0],
            "the old summary vector survived"
        );
    }
}

#[cfg(test)]
mod tests_support {
    use sqlx::{Pool, Sqlite};

    /// Creates the session ATTRIBUTED to a member.
    ///
    /// Unattributed sessions are guest sessions and retrieval refuses their
    /// summaries by design, so a fixture without an owner would be asserting
    /// about a row the pond deliberately never surfaces. Tests that want the
    /// unattributed case set `profile_id` back to NULL explicitly.
    pub async fn add_session(pool: &Pool<Sqlite>, id: &str, summary: Option<&str>, updated: &str) {
        sqlx::query("INSERT OR IGNORE INTO profiles (id, display_name) VALUES ('owner','Owner')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, created_at, profile_id) VALUES (?, datetime('now'), 'owner')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        if let Some(s) = summary {
            set_summary(pool, id, s, updated).await;
        }
    }

    pub async fn add_profile(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    pub async fn add_memory_with_vector(pool: &Pool<Sqlite>, id: &str, v: &[f32]) {
        let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        sqlx::query(
            "INSERT INTO memory_fragments (id, content, embedding, source, tags, created_at, \
             access_count, lifecycle) VALUES (?, 'x', ?, 'chat', '[]', datetime('now'), 0, 'active')",
        )
        .bind(id)
        .bind(blob)
        .execute(pool)
        .await
        .unwrap();
    }

    pub async fn set_summary(pool: &Pool<Sqlite>, id: &str, summary: &str, updated: &str) {
        sqlx::query(
            "UPDATE sessions SET rolling_summary = ?, rolling_summary_updated_at = ? WHERE id = ?",
        )
        .bind(summary)
        .bind(updated)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
}
