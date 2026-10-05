//! How a catalogue row is shown over REST.

use pond_core::models::domain::curated;
use pond_core::models::domain::engine::Engine;
use pond_core::models::domain::model_record::{ModelRecord, ModelRoleAssignment};
use pond_core::models::domain::recommended::{self, DeviceClass};
use pond_core::models::domain::taxonomy::{self, Acquisition, ModelKind, Provenance};

use crate::{MeasuredDto, ModelStatusEntry, RecommendedDto};

pub(crate) fn record_to_dto(
    m: &ModelRecord,
    assignments: &[ModelRoleAssignment],
) -> ModelStatusEntry {
    let active = assignments.iter().any(|a| a.model_id == m.id);
    let pick = curated::for_record(m);
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
        title: taxonomy::title(m, pick.map(|p| p.title)),
        quantization: m
            .quantization
            .clone()
            .or_else(|| pick.and_then(|p| p.quantization.map(str::to_string))),
        engine: Engine::for_category(&m.category).map(Into::into),
        provenance: Some(Provenance::derive(m, pick.is_some())),
        kind: Some(ModelKind::of(m)),
        acquire: Some(Acquisition::of(m, pick.is_some())),
        recommended: recommendation(&m.id, DeviceClass::current()),
    }
}

/// The suggestion for `model_id`, carrying numbers only measured on `device`'s class.
pub(crate) fn recommendation(model_id: &str, device: DeviceClass) -> Option<RecommendedDto> {
    let r = recommended::recommendation_for(model_id)?;
    Some(RecommendedDto {
        rank: r.rank,
        reason: r.reason.to_string(),
        measured: r.measured_on(device).map(MeasuredDto::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::recommended::Rank;

    #[test]
    fn a_desktop_reads_the_reason_and_no_orin_number() {
        let id = "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL";
        let desktop = recommendation(id, DeviceClass::Desktop).unwrap();
        assert_eq!(desktop.rank, Rank::Primary);
        assert!(desktop.measured.is_none());
        let orin = recommendation(id, DeviceClass::Orin).unwrap();
        let measured = orin.measured.unwrap();
        assert_eq!(measured.window_tokens, Some(16_384));
        assert_eq!(measured.tokens_per_second_min, Some(15));
        assert_eq!(measured.tokens_per_second_max, Some(16));
        assert!(recommendation("gguf/llama-3.2-3b", DeviceClass::Orin).is_none());
    }
}
