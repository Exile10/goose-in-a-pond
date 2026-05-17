//! SQLite-backed implementation of `MemoryRepository`.
//!
//! Uses the `memory_fragments` table in `pond_system.db`.
//! Migration 0005 creates the base table; 0015 adds segment/importance/decay fields.
//! Embeddings are stored as raw little-endian f32 BLOBs.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use pond_core::domain::memory::{
    MemoryEvent, MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment, MemoryTier,
};
use pond_core::ports::memory_repository::MemoryRepository;
use serde_json;
use sqlx::{Pool, Sqlite};

pub struct SqliteMemoryRepository {
    pool: Pool<Sqlite>,
}

impl SqliteMemoryRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

// ── Embedding BLOB encoding ───────────────────────────────────────────────────

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

// ── Row helper ────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct FragmentRow {
    id: String,
    profile_id: Option<String>,
    session_id: Option<String>,
    content: String,
    embedding: Option<Vec<u8>>,
    source: String,
    tags: String,
    created_at: String,
    // ── Segment-aware columns (nullable for pre-migration rows) ───────
    segment: Option<String>,
    importance: Option<f64>, // SQLite REAL → f64
    tier: Option<String>,
    decay_rate: Option<f64>,
    access_count: i64,
    last_accessed_at: Option<String>,
    lifecycle: Option<String>,
    superseded_by: Option<String>,
    /// For correction memories: what wrong claim this corrects.
    corrects: Option<String>,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn parse_segment(s: &str) -> Option<MemorySegment> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn parse_tier(s: &str) -> Option<MemoryTier> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn parse_lifecycle(s: &str) -> Option<MemoryLifecycle> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn row_to_fragment(row: FragmentRow) -> MemoryFragment {
    let tags: Vec<String> = serde_json::from_str(&row.tags).unwrap_or_default();
    let embedding = row.embedding.as_deref().map(blob_to_vec);
    MemoryFragment {
        id: row.id,
        profile_id: row.profile_id,
        session_id: row.session_id,
        content: row.content,
        embedding,
        source: row.source,
        tags,
        created_at: parse_dt(&row.created_at),
        segment: row.segment.as_deref().and_then(parse_segment),
        importance: row.importance.map(|v| v as f32),
        tier: row.tier.as_deref().and_then(parse_tier),
        decay_rate: row.decay_rate.map(|v| v as f32),
        access_count: row.access_count as u32,
        last_accessed_at: row.last_accessed_at.as_deref().map(parse_dt),
        lifecycle: row.lifecycle.as_deref().and_then(parse_lifecycle),
        superseded_by: row.superseded_by,
        corrects: row.corrects,
    }
}

/// All columns selected by all queries.
const SELECT_ALL: &str = "\
    id, profile_id, session_id, content, embedding, source, tags, created_at, \
    segment, importance, tier, decay_rate, access_count, last_accessed_at, \
    lifecycle, superseded_by, corrects";

fn segment_to_str(s: &MemorySegment) -> &'static str {
    match s {
        MemorySegment::Identity => "identity",
        MemorySegment::Preference => "preference",
        MemorySegment::Correction => "correction",
        MemorySegment::Relationship => "relationship",
        MemorySegment::Project => "project",
        MemorySegment::Knowledge => "knowledge",
        MemorySegment::Context => "context",
    }
}

fn lifecycle_to_str(l: &MemoryLifecycle) -> &'static str {
    match l {
        MemoryLifecycle::Active => "active",
        MemoryLifecycle::Archived => "archived",
        MemoryLifecycle::Merged => "merged",
    }
}

fn tier_to_str(t: &MemoryTier) -> &'static str {
    match t {
        MemoryTier::Short => "short",
        MemoryTier::Long => "long",
        MemoryTier::Permanent => "permanent",
    }
}

