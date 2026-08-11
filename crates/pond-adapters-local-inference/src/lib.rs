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
//!   - `context_size = 16384` — the ~3.2K-token turn-1 prompt plus real history
//!     room; measured KV cost is ~18 KiB/token, so this is ~288 MiB
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
pub mod tool_caller;
pub use scheduler::{
    NoopScheduler, ResourceAwareModelScheduler, JETSON_TOTAL_RAM_MB, LLM_BUDGET_MB,
};
pub use tool_caller::ToolCallerEngine;

use anyhow::Result;
use async_trait::async_trait;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use goose_providers::model::ModelConfig;
use pond_adapters_goose::provider_adapter::GooseProviderAdapter;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
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
        let model_config = ModelConfig::new(model_id);

        // On Jetson Orin Nano (CUDA build) apply hardware-specific settings to
        // the model registry entry so llama-cpp-2 picks them up at load time.
        // Jetson has unified 8 GB DRAM (CPU + GPU share the same pool), an
        // Ampere GPU (sm_87) with 1024 CUDA cores, and CUDA 12.6 on JetPack 6.2.
        #[cfg(feature = "cuda")]
        Self::apply_jetson_settings(model_id);
        #[cfg(not(feature = "cuda"))]
        Self::apply_platform_settings(model_id);

        tracing::info!(
            "initialising LocalInferenceProvider for model: {}",
            model_id
        );
        goose::providers::local_inference::configure_local_inference();
        let provider = LocalInferenceProvider::from_env().await?;

        Ok(Self {
            inner: GooseProviderAdapter::new(
                Arc::new(provider) as Arc<dyn GooseProvider>,
                model_config,
            ),
        })
    }

    /// Build the adapter, registering the model path in GIAP's data directory.
    ///
    /// Unlike `new()`, this method registers the model's `local_path` in Goose's
    /// global model registry so that `LocalInferenceProvider` can find the GGUF
    /// file at `$data_dir/models/gguf/{filename}` instead of Goose's default
    /// `~/.local/share/goose/models/` location.
    ///
    /// Accepts two formats:
    /// - HuggingFace: `"bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"`
    /// - Raw filename: `"gemma-4-E2B-it-Q4_K_M.gguf"` (file must exist in `$data_dir/models/gguf/`)
    pub async fn new_with_data_dir(model_id: &str, data_dir: &std::path::Path) -> Result<Self> {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, model_id_from_repo, LocalModelEntry, LocalModelStorage, ModelSettings,
            ToolCallingMode,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // ── Filename stem (e.g. "gemma-4-E2B-it-Q4_K_M") ───────────────────
        // Detected when: no '/', no ':', no ".gguf" extension.
        // The model catalog stores name = stem (without extension); the file on
        // disk is {stem}.gguf in the gguf directory.  Normalise by appending
        // ".gguf" and falling through to the raw filename path below.
        let owned_with_ext;
        let model_id =
            if !model_id.contains('/') && !model_id.contains(':') && !model_id.ends_with(".gguf") {
                let candidate = gguf_dir.join(format!("{}.gguf", model_id));
                if candidate.exists() {
                    owned_with_ext = format!("{}.gguf", model_id);
                    owned_with_ext.as_str()
                } else {
                    model_id // not a local stem — fall through to HF path
                }
            } else {
                model_id
            };

        // ── Raw filename (e.g. "gemma-4-E2B-it-Q4_K_M.gguf") ────────────────
        // Detected when: no ':' separator and ends with ".gguf".
        if model_id.ends_with(".gguf") && !model_id.contains(':') {
            let path = std::path::Path::new(model_id);
            let filename = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| model_id.to_string());

            // Stable synthetic registry key = stem (strip ".gguf").
            let stem = filename.trim_end_matches(".gguf").to_string();

            // Use absolute path if provided, otherwise place under data_dir.
            let local_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                gguf_dir.join(&filename)
            };

            {
                match get_registry().lock() {
                    Ok(mut registry) => {
                        if !registry.has_model(&stem) {
                            let mut settings = ModelSettings::default();
                            settings.tool_calling = ToolCallingMode::ForceNative;
                            let entry = LocalModelEntry {
                                id: stem.clone(),
                                repo_id: format!("local/{}", stem),
                                filename: filename.clone(),
                                quantization: String::new(),
                                local_path,
                                source_url: String::new(),
                                backend_id: None,
                                storage: LocalModelStorage::ManualPath,
                                settings,
                                size_bytes: 0,
                                mmproj_path: None,
                                mmproj_source_url: None,
                                mmproj_size_bytes: 0,
                                mmproj_checked: false,
                                shard_files: vec![],
                            };
                            if let Err(e) = registry.add_model(entry) {
                                tracing::warn!("Could not register GGUF model '{}': {}", stem, e);
                            }
                        } else if let Some(entry) = registry.get_model(&stem) {
                            let mut s = entry.settings.clone();
                            if s.tool_calling == ToolCallingMode::Auto {
                                s.tool_calling = ToolCallingMode::ForceNative;
                                let _ = registry.update_model_settings(&stem, s);
                            }
                        }
                    }
                    Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
                }
            }

            return Self::new(&stem).await;
        }

        // ── HuggingFace format ("repo_id:quantization") ───────────────────────
        // Parse "repo_id:quantization" — e.g. "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap_or((model_id, "Q4_K_M"));

        let id = model_id_from_repo(repo_id, quantization);

        // Derive filename: strip "-GGUF" suffix from the repo name, append "-{quant}.gguf"
        let model_name = repo_id.split('/').last().unwrap_or(repo_id);
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

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
                        let mut settings = ModelSettings::default();
                        settings.tool_calling = ToolCallingMode::ForceNative;
                        let entry = LocalModelEntry {
                            id: id.clone(),
                            repo_id: repo_id.to_string(),
                            filename: filename.clone(),
                            quantization: quantization.to_string(),
                            local_path,
                            source_url,
                            backend_id: None,
                            storage: LocalModelStorage::ManualPath,
                            settings,
                            size_bytes: 0,
                            mmproj_path: None,
                            mmproj_source_url: None,
                            mmproj_size_bytes: 0,
                            mmproj_checked: false,
                            shard_files: vec![],
                        };
                        if let Err(e) = registry.add_model(entry) {
                            tracing::warn!("Could not register GGUF model '{}': {}", id, e);
                        }
                    } else if let Some(entry) = registry.get_model(&id) {
                        let mut s = entry.settings.clone();
                        if s.tool_calling == ToolCallingMode::Auto {
                            s.tool_calling = ToolCallingMode::ForceNative;
                            let _ = registry.update_model_settings(&id, s);
                        }
                    }
                }
                Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
            }
        }

        Self::new(model_id).await
    }

    /// Apply platform-optimised model settings for non-CUDA builds (macOS Metal, CPU).
    ///
    /// On Apple Silicon (M1-M4), enables full Metal GPU offload, flash attention,
    /// and sets a reasonable 8K context window. Without this, ALL inference runs
    /// on CPU despite Metal being available — `n_gpu_layers` defaults to `None`.
    #[cfg(not(feature = "cuda"))]
    fn apply_platform_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings, ToolCallingMode,
        };

        let settings = ModelSettings {
            // Full GPU offload — Apple Silicon has unified memory so all layers
            // fit without any CPU/GPU split.
            n_gpu_layers: Some(99),
            // Dynamic: let Goose's estimate_max_context_for_memory() calculate
            // from available RAM + model KV cache cost per token. No hardcoded cap.
            context_size: None,
            // Batch 512 is optimal for Metal prefill throughput.
            n_batch: Some(512),
            // Flash attention reduces KV-cache memory by ~40%.
            flash_attention: Some(true),
            // Unified memory — mlock is unnecessary and can cause issues.
            use_mlock: false,
            // Native tool calling forced ON — Gemma 4 produces
            // <|tool_call>call:NAME{...}<tool_call|> in its trained format. The
            // GGUF's embedded (Jinja) chat template renders tool declarations;
            // ChatTemplate::Embedded is the default so no override is needed.
            tool_calling: ToolCallingMode::ForceNative,
            // Thinking OFF — GIAP handles thinking display through its own
            // PromptState + ThoughtFilter pipeline, not llama.cpp's native
            // reasoning_format which causes Gemma 4 E2B to produce immediate EOS.
            // Let llama.cpp auto-detect thread count (good on Apple Silicon).
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                if let Err(e) = registry.update_model_settings(model_id, settings) {
                    tracing::debug!(
                        "Platform settings not applied to '{}' (model not yet registered): {}",
                        model_id,
                        e
                    );
                } else {
                    tracing::info!(
                        "{} settings applied to model '{}' (n_gpu_layers=99, ctx=dynamic, flash_attn=true)",
                        // On aarch64 Linux this branch means the `cuda` feature
                        // was NOT compiled in, so the n_gpu_layers=99 below is a
                        // request no backend will honour — inference runs on the
                        // CPU. Saying "Metal" there sent me hunting a settings
                        // bug for an hour when the binary was simply built wrong.
                        if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
                            "CPU-ONLY (built without the cuda feature) —"
                        } else {
                            "Metal/platform"
                        },
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not acquire model registry lock for platform settings: {}",
                    e
                );
            }
        }
    }

    /// Patch the Goose model registry with Jetson Orin Nano–optimised settings.
    ///
    /// These settings are applied at startup and saved to `~/.local/share/goose/
    /// models/registry.json` so they persist across restarts on Jetson.
    ///
    /// The function silently ignores errors (model not yet downloaded, registry
    /// lock poisoned) — defaults will be used in that case.
    ///
    /// ## Memory-fit fail-closed (Phase 6 — FUTURE on-Jetson work, NOT done here)
    ///
    /// `n_gpu_layers = 99` requests full GPU residency. On the 8 GB Jetson this
    /// SILENTLY partial-offloads to CPU when the model exceeds the unified-memory
    /// budget (e.g. the ~5.6 GB `gemma3n:e2b`), collapsing decode to single-digit
    /// tok/s. The UI-side memory-fit guard now WARNS about this before load
    /// (`FitBadge` / `warn_if_model_spills`), but the loader itself does not yet
    /// fail closed. The recommended on-device enforcement, to add here when it can
    /// be validated on real Jetson hardware:
    ///
    ///   1. Before requesting `-ngl 99`, compare the model's on-disk size against
    ///      `MemAvailable` (see `ResourceAwareModelScheduler::memory_status`),
    ///      reserving ~1 GB headroom for KV cache + system.
    ///   2. If it will not fit, run `echo 3 > /proc/sys/vm/drop_caches` (root) to
    ///      free the page cache first — otherwise the `-ngl` allocation hits the
    ///      NvMap OOM wall (error 12). See `scripts/jetson/llama-optimization`.
    ///   3. Re-check after dropping caches; if it STILL won't fit, refuse the
    ///      full-GPU load (fail closed) and surface the spill to the UI rather
    ///      than silently degrading to a CPU/GPU split.
    ///
    /// This is intentionally NOT implemented in the cross-platform loader: it is
    /// unsafe to change from the macOS Metal build and cannot be tested here.
    /// Context size that fits THIS model in the Jetson's LLM budget.
    ///
    /// A single hardcoded constant is wrong, and shipping one OOM-killed a
    /// device: 16384 derived from E2B applied to E4B exceeds the budget and the
    /// kernel kills the server (it took gnome-shell with it).
    ///
    /// # The cost model, measured 2026-08-12
    ///
    /// The per-token figures this comment used to carry -- E2B ~18 KiB, E4B ~86
    /// KiB -- were estimates from attention geometry, and both are wrong,
    /// because the SHAPE is wrong. Gemma 4 is **interleaved sliding-window
    /// attention with KV sharing**, so llama.cpp allocates TWO caches and only
    /// one of them scales with `n_ctx`:
    ///
    /// | model | scales with n_ctx | fixed (1024-cell SWA window) |
    /// |---|---|---|
    /// | E2B | 3 layers, 6 KiB/token | 12 MiB |
    /// | E4B | 4 layers, 16 KiB/token | 40 MiB |
    ///
    /// Read off `llama_kv_cache: size = ...` at n_ctx 4096, 16384 and 32768:
    /// the first line grows exactly linearly and the second does not move.
    /// So the honest model is `slope * n_ctx + constant`, not `rate * n_ctx`.
    ///
    /// # Why the slope below is still pessimistic, and what would change it
    ///
    /// **That measurement was taken with brew llama.cpp b9110 on Metal. The
    /// engine runs vendored `llama-cpp-sys-2 =0.1.146`, which is much older.**
    /// If its llama.cpp lacks the iswa split or the KV sharing, every one of the
    /// 42 layers stores full-context KV instead of four -- 4 KiB per layer per
    /// token from the same measurement, so ~168 KiB/token.
    ///
    /// That is the number below, and the arithmetic is uncomfortable: at 8192 it
    /// needs 1344 MiB against 1367 MiB free, which fits by 23 MiB, and at 16384
    /// it needs 2688 MiB, which does not. The current 8192 for E4B is therefore
    /// where the pessimistic case *just* survives -- consistent with the board
    /// running today, and a reason not to raise the ceiling on the strength of a
    /// Metal measurement against a different llama.cpp.
    ///
    /// # Raising `MAX_CTX` alone does nothing today
    ///
    /// Worth knowing before trying it. Under the slope below the BUDGET binds
    /// first for both models -- E4B at 8,331 tokens and E2B at 18,265, each then
    /// rounded down to a power of two -- so `MAX_CTX` is not the active
    /// constraint on either. It was under the old, wrongly-shaped 96 KiB/token
    /// model, which is where the belief that it caps E2B comes from.
    ///
    /// The lever is `KV_KIB_PER_TOKEN`, and that is precisely the one that needs
    /// the device.
    ///
    /// **The check that settles it is one line on the device**: load E4B and
    /// read the two `llama_kv_cache: size` lines. Two caches with a fixed second
    /// one means the measured slope holds, E4B's real ceiling is ~84k tokens,
    /// and both `KV_KIB_PER_TOKEN` and `MAX_CTX` can rise. One cache covering 42
    /// layers means the pessimistic slope is right and nothing moves.
    ///
    /// `apply_jetson_settings` re-stamps the registry at every provider init,
    /// so this cannot be worked around by editing registry.json — it has to be
    /// right here.
    ///
    /// Derived rather than tabulated so a model we have never seen is still
    /// safe: KV budget is the LLM budget minus the weights and the compute
    /// buffers, divided by a per-token cost chosen for the widest attention
    /// geometry we ship. Rounded down to a power of two and clamped, because
    /// being a little conservative costs history and being wrong costs the box.
    ///
    /// The deeper fix belongs in the engine: `context_cap` gives a pinned
    /// `context_size` and a host `GOOSE_CONTEXT_LIMIT` priority over its own
    /// `estimate_max_context_for_memory`, so the one function that knows the
    /// real geometry is the one that never gets consulted. Capping those two
    /// branches by the memory estimate would make this helper unnecessary.
    ///
    /// Compiled on every platform even though only the CUDA build calls it: the
    /// arithmetic is pure, it is the part that can kill a board, and gating it
    /// meant neither it nor its tests ever ran on a developer machine or in CI.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn jetson_context_size(model_bytes: u64) -> u32 {
        /// Per-token cost of the cache that GROWS with `n_ctx`.
        ///
        /// 168, not the measured 16, and the doc above says why: this is the
        /// no-iswa worst case (42 layers x 4 KiB/layer/token) for an engine
        /// whose llama.cpp may predate the split-cache implementation. It is
        /// the one constant here that can OOM the device, so it holds the
        /// pessimistic value until somebody reads the real allocation on the
        /// Orin.
        const KV_KIB_PER_TOKEN: u64 = 168;
        /// The SWA cache, which does NOT scale with `n_ctx` -- 12 MiB on E2B,
        /// 40 MiB on E4B, flat from 4096 to 32768. Taken at the larger, since
        /// the budget must hold for the larger model.
        ///
        /// The old model had no constant term at all, which is why its
        /// per-token rate had to absorb one and came out wrong in both
        /// directions depending on `n_ctx`.
        const KV_FIXED_MB: u64 = 40;
        /// llama.cpp's compute buffers. Nearly flat in `n_ctx` -- measured
        /// 522 MiB at both 4096 and 16384, rising to 582 MiB at 32768 -- so 600
        /// covers the range this function can return.
        const COMPUTE_BUFFER_MB: u64 = 600;
        const MIN_CTX: u32 = 2048;
        const MAX_CTX: u32 = 16384;

        let model_mb = model_bytes / (1024 * 1024);
        let kv_mb = crate::scheduler::LLM_BUDGET_MB
            .saturating_sub(model_mb)
            .saturating_sub(COMPUTE_BUFFER_MB)
            .saturating_sub(KV_FIXED_MB);
        let tokens = (kv_mb * 1024) / KV_KIB_PER_TOKEN;

        // Largest power of two that fits, clamped.
        let mut ctx = MIN_CTX;
        while (ctx as u64) * 2 <= tokens && ctx < MAX_CTX {
            ctx *= 2;
        }
        ctx.clamp(MIN_CTX, MAX_CTX)
    }

    #[cfg(feature = "cuda")]
    fn apply_jetson_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings, ToolCallingMode,
        };

        // Size the context to THIS model. Read the weights' size from the
        // registry entry we are about to stamp; if the row or the file is not
        // there yet, assume the largest model we ship so the first load is
        // conservative rather than fatal.
        const ASSUMED_LARGEST_MODEL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
        let model_bytes = get_registry()
            .lock()
            .ok()
            .and_then(|reg| {
                reg.get_model(model_id)
                    .and_then(|e| std::fs::metadata(&e.local_path).ok())
                    .map(|m| m.len())
            })
            .unwrap_or(ASSUMED_LARGEST_MODEL_BYTES);
        let context_size = Self::jetson_context_size(model_bytes);
        tracing::info!(
            model = model_id,
            model_mb = model_bytes / (1024 * 1024),
            context_size,
            "Jetson context sized to fit this model's KV cache in the LLM budget"
        );

        let jetson_settings = ModelSettings {
            // Full GPU offload: Jetson unified memory means all layers fit in
            // the same 8 GB pool — no split between CPU and GPU DRAM.
            n_gpu_layers: Some(99),
            // 16384-token context.
            //
            // 4096 was chosen when the KV cost was assumed rather than measured,
            // and it left a fresh turn at 80% before the user had said anything:
            // the turn-1 prompt (system prefix + native tools JSON for every
            // giap extension) measures ~3,250 tokens. Two turns in, a `thinking`
            // block would overrun the window mid-generation and goose would
            // compact the conversation away to recover.
            //
            // The real cost, read from llama.cpp's own allocation on this board:
            // 24 MiB non-SWA + 48 MiB SWA = 72 MiB for 4096 cells, i.e. ~18 KiB
            // per token, because Gemma 4 E2B has n_head_kv = 1. 16384 therefore
            // costs ~288 MiB against ~5.5 GiB free with the model resident, and
            // the two buffers (96 + 192 MiB) stay clear of the ~586 MiB NvMap
            // single-allocation wall.
            //
            // This is affordable now in a way it was not before: the prompt-
            // session KV cache means a longer window buys history that is
            // re-used rather than re-prefilled every turn.
            context_size: Some(context_size),
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
            // Native tool calling forced ON — Gemma 4 produces tool calls in its
            // trained format; the GGUF's embedded (Jinja) chat template renders the
            // declarations (ChatTemplate::Embedded is the default).
            tool_calling: ToolCallingMode::ForceNative,
            // Thinking OFF — GIAP handles thinking via PromptState + ThoughtFilter.
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                if let Err(e) = registry.update_model_settings(model_id, jetson_settings) {
                    tracing::debug!(
                        "Jetson settings not applied to '{}' (model not yet registered): {}",
                        model_id,
                        e
                    );
                } else {
                    tracing::info!(
                        "Applied Jetson Orin Nano CUDA settings to model '{}'",
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not acquire model registry lock for Jetson settings: {}",
                    e
                );
            }
        }
    }
}

