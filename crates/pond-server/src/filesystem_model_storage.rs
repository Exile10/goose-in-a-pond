//! Filesystem `ModelStorage` over pond-core's `model_layout`, the one on-disk model layout.

use std::path::{Path, PathBuf};

use pond_core::models::domain::model_layout;
use pond_core::models::domain::model_record::{BinaryRecord, ModelRecord};
use pond_core::models::ports::model_storage::ModelStorage;

pub struct FilesystemModelStorage {
    data_dir: PathBuf,
}

impl FilesystemModelStorage {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }
}

impl ModelStorage for FilesystemModelStorage {
    fn path_for(&self, record: &ModelRecord) -> Option<PathBuf> {
        model_layout::path_for(
            &self.data_dir,
            &record.category,
            record.filename.as_deref()?,
        )
    }

    fn binary_path(&self, record: &BinaryRecord) -> PathBuf {
        let filename = if cfg!(windows) {
            format!("{}.exe", record.name)
        } else {
            record.name.clone()
        };
        self.data_dir.join("bin").join(filename)
    }
}
