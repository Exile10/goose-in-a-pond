//! Driven port: download a file from a URL to a local path.
//!
//! Decouples the download mechanism (HTTP, mocked in tests) from the callers
//! that need files on disk.  The implementation in `pond-server` uses `reqwest`
//! with live progress output.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

/// Download a file from a URL to disk.
#[async_trait]
pub trait ModelDownloader: Send + Sync {
    /// Download `url` to `dest`.
    ///
    /// - If `dest` already exists, this is a no-op (returns `Ok(())`).
    /// - `size_hint_mb` is used only for progress display; `0` is a valid value.
    /// - The parent directory of `dest` must already exist.
    async fn download(&self, url: &str, dest: &Path, size_hint_mb: u64) -> Result<()>;
}
