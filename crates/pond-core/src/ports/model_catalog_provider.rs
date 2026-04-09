//! Driven port: fetch the model + binary catalog from an upstream URL.
//!
//! The online catalog is a JSON document (served from `settings.model_registry_url`)
//! that lists all downloadable models and tool binaries.  Implementations live in
//! `pond-server` (HTTP fetch) and tests (in-memory mock).
//!
//! The port returns two vecs so the caller can upsert models into `ModelRepository`
//! and store binaries separately — keeping them out of the models table.

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::model_record::{BinaryRecord, ModelRecord};

/// Fetch a model + binary catalog from a remote source.
#[async_trait]
pub trait ModelCatalogProvider: Send + Sync {
    /// Download and parse the catalog at `url`.
    ///
    /// Returns `(models, binaries)` — `models` are ready for `ModelRepository::upsert()`.
    /// The implementation should be idempotent: calling twice with the same URL is safe.
    async fn fetch(&self, url: &str) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)>;
}
