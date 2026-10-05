//! Where each kind of model file lives under `<data_dir>/models`: the one mapping that storage,
//! the routes, the CLI and the clean-up all read. Pure paths; nothing here touches the disk.

use std::path::{Path, PathBuf};

use super::model_record::ModelCategory;

/// The directory a category's files live in; `None` for categories with no local file.
pub fn dir_for(data_dir: &Path, category: &ModelCategory) -> Option<PathBuf> {
    let models = data_dir.join("models");
    Some(match category {
        ModelCategory::Whisper => models,
        ModelCategory::Llamafile => models.join("llm"),
        ModelCategory::Gguf => models.join("gguf"),
        ModelCategory::Litert => super::litert::models_dir(data_dir),
        ModelCategory::TtsPiper => models.join("tts"),
        // Voices live under the engine dir: useless without its shared weights.
        ModelCategory::TtsKokoro => models.join("kokoro").join("voices"),
        ModelCategory::Embedding => models.join("embedding"),
        ModelCategory::TtsHttp | ModelCategory::Ollama => return None,
    })
}

/// A file name as stored: its last path component, so no name can reach outside its directory.
pub fn file_name(filename: &str) -> Option<&str> {
    let name = Path::new(filename).file_name()?.to_str()?;
    (!name.trim().is_empty()).then_some(name)
}

/// Where `filename` of `category` lives; `None` for a file-less category or a name with no
/// file part.
pub fn path_for(data_dir: &Path, category: &ModelCategory, filename: &str) -> Option<PathBuf> {
    let dir = dir_for(data_dir, category)?;
    let name = file_name(filename)?;
    if cfg!(windows) && *category == ModelCategory::Llamafile && !name.ends_with(".exe") {
        return Some(dir.join(format!("{name}.exe")));
    }
    Some(dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ModelCategory; 9] = [
        ModelCategory::Gguf,
        ModelCategory::Litert,
        ModelCategory::Llamafile,
        ModelCategory::Ollama,
        ModelCategory::Whisper,
        ModelCategory::TtsPiper,
        ModelCategory::TtsKokoro,
        ModelCategory::TtsHttp,
        ModelCategory::Embedding,
    ];

    #[test]
    fn every_file_category_has_its_directory_and_the_rest_have_none() {
        let dd = Path::new("/pond");
        let want = [
            (ModelCategory::Gguf, Some("/pond/models/gguf")),
            (ModelCategory::Litert, Some("/pond/models/litertlm")),
            (ModelCategory::Llamafile, Some("/pond/models/llm")),
            (ModelCategory::Ollama, None),
            (ModelCategory::Whisper, Some("/pond/models")),
            (ModelCategory::TtsPiper, Some("/pond/models/tts")),
            (ModelCategory::TtsKokoro, Some("/pond/models/kokoro/voices")),
            (ModelCategory::TtsHttp, None),
            (ModelCategory::Embedding, Some("/pond/models/embedding")),
        ];
        assert_eq!(want.len(), ALL.len());
        for (category, dir) in want {
            assert_eq!(
                dir_for(dd, &category),
                dir.map(PathBuf::from),
                "{}",
                category.as_str()
            );
        }
    }

    #[test]
    fn a_name_cannot_leave_its_directory() {
        let dd = Path::new("/pond");
        assert_eq!(
            path_for(dd, &ModelCategory::Gguf, "../../etc/x.gguf"),
            Some(PathBuf::from("/pond/models/gguf/x.gguf"))
        );
        assert_eq!(
            path_for(dd, &ModelCategory::Litert, "/abs/elsewhere/y.litertlm"),
            Some(PathBuf::from("/pond/models/litertlm/y.litertlm"))
        );
        assert_eq!(path_for(dd, &ModelCategory::Gguf, ".."), None);
        assert_eq!(path_for(dd, &ModelCategory::Gguf, ""), None);
        assert_eq!(path_for(dd, &ModelCategory::Ollama, "llama3.2"), None);
    }

    #[test]
    fn litert_files_go_where_the_engine_reads_them() {
        let dd = Path::new("/pond");
        assert_eq!(
            path_for(dd, &ModelCategory::Litert, "gemma-4-E2B-it.litertlm"),
            Some(super::super::litert::model_path(
                dd,
                "gemma-4-E2B-it.litertlm"
            ))
        );
    }
}
