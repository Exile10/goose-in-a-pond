//! Where each kind of model file lives under `<data_dir>/models`: the one mapping that storage,
//! the routes, the CLI and the clean-up all read. Pure paths; nothing here touches the disk.

use std::path::{Path, PathBuf};

use super::model_record::{ModelCategory, ModelRecord};

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

/// Whether two rows name one file: the same directory and a file name that is the same on a disk
/// that ignores case, so a row under another id or another spelling of it is found too.
pub fn share_a_file(a: &ModelRecord, b: &ModelRecord) -> bool {
    fn name(r: &ModelRecord) -> Option<&str> {
        r.filename.as_deref().and_then(file_name)
    }
    let root = Path::new("");
    match (
        name(a),
        name(b),
        dir_for(root, &a.category),
        dir_for(root, &b.category),
    ) {
        (Some(x), Some(y), Some(dx), Some(dy)) => dx == dy && x.eq_ignore_ascii_case(y),
        _ => false,
    }
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

    fn row(category: ModelCategory, id: &str, filename: Option<&str>) -> ModelRecord {
        let name = id.split_once('/').map_or(id, |(_, n)| n);
        ModelRecord {
            id: id.to_string(),
            category,
            name: name.to_string(),
            filename: filename.map(str::to_string),
            description: String::new(),
            size_mb: 0,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: true,
            is_custom: false,
        }
    }

    #[test]
    fn rows_share_a_file_by_directory_and_name_whatever_their_ids_and_case() {
        use ModelCategory::{Gguf, Litert, Ollama, Whisper};
        let a = row(Gguf, "gguf/gemma-4-e4b", Some("gemma-4-E4B-it-Q4_K_M.gguf"));
        let renamed = row(
            Gguf,
            "gguf/gemma-4-E4B-it-Q4_K_M",
            Some("gemma-4-E4B-it-Q4_K_M.gguf"),
        );
        let shouted = row(Gguf, "gguf/GEMMA", Some("GEMMA-4-E4B-IT-Q4_K_M.GGUF"));
        assert!(share_a_file(&a, &renamed));
        assert!(share_a_file(&a, &shouted), "a case-blind disk has one file");
        assert!(share_a_file(
            &a,
            &row(Gguf, "gguf/x", Some("sub/gemma-4-E4B-it-Q4_K_M.gguf"))
        ));
        assert!(!share_a_file(
            &a,
            &row(Gguf, "gguf/y", Some("gemma-4-E4B-it-Q8_0.gguf"))
        ));
        assert!(
            !share_a_file(
                &a,
                &row(Litert, "litert/z", Some("gemma-4-E4B-it-Q4_K_M.gguf"))
            ),
            "another directory is another file"
        );
        assert!(!share_a_file(
            &a,
            &row(Whisper, "whisper/w", Some("gemma-4-E4B-it-Q4_K_M.gguf"))
        ));
        let nameless = row(Ollama, "ollama/llama3.2", None);
        assert!(
            !share_a_file(&nameless, &nameless),
            "no file, nothing shared"
        );
        assert!(!share_a_file(&a, &row(Gguf, "gguf/none", None)));
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
