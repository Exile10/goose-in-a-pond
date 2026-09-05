//! Vision-encoder (mmproj) resolution for GIAP-registered GGUF models. The engine enables image
//! input only when a registry entry has `mmproj_path` set; goose's featured lookup never matches
//! the bare stem `register_gguf_model` registers, so this module fetches the encoder into a
//! GIAP-owned directory and stamps the entry. The stamp is re-read on every `Provider::stream`.

use goose::providers::local_inference::local_model_registry::{
    get_registry, MmprojSpec, FEATURED_MODELS,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Directory holding downloaded vision encoders. Kept outside `models/gguf/` so the `*.gguf`
/// scans in `resolve_gguf_filename` and `canonical_model_stem` never meet an encoder file. The
/// leaf is the NORMALISED name: registration passes the collapsed stem while readiness checks
/// pass the settings spelling, and both must land on one directory or the encoder downloads twice.
#[must_use]
pub fn mmproj_dir(data_dir: &Path, model_name: &str) -> PathBuf {
    data_dir
        .join("models")
        .join("mmproj")
        .join(normalize_model_name(model_name))
}

/// The featured vision-encoder spec for a GIAP registry stem, if the model declares one.
/// Matching is by normalised name, not repo id: the owner prefix, a trailing `-GGUF` and any
/// quant suffix are stripped from both sides before a case-insensitive comparison.
#[must_use]
pub fn featured_mmproj_for_stem(stem: &str) -> Option<&'static MmprojSpec> {
    let wanted = normalize_model_name(stem);
    FEATURED_MODELS.iter().find_map(|m| {
        let spec = m.mmproj.as_ref()?;
        // `spec` is "owner/repo-GGUF:QUANT".
        let repo = m.spec.split(':').next().unwrap_or(m.spec);
        (normalize_model_name(repo) == wanted).then_some(spec)
    })
}

/// Collapse a model spelling to a comparable key: drops the HF owner, a trailing `-GGUF`, a quant
/// suffix in either spelling (`:Q4_K_M` or `-Q4_K_M`), a `.gguf` extension, and case. `E2B`, `E4B`
/// and `E1B` (which has NO encoder) stay distinct. Unlike `canonical_model_stem`, the dash-quant
/// strip is unconditional because the encoder is a property of the family, not the quant.
fn normalize_model_name(raw: &str) -> String {
    let no_owner = raw.rsplit('/').next().unwrap_or(raw);
    let no_colon_quant = no_owner.split(':').next().unwrap_or(no_owner);
    let no_ext = no_colon_quant.trim_end_matches(".gguf");
    let lower = no_ext.to_ascii_lowercase();
    let no_repo_suffix = lower.trim_end_matches("-gguf");
    match no_repo_suffix.rsplit_once(['-', '.']) {
        Some((base, tag))
            if !base.is_empty()
                && crate::goose_agent::looks_like_quant_tag(&tag.to_uppercase()) =>
        {
            base.to_string()
        }
        _ => no_repo_suffix.to_string(),
    }
}

/// `true` when the model declares a vision encoder, whether or not the bytes are on disk yet.
/// The UI should gate its attach affordance on this rather than on a ~1 GB download having
/// finished; the turn itself checks for the bytes via [`mmproj_ready`] and says what is missing.
#[must_use]
pub fn declares_vision(model_name: &str) -> bool {
    featured_mmproj_for_stem(model_name).is_some()
}

/// `true` when the encoder bytes are present, i.e. the next turn can actually
/// look at an image.
#[must_use]
pub fn mmproj_ready(data_dir: &Path, stem: &str) -> bool {
    resolved_mmproj_path(data_dir, stem).is_some()
}

/// The on-disk encoder for a stem, if already downloaded. GIAP's own directory is checked first,
/// then the path goose's model manager would have used, so an encoder is never fetched twice.
#[must_use]
pub fn resolved_mmproj_path(data_dir: &Path, stem: &str) -> Option<PathBuf> {
    let spec = featured_mmproj_for_stem(stem)?;
    let giap = mmproj_dir(data_dir, stem).join(spec.filename);
    if is_nonempty_file(&giap) {
        return Some(giap);
    }
    let goose_owned = spec.local_path();
    if is_nonempty_file(&goose_owned) {
        return Some(goose_owned);
    }
    None
}

