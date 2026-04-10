//! SQLite-backed implementation of `MemoryRepository`.
//!
//! Uses the `memory_fragments` table in `pond_system.db` (migration 0005_memory.sql).
//! Embeddings are stored as raw little-endian f32 BLOBs.
//! `search_similar()` loads all non-NULL embedding rows, deserializes, and cosine-ranks them.
//! O(n) is fine at home-assistant scale (hundreds of fragments, not millions).

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use pond_core::domain::memory::MemoryFragment;
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
    id:         String,
    profile_id: Option<String>,
    session_id: Option<String>,
    content:    String,
    embedding:  Option<Vec<u8>>,
    source:     String,
    tags:       String,
    created_at: String,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn row_to_fragment(row: FragmentRow) -> MemoryFragment {
    let tags: Vec<String> = serde_json::from_str(&row.tags).unwrap_or_default();
    let embedding = row.embedding.as_deref().map(blob_to_vec);
    MemoryFragment {
        id:         row.id,
        profile_id: row.profile_id,
        session_id: row.session_id,
        content:    row.content,
        embedding,
        source:     row.source,
        tags,
        created_at: parse_dt(&row.created_at),
    }
}

#[async_trait]
impl MemoryRepository for SqliteMemoryRepository {
    async fn add(&self, fragment: MemoryFragment) -> Result<()> {
        let tags_json = serde_json::to_string(&fragment.tags)?;
        let created_str = fragment.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let embedding_blob = fragment.embedding.as_deref().map(vec_to_blob);

        sqlx::query(
            "INSERT INTO memory_fragments \
             (id, profile_id, session_id, content, embedding, source, tags, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&fragment.id)
        .bind(&fragment.profile_id)
        .bind(&fragment.session_id)
        .bind(&fragment.content)
        .bind(embedding_blob)
        .bind(&fragment.source)
        .bind(&tags_json)
        .bind(&created_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn search_recent(
        &self,
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let rows: Vec<FragmentRow> = match profile_id {
            Some(pid) => sqlx::query_as(
                "SELECT id, profile_id, session_id, content, embedding, source, tags, created_at \
                 FROM memory_fragments WHERE profile_id = ? \
                 ORDER BY created_at DESC LIMIT ?",
            )
            .bind(pid)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?,
            None => sqlx::query_as(
                "SELECT id, profile_id, session_id, content, embedding, source, tags, created_at \
                 FROM memory_fragments ORDER BY created_at DESC LIMIT ?",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?,
        };
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        // Load all fragments that have stored embeddings
        let rows: Vec<FragmentRow> = match profile_id {
            Some(pid) => sqlx::query_as(
                "SELECT id, profile_id, session_id, content, embedding, source, tags, created_at \
                 FROM memory_fragments WHERE embedding IS NOT NULL AND profile_id = ?",
            )
            .bind(pid)
            .fetch_all(&self.pool)
            .await?,
            None => sqlx::query_as(
                "SELECT id, profile_id, session_id, content, embedding, source, tags, created_at \
                 FROM memory_fragments WHERE embedding IS NOT NULL",
            )
            .fetch_all(&self.pool)
            .await?,
        };

        if rows.is_empty() {
            // No embeddings stored yet — fall back to recency
            return self.search_recent(profile_id, limit).await;
        }

        // Compute cosine similarity and sort descending
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
        // Use profile_id=None — a real profile UUID would need to be inserted first
        let frag = MemoryFragment::from_chat(
            "f1".to_string(),
            None,
            None,
            "Hello from chat".to_string(),
        );
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
        let frag = MemoryFragment::from_chat("f2".to_string(), None, None, "no embedding".to_string());
        repo.add(frag).await.unwrap();
        let query = vec![0.0f32; 4];
        let results = repo.search_similar(&query, None, 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_similar_ranks_by_cosine() {
        let (repo, _tmp) = make_repo().await;

        let mut high = MemoryFragment::from_chat("high".to_string(), None, None, "high sim".to_string());
        high.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);

        let mut low = MemoryFragment::from_chat("low".to_string(), None, None, "low sim".to_string());
        low.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);

        repo.add(high).await.unwrap();
        repo.add(low).await.unwrap();

        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = repo.search_similar(&query, None, 2).await.unwrap();
        assert_eq!(results[0].id, "high");
        assert_eq!(results[1].id, "low");
    }
}
