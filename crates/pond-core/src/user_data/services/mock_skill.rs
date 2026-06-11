//! In-memory mock implementation of `UserSkillRepository`.

use crate::domain::skill::UserSkill;
use crate::ports::skill::UserSkillRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct MockSkillRepository {
    skills: Arc<RwLock<Vec<UserSkill>>>,
}

impl MockSkillRepository {
    pub fn new() -> Self {
        Self {
            skills: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for MockSkillRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UserSkillRepository for MockSkillRepository {
    async fn list_active(&self) -> Result<Vec<UserSkill>> {
        let mut v: Vec<UserSkill> = self
            .skills
            .read()
            .await
            .iter()
            .filter(|s| s.active)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn list_all(&self) -> Result<Vec<UserSkill>> {
        let mut v = self.skills.read().await.clone();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn get(&self, id: &str) -> Result<Option<UserSkill>> {
        Ok(self
            .skills
            .read()
            .await
            .iter()
            .find(|s| s.id == id)
            .cloned())
    }

    async fn create(&self, skill: &UserSkill) -> Result<()> {
        self.skills.write().await.push(skill.clone());
        Ok(())
    }

    async fn update(&self, skill: &UserSkill) -> Result<()> {
        let mut store = self.skills.write().await;
        if let Some(existing) = store.iter_mut().find(|s| s.id == skill.id) {
            *existing = skill.clone();
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.skills.write().await.retain(|s| s.id != id);
        Ok(())
    }
}
