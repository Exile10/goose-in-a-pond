//! Which separate vision encoder a chat model needs, from a table generated from Hugging Face
//! (`scripts/models/vision_pairings.py`) and refreshed only through a reviewed PR. It is compiled
//! in: the pond never fetches it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::Deserialize;

use super::vision_encoder::EncoderSpec;

const TABLE_JSONL: &str = include_str!("../../../data/vision-pairings.jsonl");

/// One known model family with its encoder.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VisionPairing {
    pub model_repo: String,
    /// Chat model files known to pair; may lag the repository until the next refresh.
    pub model_files: Vec<String>,
    /// `general.architecture` of the chat model.
    pub architecture: String,
    /// `{arch}.embedding_length` of the chat model; equals the encoder's `projection_dim`.
    pub embedding_length: u32,
    /// What the household reads. Never a file name.
    pub label: String,
    pub encoder: PairedEncoder,
    /// When the generator last read the repository, as `YYYY-MM-DD`.
    pub checked_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PairedEncoder {
    /// Directory under `models/mmproj/`, lowercase (the Orin's ext4 is case-sensitive).
    pub dir: String,
    pub repo: String,
    /// Pinned commit, so an upstream re-upload can't swap bytes under a validated install.
    pub revision: String,
    pub filename: String,
    pub size_bytes: u64,
    /// The file's sha256, its LFS oid on Hugging Face.
    pub sha256: String,
    /// `clip.vision.projector_type` the encoder's header must carry.
    pub projector: String,
    /// `clip.vision.projection_dim`.
    pub projection_dim: u32,
}

impl VisionPairing {
    /// The encoder as the validators read it, borrowing from the compiled-in table.
    pub fn spec(&'static self) -> EncoderSpec {
        EncoderSpec {
            dir: &self.encoder.dir,
            repo: &self.encoder.repo,
            revision: &self.encoder.revision,
            filename: &self.encoder.filename,
            size_bytes: self.encoder.size_bytes,
            sha256: &self.encoder.sha256,
            projector: &self.encoder.projector,
            projection_dim: self.encoder.projection_dim,
            label: &self.label,
        }
    }

    /// A quantisation-aware release: its encoder has the plain one's size, not its bytes.
    pub fn is_qat(&self) -> bool {
        self.model_repo.to_ascii_lowercase().contains("qat")
    }

    pub fn lists(&self, file_name: &str) -> bool {
        self.model_files.iter().any(|f| f == file_name)
    }
}

/// The table, parsed once. A line that does not parse is skipped here; the guard test fails
/// the build on one.
pub fn pairings() -> &'static [VisionPairing] {
    static TABLE: OnceLock<Vec<VisionPairing>> = OnceLock::new();
    TABLE.get_or_init(|| parse_table(TABLE_JSONL))
}

fn parse_table(text: &str) -> Vec<VisionPairing> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| match serde_json::from_str(line) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "skipping an unreadable vision pairing line");
                None
            }
        })
        .collect()
}

/// What is known about a chat model when its pairing is looked up.
#[derive(Debug, Clone, Copy, Default)]
pub struct PairingQuery<'a> {
    /// The Hugging Face `(repo, file)` it was acquired from.
    pub source: Option<(&'a str, &'a str)>,
    /// File names it goes by: on disk, and as the catalogue spells it.
    pub file_names: &'a [&'a str],
    /// `general.architecture` and `{arch}.embedding_length` from its own header.
    pub header: Option<(&'a str, u32)>,
}

/// The pairing for a chat model, biased to `None`: a false positive tells a blind model it can
/// see. In order: the exact source, a listed file name, then the header's architecture and
/// width with the qat flag from the name. Anything else has no pairing.
pub fn vision_pairing_for(q: &PairingQuery<'_>) -> Option<&'static VisionPairing> {
    let table = pairings();
    if let Some((repo, file)) = q.source {
        if let Some(p) = table.iter().find(|p| p.model_repo == repo && p.lists(file)) {
            return Some(p);
        }
    }
    let source_name = q.source.map(|(_, file)| file);
    let names: Vec<&str> = q.file_names.iter().copied().chain(source_name).collect();
    if names
        .iter()
        .any(|n| super::litert::is_litert_model(n) || super::taxonomy::is_companion_file(n))
    {
        return None;
    }
    if let Some(p) = names.iter().find_map(|n| table.iter().find(|p| p.lists(n))) {
        return Some(p);
    }
    let (arch, width) = q.header?;
    let qat = names.iter().any(|n| n.to_ascii_lowercase().contains("qat"));
    table.iter().find(|p| {
        p.architecture == arch
            && p.embedding_length == width
            && p.encoder.projection_dim == width
            && p.is_qat() == qat
    })
}

/// The file name a catalogue model name stands for: a bare stem gains `.gguf`.
pub fn gguf_file_name(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    if base.to_ascii_lowercase().ends_with(".gguf") {
        base.to_string()
    } else {
        format!("{base}.gguf")
    }
}

/// The pairing for `chat_model`, whose GGUF (if known) is at `gguf`. Reads the header only when
/// no name is listed.
pub fn pairing_for_model(chat_model: &str, gguf: Option<&Path>) -> Option<&'static VisionPairing> {
    if chat_model.trim().is_empty() || super::litert::is_litert_model(chat_model) {
        return None;
    }
    let by_name = gguf_file_name(chat_model);
    let on_disk = gguf
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|n| *n != by_name);
    let names: Vec<&str> = std::iter::once(by_name.as_str()).chain(on_disk).collect();
    let listed = vision_pairing_for(&PairingQuery {
        file_names: &names,
        ..PairingQuery::default()
    });
    if listed.is_some() {
        return listed;
    }
    let (arch, width) = gguf.and_then(header_facts)?;
    vision_pairing_for(&PairingQuery {
        file_names: &names,
        header: Some((arch.as_str(), width)),
        ..PairingQuery::default()
    })
}

