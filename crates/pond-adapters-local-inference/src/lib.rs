//! In-process GGUF inference adapter and memory-aware model scheduler.
//!
//! Wraps Goose's [`LocalInferenceProvider`] so GIAP can load model weights
//! directly into the process — no llamafile/Ollama subprocess required.
//!
//! # Hardware acceleration
//! - **macOS**: Metal activated automatically via `llama-cpp-2` cfg flags.
//! - **Jetson Orin Nano (NVIDIA)**: Requires `--features cuda` at build time.
//!   CUDA settings are applied to the model registry at init time:
//!   - `n_gpu_layers = 99` — full offload into unified 8 GB DRAM (no separate VRAM)
//!   - `context_size = 3072` — safe headroom for GIAP's chat workflow on 8 GB
//!   - `n_batch = 512` — maximise GPU throughput on Ampere (sm_87)
//!   - `n_threads = 4` — 6-core A78AE; leave headroom for OS + voice pipeline
//!   - `flash_attention = true` — reduces KV-cache memory by ~40 % on Ampere
//!   - `use_mlock = false` — unified memory; mlock causes kernel page faults
//!
//! # Usage
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use pond_adapters_local_inference::LocalInferenceLlmAdapter;
//! use std::sync::Arc;
//!
//! let llm = Arc::new(LocalInferenceLlmAdapter::new(
//!     LocalInferenceLlmAdapter::DEFAULT_MODEL,
//! ).await?);
//! # Ok(())
//! # }
//! ```

pub mod scheduler;
pub use scheduler::{NoopScheduler, ResourceAwareModelScheduler, LLM_BUDGET_MB, JETSON_TOTAL_RAM_MB};

use anyhow::Result;
use async_trait::async_trait;
use goose::model::ModelConfig;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use pond_adapters_goose::provider_adapter::GooseProviderAdapter;
use pond_core::domain::message::ChatMessage;
use pond_core::ports::provider::LlmProvider;
use std::sync::Arc;

/// Default GGUF model for Jetson Orin Nano (8 GB).
///
/// 3B Q4_K_M ≈ 2.0 GB on disk + ~2.5 GB at runtime — leaves ample headroom
/// for the OS, voice pipeline, and other GIAP services.
pub const DEFAULT_MODEL: &str = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";

/// GIAP `LlmProvider` adapter backed by in-process GGUF inference.
///
/// Internally delegates to [`GooseProviderAdapter`] for `ChatMessage` ↔
/// Goose [`Message`] conversion so there is no duplication of that logic.
pub struct LocalInferenceLlmAdapter {
    inner: GooseProviderAdapter,
}

impl LocalInferenceLlmAdapter {
    /// Default GGUF model identifier (re-exported as an associated constant for
    /// ergonomic use via `LocalInferenceLlmAdapter::DEFAULT_MODEL`).
    pub const DEFAULT_MODEL: &'static str = DEFAULT_MODEL;

    /// Build the adapter for the given model identifier.
    ///
    /// The `model_id` can be a HuggingFace repo+filename such as
    /// `"bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"`, a local file path,
    /// or any identifier accepted by Goose's `LocalInferenceProvider`.
    ///
    /// Model weights are **not** loaded here — they load on the first
    /// `complete()` call via `InferenceRuntime::get_or_init()` (global
    /// singleton, thread-safe `StdMutex<Weak<>>`).
    pub async fn new(model_id: &str) -> Result<Self> {
        let model_config = ModelConfig {
            model_name: model_id.to_string(),
            ..Default::default()
        };

        // On Jetson Orin Nano (CUDA build) apply hardware-specific settings to
        // the model registry entry so llama-cpp-2 picks them up at load time.
        // Jetson has unified 8 GB DRAM (CPU + GPU share the same pool), an
        // Ampere GPU (sm_87) with 1024 CUDA cores, and CUDA 12.6 on JetPack 6.2.
        #[cfg(feature = "cuda")]
        Self::apply_jetson_settings(model_id);

        tracing::info!("initialising LocalInferenceProvider for model: {}", model_id);
        let provider = LocalInferenceProvider::from_env(model_config, vec![]).await?;
        let session_id = uuid::Uuid::new_v4().to_string();

        Ok(Self {
            inner: GooseProviderAdapter::new(Arc::new(provider) as Arc<dyn GooseProvider>, session_id),
        })
    }

