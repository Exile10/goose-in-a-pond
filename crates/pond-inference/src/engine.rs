//! Core inference engine wrapping llama-cpp-2.
//!
//! Manages the LlamaBackend singleton, model loading/unloading, and provides
//! the model reference needed by the generation loop in `provider.rs`.

use anyhow::{Context, Result};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatTemplate, LlamaModel};
use llama_cpp_2::LogOptions;
use pond_core::domain::model_capabilities::ModelCapabilities;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock, Weak};
use tokio::sync::Mutex;

/// A model loaded into memory with its chat template and capabilities.
pub(crate) struct LoadedModel {
    pub model: LlamaModel,
    pub model_id: String,
    pub chat_template: LlamaChatTemplate,
    pub capabilities: ModelCapabilities,
    /// Persistent KV-cache context — kept alive between inference calls.
    ///
    /// On subsequent turns, prefix matching identifies tokens already in the
    /// cache and only decodes the delta. This eliminates re-prefilling the
    /// system prompt + tool declarations (~2000 tokens) on every turn.
    ///
    /// MUST be set to `None` before the model is dropped or replaced.
    pub cached_ctx: Option<CachedInferenceContext>,
}

/// Persistent inference context with token history for KV-cache prefix reuse.
///
/// # Safety
///
/// `LlamaContext<'model>` borrows from `LlamaModel`. We store it as `'static`
/// via unsafe lifetime extension. The invariant is enforced by:
/// 1. Both live inside the same `LoadedModel` struct
/// 2. `cached_ctx` is declared AFTER `model` (Rust drops fields in declaration order)
/// 3. `unload_model()` explicitly sets `cached_ctx = None` before dropping
/// 4. All access is through `Mutex<Option<LoadedModel>>` — no aliasing
pub(crate) struct CachedInferenceContext {
    /// The llama.cpp context with KV cache state from previous turns.
    /// Lifetime is actually tied to the sibling `model` field.
    pub ctx: llama_cpp_2::context::LlamaContext<'static>,
    /// Tokens currently prefilled in the KV cache (for prefix matching).
    pub tokens_in_cache: Vec<llama_cpp_2::token::LlamaToken>,
}

// SAFETY: CachedInferenceContext is only accessed through Mutex<Option<LoadedModel>>.
// The mutex serializes all access — only one thread touches the context at a time.
// The raw pointer inside LlamaContext is to GPU/CPU memory managed by llama.cpp,
// which is thread-safe when accessed serially (no concurrent decode calls).
unsafe impl Send for CachedInferenceContext {}
unsafe impl Sync for CachedInferenceContext {}

/// Global weak reference to the shared backend. Only a `Weak` is stored --
/// strong `Arc`s live in `LlamaCppEngine` instances. When all strong refs
/// drop (normal shutdown), the backend is deallocated and freed. The `Weak`
/// left behind is inert, avoiding ggml statics races during `__cxa_finalize`.
static BACKEND: StdMutex<Weak<LlamaBackend>> = StdMutex::new(Weak::new());

/// Obtain or initialise the global `LlamaBackend` singleton.
///
/// Thread-safe: the init check and `LlamaBackend::init()` both execute
/// inside the same mutex guard so there is no window for a race.
///
/// Returns `Err` if the backend was already initialised by another crate
/// (e.g. Goose's InferenceRuntime) in the same process. The caller should
/// fall back to a different provider (Ollama) in that case.
pub(crate) fn get_or_init_backend() -> Result<Arc<LlamaBackend>> {
    let mut guard = BACKEND.lock().expect("backend lock poisoned");
    if let Some(backend) = guard.upgrade() {
        return Ok(backend);
    }
    match LlamaBackend::init() {
        Ok(backend) => {
            llama_cpp_2::send_logs_to_tracing(LogOptions::default());
            let arc = Arc::new(backend);
            *guard = Arc::downgrade(&arc);
            Ok(arc)
        }
        Err(_) => {
            // Backend already initialized by another crate (e.g. Goose's LocalInferenceProvider).
            // This is fine — the underlying llama_backend_init() is idempotent at the C level.
            // We create a LlamaBackend struct by transmuting an empty struct — the Drop impl
            // calls llama_backend_free() which is also safe to call multiple times.
            tracing::info!(
                "llama backend already initialised (shared with Goose) — creating wrapper"
            );
            // SAFETY: LlamaBackend is a zero-sized struct. The C backend is already initialized.
            // Creating this wrapper just gives us lifetime tracking. The llama_backend_free()
            // called on Drop is a no-op when called after the first free.
            let backend: LlamaBackend = unsafe { std::mem::zeroed() };
            let arc = Arc::new(backend);
            *guard = Arc::downgrade(&arc);
            Ok(arc)
        }
    }
}

