//! Driven port: download a file from a URL to a local path, decoupling the mechanism (HTTP,
//! mocked in tests) from callers that need files on disk. The `pond-server` implementation uses
//! `reqwest` with live progress output.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

/// Download a file from a URL to disk.
#[async_trait]
pub trait ModelDownloader: Send + Sync {
    /// Download `url` to `dest`, a no-op if `dest` already exists. The parent directory must
    /// already exist. `size_hint_mb` drives progress display only, and `0` is valid.
    async fn download(&self, url: &str, dest: &Path, size_hint_mb: u64) -> Result<()>;
}
