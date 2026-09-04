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

/// Whether this build can reach CUDA at all.
///
/// The one fact `pond-server` cannot work out for itself: `cuda` is a feature of
/// THIS crate, passed on the command line by `scripts/jetson/deploy.sh`, so a
/// `cfg!` in the binary would always read false. Exported as a const rather than
/// a function so it is a compile-time constant at the call site and cannot drift
/// from the feature that produced it.
pub const CUDA_ENABLED: bool = cfg!(feature = "cuda");

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

            let (tool_mode, thinking) = Self::registration_settings(&local_path);
            {
                match get_registry().lock() {
                    Ok(mut registry) => {
                        if !registry.has_model(&stem) {
                            let mut settings = ModelSettings::default();
                            settings.tool_calling = tool_mode;
                            settings.enable_thinking = thinking;
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
                            if s.tool_calling != tool_mode || s.enable_thinking != thinking {
                                s.tool_calling = tool_mode;
                                s.enable_thinking = thinking;
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
        let (tool_mode, thinking) = Self::registration_settings(&local_path);
        {
            match get_registry().lock() {
                Ok(mut registry) => {
                    if !registry.has_model(&id) {
                        let mut settings = ModelSettings::default();
                        settings.tool_calling = tool_mode;
                        settings.enable_thinking = thinking;
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
                        if s.tool_calling != tool_mode || s.enable_thinking != thinking {
                            s.tool_calling = tool_mode;
                            s.enable_thinking = thinking;
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

        // Ask the model what it can do. A file we cannot read leaves goose its
        // own judgement (`Auto`) rather than inheriting the old blanket
        // ForceNative.
        let probe = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::probe_model(&p));
        let (tools, thinking) = match &probe {
            Some(p) => Self::tool_and_thinking_for(p),
            None => (ToolCallingMode::Auto, true),
        };
        if let Some(p) = &probe {
            tracing::info!(
                model = model_id,
                tools = ?p.tools,
                thinking = ?p.thinking,
                "model capabilities read from its chat template"
            );
        }

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
            // Tool calling and thinking now come from the model's own chat
            // template rather than being forced. Gemma renders declarations and
            // keeps ForceNative; a template with no `tools` variable gets
            // ForceEmulated instead of declarations with nowhere to go.
            tool_calling: tools,
            enable_thinking: thinking,
            // `enable_thinking` is set above from the template rather than
            // left to inherit goose's `default_true()`. It said "Thinking OFF"
            // here for a long time while the code set nothing and the registry
            // on the device read `true` -- the comment described an intention
            // the code never carried out.
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
    /// The tool mode and thinking flag a GGUF at `path` should be registered
    /// with, read from its own chat template.
    ///
    /// Registration happens in `new_with_data_dir`, before any
    /// `apply_*_settings` runs, and it used to hardcode `ForceNative` in four
    /// places. Two of those only fired when the stored mode was `Auto`, which
    /// meant an entry already persisted as `ForceNative` was never revisited --
    /// so a model registered before the probe existed kept a mode its template
    /// cannot honour, forever.
    ///
    /// That is not hypothetical. DeepSeek-R1-Distill ended up with two registry
    /// rows: the quant-tagged id read `force_emulated` from the probe while the
    /// canonical stem still read `force_native`, and the stem is what the turn
    /// resolved to. The model was handed native tool declarations by a template
    /// with no `tools` variable, saw none of them, and invented an "MCP" tool
    /// interface out of the system prompt instead.
    ///
    /// So this re-stamps rather than upgrading, which matches what
    /// `apply_*_settings` already does to the same rows at every provider init.
    /// A file that cannot be read yields `Auto`, leaving goose its own
    /// judgement rather than a guess of ours.
    fn registration_settings(
        path: &std::path::Path,
    ) -> (
        goose::providers::local_inference::local_model_registry::ToolCallingMode,
        bool,
    ) {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        match Self::probe_model(path) {
            Some(probe) => Self::tool_and_thinking_for(&probe),
            None => (ToolCallingMode::Auto, true),
        }
    }

    /// What the registry should say for a model, given what its own file says
    /// it can do.
    ///
    /// Kept as a pure function over [`ModelProbe`] on purpose: the two callers
    /// are `apply_jetson_settings` (CUDA-gated, compiles only on the device) and
    /// `apply_platform_settings`. A decision buried in either would be tested by
    /// neither on a developer machine, and the CUDA one is compiled by nothing
    /// in CI.
    ///
    /// # Tools
    ///
    /// Both callers used to set `ForceNative` unconditionally. That is right for
    /// Gemma and wrong for the first model whose template takes no `tools`
    /// variable -- DeepSeek-R1-Distill, already on the development machine,
    /// renders no declarations at all, so forcing native puts them nowhere.
    ///
    /// - `Native`  -> `ForceNative`, as before.
    /// - `Absent`  -> `ForceEmulated`: the template cannot carry tools, so they
    ///   have to be described in the system prompt or not offered.
    /// - `Unknown` -> `Auto`: no template was readable, so leave goose its own
    ///   judgement rather than overriding it with a guess.
    ///
    /// # Thinking
    ///
    /// `enable_thinking` was never set, so it inherited goose's `default_true()`
    /// while the comment above it claimed "Thinking OFF". The registry on the
    /// device sided with the code. This states the value instead of inheriting
    /// it, and does not change what any currently-reasoning model does: a gated
    /// thinker still gets `true`.
    ///
    /// A model with no reasoning markers gets `false`, which is the only case
    /// this changes, and it changes it from "flag set for a model that has
    /// nothing to flag" to "off".
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn tool_and_thinking_for(
        probe: &pond_core::models::domain::model_probe::ModelProbe,
    ) -> (
        goose::providers::local_inference::local_model_registry::ToolCallingMode,
        bool,
    ) {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{Thinking, ToolSupport};

        let tools = match probe.tools {
            ToolSupport::Native => ToolCallingMode::ForceNative,
            ToolSupport::Absent => ToolCallingMode::ForceEmulated,
            ToolSupport::Unknown => ToolCallingMode::Auto,
        };
        let thinking = matches!(
            probe.thinking,
            Thinking::Gated { .. } | Thinking::Always { .. }
        );
        (tools, thinking)
    }

    /// Read a model's own account of itself, for the settings above.
    ///
    /// Walks far enough to reach `tokenizer.chat_template`, which sits 3.8-15 MB
    /// into a GGUF, behind the token array. Costs a few hundred kilobytes of
    /// real reading and about 40 ms, because everything between the keys it
    /// wants is stepped over rather than read.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn probe_model(
        path: &std::path::Path,
    ) -> Option<pond_core::models::domain::model_probe::ModelProbe> {
        use pond_core::models::domain::gguf::parse_gguf_file;
        use pond_core::models::domain::model_probe::ModelProbe;
        parse_gguf_file(path).map(|info| ModelProbe::from_gguf(&info))
    }

    /// The model's KV cost per token, read from its own GGUF header, or `None`
    /// when the header cannot settle it.
    ///
    /// `pond_core::models::domain::gguf` does the arithmetic and is exact for a
    /// dense model. The one thing the header does NOT carry is the split
    /// between full-attention and sliding-window layers, and that split is
    /// worth a factor of two: assume more SWA layers than a model really has
    /// and the cost comes out LOW, which is the direction that OOMs a board.
    ///
    /// So the rule is asymmetric on purpose:
    ///
    /// - **No `key_length_swa`** — the model is dense, every owning layer pays
    ///   the same width, and the pattern cannot change the answer. Trust it for
    ///   any architecture.
    /// - **`key_length_swa` present** — the answer depends on a ratio the file
    ///   does not state. Trust it only for an architecture whose pattern has
    ///   been confirmed against a real allocation on the device.
    ///
    /// Anything else returns `None` and the caller keeps the measured constant,
    /// so an unfamiliar model behaves exactly as it did before this existed.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn kv_cost_from_header(path: &std::path::Path) -> Option<u64> {
        use pond_core::models::domain::gguf::parse_gguf_header;

        /// Architectures whose global:SWA layer ratio has been confirmed against
        /// llama.cpp's own `llama_kv_cache ... size = N MiB (C cells, L layers)`
        /// lines on the Orin. Gemma 4: 1 global per 5 sliding, verified at both
        /// sizes (E4B 4+20 of 24 owning layers, E2B 3+12 of 15).
        const CONFIRMED_SWA_PATTERNS: &[(&str, u32)] = &[("gemma4", 5)];

        // Geometry sits in the first ~2 KB of every file measured -- offsets
        // 924-1,829 on gemma-4-E2B, well before `tokenizer.ggml.tokens` at
        // 2,061. 64 KiB is generous cover for that without reading the token
        // array, let alone the 15 MB it takes to reach the chat template.
        const HEAD_BYTES: usize = 64 * 1024;

        let mut buf = vec![0u8; HEAD_BYTES];
        let n = {
            use std::io::Read as _;
            let mut f = std::fs::File::open(path).ok()?;
            f.read(&mut buf).ok()?
        };
        buf.truncate(n);

        let info = parse_gguf_header(&buf)?;
        let arch = info.architecture.as_deref().unwrap_or_default();

        if info.key_length_swa.is_none() {
            // Dense: exact whatever the architecture. The ratio argument is
            // unused on this path.
            return info.kv_kib_per_token(0);
        }

        let (_, swa_per_global) = CONFIRMED_SWA_PATTERNS
            .iter()
            .find(|(name, _)| *name == arch)?;
        info.kv_kib_per_token(*swa_per_global)
    }

    /// Context size that fits THIS model in the Jetson's LLM budget.
    ///
    /// A single hardcoded constant is wrong, and shipping one OOM-killed a
    /// device: 16384 derived from E2B applied to E4B exceeds the budget and the
    /// kernel kills the server (it took gnome-shell with it).
    ///
    /// # The cost model, measured ON THE DEVICE 2026-08-12
    ///
    /// | model | first cache | second cache | total |
    /// |---|---|---|---|
    /// | E2B | 3 layers, 6 KiB/tok | 12 layers, 12 KiB/tok | **18 KiB/token** |
    /// | E4B | 4 layers, 16 KiB/tok | 20 layers, 40 KiB/tok | **56 KiB/token** |
    ///
    /// Read off `llama_kv_cache ... size = N MiB (C cells, L layers)` with the
    /// service stopped: E4B at n_ctx 8192 gives 128 MiB + 320 MiB, E2B at 16384
    /// gives 96 MiB + 192 MiB. **Both caches carry `n_ctx` cells**, so the cost
    /// is linear with no constant term.
    ///
    /// # A correction, because the first attempt was wrong in the unsafe direction
    ///
    /// I measured this on a Mac first (brew llama.cpp b9110, Metal) and got a
    /// different SHAPE: E4B's second cache sat at a fixed 1024 cells / 40 MiB
    /// whatever `n_ctx` was, implying 16 KiB/token plus a constant -- about a
    /// third of the real cost. I then checked the device's vendored source,
    /// found `llama-kv-cache-iswa.cpp` and the `sliding_window_pattern` key being
    /// read for `gemma4`, and concluded the device took the same path.
    ///
    /// **It does, and that was not enough.** `llama-cpp-sys-2 =0.1.146` builds
    /// the two caches but sizes BOTH to `n_ctx`; shrinking the SWA cache to the
    /// sliding window is a later llama.cpp change. A source grep cannot tell
    /// "the code path exists" from "the allocation is smaller" -- only the
    /// allocation can.
    ///
    /// So the figures this comment carried before any of this -- E2B ~18
    /// KiB/token, E4B ~86 -- were right for the device, and E2B's was exact. The
    /// commit that called them both wrong was itself wrong; the Mac numbers
    /// describe a newer llama.cpp we do not ship.
    ///
    /// The lesson is the one this constant already encoded: it can OOM a board,
    /// so it moves on a measurement from the hardware that runs it and nothing
    /// less. Declining to move it on the Mac numbers is the only reason this is
    /// a comment rather than an incident.
    ///
    /// # What this allows, and what binds instead
    ///
    /// Corrected 2026-08-16, when `JETSON_TOTAL_RAM_MB` stopped claiming the
    /// marketing 8192 and started naming the kernel's real 7620. That removed a
    /// phantom 572 MB the budget had been spending, and the two models parted
    /// company:
    ///
    /// - **E2B (2,962 MB) gets 16,384**, still `MAX_CTX`-bound with ~2,258 MB of
    ///   KV budget against the ~288 MiB it actually uses.
    /// - **E4B (4,746 MB) gets 8,192**, using 448 MiB of KV -- measured, not
    ///   estimated -- against 474 MB free. It is budget-bound with ~26 MB spare.
    ///
    /// E4B at 16,384 was never real: its 896 MiB of KV lands the process near
    /// 7.9 GB on a 7,620 MB board, so it was being served out of swap. The
    /// device measurement that caught it is in `JETSON_TOTAL_RAM_MB`.
    ///
    /// 8,192 is the smallest window that still holds E4B's own turn-1 prompt
    /// (4,678 tokens measured) with room for a reply and some history; 4,096 --
    /// what the old padded slope would now give it -- does not.
    ///
    /// Raising `MAX_CTX` still moves E2B and not E4B, and remains a LATENCY
    /// decision there: a cold prefix costs 4.19 s at 4,096 and 19.97 s at
    /// 16,384, with prefill throughput FALLING as depth grows (976 -> 820
    /// tok/s).
    ///
    /// `apply_jetson_settings` re-stamps the registry at every provider init,
    /// so this cannot be worked around by editing registry.json — it has to be
    /// right here.
    ///
    /// Derived rather than tabulated so a model we have never seen is still
    /// safe: KV budget is the LLM budget minus the weights and the compute
    /// buffers, divided by a per-token cost chosen for the widest attention
    /// geometry we ship. Rounded DOWN to a multiple of `CTX_GRANULARITY` and
    /// clamped, because being a little conservative costs history and being
    /// wrong costs the box -- but only a little, which is why that granularity
    /// is 1024 and no longer a power of two.
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
    fn jetson_context_size(model_bytes: u64, kv_kib_per_token: Option<u64>) -> u32 {
        /// Per-token KV cost for the widest geometry we ship, measured on the
        /// DEVICE: E4B is 56 KiB/token across both caches, E2B 18.
        ///
        /// This is the MEASURED cost, not a padded one, and that changed on
        /// 2026-08-16. It was 64 -- 56 measured plus headroom for a wider model
        /// -- which was free while `JETSON_TOTAL_RAM_MB` claimed the marketing
        /// 8192. Once that was corrected to the kernel's real 7620, the two
        /// paddings compounded: at 64, E4B's budget allows only 7,584 tokens and
        /// rounds to **4096**, which is smaller than E4B's own turn-1 prompt
        /// (4,678 tokens measured from `turn_metrics`). A window that cannot
        /// hold the preamble is not conservative, it is broken -- it thrashes
        /// compaction against tokens that cannot be compacted.
        ///
        /// So the padding moved out of here and into the budget, which is where
        /// it was actually needed. There is NO constant term -- both caches
        /// carry `n_ctx` cells on this llama.cpp.
        ///
        /// What still guards a model we have never seen is `COMPUTE_BUFFER_MB`
        /// (600 against a measured 522) and the clamps -- thinner than before.
        /// A model materially wider than E4B's 56 KiB/token wants the real fix
        /// named below: cap this by the engine's own geometry estimate rather
        /// than guessing a slope here.
        ///
        /// This is the constant that can OOM the board. It moves on a
        /// measurement from the Orin and nothing less; see the correction above,
        /// where the Mac said 16 and the device said 56.
        const KV_KIB_PER_TOKEN: u64 = 56;
        /// llama.cpp's compute buffers. Nearly flat in `n_ctx` -- measured
        /// 522 MiB at both 4096 and 16384, rising to 582 MiB at 32768 -- so 600
        /// covers the range this function can return.
        const COMPUTE_BUFFER_MB: u64 = 600;
        const MIN_CTX: u32 = 2048;
        const MAX_CTX: u32 = 16384;
        /// Round the answer DOWN to a multiple of this.
        ///
        /// This was a power of two until 2026-08-16, and the difference is not
        /// cosmetic: powers of two are 2x apart, so flooring to one discards up
        /// to HALF of a window the budget has already proved affordable. E4B
        /// IQ4_XS (4,496 MB) is allowed 13,220 tokens and was handed 8,192 --
        /// 5,028 tokens thrown away, which is the difference between a window
        /// that holds a conversation and one that compacts from turn one.
        ///
        /// Nothing needed the power of two. `n_ctx` has no such constraint in
        /// llama.cpp (it pads to `n_ubatch` internally), both KV caches simply
        /// carry `n_ctx` cells, and the safety here has never come from the
        /// rounding -- it comes from `KV_KIB_PER_TOKEN`, `COMPUTE_BUFFER_MB` and
        /// the budget, all of which are untouched. Flooring to 1024 is the same
        /// "round down, stay under" rule at a resolution that does not throw
        /// away what the board can afford.
        const CTX_GRANULARITY: u32 = 1024;

        let model_mb = model_bytes / (1024 * 1024);
        let kv_mb = crate::scheduler::LLM_BUDGET_MB
            .saturating_sub(model_mb)
            .saturating_sub(COMPUTE_BUFFER_MB);
        // The model's own header, when it could answer; the conservative
        // fallback when it could not. `kv_cost_from_header` returns None rather
        // than guessing, so this is a strict improvement and never a new risk:
        // an unreadable or unfamiliar model gets exactly the behaviour it had
        // before this existed.
        let slope = kv_kib_per_token
            .filter(|k| *k > 0)
            .unwrap_or(KV_KIB_PER_TOKEN);
        let tokens = (kv_mb * 1024) / slope;

        // Largest multiple of CTX_GRANULARITY that fits, clamped. Saturating at
        // MAX_CTX before the cast keeps a huge allowance (E2B's is ~41k) from
        // wrapping u32.
        let granularity = CTX_GRANULARITY as u64;
        let floored = (tokens / granularity) * granularity;
        floored.min(MAX_CTX as u64).max(MIN_CTX as u64) as u32
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
        let kv_kib = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::kv_cost_from_header(&p));
        let context_size = Self::jetson_context_size(model_bytes, kv_kib);
        tracing::info!(
            model = model_id,
            model_mb = model_bytes / (1024 * 1024),
            kv_kib_per_token = kv_kib.map_or("fallback".to_string(), |k| k.to_string()),
            context_size,
            "Jetson context sized to fit this model's KV cache in the LLM budget"
        );

        let probe = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::probe_model(&p));
        let (tools, thinking) = match &probe {
            Some(p) => Self::tool_and_thinking_for(p),
            None => (ToolCallingMode::Auto, true),
        };
        if let Some(p) = &probe {
            tracing::info!(
                model = model_id,
                tools = ?p.tools,
                thinking = ?p.thinking,
                "model capabilities read from its chat template"
            );
        }

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
            // Also a hard prerequisite for `type_v` below.
            flash_attention: Some(true),
            // KV cache at q8_0. Measured on this board (gemma-4 E4B, ctx 16384):
            // KV 296 -> 157 MiB, peak footprint 437 -> 307 MB. Quality-neutral by
            // two independent tests: greedy output is byte-identical to f16, and a
            // paired per-chunk wikitext-2 run (n = 100) gives dNLL -0.000987 +/-
            // 0.000551, t = -1.79 — indistinguishable from f16 at 95%.
            // q4_0 saves ~75 MiB more but its per-chunk variance is 6.4x higher,
            // so it is not used here.
            type_k: Some("q8_0".to_string()),
            type_v: Some("q8_0".to_string()),
            // Physical batch. The compute buffer is the second-largest allocation
            // after the weights: 522 MiB at the 512 default, 129 MiB at 128, for
            // no measured loss (decode 14.4 vs 14.3 tok/s, prefill 38.3 vs 35.6).
            n_ubatch: Some(128),
            // mlock pins pages in RAM; on unified memory this triggers kernel
            // page faults for every GPU access. Disable for correct performance.
            use_mlock: false,
            // From the model's own template, not forced. See
            // `tool_and_thinking_for`.
            tool_calling: tools,
            enable_thinking: thinking,
            // `enable_thinking` is set above from the template, not inherited.
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

    /// Both shipped models, against the DEVICE-measured cost.
    ///
    /// No longer `#[cfg(feature = "cuda")]`: this is pure arithmetic, it is the
    /// part that can kill a board, and gating it meant it never ran on a
    /// developer machine or in CI -- the only places it CAN run, since the
    /// device build is `cargo check`-only.
    #[test]
    fn jetson_context_fits_each_model_in_the_budget() {
        // The EXACT sizes of the two GGUFs on the device (`stat -Lc %s`,
        // 2026-08-16), not round numbers: this function's answer is a step
        // function of weight size, so a test fed approximations can land on a
        // different step than the board does. E4B in particular was carrying
        // 4_640_000_000 here against a real 4_977_171_584 -- a 336 MB gap, over
        // half of the free KV budget it now has.
        let e2b = LocalInferenceLlmAdapter::jetson_context_size(3_106_738_272, None);
        let e4b = LocalInferenceLlmAdapter::jetson_context_size(4_977_171_584, None);
        assert_eq!(e2b, 16384, "E2B should keep the full window");
        assert_eq!(
            e4b, 8192,
            "E4B should get half the window. It briefly got 16384, on a budget that claimed the \
             marketing 8192 MB of RAM; the kernel reports 7620, and at the real figure E4B's \
             16384 needs 896 MiB of KV it does not have -- it was running out of swap."
        );
        assert!(
            e4b <= e2b,
            "a bigger model must never get a bigger context, got E4B {e4b} vs E2B {e2b}"
        );
    }

    /// E4B at its window fits the MEASURED budget with real headroom, and
    /// doubling again does not fit at all.
    ///
    /// Both halves matter. The first is the safety claim; the second is why
    /// 8192 is the honest ceiling for E4B on memory grounds and not merely
    /// because `MAX_CTX` says so. Arithmetic is redone here rather than copied
    /// from the function, so a test that recomputed it the same way cannot agree
    /// with the same mistake.
    #[test]
    fn e4b_fits_its_window_and_could_not_take_another_doubling() {
        /// Measured on the Orin: 128 MiB + 320 MiB at n_ctx 8192, both caches
        /// carrying n_ctx cells, so 56 KiB/token with no constant term.
        const MEASURED_KIB_PER_TOKEN: u64 = 56;
        let weights_mb = 4_640_000_000u64 / (1024 * 1024);
        let free_mb = crate::scheduler::LLM_BUDGET_MB - weights_mb - 600;

        let chosen = LocalInferenceLlmAdapter::jetson_context_size(4_640_000_000, None) as u64;
        let needed_mb = (chosen * MEASURED_KIB_PER_TOKEN) / 1024;
        assert!(
            needed_mb < free_mb,
            "E4B was given {chosen} tokens, needing {needed_mb} MiB of KV against {free_mb} MiB \
             free. Exceeding this is what OOM-killed the board and took gnome-shell with it."
        );

        let doubled_mb = (chosen * 2 * MEASURED_KIB_PER_TOKEN) / 1024;
        assert!(
            doubled_mb > free_mb,
            "doubling E4B's window now fits ({doubled_mb} MiB vs {free_mb} MiB free), so memory \
             is no longer what caps it. If the budget really grew, raise MAX_CTX deliberately -- \
             and weigh prefill, which is 19.97 s cold at 16384 and degrades with depth."
        );
    }

    /// The two models reach the same window for DIFFERENT reasons, and a reader
    /// changing either constant should know which one they are moving.
    ///
    /// E2B is capped by `MAX_CTX` with enormous room to spare; E4B is capped by
    /// its budget, which happens to round to the same number. Raising `MAX_CTX`
    /// would move E2B and not E4B.
    /// The slope itself, pinned where the ceiling cannot hide it.
    ///
    /// This used to be the ONLY guard on the slope: both shipped models landed
    /// on 16,384, E2B because `MAX_CTX` capped it and E4B because its budget
    /// rounded there, so reverting the slope to the Mac's 16 KiB/token left
    /// every other test green. That mutation was run and passed, which is why
    /// this test exists.
    ///
    /// Since the budget was corrected to the kernel's real 7620 MB, E4B is
    /// budget-bound at 8,192 and guards the slope directly -- lowering it to 16
    /// would hand E4B 16,384 and fail the first test in this file. This one now
    /// earns its place as defence in depth, and as the guard that survives
    /// somebody changing which models ship.
    ///
    /// A model around 4.5 GB sits where the BUDGET decides the answer below the
    /// ceiling, so the slope stays observable: 12,288 at the measured cost,
    /// 16,384 (the clamp) at the Mac's.
    ///
    /// The size is chosen to land MID-BAND -- it affords 12,800 tokens, 512
    /// clear of both 12,288 and 13,312. The previous 4_770_000_000 sat 19
    /// tokens from a boundary and flipped the expected value the moment the
    /// rounding granularity changed, which is a test measuring the floor rather
    /// than the slope it is named for.
    #[test]
    fn the_per_token_slope_is_observable_on_a_model_the_ceiling_does_not_cap() {
        let ctx = LocalInferenceLlmAdapter::jetson_context_size(4_739_563_520, None);
        assert_eq!(
            ctx, 12288,
            "a 4.5 GB model got {ctx} tokens. At the device-measured cost it should get 12288; \
             16384 means the slope has been lowered towards the Mac's 16 KiB/token, which \
             describes a newer llama.cpp than the one this device ships and understates the real \
             allocation by roughly three times."
        );
    }

    /// The granularity itself, because throwing away affordable context is what
    /// this function did for weeks without any test noticing.
    ///
    /// E4B IQ4_XS is the case that exposed it: 4,496 MB of weights leave a KV
    /// budget that affords 13,220 tokens, and the old power-of-two floor handed
    /// back 8,192 -- under the 4,678-token preamble plus growth, so compaction
    /// fired on turn one. Any rounding coarser than this reintroduces that.
    #[test]
    fn rounding_does_not_discard_context_the_budget_affords() {
        // The real IQ4_XS file on the device: 4,715,416,704 bytes.
        let ctx = LocalInferenceLlmAdapter::jetson_context_size(4_715_416_704, None);
        assert_eq!(
            ctx, 12288,
            "E4B IQ4_XS got {ctx}. Its budget affords 13,220 tokens, so anything at or below \
             8192 means the rounding went back to powers of two and is discarding a third of \
             the window the board can actually hold."
        );

        // And the floor still rounds DOWN, never up, at every offset.
        for bytes in [4_600_000_000u64, 4_700_000_000, 4_800_000_000] {
            let ctx = LocalInferenceLlmAdapter::jetson_context_size(bytes, None) as u64;
            let model_mb = bytes / (1024 * 1024);
            let kv_mb = crate::scheduler::LLM_BUDGET_MB
                .saturating_sub(model_mb)
                .saturating_sub(600);
            let affords = (kv_mb * 1024) / 56;
            assert!(
                ctx <= affords.max(2048),
                "{bytes} bytes: handed {ctx} tokens against an affordable {affords}"
            );
            assert_eq!(
                ctx % 1024,
                0,
                "{bytes} bytes: {ctx} is not a multiple of 1024"
            );
        }
    }

    #[test]
    fn e2b_is_ceiling_bound_and_e4b_is_budget_bound() {
        const E2B_KIB_PER_TOKEN: u64 = 18;
        let e2b_weights = 2_890_000_000u64 / (1024 * 1024);
        let e2b_free = crate::scheduler::LLM_BUDGET_MB - e2b_weights - 600;
        let e2b_allows = (e2b_free * 1024) / E2B_KIB_PER_TOKEN;
        assert!(
            e2b_allows > 100_000,
            "E2B's memory should allow far more than it gets ({e2b_allows}); it is MAX_CTX that \
             stops it, and that is a latency decision rather than a memory one"
        );

        const E4B_KIB_PER_TOKEN: u64 = 56;
        let e4b_weights = 4_640_000_000u64 / (1024 * 1024);
        let e4b_free = crate::scheduler::LLM_BUDGET_MB - e4b_weights - 600;
        let e4b_allows = (e4b_free * 1024) / E4B_KIB_PER_TOKEN;
        assert!(
            (8_192..16_384).contains(&e4b_allows),
            "E4B's memory should allow its 8192 window but not a doubling of it, got \
             {e4b_allows}. Outside that range the budget is no longer what binds it and this \
             test's name is a lie."
        );
    }

    /// Wiring the header-derived cost in must not move either shipped model.
    ///
    /// That is the whole reason this could land without the device: E2B
    /// computes 18 KiB/token but is `MAX_CTX`-bound either way, and both E4B
    /// quants compute exactly the 56 the constant already carried. A diff that
    /// changes nothing today changes only models nobody has loaded yet.
    #[test]
    fn header_derived_cost_is_a_no_op_for_the_shipped_models() {
        // (weights, computed KiB/token, expected window)
        let cases = [
            (3_106_738_272u64, 18u64, 16384u32), // E2B Q4_K_M
            (4_977_171_584, 56, 8192),           // E4B Q4_K_M
            (4_715_416_704, 56, 12288),          // E4B IQ4_XS
        ];
        for (bytes, kv, want) in cases {
            let fallback = LocalInferenceLlmAdapter::jetson_context_size(bytes, None);
            let derived = LocalInferenceLlmAdapter::jetson_context_size(bytes, Some(kv));
            assert_eq!(
                derived, want,
                "{bytes} bytes at {kv} KiB/token should give {want}, got {derived}"
            );
            assert_eq!(
                derived, fallback,
                "{bytes} bytes: header-derived {derived} must match the fallback {fallback}                  for a model we already ship -- if this moved, the wiring changed behaviour                  on hardware nobody re-measured"
            );
        }
    }

    /// A cheaper model gets the context its own geometry affords, which is the
    /// point of reading the header at all.
    ///
    /// The size has to be chosen with care: a light model is `MAX_CTX`-bound at
    /// BOTH costs and the comparison proves nothing. At 4,500 MB the KV budget
    /// is 720 MB, which affords 13,166 tokens at 56 KiB (budget-bound, floors
    /// to 12,288) and 26,331 at 28 (ceiling-bound at 16,384). That gap is the
    /// context a blanket constant was quietly charging for.
    #[test]
    fn a_cheaper_model_is_no_longer_charged_the_widest_geometry() {
        let bytes = 4_500u64 * 1024 * 1024;
        let blanket = LocalInferenceLlmAdapter::jetson_context_size(bytes, None);
        let real = LocalInferenceLlmAdapter::jetson_context_size(bytes, Some(28));
        assert!(
            real > blanket,
            "a 28 KiB/token model should get more than the 56 KiB/token fallback allows,              got {real} against {blanket}"
        );
    }

    /// The direction that matters: a WIDER model must be charged more and get
    /// less, rather than inheriting a constant that flatters it.
    ///
    /// 168 KiB/token is the figure this file once carried for E4B before the
    /// device corrected it -- a real number from a real mistake.
    #[test]
    fn a_wider_model_is_charged_for_it() {
        let bytes = 4_000_000_000u64;
        let blanket = LocalInferenceLlmAdapter::jetson_context_size(bytes, None);
        let real = LocalInferenceLlmAdapter::jetson_context_size(bytes, Some(168));
        assert!(
            real < blanket,
            "a 168 KiB/token model must get LESS than the 56 fallback grants, got {real}              against {blanket}; this is the direction that OOMs the board"
        );
    }

    /// A zero or absent slope must fall back, never divide by zero and never
    /// hand out an unbounded window.
    #[test]
    fn a_useless_slope_falls_back_rather_than_dividing_by_zero() {
        let bytes = 4_977_171_584u64;
        let fallback = LocalInferenceLlmAdapter::jetson_context_size(bytes, None);
        assert_eq!(
            LocalInferenceLlmAdapter::jetson_context_size(bytes, Some(0)),
            fallback
        );
    }

    /// The decision the two `apply_*_settings` paths share, tested here because
    /// the CUDA one is compiled by nothing on a developer machine or in CI.
    /// The adapter's own path, end to end, against the real files.
    ///
    /// The unit tests above build a `ModelProbe` by hand and check the
    /// decision. This checks that `probe_model` actually reads one off a GGUF
    /// and that the decision it produces differs across the collection -- the
    /// failure mode being a probe that quietly returns the same answer for
    /// everything and looks like it works.
    ///
    /// ```text
    /// GIAP_TEST_GGUF_DIR="$HOME/Library/Application Support/goose-in-a-pond/models/gguf" \
    ///   cargo test -p pond-adapters-local-inference --lib -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn probe_model_reads_real_files_and_separates_them() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;

        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        let mut modes = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let (mode, thinking) = match LocalInferenceLlmAdapter::probe_model(&path) {
                Some(p) => LocalInferenceLlmAdapter::tool_and_thinking_for(&p),
                None => (ToolCallingMode::Auto, true),
            };
            eprintln!(
                "{:<44} {:?} thinking={}",
                path.file_name().unwrap().to_string_lossy(),
                mode,
                thinking
            );
            *modes.entry(format!("{mode:?}")).or_insert(0) += 1;
        }
        assert!(!modes.is_empty(), "no GGUF files under {dir}");
        assert!(
            modes.len() > 1,
            "every model resolved to the same tool mode ({modes:?}); a probe that cannot \
             tell them apart is the blanket ForceNative with extra steps"
        );
        assert!(
            modes.contains_key("ForceEmulated"),
            "expected at least one model whose template carries no `tools` variable; \
             got {modes:?}"
        );
    }

    #[test]
    fn a_tool_using_model_keeps_native_calling() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let gemma = ModelProbe {
            tools: ToolSupport::Native,
            thinking: Thinking::Gated {
                marker: "<|think|>".into(),
            },
            context_window_tokens: Some(131072),
            architecture: Some("gemma4".into()),
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&gemma);
        assert_eq!(tools, ToolCallingMode::ForceNative);
        assert!(
            thinking,
            "a gated thinker must still get true -- this wiring must not change what \
             a currently-reasoning model does"
        );
    }

    /// The case the blanket ForceNative gets wrong, and the reason any of this
    /// exists. DeepSeek-R1-Distill's template takes no `tools` variable.
    #[test]
    fn a_model_whose_template_cannot_carry_tools_is_not_forced_native() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let deepseek = ModelProbe {
            tools: ToolSupport::Absent,
            thinking: Thinking::Always {
                marker: "<think>".into(),
            },
            context_window_tokens: Some(131072),
            architecture: Some("qwen2".into()),
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&deepseek);
        assert_eq!(
            tools,
            ToolCallingMode::ForceEmulated,
            "forcing native on a template with no `tools` variable renders declarations \
             nowhere at all"
        );
        assert!(
            thinking,
            "it reasons unconditionally; there is no flag to clear"
        );
    }

    /// A file we could not read is not evidence of anything, so goose keeps its
    /// own judgement rather than inheriting our guess.
    #[test]
    fn an_unreadable_model_defers_rather_than_forcing() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let unknown = ModelProbe {
            tools: ToolSupport::Unknown,
            thinking: Thinking::Unknown,
            context_window_tokens: None,
            architecture: None,
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&unknown);
        assert_eq!(tools, ToolCallingMode::Auto);
        assert!(!thinking, "nothing said it reasons");
    }

    /// A tool user with no reasoning markers gets the flag cleared. This is the
    /// only case the thinking half changes, and it changes it from "set for a
    /// model with nothing to set" to off.
    #[test]
    fn a_model_with_no_reasoning_markers_does_not_get_the_flag() {
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let plain = ModelProbe {
            tools: ToolSupport::Native,
            thinking: Thinking::Absent,
            context_window_tokens: Some(32768),
            architecture: Some("gemma3".into()),
        };
        let (_, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&plain);
        assert!(!thinking);
    }

    #[test]
    fn jetson_context_floors_for_an_oversized_model() {
        assert_eq!(
            LocalInferenceLlmAdapter::jetson_context_size(9_000_000_000, None),
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

    /// The Jetson tuning block, type-checked on a machine that cannot build it.
    ///
    /// `apply_jetson_settings` is `#[cfg(feature = "cuda")]`, so it is compiled
    /// by no developer machine and by no CI job — `cargo check -p pond-server`
    /// does not pass that feature. Everything it writes is therefore reviewed
    /// rather than compiled, and that gap has already cost a real breakage:
    /// the parent set `type_k`, `type_v` and `n_ubatch` from fe68ccdd against
    /// `ModelSettings` fields that existed only in the goose submodule's
    /// WORKING TREE, so the pinned commit could not build for CUDA and nothing
    /// on any Mac or in CI could notice.
    ///
    /// This constructs the same struct literal, with the same field names and
    /// the same types, on whatever platform is running the tests. It cannot
    /// check the VALUES are right for the Orin — only hardware can — but it
    /// fails the build the moment the submodule stops carrying a field the
    /// device code sets.
    #[test]
    fn the_jetson_settings_block_still_type_checks_off_device() {
        use goose::providers::local_inference::local_model_registry::{
            ModelSettings, ToolCallingMode,
        };

        let settings = ModelSettings {
            n_gpu_layers: Some(99),
            context_size: Some(16384),
            n_batch: Some(512),
            n_threads: Some(4),
            flash_attention: Some(true),
            type_k: Some("q8_0".to_string()),
            type_v: Some("q8_0".to_string()),
            n_ubatch: Some(128),
            use_mlock: false,
            tool_calling: ToolCallingMode::ForceNative,
            enable_thinking: true,
            ..Default::default()
        };

        assert_eq!(settings.n_ubatch, Some(128));
        assert_eq!(settings.type_k.as_deref(), Some("q8_0"));
        assert_eq!(settings.type_v.as_deref(), Some("q8_0"));
        // Quantising V without flash attention is refused by llama.cpp, so the
        // pairing is part of what this pins.
        assert_eq!(settings.flash_attention, Some(true));
    }
}
