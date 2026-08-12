//! Vision-encoder (mmproj) resolution for GIAP-registered GGUF models.
//!
//! # Why this exists
//!
//! The engine's multimodal path is gated on exactly one thing:
//! `has_vision = request.resolved_model.mmproj_path.is_some()`
//! (`goose-local-inference/src/llamacpp/mod.rs`). Everything else — the
//! `vision_capable` settings flag, the featured-model table — is advisory.
//!
//! `GooseAdapter::register_gguf_model` registers GIAP's own GGUFs under a BARE
//! STEM (`gemma-4-E2B-it`) with `repo_id = "local/<stem>"`, because GIAP owns the
//! files under its own data dir and must not let goose treat them as deletable
//! goose-managed storage. That choice has a side effect: goose's own
//! `LocalModelEntry::enrich_with_featured_mmproj` looks the model up with
//! `featured_mmproj_spec(&self.id)`, which compares against the featured HF repo
//! id (`unsloth/gemma-4-E2B-it-GGUF`). A bare stem never matches, so
//! `mmproj_path` stayed `None` forever and no GIAP-registered model could ever
//! see an image, however vision-capable the weights were.
//!
//! This module closes that gap on the GIAP side: map a stem back to its featured
//! entry, fetch the encoder once into a GIAP-owned directory, and stamp the
//! registry entry. `resolve_model_path` runs on EVERY `Provider::stream`, so a
//! stamp taken after a background download turns vision on for the next turn —
//! no provider rebuild, no restart.

use goose::providers::local_inference::local_model_registry::{
    get_registry, MmprojSpec, FEATURED_MODELS,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Directory holding downloaded vision encoders.
///
/// Deliberately NOT `models/gguf/`: `resolve_gguf_filename` and
/// `canonical_model_stem` scan that directory for `*.gguf` to map a display name
/// onto a weights file, and an `mmproj-BF16.gguf` sitting in there is a
/// candidate they would have to learn to ignore. Keeping encoders in their own
/// subtree means those scans stay as simple as they are.
/// The directory name is the NORMALISED model name, not the caller's spelling.
/// Registration passes the collapsed registry stem (`gemma-4-E2B-it`) while the
/// capability and readiness checks pass the settings spelling
/// (`gemma-4-E2B-it-Q4_K_M`); both must land on one directory or the encoder is
/// downloaded twice and found neither time.
#[must_use]
pub fn mmproj_dir(data_dir: &Path, model_name: &str) -> PathBuf {
    data_dir
        .join("models")
        .join("mmproj")
        .join(normalize_model_name(model_name))
}

/// The featured vision-encoder spec for a GIAP registry stem, if the model
/// declares one.
///
/// Matching is by normalised name rather than repo id: GIAP's stem is
/// `gemma-4-E2B-it` and the featured repo is `unsloth/gemma-4-E2B-it-GGUF`, so
/// the comparison strips the owner prefix, a trailing `-GGUF`, and any quant
/// suffix the caller left on, then compares case-insensitively.
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

/// Collapse a model spelling to a comparable key.
///
/// Drops the HF owner, a trailing `-GGUF`, a quant suffix in either spelling
/// (`:Q4_K_M` as goose writes it, `-Q4_K_M` as GIAP's settings do), a `.gguf`
/// extension, and case. Everything else is preserved, so `E2B` and `E4B` (and
/// `E1B`, which has NO encoder) stay distinct.
///
/// The dash-quant strip is unconditional here, unlike `canonical_model_stem`
/// which only collapses when both spellings resolve to the same file on disk.
/// That check exists to protect a deliberate quant PIN from losing its identity;
/// which vision encoder a model uses is a property of the family, not the quant,
/// so no such care is needed.
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

/// `true` when the model declares a vision encoder, whether or not the encoder
/// bytes are on disk yet.
///
/// This is what the UI should gate its attach affordance on: a user who has just
/// selected a vision model should not be told the model cannot read images
/// merely because a ~1 GB download has not finished. The turn itself checks for
/// the bytes and says so precisely (see [`mmproj_ready`]).
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

/// The on-disk encoder for a stem, if it is already downloaded.
///
/// Checks GIAP's own directory first, then the path goose would have used, so an
/// encoder fetched by goose's model manager is reused rather than downloaded
/// twice.
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

/// Ensure the vision encoder for `stem` is available, downloading it in the
/// background if it is not.
///
/// Returns immediately. Deliberately NOT blocking: the encoder is ~1 GB and this
/// runs inside a provider build on the path of a model switch, so blocking would
/// stall the first chat after switching models for minutes with no feedback.
/// Instead the turn that needs it reports precisely what is missing
/// ([`mmproj_ready`]), and the stamp lands as soon as the bytes do — the engine
/// re-reads the registry on every generation, so nothing has to be restarted.
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

    // PAI-2 P6a: one gate for the whole encoder fetch. This is the single
    // choke point -- every caller reaches the network through here -- and it
    // runs inside a spawned task, so `record_egress`'s `tokio::spawn` has a
    // runtime. The caller already logs a failed download as a warning and
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

    /// Both quant spellings must resolve. This is not cosmetic: `Settings`
    /// stores `chat_model = "gemma-4-E2B-it-Q4_K_M"` while the registry key is
    /// the collapsed stem `gemma-4-E2B-it`, and BOTH are passed to this module
    /// (the capability check and the turn-time readiness check use the settings
    /// spelling; registration uses the stem). An earlier version only stripped a
    /// colon-quant and reported the real on-disk vision model as text-only.
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
