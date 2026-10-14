//! Attaches a speculative-decoding drafter already on disk. The engine finds one only through the
//! registry, so this must live on the serving path: `GooseAdapter` never goes through
//! `LocalInferenceLlmAdapter`. Nothing here fetches a drafter.

use goose::providers::local_inference::local_model_registry::{
    get_registry, LocalModelEntry, LocalModelStorage, ModelSettings,
};
use pond_core::models::domain::drafter::{drafter_for, drafter_path, is_drafter_head};
use std::path::Path;

/// Register `model_name`'s drafter if its weights are in `data_dir`, and point the row the engine
/// loads at it; returns the drafter's id. Called on every provider build, so a drafter added or
/// deleted after boot is picked up or dropped without a restart. It takes the registry lock, so
/// never call it holding a registry guard.
pub fn attach_drafter(data_dir: &Path, model_name: &str) -> Option<String> {
    let spec = drafter_for(model_name)?;
    let path = drafter_path(data_dir, &spec);
    let present = path.exists() && file_is_drafter(&path);
    let mut registry = match get_registry().lock() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("registry lock poisoned, not attaching the drafter: {e}");
            return None;
        }
    };
    if !present {
        // A drafter deleted since it was attached must not be handed to the next load.
        if let Some(mut settings) = registry.get_model(model_name).map(|e| e.settings.clone()) {
            if settings.draft_model.as_deref() == Some(spec.id) {
                settings.draft_model = None;
                let _ = registry.update_model_settings(model_name, settings);
            }
        }
        return None;
    }
    if registry
        .get_model(spec.id)
        .is_none_or(|e| e.local_path != path)
    {
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
            // Defaults on purpose: a drafter's context is built from the TARGET's settings.
            settings: ModelSettings::default(),
            size_bytes: 0,
            mmproj_path: None,
            mmproj_source_url: None,
            mmproj_size_bytes: 0,
            mmproj_checked: false,
            shard_files: vec![],
        };
        let _ = registry.remove_model(spec.id);
        if let Err(e) = registry.add_model(entry) {
            tracing::warn!("could not register the MTP drafter: {e}");
            return None;
        }
        tracing::info!(drafter = spec.id, "MTP drafter registered");
    }
    // Read-modify-write: `update_model_settings` replaces the whole `ModelSettings`.
    let mut settings = registry.get_model(model_name)?.settings.clone();
    if settings.draft_model.as_deref() != Some(spec.id) {
        settings.draft_model = Some(spec.id.to_string());
        match registry.update_model_settings(model_name, settings) {
            Ok(()) => tracing::info!(
                model = model_name,
                drafter = spec.id,
                "speculative decoding attached"
            ),
            Err(e) => {
                tracing::warn!("could not point '{model_name}' at its drafter: {e}");
                return None;
            }
        }
    }
    Some(spec.id.to_string())
}

/// The first 16 KiB of `path` name the `gemma4-assistant` architecture.
fn file_is_drafter(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; 16 * 1024];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    head.truncate(n);
    is_drafter_head(&head)
}
