//! In-memory mock implementation of `PromptTemplateRepository`.

use crate::user_data::domain::prompt_template::PromptTemplate;
use crate::user_data::ports::prompt_template::PromptTemplateRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct MockPromptTemplateRepository {
    templates: Arc<RwLock<Vec<PromptTemplate>>>,
}

impl MockPromptTemplateRepository {
    pub fn new() -> Self {
        Self {
            templates: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for MockPromptTemplateRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PromptTemplateRepository for MockPromptTemplateRepository {
    async fn get(&self, name: &str) -> Result<Option<PromptTemplate>> {
        Ok(self
            .templates
            .read()
            .await
            .iter()
            .find(|t| t.name == name)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<PromptTemplate>> {
        let mut v = self.templates.read().await.clone();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn upsert(&self, template: &PromptTemplate) -> Result<()> {
        let mut store = self.templates.write().await;
        if let Some(existing) = store.iter_mut().find(|t| t.name == template.name) {
            *existing = template.clone();
        } else {
            store.push(template.clone());
        }
        Ok(())
    }

    async fn seed_system_template(&self, template: &PromptTemplate) -> Result<()> {
        let mut store = self.templates.write().await;
        match store.iter_mut().find(|t| t.name == template.name) {
            Some(existing) if existing.is_customized => {}
            Some(existing) => {
                *existing = PromptTemplate {
                    is_customized: false,
                    ..template.clone()
                };
            }
            None => store.push(PromptTemplate {
                is_customized: false,
                ..template.clone()
            }),
        }
        Ok(())
    }

    async fn insert_if_absent(&self, template: &PromptTemplate) -> Result<bool> {
        let mut store = self.templates.write().await;
        if store.iter().any(|t| t.name == template.name) {
            return Ok(false);
        }
        store.push(template.clone());
        Ok(true)
    }

    async fn delete(&self, name: &str) -> Result<()> {
        self.templates.write().await.retain(|t| t.name != name);
        Ok(())
    }
}