    /// Build the adapter, registering the model path in GIAP's data directory.
    ///
    /// Unlike `new()`, this method registers the model's `local_path` in Goose's
    /// global model registry so that `LocalInferenceProvider` can find the GGUF
    /// file at `$data_dir/models/gguf/{filename}` instead of Goose's default
    /// `~/.local/share/goose/models/` location.
    pub async fn new_with_data_dir(model_id: &str, data_dir: &std::path::Path) -> Result<Self> {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, LocalModelEntry, ModelSettings, model_id_from_repo,
        };

        // Parse "repo_id:quantization" — e.g. "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"
        let (repo_id, quantization) = model_id
            .rsplit_once(':')
            .unwrap_or((model_id, "Q4_K_M"));

        let id = model_id_from_repo(repo_id, quantization);

        // Derive filename: strip "-GGUF" suffix from the repo name, append "-{quant}.gguf"
        let model_name = repo_id.split('/').last().unwrap_or(repo_id);
        let base_name  = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename   = format!("{}-{}.gguf", base_name, quantization);

        let gguf_dir   = data_dir.join("models").join("gguf");
        let local_path = gguf_dir.join(&filename);
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        // Register / update local_path in Goose's global registry.
        // The lock is dropped before calling Self::new() to avoid deadlock.
        {
            match get_registry().lock() {
                Ok(mut registry) => {
                    if !registry.has_model(&id) {
                        let entry = LocalModelEntry {
                            id:           id.clone(),
                            repo_id:      repo_id.to_string(),
                            filename:     filename.clone(),
                            quantization: quantization.to_string(),
                            local_path,
                            source_url,
                            settings:     ModelSettings::default(),
                            size_bytes:   0,
                        };
                        if let Err(e) = registry.add_model(entry) {
                            tracing::warn!("Could not register GGUF model '{}': {}", id, e);
                        }
                    }
                }
                Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
            }
        }

        Self::new(model_id).await
    }

    /// Patch the Goose model registry with Jetson Orin Nano–optimised settings.
    ///
    /// These settings are applied at startup and saved to `~/.local/share/goose/
    /// models/registry.json` so they persist across restarts on Jetson.
    ///
    /// The function silently ignores errors (model not yet downloaded, registry
    /// lock poisoned) — defaults will be used in that case.
    #[cfg(feature = "cuda")]
    fn apply_jetson_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings,
        };

        let jetson_settings = ModelSettings {
            // Full GPU offload: Jetson unified memory means all layers fit in
            // the same 8 GB pool — no split between CPU and GPU DRAM.
            n_gpu_layers: Some(99),
            // 3072-token context fits the GIAP chat workflow with room for the
            // system prompt + history, while keeping KV-cache pressure manageable.
            context_size: Some(3072),
            // Batch size 512 keeps Ampere SMs saturated during prefill without
            // exceeding the available memory bandwidth (68 GB/s).
            n_batch: Some(512),
            // Use 4 CPU threads for tokenisation / sampling on the 6-core A78AE.
            // Leaving 2 cores free for the OS, audio pipeline, and GIAP services.
            n_threads: Some(4),
            // Flash attention halves KV-cache memory on Ampere (native support).
            flash_attention: Some(true),
            // mlock pins pages in RAM; on unified memory this triggers kernel
            // page faults for every GPU access. Disable for correct performance.
            use_mlock: false,
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                if let Err(e) = registry.update_model_settings(model_id, jetson_settings) {
                    tracing::debug!(
                        "Jetson settings not applied to '{}' (model not yet registered): {}",
                        model_id, e
                    );
                } else {
                    tracing::info!(
                        "Applied Jetson Orin Nano CUDA settings to model '{}'",
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Could not acquire model registry lock for Jetson settings: {}", e);
            }
        }
    }
}

#[async_trait]
impl LlmProvider for LocalInferenceLlmAdapter {
    async fn complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        self.inner.complete(system, messages).await
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

// ── Unit tests (no model weights required) ────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Integration tests that require a real model are marked `#[ignore]` and
    /// gated on the `GIAP_TEST_MODEL_PATH` environment variable.
    ///
    /// Run with:
    /// ```bash
    /// GIAP_TEST_MODEL_PATH=/path/to/model.gguf \
    ///   cargo test -p pond-adapters-local-inference -- --ignored
    /// ```
    #[test]
    fn default_model_constant_is_set() {
        assert!(!super::DEFAULT_MODEL.is_empty());
    }
}
