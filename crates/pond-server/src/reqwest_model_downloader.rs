//! Concrete `ModelDownloader` implementation that wraps `model_download::download_file`.
//!
//! This is the production downloader used in `auto_download_assigned_models` and at
//! server startup.  Tests use `pond_core::models::mocks::mock_model_downloader::MockModelDownloader`.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use pond_core::models::ports::model_downloader::ModelDownloader;

/// Downloads model files via reqwest with live progress output.
pub struct ReqwestModelDownloader;

#[async_trait]
impl ModelDownloader for ReqwestModelDownloader {
    async fn download(&self, url: &str, dest: &Path, size_hint_mb: u64) -> Result<()> {
        // Contract: no-op if file already exists.
        if dest.exists() {
            return Ok(());
        }
        crate::model_download::download_file(url, dest, size_hint_mb).await
    }
}
