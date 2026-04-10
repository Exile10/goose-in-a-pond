//! Driven port: resolve on-disk paths for model files and tool binaries.
//!
//! This is the single place that knows the directory layout under `data_dir`.
//! The implementation in `pond-server` maps each `ModelCategory` to its
//! subdirectory.  Tests can use an in-memory stub.

use std::path::PathBuf;

use crate::domain::model_record::{BinaryRecord, ModelRecord};

/// Resolve on-disk locations for models and tool binaries.
pub trait ModelStorage: Send + Sync {
    /// Canonical on-disk path for a model file.
    ///
    /// Returns `None` for categories that have no local file
    /// (`ModelCategory::Ollama`, `ModelCategory::TtsHttp`).
    fn path_for(&self, record: &ModelRecord) -> Option<PathBuf>;

    /// True when `path_for(record)` exists on disk.
    fn is_present(&self, record: &ModelRecord) -> bool {
        self.path_for(record)
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    /// On-disk path for a tool binary (e.g. `whisper-server`, `piper`).
    fn binary_path(&self, record: &BinaryRecord) -> PathBuf;

    /// True when `binary_path(record)` exists on disk.
    fn binary_present(&self, record: &BinaryRecord) -> bool {
        self.binary_path(record).exists()
    }
}
