//! Kokoro voice style vectors. Each voice is `voices/<name>.bin`: a raw little-endian `f32`
//! array of shape `[510, 256]` (522,240 bytes, no container). Row `n` is the style for `n`
//! phoneme tokens; the wrong row drifts the prosody rather than failing. Only the selected
//! voice is resident, since all 28 at once would be 14 MB to save a 522 KB read on a change.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Style-vector width the model expects.
pub const STYLE_DIM: usize = 256;
/// Rows in a voice file — one per possible token count.
pub const STYLE_ROWS: usize = 510;
/// Exact size of a well-formed voice file.
pub const VOICE_FILE_BYTES: usize = STYLE_ROWS * STYLE_DIM * 4;

/// One voice's length-conditioned style table.
#[derive(Debug, Clone)]
pub struct StyleTable {
    name: String,
    rows: Vec<f32>,
}

impl StyleTable {
    /// Read `<dir>/<name>.bin`.
    pub fn load(dir: &Path, name: &str) -> Result<Self> {
        let path = voice_path(dir, name)?;
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read Kokoro voice at {}", path.display()))?;
        Self::from_bytes(name, &bytes)
    }

    /// Parse raw little-endian f32 rows.
    pub fn from_bytes(name: &str, bytes: &[u8]) -> Result<Self> {
        if bytes.len() != VOICE_FILE_BYTES {
            return Err(anyhow!(
                "Kokoro voice {name:?} is {} bytes, expected {VOICE_FILE_BYTES} \
                 ({STYLE_ROWS}x{STYLE_DIM} f32) — wrong file or a truncated download",
                bytes.len()
            ));
        }
        let rows = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok(Self {
            name: name.to_string(),
            rows,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The style row for a sequence of `token_count` phoneme tokens. Clamped rather than
    /// checked: chunks are capped at 510 tokens upstream, and an out-of-range index should
    /// degrade prosody, never panic mid-speech.
    pub fn style_for(&self, token_count: usize) -> &[f32] {
        let row = token_count.min(STYLE_ROWS - 1);
        &self.rows[row * STYLE_DIM..(row + 1) * STYLE_DIM]
    }
}

/// Resolve a voice name to its file, rejecting anything that could escape `dir`.
///
/// Voice names reach this from settings and from the HTTP preview endpoint, so
/// they are untrusted input joined onto a path.
pub fn voice_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(anyhow!(
            "invalid Kokoro voice name {name:?} — expected letters, digits, _ or -"
        ));
    }
    Ok(dir.join(format!("{name}.bin")))
}

/// Every voice installed in `dir`, sorted. Missing dir reads as "none".
pub fn installed(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "bin").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(fill: impl Fn(usize) -> f32) -> StyleTable {
        let bytes: Vec<u8> = (0..STYLE_ROWS * STYLE_DIM)
            .flat_map(|i| fill(i).to_le_bytes())
            .collect();
        StyleTable::from_bytes("test", &bytes).unwrap()
    }

    #[test]
    fn rejects_a_wrong_sized_file() {
        let err = StyleTable::from_bytes("af_heart", &[0u8; 1024]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("1024"), "{msg}");
        assert!(msg.contains(&VOICE_FILE_BYTES.to_string()), "{msg}");
    }

    #[test]
    fn accepts_the_real_file_size() {
        assert_eq!(VOICE_FILE_BYTES, 522_240, "matches the shipped voice files");
        assert!(StyleTable::from_bytes("af_heart", &vec![0u8; VOICE_FILE_BYTES]).is_ok());
    }

    #[test]
    fn style_row_matches_token_count() {
        // Row n is filled with the value n, so the row index is readable back.
        let t = table(|i| (i / STYLE_DIM) as f32);
        assert_eq!(t.style_for(0)[0], 0.0);
        assert_eq!(t.style_for(37).len(), STYLE_DIM);
        assert!(t.style_for(37).iter().all(|v| *v == 37.0));
    }

    /// Prosody may degrade past the table; speaking must not stop.
    #[test]
    fn style_row_clamps_instead_of_panicking() {
        let t = table(|i| (i / STYLE_DIM) as f32);
        assert!(t
            .style_for(999_999)
            .iter()
            .all(|v| *v == (STYLE_ROWS - 1) as f32));
        assert_eq!(t.style_for(usize::MAX).len(), STYLE_DIM);
    }

    #[test]
    fn voice_names_cannot_escape_the_directory() {
        let dir = Path::new("/models/voices");
        assert!(voice_path(dir, "af_heart").is_ok());
        assert!(voice_path(dir, "am-michael").is_ok());
        for bad in ["../../etc/passwd", "af/heart", "", "af heart", "af.heart"] {
            assert!(voice_path(dir, bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn voice_path_is_the_name_plus_bin() {
        assert_eq!(
            voice_path(Path::new("/v"), "af_heart").unwrap(),
            PathBuf::from("/v/af_heart.bin")
        );
    }

    #[test]
    fn installed_is_empty_for_a_missing_directory() {
        assert!(installed(Path::new("/nope/definitely/not/here")).is_empty());
    }

    /// Sorted, and only `.bin` — the directory also holds the tokenizer and the
    /// engine weights, and neither is a voice.
    #[test]
    fn installed_lists_sorted_bin_stems_only() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "bm_george.bin",
            "af_heart.bin",
            "notes.txt",
            "model_quantized.onnx",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        assert_eq!(installed(dir.path()), vec!["af_heart", "bm_george"]);
    }
}
