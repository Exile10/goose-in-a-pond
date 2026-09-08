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
    get_registry, LocalModelEntry, LocalModelStorage, ModelSettings,
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
    match registry.add_model(entry) {
        Ok(()) => {
            tracing::info!(drafter = spec.id, "MTP drafter registered");
            Some(spec.id.to_string())
        }
        Err(e) => {
            tracing::warn!("could not register the MTP drafter: {e}");
            None
        }
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