#[async_trait]
impl MemoryRepository for SqliteMemoryRepository {
    async fn add(&self, fragment: MemoryFragment) -> Result<()> {
        let tags_json = serde_json::to_string(&fragment.tags)?;
        let created_str = fragment.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let embedding_blob = fragment.embedding.as_deref().map(vec_to_blob);
        let segment_str = fragment.segment.as_ref().map(segment_to_str);
        let tier_str = fragment.tier.as_ref().map(tier_to_str).or(Some("long"));
        let lifecycle_str = Some(
            fragment
                .lifecycle
                .as_ref()
                .map(lifecycle_to_str)
                .unwrap_or("active"),
        );
        let last_accessed_str = fragment
            .last_accessed_at
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string());

        sqlx::query(
            "INSERT INTO memory_fragments \
             (id, profile_id, session_id, content, embedding, source, tags, created_at, \
              segment, importance, tier, decay_rate, access_count, last_accessed_at, \
              lifecycle, superseded_by, corrects) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&fragment.id)
        .bind(&fragment.profile_id)
        .bind(&fragment.session_id)
        .bind(&fragment.content)
        .bind(embedding_blob)
        .bind(&fragment.source)
        .bind(&tags_json)
        .bind(&created_str)
        .bind(segment_str)
        .bind(fragment.importance.map(|v| v as f64))
        .bind(tier_str)
        .bind(fragment.decay_rate.map(|v| v as f64))
        .bind(fragment.access_count as i64)
        .bind(last_accessed_str)
        .bind(lifecycle_str)
        .bind(&fragment.superseded_by)
        .bind(&fragment.corrects)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn search_recent(
        &self,
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let query = match profile_id {
            Some(_) => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE profile_id = ? AND (lifecycle IS NULL OR lifecycle = 'active') \
                 ORDER BY created_at DESC LIMIT ?"
            ),
            None => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE (lifecycle IS NULL OR lifecycle = 'active') \
                 ORDER BY created_at DESC LIMIT ?"
            ),
        };

        let rows: Vec<FragmentRow> = match profile_id {
            Some(pid) => {
                sqlx::query_as(&query)
                    .bind(pid)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query_as(&query)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let query = match profile_id {
            Some(_) => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE embedding IS NOT NULL AND profile_id = ? \
                 AND (lifecycle IS NULL OR lifecycle = 'active')"
            ),
            None => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE embedding IS NOT NULL \
                 AND (lifecycle IS NULL OR lifecycle = 'active')"
            ),
        };

        let rows: Vec<FragmentRow> = match profile_id {
            Some(pid) => {
                sqlx::query_as(&query)
                    .bind(pid)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => sqlx::query_as(&query).fetch_all(&self.pool).await?,
        };

        if rows.is_empty() {
            return self.search_recent(profile_id, limit).await;
        }

        let mut scored: Vec<(f32, MemoryFragment)> = rows
            .into_iter()
            .map(row_to_fragment)
            .filter_map(|f| {
                let emb = f.embedding.clone()?;
                let score = cosine_similarity(query_embedding, &emb);
                Some((score, f))
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, f)| f).collect())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM memory_fragments WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn search_by_content(
        &self,
        keywords: &[String],
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        if keywords.is_empty() {
            return Ok(vec![]);
        }

        // Build a WHERE clause with OR'd LIKE conditions for each keyword.
        // e.g. (content LIKE '%cat%' OR content LIKE '%dog%')
        let like_clauses: Vec<String> = keywords
            .iter()
            .map(|_| "LOWER(content) LIKE ?".to_string())
            .collect();
        let likes_sql = like_clauses.join(" OR ");

        let profile_filter = if profile_id.is_some() {
            "AND profile_id = ?"
        } else {
            ""
        };

        let sql = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE (lifecycle IS NULL OR lifecycle = 'active') \
             AND ({likes_sql}) \
             {profile_filter} \
             ORDER BY COALESCE(importance, 0.5) DESC, created_at DESC \
             LIMIT ?"
        );