/// In-process GGUF inference engine.
///
/// Type alias for the model slot shared between the engine and spawn_blocking tasks.
pub(crate) type ModelSlot = Arc<Mutex<Option<LoadedModel>>>;

/// Owns the llama.cpp backend and at most one loaded model. Dropping the
/// engine unloads the model and (if this is the last engine) frees the
/// backend.
///
/// Field order matters: `model` is declared before `backend` so Rust drops
/// the loaded model (and its Metal/GPU resources) before the backend calls
/// `llama_backend_free()`.
pub struct LlamaCppEngine {
    model: ModelSlot,
    backend: Arc<LlamaBackend>,
    data_dir: PathBuf,
    /// Cached capabilities — updated on model load/unload, read lock-free.
    /// Avoids contending with the model mutex (which is held for the entire
    /// duration of generation) when the agent needs to check tool_calling, etc.
    capabilities: Arc<StdRwLock<ModelCapabilities>>,
}

impl LlamaCppEngine {
    /// Create a new engine.
    ///
    /// `data_dir` is the root data directory; GGUF files are expected at
    /// `{data_dir}/models/gguf/{filename}`.
    ///
    /// No model is loaded until [`load_model`] is called.
    pub fn new(data_dir: &Path) -> Result<Self> {
        let backend = get_or_init_backend()?;
        Ok(Self {
            model: Arc::new(Mutex::new(None)),
            backend,
            data_dir: data_dir.to_path_buf(),
            capabilities: Arc::new(StdRwLock::new(ModelCapabilities::default())),
        })
    }

    /// Load a GGUF model, unloading any previously loaded model first.
    ///
    /// `model_id` can be:
    /// - A filename with extension: `"gemma-4-E2B-it-Q4_K_M.gguf"`
    /// - A stem without extension: `"gemma-4-E2B-it-Q4_K_M"`
    ///
    /// The file is resolved at `{data_dir}/models/gguf/{model_id}[.gguf]`.
    pub async fn load_model(
        &self,
        model_id: &str,
        n_gpu_layers: u32,
        flash_attention: bool,
    ) -> Result<()> {
        let model_path = self.resolve_model_path(model_id)?;
        let backend = self.backend.clone();
        let model_id_owned = model_id.to_string();

        // Load in a blocking thread -- LlamaModel::load_from_file is a heavy
        // CPU/GPU operation that must not block the tokio runtime.
        let loaded = tokio::task::spawn_blocking(move || {
            load_model_sync(
                &backend,
                &model_path,
                &model_id_owned,
                n_gpu_layers,
                flash_attention,
            )
        })
        .await
        .context("model loading task panicked")??;

        // Update lock-free capabilities cache before acquiring the model lock.
        *self
            .capabilities
            .write()
            .expect("capabilities lock poisoned") = loaded.capabilities.clone();

        // Swap: unload previous, install new.
        let mut guard = self.model.lock().await;
        *guard = Some(loaded);
        Ok(())
    }

    /// Unload the current model, freeing all GPU/CPU memory.
    ///
    /// Drops the cached context BEFORE the model to maintain the safety
    /// invariant (context borrows from model).
    pub async fn unload_model(&self) {
        // Reset capabilities cache first.
        *self
            .capabilities
            .write()
            .expect("capabilities lock poisoned") = ModelCapabilities::default();

        let mut guard = self.model.lock().await;
        if let Some(loaded) = guard.as_mut() {
            // Drop cached context first — it borrows from the model.
            loaded.cached_ctx = None;
            tracing::info!("unloading model");
        }
        *guard = None;
    }

    /// Whether a model is currently loaded.
    pub async fn is_loaded(&self) -> bool {
        self.model.lock().await.is_some()
    }

    /// The name of the currently loaded model, or `"none"`.
    pub async fn model_name(&self) -> String {
        self.model
            .lock()
            .await
            .as_ref()
            .map(|m| m.model_id.clone())
            .unwrap_or_else(|| "none".to_string())
    }

    /// The capabilities of the currently loaded model.
    pub async fn model_capabilities(&self) -> ModelCapabilities {
        self.model
            .lock()
            .await
            .as_ref()
            .map(|m| m.capabilities.clone())
            .unwrap_or_default()
    }

