//! PAI-2 P3, chokepoint 1: redact a fragment's content before it is stored.
//!
//! A decorator over the port, wrapping the single `memory_repo` construction
//! in `run_server`, so extraction, the `giap-memory` MCP tool, `POST /memories`
//! and consolidation are all covered without any of them knowing.
//!
//! **Every method is forwarded explicitly.** Eighteen of the twenty-two have
//! default bodies on the trait (`Ok(0)`, `vec![]`, `Ok(())`), so a decorator
//! that forgets one does not fail to compile — it silently answers the default
//! and the row never reaches SQLite. `every_memory_repository_method_is_forwarded`
//! is the guard.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::security::domain::event::PrivacySensitivity;
use crate::security::domain::redaction::RedactionLevel;
use crate::security::ports::redactor::Redactor;
use crate::user_data::domain::memory::{
    MemoryEdge, MemoryEvent, MemoryEventKind, MemoryFragment, MemoryGraph, MemoryLifecycle,
    MemorySegment,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::ports::memory_repository::MemoryRepository;

pub struct RedactingMemoryRepository {
    inner: Arc<dyn MemoryRepository + Send + Sync>,
    redactor: Arc<dyn Redactor>,
}

impl RedactingMemoryRepository {
    /// `Secrets`, not `Full`, and the choice is the whole product argument.
    /// A memory is read back into the model's context, and PAI-2 section 3.3's
    /// non-goal — the model is not blindfolded — dies if the assistant can
    /// never recall the user's own email address. A credential is different:
    /// it rotates, it is never worth recalling, and it is the one thing whose
    /// presence in a durable store is pure liability.
    pub const LEVEL: RedactionLevel = RedactionLevel::Secrets;

    pub fn new(
        inner: Arc<dyn MemoryRepository + Send + Sync>,
        redactor: Arc<dyn Redactor>,
    ) -> Self {
        Self { inner, redactor }
    }
}

#[async_trait]
impl MemoryRepository for RedactingMemoryRepository {
    async fn add(&self, mut fragment: MemoryFragment) -> Result<()> {
        let result = self.redactor.redact(&fragment.content, Self::LEVEL);
        if !result.findings.is_empty() {
            let kinds: Vec<&str> = result.findings.iter().map(|k| k.as_str()).collect();
            tracing::info!(
                target: "giap::trace",
                memory_id = %fragment.id,
                kinds = %kinds.join(","),
                "[redaction] removed credential-shaped material before storing a memory"
            );
            // The embedding was computed over the unredacted text, so it is a
            // durable derivative of the secret. Drop it; the startup backfill
            // re-embeds from `content`, which is now the redacted string.
            if result.highest_sensitivity() == Some(PrivacySensitivity::Secret) {
                fragment.embedding = None;
            }
        }
        fragment.content = result.text;
        self.inner.add(fragment).await
    }

    async fn search_recent(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.inner.search_recent(scope, limit).await
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.inner
            .search_similar(query_embedding, scope, limit)
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }

    async fn count_for_profile(&self, profile_id: &str) -> Result<u64> {
        self.inner.count_for_profile(profile_id).await
    }

    async fn search_unembedded(&self, limit: usize) -> Result<Vec<MemoryFragment>> {
        self.inner.search_unembedded(limit).await
    }

    async fn search_stale_dimension(
        &self,
        expected_dims: usize,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.inner
            .search_stale_dimension(expected_dims, limit)
            .await
    }

    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
        self.inner.update_embedding(id, embedding).await
    }

    async fn search_by_content(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.inner.search_by_content(keywords, scope, limit).await
    }

    async fn record_access(&self, id: &str) -> Result<()> {
        self.inner.record_access(id).await
    }

    /// Redacted before it is stored, exactly as `add` is.
    ///
    /// An edit is new text arriving from a person, so it goes through the same
    /// chokepoint the original did — forwarding it raw would let a secret enter
    /// by being typed into a correction, which is the one door `add` closes.
    /// The adapter clears the embedding, so the vector is rebuilt from the
    /// redacted words rather than describing what was removed.
    async fn update_content(&self, id: &str, content: &str) -> Result<()> {
        let result = self.redactor.redact(content, Self::LEVEL);
        if !result.findings.is_empty() {
            let kinds: Vec<&str> = result.findings.iter().map(|k| k.as_str()).collect();
            tracing::info!(
                target: "giap::trace",
                memory_id = %id,
                kinds = %kinds.join(","),
                "[redaction] removed credential-shaped material before storing a memory edit"
            );
        }
        // No embedding to drop here: the adapter clears it on every content
        // change, so the vector is always rebuilt from whatever text this call
        // stored — which is the redacted string.
        self.inner.update_content(id, &result.text).await
    }

    async fn update_lifecycle(&self, id: &str, lifecycle: MemoryLifecycle) -> Result<()> {
        self.inner.update_lifecycle(id, lifecycle).await
    }

    async fn search_by_segment(
        &self,
        segment: MemorySegment,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.inner.search_by_segment(segment, scope, limit).await
    }

    async fn search_scoreable(&self, scope: &ProfileScope) -> Result<Vec<MemoryFragment>> {
        self.inner.search_scoreable(scope).await
    }

    async fn batch_update_lifecycle(&self, updates: &[(String, MemoryLifecycle)]) -> Result<()> {
        self.inner.batch_update_lifecycle(updates).await
    }

    async fn mark_superseded(&self, id: &str, superseded_by: &str) -> Result<()> {
        self.inner.mark_superseded(id, superseded_by).await
    }

    async fn add_edge(&self, edge: MemoryEdge) -> Result<()> {
        self.inner.add_edge(edge).await
    }

    async fn get_edges_from(&self, memory_id: &str) -> Result<Vec<MemoryEdge>> {
        self.inner.get_edges_from(memory_id).await
    }

    async fn get_edges_to(&self, memory_id: &str) -> Result<Vec<MemoryEdge>> {
        self.inner.get_edges_to(memory_id).await
    }

    async fn get_subgraph(&self, root_ids: &[String], max_depth: u32) -> Result<MemoryGraph> {
        self.inner.get_subgraph(root_ids, max_depth).await
    }

    async fn log_event(
        &self,
        kind: MemoryEventKind,
        memory_id: &str,
        session_id: Option<&str>,
        data: Option<&str>,
    ) -> Result<()> {
        self.inner
            .log_event(kind, memory_id, session_id, data)
            .await
    }

    async fn get_events(&self, memory_id: Option<&str>, limit: usize) -> Result<Vec<MemoryEvent>> {
        self.inner.get_events(memory_id, limit).await
    }

    async fn update_segment(
        &self,
        id: &str,
        segment: MemorySegment,
        importance: f32,
    ) -> Result<()> {
        self.inner.update_segment(id, segment, importance).await
    }

    async fn log_consolidation_run(
        &self,
        mode: &str,
        memory_count: usize,
        accepted: usize,
        rejected: usize,
        duration_ms: u64,
        details: Option<&str>,
    ) -> Result<i64> {
        self.inner
            .log_consolidation_run(mode, memory_count, accepted, rejected, duration_ms, details)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::domain::redaction::RedactionKind;
    use crate::security::mocks::mock_redactor::MockRedactor;
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    fn frag(id: &str, content: &str) -> MemoryFragment {
        let mut f = MemoryFragment::from_chat(
            id.to_string(),
            None,
            Some("sess-1".to_string()),
            content.to_string(),
        );
        f.embedding = Some(vec![0.5_f32; 4]);
        f
    }

    fn wire() -> (
        Arc<MockMemoryRepository>,
        Arc<MockRedactor>,
        RedactingMemoryRepository,
    ) {
        let inner = Arc::new(MockMemoryRepository::new());
        let redactor = Arc::new(MockRedactor::replacing(KEY, RedactionKind::ApiKey));
        let repo = RedactingMemoryRepository::new(inner.clone(), redactor.clone());
        (inner, redactor, repo)
    }

    #[tokio::test]
    async fn a_credential_is_removed_before_storage() {
        let (inner, _r, repo) = wire();
        repo.add(frag("m1", &format!("my stripe key is {KEY} keep it")))
            .await
            .unwrap();

        let stored = inner
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1, "the fragment must still be stored");
        assert!(!stored[0].content.contains(KEY), "{}", stored[0].content);
        assert!(stored[0].content.contains("[redacted:api-key]"));
        assert!(stored[0].content.contains("keep it"), "prose was mangled");
        assert!(
            stored[0].embedding.is_none(),
            "an embedding computed over the secret is a durable derivative of it"
        );
    }

    #[tokio::test]
    async fn an_ordinary_memory_is_stored_byte_for_byte() {
        let (inner, _r, repo) = wire();
        let text = "Jerry prefers the lamp at 40 percent after 8pm";
        repo.add(frag("m2", text)).await.unwrap();

        let stored = inner
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored[0].content, text);
        assert!(
            stored[0].embedding.is_some(),
            "a memory with no finding must keep its vector"
        );
    }

    #[tokio::test]
    async fn the_level_is_secrets_not_full() {
        let (_i, redactor, repo) = wire();
        repo.add(frag("m3", "anything")).await.unwrap();
        let calls = redactor.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1,
            RedactionLevel::Secrets,
            "memory redacts credentials, not contact details -- see LEVEL's doc"
        );
    }

    #[tokio::test]
    async fn a_read_goes_straight_through() {
        let (inner, redactor, repo) = wire();
        inner.add(frag("m4", "stored directly")).await.unwrap();
        let got = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert!(
            redactor.calls().is_empty(),
            "reads must not be routed through the redactor"
        );
    }

    #[test]
    fn every_memory_repository_method_is_forwarded() {
        const PORT: &str = include_str!("../ports/memory_repository.rs");
        const SELF_SRC: &str = include_str!("redacting_memory_repository.rs");

        let names: Vec<&str> = PORT
            .lines()
            .filter_map(|l| l.trim().strip_prefix("async fn "))
            .filter_map(|rest| rest.split(['(', '<']).next())
            .collect();
        assert!(
            names.len() >= 20,
            "the parser found only {} methods on MemoryRepository -- it has \
             stopped matching, so this guard is asserting nothing",
            names.len()
        );

        let body = SELF_SRC
            .split("impl MemoryRepository for RedactingMemoryRepository")
            .nth(1)
            .expect("decorator impl block not found -- the guard cannot see it");

        for name in &names {
            assert!(
                body.contains(&format!("async fn {name}(")),
                "MemoryRepository::{name} is not forwarded by \
                 RedactingMemoryRepository; the trait's default implementation \
                 would silently take over and the row would never reach SQLite"
            );
        }
    }
}
