//! Which helper model a chat model needs for speculative decoding.
//!
//! Speculative decoding runs a second, tiny model beside the chat model: it
//! proposes tokens, the chat model verifies them, and several can be accepted
//! per forward pass. Measured on a Jetson Orin Nano, that takes a real turn
//! from 31 to 49 tok/s.
//!
//! This mapping lives in the domain because two layers need it and neither
//! should own it: the server fetches the file, and the local-inference adapter
//! decides whether to point the engine at it. A copy in each would drift, and
//! the failure mode of a drifted copy is a drafter that downloads and is never
//! used.

/// The drafter that pairs with a given chat model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrafterSpec {
    /// Registry id, and the value `ModelSettings::draft_model` refers to.
    pub id: &'static str,
    pub repo: &'static str,
    pub filename: &'static str,
    pub approx_mb: u64,
}

/// The MTP drafter for `chat_model`, if one exists.
///
/// Matched on the model FAMILY rather than the full id, because one family
/// appears under many spellings -- `gemma-4-E2B-it`, `gemma-4-E2B-it-qat`,
/// `gemma-4-E2B-it-qat-UD-Q4_K_XL` -- while a drafter is tied to the
/// architecture and not to the quantisation.
///
/// The pairing is not interchangeable: an E4B drafter has a different hidden
/// size and cannot draft for an E2B target.
pub fn drafter_for(chat_model: &str) -> Option<DrafterSpec> {
    let m = chat_model.to_ascii_lowercase();
    if !m.contains("gemma-4") && !m.contains("gemma4") {
        return None;
    }
    // Gemma 4's drafters ship at the root of the same unsloth repositories the
    // quantised weights come from.
    if m.contains("e2b") {
        Some(DrafterSpec {
            id: "mtp-gemma-4-E2B-it",
            repo: "unsloth/gemma-4-E2B-it-qat-GGUF",
            filename: "mtp-gemma-4-E2B-it.gguf",
            approx_mb: 57,
        })
    } else if m.contains("e4b") {
        Some(DrafterSpec {
            id: "mtp-gemma-4-E4B-it",
            repo: "unsloth/gemma-4-E4B-it-qat-GGUF",
            filename: "mtp-gemma-4-E4B-it.gguf",
            approx_mb: 57,
        })
    } else {
        None
    }
}

/// Where the drafter's weights live under a pond's data directory.
pub fn drafter_path(data_dir: &std::path::Path, spec: &DrafterSpec) -> std::path::PathBuf {
    data_dir.join("models").join("gguf").join(spec.filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_family_decides_the_drafter_not_the_quantisation() {
        for id in [
            "gemma-4-E2B-it",
            "gemma-4-E2B-it-qat",
            "gemma-4-E2B-it-qat-UD-Q4_K_XL",
            "GEMMA-4-e2b-IT",
        ] {
            assert_eq!(drafter_for(id).unwrap().id, "mtp-gemma-4-E2B-it", "{id}");
        }
        assert_eq!(
            drafter_for("gemma-4-E4B-it-qat-UD-Q4_K_XL").unwrap().id,
            "mtp-gemma-4-E4B-it"
        );
    }

    #[test]
    fn a_model_with_no_drafter_gets_none() {
        for id in [
            "Llama-3.2-3B-Instruct",
            "gemma-4-12b-it",
            "granite-4.1-3b",
            "gemma-4-E5B-it",
        ] {
            assert!(drafter_for(id).is_none(), "{id}");
        }
    }

    #[test]
    fn the_two_families_never_share_a_drafter() {
        let e2b = drafter_for("gemma-4-E2B-it").unwrap();
        let e4b = drafter_for("gemma-4-E4B-it").unwrap();
        assert_ne!(e2b.id, e4b.id);
        assert_ne!(e2b.filename, e4b.filename);
    }
}
