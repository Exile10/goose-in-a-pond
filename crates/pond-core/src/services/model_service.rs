//! Model management service — the authoritative orchestrator for the model lifecycle.
//!
//! `ModelService` is the single entry-point for all model-related operations:
//! fetching the catalog, downloading files, assigning models to roles, and resolving
//! which model to use at runtime.  It has no knowledge of HTTP, SQLite, or file paths
//! — those concerns live in the port implementations wired in `pond-server`.
//!
//! # Typical startup sequence
//! ```text
//! 1. model_service.seed_catalog(url)     // first run: fetch + upsert catalog
//!    model_service.sync_disk_flags()     // subsequent runs: refresh downloaded flags
//! 2. model_service.model_for_role("asr") // find the assigned ASR model
//! 3. model_service.ensure_downloaded(id) // download if the file is missing
//! 4. pass the returned PathBuf to the subprocess launcher
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::domain::model_record::{BinaryRecord, ModelCategory, ModelRecord, ModelRoleAssignment};
use crate::ports::model_catalog_provider::ModelCatalogProvider;
use crate::ports::model_downloader::ModelDownloader;
use crate::ports::model_repository::ModelRepository;
use crate::ports::model_storage::ModelStorage;

pub struct ModelService {
    repo:       Arc<dyn ModelRepository>,
    catalog:    Arc<dyn ModelCatalogProvider>,
    downloader: Arc<dyn ModelDownloader>,
    storage:    Arc<dyn ModelStorage>,
}

impl ModelService {
    pub fn new(
        repo:       Arc<dyn ModelRepository>,
        catalog:    Arc<dyn ModelCatalogProvider>,
        downloader: Arc<dyn ModelDownloader>,
        storage:    Arc<dyn ModelStorage>,
    ) -> Self {
        Self { repo, catalog, downloader, storage }
    }

    // ── Catalog ───────────────────────────────────────────────────────────────

    /// Fetch the catalog from `url` and upsert all records into the repository.
    ///
    /// Use this on **first run** (empty DB).  Returns the number of models upserted.
    /// `is_custom=true` rows are never overwritten (enforced by `ModelRepository::upsert`).
    pub async fn seed_catalog(&self, url: &str) -> Result<usize> {
        let (models, _binaries) = self.catalog.fetch(url).await?;
        let count = models.len();
        for mut record in models {
            // Set downloaded flag from disk before upserting
            record.downloaded = self.storage.is_present(&record);
            self.repo.upsert(&record).await?;
        }
        Ok(count)
    }

    /// Fetch the catalog from `url` and refresh non-custom records.
    ///
    /// Same as `seed_catalog` but safe to call repeatedly — custom models are preserved.
    pub async fn refresh_catalog(&self, url: &str) -> Result<usize> {
        self.seed_catalog(url).await
    }

    /// Fetch tool binaries from the catalog URL without touching the model catalog.
    ///
    /// Returns the `BinaryRecord` list so the caller can download required binaries.
    pub async fn fetch_binaries(&self, url: &str) -> Result<Vec<BinaryRecord>> {
        let (_models, binaries) = self.catalog.fetch(url).await?;
        Ok(binaries)
    }

    // ── Disk sync ─────────────────────────────────────────────────────────────

