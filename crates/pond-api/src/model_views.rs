//! How a catalogue row is shown over REST.

use pond_core::models::domain::engine::Engine;
use pond_core::models::domain::model_record::{ModelRecord, ModelRoleAssignment};
use pond_core::models::domain::taxonomy::{self, Acquisition, ModelKind, Provenance};

use crate::ModelStatusEntry;

pub(crate) fn record_to_dto(
    m: &ModelRecord,
    assignments: &[ModelRoleAssignment],
) -> ModelStatusEntry {
    let active = assignments.iter().any(|a| a.model_id == m.id);
    ModelStatusEntry {
        category: m.category.as_str().to_string(),
        name: m.name.clone(),
        description: m.description.clone(),
        size_mb: m.size_mb,
        downloaded: m.downloaded,
        active,
        url: m.url.clone(),
        hf_id: m.hf_id.clone(),
        filename: m.filename.clone(),
        ram_estimate_mb: m.ram_estimate_mb,
        recommended_role: m.recommended_role.clone(),
        context_length: m.context_length,
        asr_language: m.asr_language.clone(),
        asr_size: m.asr_size.clone(),
        tts_engine: m.tts_engine.clone(),
        config_filename: m.config_filename.clone(),
        // `list_models` fills these for GGUF rows: the agent's verdict may read a header.
        reads_images: None,
        image_support_bytes: None,
        id: m.id.clone(),
        title: taxonomy::title(m, None),
        quantization: m.quantization.clone(),
        engine: Engine::for_category(&m.category).map(Into::into),
        provenance: Some(Provenance::derive(m, false)),
        kind: Some(ModelKind::of(m)),
        acquire: Some(Acquisition::of(m, false)),
    }
}