/// The encoder `chat_model` needs; see [`pairing_for_model`].
pub fn encoder_for_model(chat_model: &str, gguf: Option<&Path>) -> Option<EncoderSpec> {
    pairing_for_model(chat_model, gguf).map(VisionPairing::spec)
}

/// The encoder for a file acquired from `repo`, by rules one and two.
pub fn encoder_for_source(repo: &str, file: &str) -> Option<EncoderSpec> {
    vision_pairing_for(&PairingQuery {
        source: Some((repo, file)),
        ..PairingQuery::default()
    })
    .map(VisionPairing::spec)
}

pub fn encoder_by_dir(dir: &str) -> Option<EncoderSpec> {
    pairings()
        .iter()
        .find(|p| p.encoder.dir == dir)
        .map(VisionPairing::spec)
}

/// The encoder a Hugging Face file is, at its pinned revision.
pub fn encoder_pinned(repo: &str, revision: &str, filename: &str) -> Option<EncoderSpec> {
    pairings()
        .iter()
        .find(|p| {
            p.encoder.repo == repo
                && p.encoder.revision == revision
                && p.encoder.filename == filename
        })
        .map(VisionPairing::spec)
}

/// `(architecture, embedding_length)` from a GGUF's header, cached per (path, length, mtime).
fn header_facts(path: &Path) -> Option<(String, u32)> {
    type Key = (PathBuf, u64, Option<SystemTime>);
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<(String, u32)>>>> = OnceLock::new();
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let key: Key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let facts = read_header_facts(path);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, facts.clone());
    }
    facts
}

