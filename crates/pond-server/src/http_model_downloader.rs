//! HTTP implementation of `ModelDownloader`.
//!
//! Wraps the existing `model_download::download_file()` helper so all download
//! logic stays in one place.  No model-specific knowledge — just URL + path + size.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use pond_core::ports::model_downloader::ModelDownloader;

pub struct HttpModelDownloader;

impl HttpModelDownloader {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ModelDownloader for HttpModelDownloader {
    async fn download(&self, url: &str, dest: &Path, size_hint_mb: u64) -> Result<()> {
        if dest.exists() {
            return Ok(()); // already on disk
        }
        crate::model_download::download_file(url, dest, size_hint_mb).await
    }
}
