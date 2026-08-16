//! Filesystem implementation of `ModelStorage`.
//!
//! This is the **single place** that defines the on-disk directory layout
//! for all model categories and tool binaries under `data_dir`.
//!
//! | Category       | Directory                       |
//! |----------------|---------------------------------|
//! | Whisper        | `models/`                       |
//! | Llamafile      | `models/llm/`                   |
//! | Gguf           | `models/gguf/`                  |
//! | TtsPiper       | `models/tts/`                   |
//! | TtsHttp/Ollama | no local file (`path_for` → None)|
//! | Binaries       | `bin/`                          |

use std::path::{Path, PathBuf};

use pond_core::models::domain::model_record::{BinaryRecord, ModelCategory, ModelRecord};
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
        let filename = record.filename.as_deref()?;
        let path = match record.category {
            ModelCategory::Whisper => self.data_dir.join("models").join(filename),
            ModelCategory::Llamafile => {
                let base = self.data_dir.join("models").join("llm").join(filename);
                #[cfg(windows)]
                let base = PathBuf::from(format!("{}.exe", base.display()));
                base
            }
            ModelCategory::Gguf => self.data_dir.join("models").join("gguf").join(filename),
            ModelCategory::TtsPiper => self.data_dir.join("models").join("tts").join(filename),
            // Voices sit under the engine dir: they are useless without the
            // shared weights, and keeping them together means one directory
            // to check when TTS goes quiet.
            ModelCategory::TtsKokoro => self
                .data_dir
                .join("models")
                .join("kokoro")
                .join("voices")
                .join(filename),
            ModelCategory::Embedding => self
                .data_dir
                .join("models")
                .join("embedding")
                .join(filename),
            // Server-side or auto-downloaded: no local file to manage
            ModelCategory::TtsHttp | ModelCategory::Ollama => return None,
        };
        Some(path)
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
