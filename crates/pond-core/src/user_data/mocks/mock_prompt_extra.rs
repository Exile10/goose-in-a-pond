//! In-memory mock implementation of `PromptExtraRepository`.

use crate::user_data::domain::prompt_extra::PromptExtra;
use crate::user_data::ports::prompt_extra::PromptExtraRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct MockPromptExtraRepository {
    extras: Arc<RwLock<Vec<PromptExtra>>>,
}

impl MockPromptExtraRepository {
    pub fn new() -> Self {
        Self {
            extras: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for MockPromptExtraRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PromptExtraRepository for MockPromptExtraRepository {
    async fn list_active(&self) -> Result<Vec<PromptExtra>> {
        let mut v: Vec<PromptExtra> = self
            .extras
            .read()
            .await
            .iter()
            .filter(|e| e.active)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.key.cmp(&b.key)));
        Ok(v)
    }

    async fn list_all(&self) -> Result<Vec<PromptExtra>> {
        let mut v = self.extras.read().await.clone();
        v.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.key.cmp(&b.key)));
        Ok(v)
    }

    async fn upsert(&self, extra: &PromptExtra) -> Result<()> {
        let mut store = self.extras.write().await;
        if let Some(existing) = store.iter_mut().find(|e| e.key == extra.key) {
            *existing = extra.clone();
        } else {
            store.push(extra.clone());
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.extras.write().await.retain(|e| e.key != key);
        Ok(())
    }
}