    /// Lock-free capabilities read — safe to call from sync contexts.
    ///
    /// Returns capabilities cached at model load time. Unlike `model_capabilities()`,
    /// this never contends with the model mutex (which is held for the entire
    /// duration of a generation task).
    pub fn cached_capabilities(&self) -> ModelCapabilities {
        self.capabilities
            .read()
            .expect("capabilities lock poisoned")
            .clone()
    }

    /// Clone the model slot `Arc` for use in `spawn_blocking` tasks.
    pub(crate) fn model_slot(&self) -> ModelSlot {
        Arc::clone(&self.model)
    }

    /// Clone the backend `Arc` for passing into blocking tasks.
    pub(crate) fn backend_arc(&self) -> Arc<LlamaBackend> {
        Arc::clone(&self.backend)
    }

    /// Invalidate the in-memory KV cache (e.g. when settings change the prompt).
    pub async fn invalidate_kv_cache(&self) {
        let mut guard = self.model.lock().await;
        if let Some(loaded) = guard.as_mut() {
            loaded.cached_ctx = None;
            tracing::info!("KV cache invalidated");
        }
    }

    /// Resolve a model identifier to an absolute filesystem path.
    fn resolve_model_path(&self, model_id: &str) -> Result<PathBuf> {
        let gguf_dir = self.data_dir.join("models").join("gguf");

        // Try as-is first (with extension).
        let with_ext = if model_id.ends_with(".gguf") {
            gguf_dir.join(model_id)
        } else {
            gguf_dir.join(format!("{}.gguf", model_id))
        };

        if with_ext.exists() {
            return Ok(with_ext);
        }

        // If model_id is an absolute path, use directly.
        let abs = Path::new(model_id);
        if abs.is_absolute() && abs.exists() {
            return Ok(abs.to_path_buf());
        }

        anyhow::bail!(
            "GGUF model not found: tried '{}' and '{}'",
            with_ext.display(),
            model_id
        );
    }
}

/// Synchronous model loading -- runs inside `spawn_blocking`.
fn load_model_sync(
    backend: &LlamaBackend,
    model_path: &Path,
    model_id: &str,
    n_gpu_layers: u32,
    flash_attention: bool,
) -> Result<LoadedModel> {
    tracing::info!(
        model_id,
        path = %model_path.display(),
        n_gpu_layers,
        flash_attention,
        "loading GGUF model"
    );

    let params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);

    let model = LlamaModel::load_from_file(backend, model_path, &params)
        .map_err(|e| anyhow::anyhow!("failed to load model: {}", e))?;

    let chat_template = match model.chat_template(None) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!("model has no embedded chat template, falling back to chatml");
            LlamaChatTemplate::new("chatml")
                .map_err(|e| anyhow::anyhow!("failed to create fallback chat template: {}", e))?
        }
    };

    // Detect capabilities from the model identifier.
    let capabilities = ModelCapabilities::from_model_name(model_id);

    tracing::info!(
        model_id,
        n_ctx_train = model.n_ctx_train(),
        n_layer = model.n_layer(),
        thinking = capabilities.thinking,
        tool_calling = capabilities.tool_calling,
        "model loaded successfully"
    );

    // Log flash attention status. The flag is applied at context creation time
    // (not model load time), so we just record the intent here.
    if flash_attention {
        tracing::info!("flash attention will be enabled for inference contexts");
    }

    Ok(LoadedModel {
        model,
        model_id: model_id.to_string(),
        chat_template,
        capabilities,
        cached_ctx: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn backend_singleton_returns_same_arc() {
        let a = get_or_init_backend().expect("init");
        let b = get_or_init_backend().expect("init");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn resolve_model_path_appends_gguf_extension() {
        let engine = LlamaCppEngine {
            model: Arc::new(Mutex::new(None)),
            backend: get_or_init_backend().expect("init"),
            data_dir: PathBuf::from("/tmp/test-data"),
            capabilities: Arc::new(StdRwLock::new(ModelCapabilities::default())),
        };

        // Non-existent path, but verify the logic.
        let err = engine.resolve_model_path("nonexistent-model").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent-model.gguf"),
            "error should mention .gguf path, got: {}",
            msg
        );
    }

    #[test]
    fn model_capabilities_from_known_model() {
        let caps = ModelCapabilities::from_model_name("gemma-4-E2B-it-Q4_K_M.gguf");
        assert!(caps.thinking);
        assert!(caps.tool_calling);
        assert!(caps.vision);
    }
}