fn is_nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Stamp a resolved encoder onto the registry entry for `stem`.
///
/// Idempotent, and cheap enough to call on every provider build. Returns `true`
/// when the entry ended up with an encoder attached.
pub fn stamp_registry_entry(data_dir: &Path, stem: &str) -> bool {
    let Some(path) = resolved_mmproj_path(data_dir, stem) else {
        return false;
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let Ok(mut registry) = get_registry().lock() else {
        tracing::warn!("GGUF registry lock poisoned; cannot attach vision encoder");
        return false;
    };
    let Some(entry) = registry.get_model(stem) else {
        return false;
    };
    if entry.mmproj_path.as_deref() == Some(path.as_path()) && entry.mmproj_size_bytes == size {
        return true; // already stamped
    }

    let mut updated = entry.clone();
    updated.mmproj_path = Some(path.clone());
    updated.mmproj_size_bytes = size;
    updated.mmproj_checked = true;
    updated.settings.vision_capable = true;
    // The engine subtracts this from its context-memory budget before sizing the
    // KV cache; leaving it at zero would over-allocate context and then OOM when
    // the encoder loads.
    updated.settings.mmproj_size_bytes = size;

    match registry.add_model(updated) {
        Ok(_) => {
            tracing::info!(
                model = %stem,
                mmproj = %path.display(),
                size_mb = size / (1024 * 1024),
                "vision encoder attached; image input is available for this model"
            );
            true
        }
        Err(e) => {
            tracing::warn!(model = %stem, error = %e, "could not attach vision encoder");
            false
        }
    }
}

/// Stems whose encoder download is already running, so repeated provider builds
/// do not start the same ~1 GB transfer several times over.
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Ensure the vision encoder for `stem` is available, downloading it in the background if not.
/// Returns immediately: this runs inside a provider build on the model-switch path, and blocking
/// on a ~1 GB transfer would stall the first chat for minutes. The turn reports what is missing
/// via [`mmproj_ready`]; the engine re-reads the registry per generation, so the stamp lands live.
pub fn ensure_mmproj_available(data_dir: &Path, stem: &str) {
    let Some(spec) = featured_mmproj_for_stem(stem) else {
        return; // text-only model, nothing to fetch
    };
    if stamp_registry_entry(data_dir, stem) {
        return; // already on disk
    }

    {
        let mut guard = in_flight().lock().unwrap_or_else(|e| e.into_inner());
        if !guard.insert(stem.to_string()) {
            return; // a download for this stem is already running
        }
    }

    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        spec.repo, spec.filename
    );
    let dest_dir = mmproj_dir(data_dir, stem);
    let dest = dest_dir.join(spec.filename);
    let stem_owned = stem.to_string();
    let data_dir_owned = data_dir.to_path_buf();

    tokio::spawn(async move {
        tracing::info!(
            model = %stem_owned,
            url = %url,
            "downloading vision encoder in the background; image input becomes available when it completes"
        );
        let outcome = download_to(&url, &dest_dir, &dest).await;
        match outcome {
            Ok(bytes) => {
                tracing::info!(
                    model = %stem_owned,
                    size_mb = bytes / (1024 * 1024),
                    "vision encoder downloaded"
                );
                stamp_registry_entry(&data_dir_owned, &stem_owned);
            }
            Err(e) => tracing::warn!(
                model = %stem_owned,
                error = %e,
                "vision encoder download failed; this model stays text-only until it succeeds"
            ),
        }
        in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&stem_owned);
    });
}