        let mut query = sqlx::query_as::<_, FragmentRow>(&sql);

        // Bind each keyword as '%keyword%'
        for kw in keywords {
            query = query.bind(format!("%{}%", kw.to_lowercase()));
        }

        if let Some(pid) = profile_id {
            query = query.bind(pid);
        }

        query = query.bind(limit as i64);

        let rows: Vec<FragmentRow> = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    // ── Segment-aware methods ────────────────────────────────────────────────

    async fn record_access(&self, id: &str) -> Result<()> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        sqlx::query(
            "UPDATE memory_fragments \
             SET access_count = access_count + 1, last_accessed_at = ? \
             WHERE id = ?",
        )
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_lifecycle(&self, id: &str, lifecycle: MemoryLifecycle) -> Result<()> {
        sqlx::query("UPDATE memory_fragments SET lifecycle = ? WHERE id = ?")
            .bind(lifecycle_to_str(&lifecycle))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn search_by_segment(
        &self,
        segment: MemorySegment,
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let query = match profile_id {
            Some(_) => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE segment = ? AND profile_id = ? \
                 AND (lifecycle IS NULL OR lifecycle = 'active') \
                 ORDER BY created_at DESC LIMIT ?"
            ),
            None => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE segment = ? AND (lifecycle IS NULL OR lifecycle = 'active') \
                 ORDER BY created_at DESC LIMIT ?"
            ),
        };

