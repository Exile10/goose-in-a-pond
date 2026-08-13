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
use pond_core::context::vector_index::{Corpus, IndexHealth, VectorEntry, VectorHit, VectorIndex};
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
        (Corpus::Summary, ProfileScope::Owner(_)) => ("AND 1 = 0".into(), None),
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
            // The JOIN is the existence check: an orphan whose source row is
            // gone simply does not match, which is what makes a vector with no
            // text harmless rather than a leak.
            let sql = format!(
                "SELECT v.row_id, v.vector FROM vectors v \
                 JOIN {table} s ON s.{id_col} = v.row_id \
                 WHERE v.corpus = ? AND v.model_id = ? {scope_pred}"
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
        let extra = match corpus {
            Corpus::Summary => "AND s.rolling_summary IS NOT NULL AND s.rolling_summary != ''",
            Corpus::Memory => "AND (s.lifecycle IS NULL OR s.lifecycle = 'active')",
            Corpus::Context => "",
        };
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

    async fn health(&self, model_id: &str) -> Result<IndexHealth> {
        let (matching,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vectors WHERE model_id = ?")
            .bind(model_id)
            .fetch_one(&self.pool)
            .await?;
        let (mismatched,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM vectors WHERE model_id != ?")
                .bind(model_id)
                .fetch_one(&self.pool)
                .await?;

        let mut missing = 0i64;
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let extra = match corpus {
                Corpus::Summary => "AND s.rolling_summary IS NOT NULL AND s.rolling_summary != ''",
                Corpus::Memory => "AND (s.lifecycle IS NULL OR s.lifecycle = 'active')",
                Corpus::Context => "",
            };
            let sql = format!(
                "SELECT COUNT(*) FROM {table} s \
                 LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
                 WHERE v.row_id IS NULL {extra}"
            );
            let (n,): (i64,) = sqlx::query_as(&sql)
                .bind(corpus.as_str())
                .fetch_one(&self.pool)
                .await?;
            missing += n;
        }

        Ok(IndexHealth {
            matching: matching.max(0) as u64,
            mismatched: mismatched.max(0) as u64,
            missing: missing.max(0) as u64,
        })
    }
}

#[cfg(test)]
mod tests {
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

    pub async fn add_session(pool: &Pool<Sqlite>, id: &str, summary: Option<&str>, updated: &str) {
        sqlx::query("INSERT INTO sessions (id, created_at) VALUES (?, datetime('now'))")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        if let Some(s) = summary {
            set_summary(pool, id, s, updated).await;
        }
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
