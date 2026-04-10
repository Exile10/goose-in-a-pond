//! Server startup helpers that are also reachable from integration tests.
//!
//! Exposing these through the crate's library target (lib.rs) lets integration
//! tests in `tests/` import them without duplicating code.

use std::sync::Arc;

use pond_core::domain::model_record::ModelCategory;
use pond_core::ports::model_downloader::ModelDownloader;
use pond_core::ports::model_repository::ModelRepository;
use pond_core::ports::model_storage::ModelStorage;

/// Background task: download any role-assigned model whose file is missing from disk.
///
/// Called once at server startup (via `tokio::spawn`) after `sync_assignments_to_settings`.
/// Runs non-blocking so the HTTP server is available immediately while models download.
///
/// # Logic
/// 1. Read all current role → model_id assignments from the repository.
/// 2. For each assignment fetch the `ModelRecord`.
/// 3. Skip models with no local file (`Ollama`, `TtsHttp`).
/// 4. If the file is present on disk but `downloaded` flag is false → fix the DB flag.
/// 5. If the file is absent → download it; on success flip `downloaded = true`.
///
/// Returns the number of downloads triggered (useful for tests and status logs).
pub async fn auto_download_assigned_models(
    repo:       Arc<dyn ModelRepository + Send + Sync>,
    storage:    Arc<dyn ModelStorage + Send + Sync>,
    downloader: Arc<dyn ModelDownloader + Send + Sync>,
) -> usize {
    let assignments = match repo.list_assignments().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("auto_download: failed to read assignments: {e}");
            return 0;
        }
    };

    let mut triggered = 0usize;

    for a in &assignments {
        let record = match repo.get_by_id(&a.model_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(
                    "auto_download: model '{}' in assignment but not in catalog — skipping",
                    a.model_id
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("auto_download: DB error fetching '{}': {e}", a.model_id);
                continue;
            }
        };

        // Server-side models need no local file — skip silently.
        if matches!(record.category, ModelCategory::Ollama | ModelCategory::TtsHttp) {
            continue;
        }

        let on_disk = storage.is_present(&record);

        // DB drift: file on disk but flag is false — correct without re-downloading.
        if on_disk && !record.downloaded {
            if let Err(e) = repo.set_downloaded(&record.id, true).await {
                tracing::warn!("auto_download: failed to fix DB flag for '{}': {e}", record.id);
            } else {
                tracing::info!(
                    "auto_download: corrected downloaded flag for '{}' (file already on disk)",
                    record.id
                );
            }
            continue;
        }

        // File present and flag correct — nothing to do.
        if on_disk {
            continue;
        }

        // File absent — need to download.
        let url = match record.url.as_deref() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => {
                tracing::warn!(
                    "auto_download: model '{}' (role '{}') has no download URL — cannot auto-download",
                    record.id, a.role
                );
                continue;
            }
        };

        let path = match storage.path_for(&record) {
            Some(p) => p,
            None => {
                tracing::warn!("auto_download: no local path for '{}' — skipping", record.id);
                continue;
            }
        };

        tracing::info!(
            "auto_download: '{}' assigned to role '{}' but not on disk — downloading…",
            record.id, a.role
        );

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                tracing::warn!(
                    "auto_download: could not create dir {}: {e}",
                    parent.display()
                );
                continue;
            }
        }

        match downloader.download(&url, &path, record.size_mb).await {
            Ok(()) => {
                if let Err(e) = repo.set_downloaded(&record.id, true).await {
                    tracing::warn!(
                        "auto_download: downloaded '{}' but failed to update DB: {e}",
                        record.id
                    );
                } else {
                    tracing::info!("auto_download: '{}' ready at {}", record.id, path.display());
                    triggered += 1;
                }
            }
            Err(e) => {
                tracing::warn!("auto_download: failed to download '{}': {e}", record.id);
            }
        }
    }

    triggered
}
