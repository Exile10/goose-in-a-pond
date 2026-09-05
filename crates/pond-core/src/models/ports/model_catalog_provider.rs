//! Driven port: fetch the model and binary catalog from upstream sources. Implementations live in
//! `pond-server`: `StaticModelCatalogProvider` (curated lists), `OllamaCatalogProvider` (the local
//! instance's `/api/tags`) and `CompositeModelCatalogProvider`. Two vecs come back so the caller
//! upserts models into `ModelRepository` and stores binaries out of the models table.

use anyhow::Result;
use async_trait::async_trait;

use crate::models::domain::model_record::{BinaryRecord, ModelRecord};

/// Fetch a model + binary catalog from upstream sources.
#[async_trait]
pub trait ModelCatalogProvider: Send + Sync {
    /// Fetch and return `(models, binaries)`.
    ///
    /// `models` are ready for `ModelRepository::upsert()`.
    /// The implementation should be idempotent: calling twice is safe.
    async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)>;
}