fn read_header_facts(path: &Path) -> Option<(String, u32)> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(super::device_budget::HEAD_BYTES);
    std::fs::File::open(path)
        .ok()?
        .take(super::device_budget::HEAD_BYTES as u64)
        .read_to_end(&mut head)
        .ok()?;
    let info = super::gguf::parse_gguf_header(&head)?;
    Some((info.architecture?, info.embedding_length?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::curated;
    use crate::models::domain::engine::Engine;
    use crate::models::domain::gguf::test_gguf::{GgufWriter, BF16};

    fn hex(s: &str, n: usize) -> bool {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    /// Every line is a usable pin: a change here changes which bytes every pond trusts.
    #[test]
    fn the_embedded_table_is_complete_and_consistent() {
        let lines = TABLE_JSONL.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(lines > 0, "the pairing table is empty");
        assert_eq!(
            pairings().len(),
            lines,
            "a line of the table does not parse"
        );

        let mut files = std::collections::BTreeSet::new();
        let mut by_dir: HashMap<&str, &PairedEncoder> = HashMap::new();
        for p in pairings() {
            let e = &p.encoder;
            assert!(hex(&e.revision, 40), "{}: revision", p.model_repo);
            assert!(hex(&e.sha256, 64), "{}: sha256", p.model_repo);
            assert!(e.size_bytes > 0, "{}", p.model_repo);
            assert_eq!(
                e.projection_dim, p.embedding_length,
                "{}: the encoder must project into the model's width",
                p.model_repo
            );
            assert_eq!(e.dir, e.dir.to_ascii_lowercase(), "{}", p.model_repo);
            assert!(!p.label.contains(".gguf"), "{}: a label", p.model_repo);
            assert_eq!(p.checked_at.len(), 10, "{}: a date", p.model_repo);
            assert!(!p.model_files.is_empty(), "{}", p.model_repo);
            for f in &p.model_files {
                assert!(
                    files.insert((p.model_repo.as_str(), f.as_str())),
                    "{} {f} appears twice",
                    p.model_repo
                );
                assert!(
                    !crate::models::domain::taxonomy::is_companion_file(f),
                    "{f}"
                );
            }
            if let Some(other) = by_dir.insert(&e.dir, e) {
                assert_eq!(other, e, "{}: one dir, two encoders", e.dir);
            }
        }
    }

    /// Encoders already installed under `models/mmproj/<dir>/` must stay valid.
    #[test]
    fn the_first_lines_are_the_pins_ponds_already_installed() {
        #[rustfmt::skip]
        let want: [(&str, &str, &str, u64, &str, &str, u32); 5] = [
            ("gemma-4-e2b-it", "unsloth/gemma-4-E2B-it-GGUF",
             "0314792d7f1f7e229411f620751375812bb9faf2", 986_833_728,
             "a402f10fb5780bf91d03a10cd89061139f522bee2e679b1291bbfdcd71d9547d", "gemma4v", 1536),
            ("gemma-4-e2b-it-qat", "unsloth/gemma-4-E2B-it-qat-GGUF",
             "66a399f68ddd113b06dff02fca9523e55465d11d", 986_833_728,
             "38b33846f56426cd650e0e574d78de125abdfcedf35c0d7f6929f6ffe26efe02", "gemma4v", 1536),
            ("gemma-4-e4b-it", "unsloth/gemma-4-E4B-it-GGUF",
             "bfc15c382204943c3a8fff0c750b94ae2364d7a3", 991_552_320,
             "ee01cba03fd9c71ea2ea722225d24a84f72e7197714367e550ef705ef8851bc6", "gemma4v", 2560),
            ("gemma-4-e4b-it-qat", "unsloth/gemma-4-E4B-it-qat-GGUF",
             "8c5a9e4fd5482e2be20fe0bf013b4c262a8f4265", 991_552_320,
             "7c9bafa27f82d658eda805c1d82ef62bb0368e1ff75f64f77de58ad318beaaf9", "gemma4v", 2560),
            ("gemma-4-12b-it", "unsloth/gemma-4-12b-it-GGUF",
             "fc034cfff751157913579611efad8462ac1be606", 175_115_840,
             "2e269f906eb15169ee9ce880ea649bd6d42d4964c21f8ede10d0d0efc738bcbb", "gemma4uv", 3840),
        ];
        for (dir, repo, rev, size, sha, projector, dim) in want {
            let spec = encoder_by_dir(dir).unwrap_or_else(|| panic!("{dir} is gone"));
            assert_eq!(spec.repo, repo, "{dir}");
            assert_eq!(spec.revision, rev, "{dir}");
            assert_eq!(spec.filename, "mmproj-BF16.gguf", "{dir}");
            assert_eq!(spec.size_bytes, size, "{dir}");
            assert_eq!(spec.sha256, sha, "{dir}");
            assert_eq!(spec.projector, projector, "{dir}");
            assert_eq!(spec.projection_dim, dim, "{dir}");
        }
    }

    #[test]
    fn qat_and_plain_encoders_share_a_size_and_not_an_identity() {
        for family in ["gemma-4-e2b-it", "gemma-4-e4b-it"] {
            let plain = encoder_by_dir(family).unwrap();
            let qat = encoder_by_dir(&format!("{family}-qat")).unwrap();
            assert_eq!(plain.size_bytes, qat.size_bytes, "{family}");
            assert_ne!(plain.sha256, qat.sha256, "{family}");
            assert_ne!(plain.repo, qat.repo, "{family}");
        }
        let qat = encoder_for_model("gemma-4-E2B-it-qat-UD-Q4_K_XL", None).unwrap();
        let plain = encoder_for_model("gemma-4-E2B-it-Q4_K_M", None).unwrap();
        assert_ne!(qat.dir, plain.dir);
    }

    #[test]
    fn every_llama_cpp_pick_has_its_declared_pairing() {
        for pick in curated::CURATED
            .iter()
            .filter(|p| p.engine == Engine::LlamaCpp)
        {
            let spec = encoder_for_source(pick.repo, pick.filename)
                .unwrap_or_else(|| panic!("{} has no pairing", pick.filename));
            assert_eq!(Some(spec.dir), pick.pictures, "{}", pick.filename);
        }
        for pick in curated::CURATED
            .iter()
            .filter(|p| p.engine == Engine::LiteRtLm)
        {
            assert!(encoder_for_source(pick.repo, pick.filename).is_none());
        }
    }

    #[test]
    fn the_exact_source_wins_then_a_listed_file_name() {
        let qat = encoder_for_source(
            "unsloth/gemma-4-E4B-it-qat-GGUF",
            "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
        )
        .unwrap();
        assert_eq!(qat.dir, "gemma-4-e4b-it-qat");
        // Another publisher's copy of a listed file pairs by its name.
        let copy = encoder_for_source("someone/gemma-4-E4B-it-GGUF", "gemma-4-E4B-it-Q4_K_M.gguf");
        assert_eq!(copy.map(|s| s.dir), Some("gemma-4-e4b-it"));
        // A repository we know, a file we don't: no guess without a header.
        assert!(
            encoder_for_source("unsloth/gemma-4-E4B-it-GGUF", "gemma-4-E4B-it-Q9_K.gguf").is_none()
        );
    }

    #[test]
    fn the_names_households_use_find_their_rows() {
        let cases: &[(&str, Option<&str>)] = &[
            ("gemma-4-E2B-it-Q4_K_M", Some("gemma-4-e2b-it")),
            ("gemma-4-E2B-it-qat-UD-Q4_K_XL", Some("gemma-4-e2b-it-qat")),
            ("gemma-4-E4B-it-Q4_K_M", Some("gemma-4-e4b-it")),
            ("gemma-4-E4B-it-Q5_K_M.gguf", Some("gemma-4-e4b-it")),
            ("gemma-4-E4B-it-qat-UD-Q4_K_XL", Some("gemma-4-e4b-it-qat")),
            ("gemma-4-12b-it-IQ4_XS", Some("gemma-4-12b-it")),
            ("gemma-4-E4B-it-IQ4_XS", Some("gemma-4-e4b-it")),
            ("Llama-3.2-3B-Instruct-Q4_K_M", None),
            ("DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M", None),
            ("mtp-gemma-4-E2B-it", None),
            ("gemma-4-E2B-it-assistant-F16", None),
            ("gemma-4-E2B-it.litertlm", None),
            ("gemma-4-E4B-it.litertlm", None),
            // A bare family name names no file, and there is no header to read.
            ("gemma-4-E2B-it", None),
            ("", None),
        ];
        for (name, want) in cases {
            assert_eq!(
                encoder_for_model(name, None).map(|s| s.dir),
                *want,
                "encoder_for_model({name:?})"
            );
        }
    }

    fn chat_gguf(arch: &str, width: u32) -> Vec<u8> {
        GgufWriter::new()
            .str("general.architecture", arch)
            .u32(&format!("{arch}.embedding_length"), width)
            .tensor("token_embd.weight", &[4, 4], BF16)
            .build_complete()
    }

    #[test]
    fn a_file_no_list_names_pairs_by_its_header_and_its_qat_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = tmp.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let plain = write("my-e2b-finetune.gguf", &chat_gguf("gemma4", 1536));
        assert_eq!(
            encoder_for_model("my-e2b-finetune", Some(&plain)).map(|s| s.dir),
            Some("gemma-4-e2b-it")
        );
        let qat = write("my-e4b-qat-export.gguf", &chat_gguf("gemma4", 2560));
        assert_eq!(
            encoder_for_model("my-e4b-qat-export", Some(&qat)).map(|s| s.dir),
            Some("gemma-4-e4b-it-qat")
        );
        // The registry's canonical stem for a qat file is not listed; the header still pairs it.
        assert_eq!(
            encoder_for_model("gemma-4-E4B-it-qat", Some(&qat)).map(|s| s.dir),
            Some("gemma-4-e4b-it-qat")
        );
        // DeepSeek-R1-Distill-Qwen-1.5B shares E2B's width and is not Gemma.
        let qwen = write("qwen-1.5b.gguf", &chat_gguf("qwen2", 1536));
        assert!(encoder_for_model("qwen-1.5b", Some(&qwen)).is_none());
        // No qat 12B encoder is known, so a qat 12B reads text only.
        let qat12 = write("gemma-12b-qat.gguf", &chat_gguf("gemma4", 3840));
        assert!(encoder_for_model("gemma-12b-qat", Some(&qat12)).is_none());
        // A companion's file never pairs, whatever its header.
        let mtp = write("mtp-gemma-4-E2B-it.gguf", &chat_gguf("gemma4", 1536));
        assert!(encoder_for_model("mtp-gemma-4-E2B-it", Some(&mtp)).is_none());
    }

    #[test]
    fn encoder_files_are_pinned_by_the_table() {
        let spec = encoder_by_dir("gemma-4-e2b-it-qat").unwrap();
        assert_eq!(
            encoder_pinned(spec.repo, spec.revision, spec.filename),
            Some(spec)
        );
        assert_eq!(encoder_pinned(spec.repo, "main", spec.filename), None);
        assert_eq!(gguf_file_name("a/b/c-Q4_K_M"), "c-Q4_K_M.gguf");
        assert_eq!(gguf_file_name("c-Q4_K_M.gguf"), "c-Q4_K_M.gguf");
    }
}