/// Stream a URL to `dest`, via a `.part` file so an interrupted transfer can
/// never be mistaken for a complete encoder.
async fn download_to(url: &str, dir: &Path, dest: &Path) -> anyhow::Result<u64> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    tokio::fs::create_dir_all(dir).await?;
    let part = dest.with_extension("part");

    // PAI-2 P6a: the single egress gate for every encoder fetch. It runs inside a spawned task,
    // so `record_egress`'s `tokio::spawn` has a runtime. The caller logs a failed download and
    // leaves the model text-only, so a refusal degrades rather than breaks.
    let call = pond_core::shared::services::egress::begin(url, "GET")?;
    let sent = reqwest::Client::builder()
        // A ~1 GB transfer on a slow home link must not trip a default timeout;
        // the read timeout below is what catches a genuinely dead connection.
        .read_timeout(std::time::Duration::from_secs(120))
        .build()?
        .get(url)
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent?.error_for_status()?;

    let mut file = tokio::fs::File::create(&part).await?;
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&part, dest).await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_stem_resolves_to_its_featured_encoder() {
        // This is the exact spelling GooseAdapter registers.
        let spec = featured_mmproj_for_stem("gemma-4-E2B-it").expect("E2B declares an encoder");
        assert_eq!(spec.repo, "unsloth/gemma-4-E2B-it-GGUF");
        assert_eq!(spec.filename, "mmproj-BF16.gguf");
    }

    /// Both quant spellings must resolve: `Settings` stores `chat_model = "gemma-4-E2B-it-Q4_K_M"`
    /// while the registry key is the collapsed stem `gemma-4-E2B-it`, and both spellings reach
    /// this module (readiness and capability checks use the settings one, registration the stem).
    #[test]
    fn both_quant_spellings_resolve() {
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it-Q4_K_M").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it:Q4_K_M").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it-Q4_K_M.gguf").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E4B-it-Q5_K_M").is_some());
    }

    /// A quant strip must not eat a real name segment.
    #[test]
    fn a_non_quant_trailing_segment_is_kept() {
        // "it" is not a quant tag, so the stem survives intact and still matches.
        assert_eq!(normalize_model_name("gemma-4-E2B-it"), "gemma-4-e2b-it");
        // A hypothetical model whose last segment merely starts with Q but has no
        // digit after it is not treated as a quant.
        assert_eq!(normalize_model_name("some-model-Queen"), "some-model-queen");
    }

    #[test]
    fn the_full_featured_repo_spelling_also_resolves() {
        assert!(featured_mmproj_for_stem("unsloth/gemma-4-E2B-it-GGUF").is_some());
        assert!(featured_mmproj_for_stem("unsloth/gemma-4-E2B-it-GGUF:Q4_K_M").is_some());
    }

    /// The whole reason a name heuristic is not good enough: E1B matches
    /// `gemma-4` but ships no vision encoder.
    #[test]
    fn e1b_declares_no_vision_even_though_it_is_a_gemma_4() {
        assert!(!declares_vision("gemma-4-E1B-it"));
        assert!(declares_vision("gemma-4-E2B-it"));
        assert!(declares_vision("gemma-4-E4B-it"));
    }

    #[test]
    fn text_only_families_declare_no_vision() {
        assert!(!declares_vision("Llama-3.2-3B-Instruct"));
        assert!(!declares_vision("Hermes-2-Pro-Mistral-7B"));
        assert!(!declares_vision("something-nobody-has-heard-of"));
    }

    #[test]
    fn every_vision_capable_featured_model_is_reachable_by_its_stem() {
        for m in FEATURED_MODELS.iter().filter(|m| m.mmproj.is_some()) {
            let repo = m.spec.split(':').next().unwrap();
            let stem = repo
                .rsplit('/')
                .next()
                .unwrap()
                .trim_end_matches("-GGUF")
                .to_string();
            assert!(
                declares_vision(&stem),
                "featured vision model {stem} is not reachable by its bare stem"
            );
        }
    }

    #[test]
    fn encoders_live_outside_the_weights_directory() {
        let dir = mmproj_dir(Path::new("/data"), "gemma-4-E2B-it");
        assert_eq!(
            dir,
            Path::new("/data/models/mmproj/gemma-4-e2b-it"),
            "an mmproj inside models/gguf would confuse resolve_gguf_filename"
        );
    }

    /// Every spelling of one model must resolve to ONE encoder directory,
    /// otherwise the registration path and the readiness check disagree and the
    /// encoder is downloaded twice and found neither time.
    #[test]
    fn every_spelling_of_a_model_shares_one_encoder_directory() {
        let d = Path::new("/data");
        let canonical = mmproj_dir(d, "gemma-4-E2B-it");
        for spelling in [
            "gemma-4-E2B-it-Q4_K_M",
            "gemma-4-E2B-it:Q4_K_M",
            "gemma-4-E2B-it.gguf",
            "unsloth/gemma-4-E2B-it-GGUF:Q4_K_M",
        ] {
            assert_eq!(mmproj_dir(d, spelling), canonical, "spelling: {spelling}");
        }
        assert_ne!(mmproj_dir(d, "gemma-4-E4B-it"), canonical);
    }

    #[test]
    fn an_absent_encoder_is_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!mmproj_ready(tmp.path(), "gemma-4-E2B-it"));
        // A text-only model is never "ready" either, and must not be reported as
        // declaring vision.
        assert!(!mmproj_ready(tmp.path(), "gemma-4-E1B-it"));
    }

    #[test]
    fn a_zero_byte_encoder_does_not_count_as_downloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = mmproj_dir(tmp.path(), "gemma-4-E2B-it");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mmproj-BF16.gguf"), b"").unwrap();
        assert!(
            !mmproj_ready(tmp.path(), "gemma-4-E2B-it"),
            "a truncated or empty file must not be mistaken for an encoder"
        );

        std::fs::write(dir.join("mmproj-BF16.gguf"), b"not really a gguf").unwrap();
        assert!(mmproj_ready(tmp.path(), "gemma-4-E2B-it"));
    }
}
