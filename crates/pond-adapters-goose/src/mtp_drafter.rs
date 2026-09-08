//! Drafter registration for the live serving path.
//!
//! Speculative decoding needs a second, small model beside the chat model, and
//! the engine finds it by name through the registry — so a drafter file with no
//! registry row is invisible however correctly it was downloaded.
//!
//! This lives here, next to [`crate::vision_encoder`], because this is the path
//! that actually serves: `GooseAdapter` builds `LocalInferenceProvider`
//! directly and never goes through `LocalInferenceLlmAdapter`, which feeds the
//! quarantined PondAgent loop. Registering from there looks right, compiles,
//! and never runs.

use goose::providers::local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelRegistry, LocalModelStorage, ModelSettings,
};
use pond_core::models::domain::drafter::{drafter_for, drafter_path};
use std::path::Path;

/// Put this model's MTP drafter in the registry, if its weights are on disk.
///
/// Returns the registry id when speculation is available. Called on every
/// provider build rather than once, which is what makes the arrangement
/// self-correcting: a drafter downloaded after boot is picked up without a
/// restart, and one that has been deleted stops being referenced instead of
/// failing the next context creation.
pub fn ensure_drafter_registered(data_dir: &Path, model_name: &str) -> Option<String> {
    let spec = drafter_for(model_name)?;
    let path = drafter_path(data_dir, &spec);
    if !path.exists() {
        return None;
    }

    let mut registry = match get_registry().lock() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("registry lock poisoned, not registering the drafter: {e}");
            return None;
        }
    };
    if registry
        .get_model(spec.id)
        .is_some_and(|e| e.local_path == path)
    {
        return Some(spec.id.to_string());
    }

    let entry = LocalModelEntry {
        id: spec.id.to_string(),
        repo_id: spec.repo.to_string(),
        filename: spec.filename.to_string(),
        quantization: String::new(),
        local_path: path,
        source_url: format!(
            "https://huggingface.co/{}/resolve/main/{}",
            spec.repo, spec.filename
        ),
        backend_id: None,
        storage: LocalModelStorage::ManualPath,
        // Defaults on purpose: a drafter is never a chat model, so nothing
        // reads its context size, tool mode or thinking flag. Its context is
        // built from the TARGET's settings, beside the target's own.
        settings: ModelSettings::default(),
        size_bytes: 0,
        mmproj_path: None,
        mmproj_source_url: None,
        mmproj_size_bytes: 0,
        mmproj_checked: false,
        shard_files: vec![],
    };
    if let Err(e) = registry.add_model(entry) {
        tracing::warn!("could not register the MTP drafter: {e}");
        return None;
    }
    tracing::info!(drafter = spec.id, "MTP drafter registered");
    point_target_at_drafter(&mut registry, model_name, spec.id);
    Some(spec.id.to_string())
}

/// Set `draft_model` on the row the ENGINE resolves.
///
/// `apply_jetson_settings` stamps the model id as spelled in settings
/// (`gemma-4-E2B-it-qat-UD-Q4_K_XL`), while the engine loads the canonical stem
/// (`gemma-4-E2B-it-qat`) that `register_gguf_model` returns. Those are two rows
/// in one registry, and a `draft_model` written to the first is never read. This
/// is called with the canonical key, because that is what the caller in
/// `goose_agent` has in hand.
///
/// Read-modify-write rather than a fresh block: `update_model_settings` replaces
/// the whole `ModelSettings`, so constructing one here would drop whatever else
/// the row is carrying.
fn point_target_at_drafter(
    registry: &mut impl std::ops::DerefMut<Target = LocalModelRegistry>,
    model_id: &str,
    drafter_id: &str,
) {
    let Some(mut settings) = registry.get_model(model_id).map(|e| e.settings.clone()) else {
        return;
    };
    if settings.draft_model.as_deref() == Some(drafter_id) {
        return;
    }
    settings.draft_model = Some(drafter_id.to_string());
    match registry.update_model_settings(model_id, settings) {
        Ok(()) => tracing::info!(
            model = model_id,
            drafter = drafter_id,
            "speculation enabled"
        ),
        Err(e) => tracing::warn!("could not point '{model_id}' at its drafter: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_with_no_drafter_registers_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_drafter_registered(tmp.path(), "Llama-3.2-3B-Instruct").is_none());
    }

    #[test]
    fn a_drafter_that_is_not_on_disk_registers_nothing() {
        // The registry row is what the engine resolves, so writing one for a
        // file that is not there would turn a missing download into a failed
        // context creation on the next turn.
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_drafter_registered(tmp.path(), "gemma-4-E2B-it-qat").is_none());
    }
}