/// Strip thinking-token preambles emitted by reasoning-capable models.
///
/// Handles two formats:
///
/// 1. **Gemma 4**: `<|channel>thought … <channel|>ACTUAL REPLY`
///    Everything after the last `<channel|>` is the real response.
///
/// 2. **Qwen3 / DeepSeek-R1 / QwQ**: `<think>…</think>ACTUAL REPLY`
///    Everything inside `<think>…</think>` tags is stripped.
///
/// If neither pattern is present the original text is returned unchanged.
fn strip_thinking_tokens(text: &str) -> String {
    // Gemma 4 format — return everything after the last <channel|>.
    // If nothing follows the tag, return empty (the tag was the entire text).
    const CHANNEL_CLOSE: &str = "<channel|>";
    if let Some(pos) = text.rfind(CHANNEL_CLOSE) {
        return text[pos + CHANNEL_CLOSE.len()..].trim().to_string();
    }

    // Also handle <thought>…</thought> (alternate reasoning tag format)
    if text.contains("<thought>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<thought>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</thought>") {
                    rest = &rest[start + end + "</thought>".len()..];
                } else {
                    break; // unclosed — discard tail
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        return String::new();
    }

    // <think>…</think> format — strip all blocks
    if text.contains("<think>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<think>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</think>") {
                    rest = &rest[start + end + "</think>".len()..];
                } else {
                    // Unclosed <think> — discard the rest
                    break;
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    text.to_string()
}

#[async_trait]
impl LlmProvider for LocalInferenceLlmAdapter {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        let name = self.inner.model_name();
        pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(&name)
    }

    async fn complete(&self, system: &str, messages: Vec<ChatMessage>) -> Result<ChatMessage> {
        let mut msg = self.inner.complete(system, messages).await?;
        msg.content = strip_thinking_tokens(&msg.content);
        Ok(msg)
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

impl LocalInferenceLlmAdapter {
    /// Like `complete()` but returns the RAW model output WITHOUT applying
    /// `strip_thinking_tokens()`. Useful for diagnostics — see what the model
    /// actually emits before any post-processing.
    pub async fn raw_complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        self.inner.complete(system, messages).await
    }
}

// ── Unit tests (no model weights required) ────────────────────────────────────

#[cfg(test)]
mod tests {

    /// The two models GIAP actually ships on the Orin, by measured file size.
    ///
    /// E4B at 16384 is what OOM-killed the device: ~4.6 GiB of weights plus
    /// against a 6,392 MB budget. It must come back smaller than E2B's.
    ///
    /// No longer `#[cfg(feature = "cuda")]`: this is pure arithmetic, it is the
    /// part that can kill a board, and gating it meant it never ran on a
    /// developer machine or in CI -- the only places it CAN run, since the
    /// device build is `cargo check`-only.
    #[test]
    fn jetson_context_fits_each_model_in_the_budget() {
        let e2b = LocalInferenceLlmAdapter::jetson_context_size(2_890_000_000);
        let e4b = LocalInferenceLlmAdapter::jetson_context_size(4_640_000_000);
        assert_eq!(e2b, 16384, "E2B should keep the full window");
        assert!(e4b <= 8192, "E4B must shrink, got {e4b}");
        assert!(e4b >= 2048, "E4B must stay usable, got {e4b}");
        assert!(e4b < e2b, "a bigger model must not get a bigger context");
    }

    /// The pessimistic slope is the one that keeps E4B inside the budget, and
    /// this pins the margin rather than the verdict.
    ///
    /// At the no-iswa worst case the answer fits by ~23 MiB out of 1,367, and a
    /// reader who changes `KV_KIB_PER_TOKEN` or `MAX_CTX` without measuring on
    /// the device should see how little room there was. It is deliberately
    /// arithmetic this test redoes rather than a number copied from the
    /// function -- a test that recomputed it the same way would agree with any
    /// mistake.
    #[test]
    fn e4b_at_its_current_window_only_just_fits_the_pessimistic_budget() {
        const NO_ISWA_KIB_PER_TOKEN: u64 = 168;
        let budget_mb = crate::scheduler::LLM_BUDGET_MB;
        let weights_mb = 4_640_000_000u64 / (1024 * 1024);
        let free_mb = budget_mb - weights_mb - 600 - 40;

        let chosen = LocalInferenceLlmAdapter::jetson_context_size(4_640_000_000) as u64;
        let needed_mb = (chosen * NO_ISWA_KIB_PER_TOKEN) / 1024;
        assert!(
            needed_mb <= free_mb,
            "E4B was given {chosen} tokens, which needs {needed_mb} MiB of KV under the no-iswa \
             worst case against {free_mb} MiB free. That is the case the device might be in, and \
             exceeding it is what OOM-killed the board and took gnome-shell with it."
        );

        // And the other direction: doubling it does NOT fit, which is why the
        // ceiling has not moved on the strength of a Mac measurement.
        let doubled_mb = (chosen * 2 * NO_ISWA_KIB_PER_TOKEN) / 1024;
        assert!(
            doubled_mb > free_mb,
            "doubling E4B's window now fits the pessimistic budget ({doubled_mb} MiB vs \
             {free_mb} MiB free). Either the budget grew or the slope changed -- if the device \
             has been measured and really does split its KV cache, raise MAX_CTX deliberately \
             and rewrite this test rather than deleting it."
        );
    }

    /// What the MEASURED geometry would allow, kept as an executable record of
    /// the 2026-08-12 measurement so the number is not lost in a commit message.
    ///
    /// Asserts nothing about production. It exists so that whoever runs the
    /// one-line check on the Orin can see immediately what is unlocked: E4B goes
    /// from 8,192 to roughly 84,000 tokens of headroom, which `MAX_CTX` would
    /// then be the only thing capping.
    #[test]
    fn the_measured_geometry_would_allow_far_more_than_the_pessimistic_one() {
        // Measured on llama.cpp b9110 / Metal at n_ctx 4096, 16384 and 32768:
        // E4B's growing cache is 16 KiB/token and its SWA cache is a flat 40 MiB.
        const MEASURED_KIB_PER_TOKEN: u64 = 16;
        let weights_mb = 4_640_000_000u64 / (1024 * 1024);
        let free_mb = crate::scheduler::LLM_BUDGET_MB - weights_mb - 600 - 40;
        let would_allow = (free_mb * 1024) / MEASURED_KIB_PER_TOKEN;

        assert!(
            would_allow > 80_000,
            "the measured slope should leave E4B room for >80k tokens, got {would_allow}"
        );
        assert!(
            would_allow > 8 * LocalInferenceLlmAdapter::jetson_context_size(4_640_000_000) as u64,
            "the measured geometry allows less than 8x what production gives E4B; either the \
             measurement or the production constant has changed and the gap this test records \
             no longer exists"
        );
    }

    /// A model larger than the whole budget must still return something
    /// loadable rather than zero or a panic.
    #[test]
    fn jetson_context_floors_for_an_oversized_model() {
        assert_eq!(
            LocalInferenceLlmAdapter::jetson_context_size(9_000_000_000),
            2048
        );
    }
    /// Integration tests that require a real model are marked `#[ignore]` and
    /// gated on the `GIAP_TEST_MODEL_PATH` environment variable.
    ///
    /// Run with:
    /// ```bash
    /// GIAP_TEST_MODEL_PATH=/path/to/model.gguf \
    ///   cargo test -p pond-adapters-local-inference -- --ignored
    /// ```
    use super::*;

    #[test]
    fn default_model_constant_is_set() {
        assert!(!DEFAULT_MODEL.is_empty());
    }

    #[test]
    fn default_model_has_huggingface_format() {
        // Expected: "org/repo-GGUF:QUANTIZATION"
        assert!(
            DEFAULT_MODEL.contains('/'),
            "DEFAULT_MODEL should be a HuggingFace repo path: {DEFAULT_MODEL}"
        );
        assert!(
            DEFAULT_MODEL.contains(':'),
            "DEFAULT_MODEL should have a quantization suffix (':'): {DEFAULT_MODEL}"
        );
    }

    #[test]
    fn associated_constant_matches_module_constant() {
        assert_eq!(
            LocalInferenceLlmAdapter::DEFAULT_MODEL,
            DEFAULT_MODEL,
            "associated constant must re-export the same value"
        );
    }

    // ── data_dir path-construction logic (no model weights required) ──────────

    /// Exercise the filename-derivation logic inside `new_with_data_dir` without
    /// touching the filesystem or loading a model.  We cannot call
    /// `new_with_data_dir` directly (it eventually calls `LocalInferenceProvider::
    /// from_env` which tries to download weights), so we replicate the pure
    /// filename logic here and assert the expected result.
    #[test]
    fn data_dir_filename_derivation_strips_gguf_suffix() {
        let model_id = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "Llama-3.2-3B-Instruct-Q4_K_M.gguf");
    }

    #[test]
    fn data_dir_filename_without_gguf_suffix_kept_as_is() {
        let model_id = "bartowski/SomeModel:Q8_0";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "SomeModel-Q8_0.gguf");
    }

    #[test]
    fn data_dir_gguf_path_is_under_models_gguf() {
        let data_dir = std::path::Path::new("/home/user/.giap");
        let filename = "Llama-3.2-3B-Instruct-Q4_K_M.gguf";
        let gguf_dir = data_dir.join("models").join("gguf");
        let local_path = gguf_dir.join(filename);

        assert_eq!(
            local_path.to_string_lossy(),
            "/home/user/.giap/models/gguf/Llama-3.2-3B-Instruct-Q4_K_M.gguf"
        );
    }

    #[test]
    fn data_dir_source_url_is_huggingface_resolve() {
        let repo_id = "bartowski/Llama-3.2-3B-Instruct-GGUF";
        let filename = "Llama-3.2-3B-Instruct-Q4_K_M.gguf";
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        assert!(source_url.starts_with("https://huggingface.co/"));
        assert!(source_url.contains("/resolve/main/"));
        assert!(source_url.ends_with(filename));
    }

    #[test]
    fn strip_thinking_tokens_removes_gemma4_preamble() {
        // Actual Gemma 4 format: opening = <|channel>thought, closing = <channel|>
        let raw = "<|channel>thought Some reasoning here.<channel|>Hello! I am Goose.";
        assert_eq!(strip_thinking_tokens(raw), "Hello! I am Goose.");
    }

    #[test]
    fn strip_thinking_tokens_no_tag_returns_original() {
        let raw = "Hello! I am Goose.";
        assert_eq!(strip_thinking_tokens(raw), "Hello! I am Goose.");
    }

    #[test]
    fn strip_thinking_tokens_multiline_thinking() {
        let raw = "<|channel>thought\nStep 1.\nStep 2.\n<channel|>The answer is 4.";
        assert_eq!(strip_thinking_tokens(raw), "The answer is 4.");
    }

    #[test]
    fn strip_thinking_tokens_uses_last_close_tag() {
        // If multiple <channel|> appear, we take everything after the last one
        let raw = "<|channel>thought Step 1.<channel|>intermediate<channel|>Final answer.";
        assert_eq!(strip_thinking_tokens(raw), "Final answer.");
    }

    #[test]
    fn strip_thinking_tokens_channel_close_at_end_returns_empty() {
        // Edge case: <channel|> at end with nothing after → should return empty,
        // not the original text containing the tag.
        let raw = "<|channel>thought reasoning here<channel|>";
        assert_eq!(strip_thinking_tokens(raw), "");
    }

    #[test]
    fn strip_thinking_tokens_removes_thought_tags() {
        let raw = "<thought>internal reasoning</thought>Hello!";
        assert_eq!(strip_thinking_tokens(raw), "Hello!");
    }

    #[test]
    fn strip_thinking_tokens_thought_only_returns_empty() {
        let raw = "<thought>only reasoning</thought>";
        assert_eq!(strip_thinking_tokens(raw), "");
    }

    #[test]
    fn model_id_rsplit_fallback_uses_q4_k_m() {
        // When no ':' quantization suffix is present, rsplit_once returns None
        // and the fallback "Q4_K_M" is used.
        let model_id = "some-model-without-quant";
        let (_repo_id, quantization) = model_id.rsplit_once(':').unwrap_or((model_id, "Q4_K_M"));
        assert_eq!(quantization, "Q4_K_M");
    }
}