    /// Walk all records in the repository and update `downloaded` flags from disk.
    ///
    /// Call on every startup after the first run to reflect files added/removed since
    /// the last session.  Returns the number of records whose flag changed.
    pub async fn sync_disk_flags(&self) -> Result<usize> {
        let records = self.repo.list_all().await?;
        let mut changed = 0usize;
        for record in &records {
            let on_disk = self.storage.is_present(record);
            if on_disk != record.downloaded {
                self.repo.set_downloaded(&record.id, on_disk).await?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    // ── Role assignments ──────────────────────────────────────────────────────

    /// Return the `ModelRecord` assigned to `role`, or `None` if no assignment exists.
    pub async fn model_for_role(&self, role: &str) -> Result<Option<ModelRecord>> {
        let assignment = self.repo.get_assignment(role).await?;
        match assignment {
            None    => Ok(None),
            Some(a) => self.repo.get_by_id(&a.model_id).await,
        }
    }

    /// Return all current role assignments.
    pub async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
        self.repo.list_assignments().await
    }

    /// Assign `model_id` to `role`.
    ///
    /// Validates that the model's category is compatible with the role
    /// (e.g. only LLM models can be assigned to "chat"/"think"/"task").
    pub async fn assign_role(&self, role: &str, model_id: &str) -> Result<()> {
        let record = self.repo.get_by_id(model_id).await?
            .ok_or_else(|| anyhow!("Model '{}' not found in catalog", model_id))?;

        if !ModelRoleAssignment::category_matches_role(&record.category, role) {
            return Err(anyhow!(
                "Model category '{}' is not valid for role '{}'",
                record.category.as_str(), role
            ));
        }

        self.repo.set_assignment(role, model_id).await
    }

    /// Remove the assignment for `role` (no-op if unset).
    pub async fn clear_role(&self, role: &str) -> Result<()> {
        self.repo.clear_assignment(role).await
    }

    // ── Download ──────────────────────────────────────────────────────────────

    /// Ensure the model file for `model_id` is on disk.
    ///
    /// - If the file already exists, returns its path immediately.
    /// - If the record has a `url`, downloads to the storage path.
    /// - If neither condition holds, returns an error.
    pub async fn ensure_downloaded(&self, model_id: &str) -> Result<PathBuf> {
        let record = self.repo.get_by_id(model_id).await?
            .ok_or_else(|| anyhow!("Model '{}' not found in catalog", model_id))?;

        let path = self.storage.path_for(&record)
            .ok_or_else(|| anyhow!("Model '{}' has no local file path (it is server-side only)", model_id))?;

        if path.exists() {
            return Ok(path);
        }

        let url = record.url.as_deref()
            .ok_or_else(|| anyhow!("Model '{}' has no download URL in the catalog", model_id))?;

        // Create parent directory
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.downloader.download(url, &path, record.size_mb).await?;
        self.repo.set_downloaded(model_id, true).await?;

        Ok(path)
    }

    /// Ensure a tool binary is on disk. Returns its path.
    pub async fn ensure_binary_downloaded(&self, record: &BinaryRecord) -> Result<PathBuf> {
        let path = self.storage.binary_path(record);
        if path.exists() {
            return Ok(path);
        }

        let url = record.url_for_current_platform()
            .ok_or_else(|| anyhow!(
                "Binary '{}' has no download URL for platform '{}'",
                record.name,
                BinaryRecord::current_platform_key()
            ))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.downloader.download(url, &path, 0).await?;

        // Make the binary executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            std::fs::set_permissions(&path, perms)?;
        }

        Ok(path)
    }

    // ── Listing ───────────────────────────────────────────────────────────────

    /// List all models, with `downloaded` flags refreshed from disk.
    pub async fn list_all(&self) -> Result<Vec<ModelRecord>> {
        self.repo.list_all().await
    }

    /// List models in a single category, with `downloaded` flags refreshed from disk.
    pub async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>> {
        self.repo.list_by_category(category).await
    }

    /// Look up a model by its stable id (`"{category}/{name}"`).
    pub async fn get(&self, model_id: &str) -> Result<Option<ModelRecord>> {
        self.repo.get_by_id(model_id).await
    }

    /// Register a user-added model (sets `is_custom = true`).
    pub async fn add_custom(&self, mut record: ModelRecord) -> Result<()> {
        record.is_custom = true;
        record.downloaded = self.storage.is_present(&record);
        self.repo.upsert(&record).await
    }

    /// Mark a model as downloaded (or not). Used by download progress handlers.
    pub async fn set_downloaded(&self, model_id: &str, downloaded: bool) -> Result<()> {
        self.repo.set_downloaded(model_id, downloaded).await
    }
}
