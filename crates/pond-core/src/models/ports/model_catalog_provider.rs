//! Driven port: fetch the model + binary catalog from upstream sources.
//!
//! Implementations are source-specific and live in `pond-server`:
//! - `StaticModelCatalogProvider` — curated lists for Whisper, Piper, Llamafile, GGUF
//! - `OllamaCatalogProvider`       — queries the local Ollama instance (`/api/tags`)
//! - `CompositeModelCatalogProvider` — aggregates the above
//!
//! The port returns two vecs so the caller can upsert models into `ModelRepository`
//! and store binaries separately — keeping them out of the models table.

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