        let seg = segment_to_str(&segment);
        let rows: Vec<FragmentRow> = match profile_id {
            Some(pid) => {
                sqlx::query_as(&query)
                    .bind(seg)
                    .bind(pid)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query_as(&query)
                    .bind(seg)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn search_scoreable(&self, profile_id: Option<&str>) -> Result<Vec<MemoryFragment>> {
        let query = match profile_id {
            Some(_) => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE (lifecycle IS NULL OR lifecycle = 'active') \
                 AND importance IS NOT NULL AND profile_id = ? \
                 ORDER BY created_at DESC LIMIT 1000"
            ),
            None => format!(
                "SELECT {SELECT_ALL} FROM memory_fragments \
                 WHERE (lifecycle IS NULL OR lifecycle = 'active') \
                 AND importance IS NOT NULL \
                 ORDER BY created_at DESC LIMIT 1000"
            ),
        };

        let rows: Vec<FragmentRow> = match profile_id {
            Some(pid) => {
                sqlx::query_as(&query)
                    .bind(pid)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => sqlx::query_as(&query).fetch_all(&self.pool).await?,
        };
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn batch_update_lifecycle(&self, updates: &[(String, MemoryLifecycle)]) -> Result<()> {
        for (id, lifecycle) in updates {
            self.update_lifecycle(id, lifecycle.clone()).await?;
        }
        Ok(())
    }

    async fn mark_superseded(&self, id: &str, superseded_by: &str) -> Result<()> {
        sqlx::query(
            "UPDATE memory_fragments SET lifecycle = 'merged', superseded_by = ? WHERE id = ?",
        )
        .bind(superseded_by)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── Audit log ───────────────────────────────────────────────────────────

    async fn log_event(
        &self,
        kind: MemoryEventKind,
        memory_id: &str,
        session_id: Option<&str>,
        data: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_events (event_kind, memory_id, session_id, data) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(kind.to_string())
        .bind(memory_id)
        .bind(session_id)
        .bind(data)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_events(&self, memory_id: Option<&str>, limit: usize) -> Result<Vec<MemoryEvent>> {
        let rows: Vec<EventRow> = match memory_id {
            Some(mid) => {
                sqlx::query_as(
                    "SELECT id, event_kind, memory_id, session_id, data, created_at \
                     FROM memory_events WHERE memory_id = ? \
                     ORDER BY created_at DESC, id DESC LIMIT ?",
                )
                .bind(mid)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT id, event_kind, memory_id, session_id, data, created_at \
                     FROM memory_events ORDER BY created_at DESC, id DESC LIMIT ?",
                )
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(row_to_event).collect())
    }

    async fn update_segment(
        &self,
        id: &str,
        segment: pond_core::domain::memory::MemorySegment,
        importance: f32,
    ) -> anyhow::Result<()> {
        let seg_str = format!("{:?}", segment).to_lowercase();
        sqlx::query("UPDATE memory_fragments SET segment = ?, importance = ? WHERE id = ?")
            .bind(&seg_str)
            .bind(importance as f64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn log_consolidation_run(
        &self,
        mode: &str,
        memory_count: usize,
        accepted: usize,
        rejected: usize,
        duration_ms: u64,
        details: Option<&str>,
    ) -> anyhow::Result<i64> {
        let row = sqlx::query_scalar::<_, i64>(
            "INSERT INTO consolidation_runs (mode, memory_count, accepted, rejected, duration_ms, details, completed_at)
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
             RETURNING id",
        )
        .bind(mode)
        .bind(memory_count as i64)
        .bind(accepted as i64)
        .bind(rejected as i64)
        .bind(duration_ms as i64)
        .bind(details)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }
}

// ── Event row helper ─────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    event_kind: String,
    memory_id: String,
    session_id: Option<String>,
    data: Option<String>,
    created_at: String,
}

fn parse_event_kind(s: &str) -> MemoryEventKind {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .unwrap_or(MemoryEventKind::Written)
}

fn row_to_event(row: EventRow) -> MemoryEvent {
    MemoryEvent {
        id: row.id,
        event_kind: parse_event_kind(&row.event_kind),
        memory_id: row.memory_id,
        session_id: row.session_id,
        data: row.data,
        created_at: row.created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn make_repo() -> (SqliteMemoryRepository, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteMemoryRepository::new(db.system), tmp)
    }

    #[tokio::test]
    async fn add_and_search_recent() {
        let (repo, _tmp) = make_repo().await;
        let frag =
            MemoryFragment::from_chat("f1".to_string(), None, None, "Hello from chat".to_string());
        repo.add(frag).await.unwrap();
        let results = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Hello from chat");
    }

    #[tokio::test]
    async fn delete_removes_fragment() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_chat("del1".to_string(), None, None, "bye".to_string());
        repo.add(frag).await.unwrap();
        repo.delete("del1").await.unwrap();
        assert!(repo.search_recent(None, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_similar_falls_back_to_recent_when_no_embeddings() {
        let (repo, _tmp) = make_repo().await;
        let frag =
            MemoryFragment::from_chat("f2".to_string(), None, None, "no embedding".to_string());
        repo.add(frag).await.unwrap();
        let query = vec![0.0f32; 4];
        let results = repo.search_similar(&query, None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_similar_ranks_by_cosine() {
        let (repo, _tmp) = make_repo().await;

        let mut high =
            MemoryFragment::from_chat("high".to_string(), None, None, "high sim".to_string());
        high.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);

        let mut low =
            MemoryFragment::from_chat("low".to_string(), None, None, "low sim".to_string());
        low.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);

        repo.add(high).await.unwrap();
        repo.add(low).await.unwrap();

        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = repo.search_similar(&query, None, 2).await.unwrap();
        assert_eq!(results[0].id, "high");
        assert_eq!(results[1].id, "low");
    }

    #[tokio::test]
    async fn add_with_segment_fields() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_extraction(
            "ext1".to_string(),
            None,
            "User prefers dark mode".to_string(),
            MemorySegment::Preference,
            0.75,
            None,
        );
        repo.add(frag).await.unwrap();
        let results = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].segment, Some(MemorySegment::Preference));
        assert!((results[0].importance.unwrap() - 0.75).abs() < 0.01);
        assert_eq!(results[0].tier, Some(MemoryTier::Long));
        assert_eq!(results[0].lifecycle, Some(MemoryLifecycle::Active));
    }

    #[tokio::test]
    async fn record_access_increments_count() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_extraction(
            "acc1".to_string(),
            None,
            "Test access".to_string(),
            MemorySegment::Knowledge,
            0.5,
            None,
        );
        repo.add(frag).await.unwrap();
        repo.record_access("acc1").await.unwrap();
        repo.record_access("acc1").await.unwrap();
        let results = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(results[0].access_count, 2);
        assert!(results[0].last_accessed_at.is_some());
    }

    #[tokio::test]
    async fn update_lifecycle_hides_from_search() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_extraction(
            "arch1".to_string(),
            None,
            "To archive".to_string(),
            MemorySegment::Context,
            0.2,
            None,
        );
        repo.add(frag).await.unwrap();
        repo.update_lifecycle("arch1", MemoryLifecycle::Archived)
            .await
            .unwrap();
        // Archived memories should not appear in search_recent
        let results = repo.search_recent(None, 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_by_content_finds_matching_keywords() {
        let (repo, _tmp) = make_repo().await;
        repo.add(MemoryFragment::from_extraction(
            "cat1".to_string(),
            None,
            "User loves cats".to_string(),
            MemorySegment::Preference,
            0.8,
            None,
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_extraction(
            "dog1".to_string(),
            None,
            "User has a dog named Rex".to_string(),
            MemorySegment::Knowledge,
            0.6,
            None,
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_extraction(
            "work1".to_string(),
            None,
            "User works at Jarida".to_string(),
            MemorySegment::Identity,
            0.85,
            None,
        ))
        .await
        .unwrap();

        // Search for "cats" — should find cat1
        let results = repo
            .search_by_content(&["cats".to_string()], None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "cat1");

        // Search for "dog" — should find dog1
        let results = repo
            .search_by_content(&["dog".to_string()], None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "dog1");

        // Search for "cats" + "dog" — should find both
        let results = repo
            .search_by_content(&["cats".to_string(), "dog".to_string()], None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        // Empty keywords — no results
        let results = repo.search_by_content(&[], None, 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_by_segment_filters() {
        let (repo, _tmp) = make_repo().await;
        repo.add(MemoryFragment::from_extraction(
            "id1".to_string(),
            None,
            "Name is Jerry".to_string(),
            MemorySegment::Identity,
            0.85,
            None,
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_extraction(
            "pref1".to_string(),
            None,
            "Likes dark mode".to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        ))
        .await
        .unwrap();

        let identities = repo
            .search_by_segment(MemorySegment::Identity, None, 10)
            .await
            .unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, "id1");
    }

    #[tokio::test]
    async fn log_and_get_events() {
        let (repo, _tmp) = make_repo().await;

        repo.log_event(MemoryEventKind::Extracted, "mem-1", Some("sess-1"), None)
            .await
            .unwrap();
        repo.log_event(MemoryEventKind::Written, "mem-1", None, Some("via MCP"))
            .await
            .unwrap();
        repo.log_event(MemoryEventKind::Recalled, "mem-2", None, None)
            .await
            .unwrap();

        // All events
        let all = repo.get_events(None, 100).await.unwrap();
        assert_eq!(all.len(), 3);

        // Filtered by memory_id
        let mem1_events = repo.get_events(Some("mem-1"), 100).await.unwrap();
        assert_eq!(mem1_events.len(), 2);
        // Both events should be for mem-1
        let kinds: Vec<_> = mem1_events.iter().map(|e| &e.event_kind).collect();
        assert!(kinds.contains(&&MemoryEventKind::Extracted));
        assert!(kinds.contains(&&MemoryEventKind::Written));
        // The extracted event should carry the session_id
        let extracted = mem1_events
            .iter()
            .find(|e| e.event_kind == MemoryEventKind::Extracted)
            .unwrap();
        assert_eq!(extracted.session_id.as_deref(), Some("sess-1"));
    }
}
