//! In-process GGUF inference adapter and memory-aware model scheduler: wraps Goose's
//! [`LocalInferenceProvider`] so GIAP loads model weights into this process, with no llamafile
//! or Ollama subprocess. macOS gets Metal automatically; the Jetson Orin Nano needs
//! `--features cuda` at build time, and `apply_jetson_settings` stamps its registry entry.

/// Whether this build can reach CUDA at all.
///
/// `cuda` is a feature of THIS crate, passed on the command line by `scripts/jetson/deploy.sh`,
/// so a `cfg!` in `pond-server` always reads false. A const, so it cannot drift from that.
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
    /// `model_id` is anything Goose's `LocalInferenceProvider` accepts: a HuggingFace repo plus
    /// quant, or a local path. Weights load on the first `complete()` call, not here.
    pub async fn new(model_id: &str) -> Result<Self> {
        let model_config = ModelConfig::new(model_id);

        Self::apply_model_settings(model_id);

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

    /// Build the adapter, registering the model's `local_path` in Goose's global registry so
    /// `LocalInferenceProvider` finds the GGUF under `$data_dir/models/gguf/` instead of its own
    /// `~/.local/share/goose/models/`. `model_id` is either a HuggingFace `repo:QUANT` id or a
    /// raw `.gguf` filename, which must already exist in that directory.
    pub async fn new_with_data_dir(model_id: &str, data_dir: &std::path::Path) -> Result<Self> {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, model_id_from_repo, LocalModelEntry, LocalModelStorage, ModelSettings,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // Filename stem (no '/', no ':', no ".gguf"): the model catalog stores the name without
        // the extension while the file on disk is {stem}.gguf. Normalise by appending ".gguf"
        // and falling through to the raw-filename path below.
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

    /// Stamp the model registry so llama-cpp-2 picks up device settings at load time.
    ///
    /// A device profile lets a non-CUDA build take the Jetson branch on purpose: otherwise
    /// `jetson_context_size`, the arithmetic that can OOM the board, has no caller off-device.

    /// The drafter id to hand the engine, or `None` to decode without speculation.
    ///
    /// Checks the file, not just the row: a registry entry whose weights have
    /// been deleted would otherwise fail inside context creation on the next
    /// turn, which reads as the engine breaking rather than as a missing file.
    fn registered_drafter(model_id: &str) -> Option<String> {
        use goose::providers::local_inference::local_model_registry::get_registry;
        use pond_core::models::domain::drafter::drafter_for;

        let spec = drafter_for(model_id)?;
        let registry = get_registry().lock().ok()?;
        let entry = registry.get_model(spec.id)?;
        entry.local_path.exists().then(|| spec.id.to_string())
    }

    fn apply_model_settings(model_id: &str) {
        #[cfg(feature = "cuda")]
        Self::apply_jetson_settings(model_id);

        #[cfg(not(feature = "cuda"))]
        if pond_core::models::domain::device_profile::stamping_device_model_settings() {
            Self::apply_jetson_settings(model_id);
        } else {
            Self::apply_platform_settings(model_id);
        }
    }

    /// Apply platform-optimised model settings for non-CUDA builds (macOS Metal, CPU).
    ///
    /// On Apple Silicon this enables full Metal GPU offload and flash attention. Without it
    /// `n_gpu_layers` defaults to `None` and ALL inference runs on the CPU despite Metal.
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
            // `enable_thinking` is set above from the template rather than left to inherit
            // goose's `default_true()`. Thread count is left for llama.cpp to auto-detect,
            // which is the right choice on Apple Silicon.
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
                        // was NOT compiled in, so n_gpu_layers=99 is a request no
                        // backend honours and inference runs on the CPU. Saying
                        // "Metal" here disguises a wrongly built binary.
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

    /// The tool mode and thinking flag a GGUF at `path` should be registered with, read from its
    /// own chat template by `new_with_data_dir` before any `apply_*_settings` runs. It re-stamps
    /// rather than upgrading, matching `apply_*_settings`: an entry once persisted as
    /// `ForceNative` would otherwise keep a mode its template cannot honour forever.
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

    /// What the registry should say for a model, given what its own file says it can do.
    ///
    /// Pure over [`ModelProbe`] so both callers share one decision; the CUDA one is compiled by
    /// nothing in CI. A template with no `tools` variable renders no declarations to force into.
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
    /// Walks far enough to reach `tokenizer.chat_template`, 3.8-15 MB into a GGUF behind the
    /// token array: about 40 ms, because everything between the wanted keys is stepped over.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn probe_model(
        path: &std::path::Path,
    ) -> Option<pond_core::models::domain::model_probe::ModelProbe> {
        use pond_core::models::domain::gguf::parse_gguf_file;
        use pond_core::models::domain::model_probe::ModelProbe;
        parse_gguf_file(path).map(|info| ModelProbe::from_gguf(&info))
    }

    /// The model's KV cost per token from its own GGUF header, or `None` when the header cannot
    /// settle it and the caller must keep the measured constant. Exact for a dense model; where
    /// `key_length_swa` is present the global-to-SWA layer ratio the header omits is worth a
    /// factor of two in the direction that OOMs a board, so trust only confirmed architectures.
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

    /// Context size that fits THIS model in the Jetson's LLM budget: the budget less the weights
    /// and the compute buffers, divided by a measured per-token KV cost, floored to
    /// `CTX_GRANULARITY`. A single hardcoded constant OOM-killed a board, so only a measurement
    /// from the Orin may move it. `apply_jetson_settings` re-stamps the registry at every init.
    fn jetson_context_size(
        model_bytes: u64,
        drafter_bytes: u64,
        kv_kib_per_token: Option<u64>,
    ) -> u32 {
        // The budget of the device we BELIEVE we are: the constant unless a
        // device profile is emulating another board. See `scripts/jetson-emu.sh`.
        Self::context_size_for_budget(
            crate::scheduler::llm_budget_mb(),
            model_bytes,
            drafter_bytes,
            kv_kib_per_token,
        )
    }

    /// The derivation itself, against an explicit budget.
    ///
    /// Split from [`Self::jetson_context_size`] so another board's budget can be asked for
    /// without touching the process environment, a data race in a threaded test binary.
    fn context_size_for_budget(
        budget_mb: u64,
        model_bytes: u64,
        drafter_bytes: u64,
        kv_kib_per_token: Option<u64>,
    ) -> u32 {
        /// Per-token KV cost for the widest geometry we ship, MEASURED on the Orin (the Mac said
        /// 16): E4B is 56 KiB/token across both caches, E2B 18. No padding here (it lives in the
        /// budget) and no constant term, since both caches carry `n_ctx` cells on this llama.cpp.
        /// Padding to 64 floored E4B to 4096, under its own 4,678-token turn-1 prompt.
        const KV_KIB_PER_TOKEN: u64 = 56;
        /// llama.cpp's compute buffers. Nearly flat in `n_ctx` -- measured
        /// 522 MiB at both 4096 and 16384, rising to 582 MiB at 32768 -- so 600
        /// covers the range this function can return.
        const COMPUTE_BUFFER_MB: u64 = 600;
        /// The drafter's compute buffers and graph, on top of its weights.
        ///
        /// Its KV is NOT here, and that is the point: with `ctx_other` the
        /// drafter shares the target's cache, which is visible in the phase
        /// timings as `process` costing 0.0 ms/step -- llama.cpp skips the
        /// catch-up decode only when the memory is shared. So the drafter's
        /// cost is flat in `n_ctx` and belongs in the budget, not in the slope.
        ///
        /// 64 rather than a measured figure: the honest measurement (MemAvailable
        /// either side of building a drafter context) read 38-47 MB at 8192 and
        /// 16384 against a 57 MB file, and it is an UNDER-estimate -- mmap'd
        /// weights come out of reclaimable page cache, which MemAvailable counts
        /// as available. Rounding up past the file size costs a few hundred
        /// tokens of window and buys the margin that measurement could not
        /// establish.
        const DRAFTER_COMPUTE_MB: u64 = 64;
        const MIN_CTX: u32 = 2048;
        const MAX_CTX: u32 = 16384;
        /// Round the answer DOWN to a multiple of this. Not a power of two: those are 2x apart,
        /// so flooring to one discards up to HALF of a window the budget already proved
        /// affordable (E4B IQ4_XS: 13,220 allowed, 8,192 handed out). `n_ctx` needs no power of
        /// two in llama.cpp; safety comes from the slope, the compute buffer and the budget.
        const CTX_GRANULARITY: u32 = 1024;

        let model_mb = model_bytes / (1024 * 1024);
        // A drafter is a second set of weights resident for the whole session.
        // Leaving it out of the budget is what let the window be sized as though
        // only one model were loaded.
        let drafter_mb = if drafter_bytes > 0 {
            drafter_bytes / (1024 * 1024) + DRAFTER_COMPUTE_MB
        } else {
            0
        };
        let kv_mb = budget_mb
            .saturating_sub(model_mb)
            .saturating_sub(COMPUTE_BUFFER_MB)
            .saturating_sub(drafter_mb);
        // The model's own header when it could answer, else the conservative fallback.
        // `kv_cost_from_header` returns None rather than guessing, so an unreadable or unfamiliar
        // model gets exactly the fallback behaviour and this is never a new risk.
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

    /// Patch the Goose model registry with Jetson Orin Nano settings; errors (model not yet
    /// downloaded, poisoned lock) are ignored and defaults apply. Not yet fail-closed on memory
    /// fit: `n_gpu_layers = 99` silently partial-offloads to CPU past the budget. An on-device
    /// guard must drop the page cache before the `-ngl` load or NvMap fails with error 12.
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
        // Decided BEFORE the window is sized, not after: a drafter is a second
        // set of weights resident for the whole session, so the window has to be
        // computed against what will actually be loaded. Sizing first and
        // attaching a drafter afterwards is how a window gets handed out that
        // only fits when speculation is off.
        let draft_model = Self::registered_drafter(model_id);
        let drafter_bytes = draft_model
            .as_deref()
            .and_then(|id| {
                get_registry()
                    .lock()
                    .ok()?
                    .get_model(id)
                    .and_then(|e| std::fs::metadata(&e.local_path).ok())
                    .map(|m| m.len())
            })
            .unwrap_or(0);
        let context_size = Self::jetson_context_size(model_bytes, drafter_bytes, kv_kib);
        tracing::info!(
            model = model_id,
            model_mb = model_bytes / (1024 * 1024),
            kv_kib_per_token = kv_kib.map_or("fallback".to_string(), |k| k.to_string()),
            context_size,
            drafter_mb = drafter_bytes / (1024 * 1024),
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
            // The turn-1 prompt (system prefix plus native tools JSON) measures ~3,250 tokens, so
            // a 4096 window starts a fresh turn at 80% full and goose compacts mid-generation.
            // Measured on this board, Gemma 4 E2B costs ~18 KiB/token, so 16384 is ~288 MiB in
            // two buffers (96 + 192) that stay clear of the ~586 MiB NvMap allocation wall.
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
            // KV cache at q8_0. Measured on this board (gemma-4 E4B, ctx 16384): KV 296 -> 157
            // MiB, peak footprint 437 -> 307 MB. Quality-neutral: greedy output byte-identical to
            // f16 and paired wikitext-2 dNLL -0.000987 +/- 0.000551 (n = 100, t = -1.79). q4_0
            // saves ~75 MiB more but its per-chunk variance is 6.4x higher, so it is not used.
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
            // Speculative decoding, when this model's drafter is registered AND
            // its weights are still on disk. Re-decided on every provider build
            // rather than configured once: `update_model_settings` below
            // replaces the whole block, so a `draft_model` set by hand in
            // registry.json is erased here anyway. Deciding it from the file
            // system each time is what makes that safe -- a deleted drafter
            // stops being referenced instead of failing the next context
            // creation, and a newly downloaded one is picked up without a
            // restart.
            draft_model,
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

/// Strip thinking-token preambles emitted by reasoning-capable models: Gemma 4's
/// `<|channel>thought ... <channel|>REPLY` (keep everything after the last `<channel|>`) and
/// Qwen3 / DeepSeek-R1 / QwQ's `<think>...</think>REPLY` (drop the tag contents). Text with
/// neither pattern is returned unchanged.
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

    /// Both shipped models, against the DEVICE-measured cost. Not `#[cfg(feature = "cuda")]`:
    /// this is pure arithmetic that can kill a board, and a developer machine or CI is the only
    /// place it CAN run, since the device build is `cargo check`-only.
    #[test]
    fn jetson_context_fits_each_model_in_the_budget() {
        // The EXACT sizes of the two GGUFs on the device (`stat -Lc %s`, 2026-08-16): the answer
        // is a step function of weight size, so approximations can land on a different step than
        // the board does (a 336 MB gap once hid over half of E4B's free KV budget).
        let e2b = LocalInferenceLlmAdapter::jetson_context_size(3_106_738_272, 0, None);
        let e4b = LocalInferenceLlmAdapter::jetson_context_size(4_977_171_584, 0, None);
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

    /// The emulator's whole claim, as arithmetic: a different device budget produces a different
    /// window for the same weights. If the budget stopped reaching the derivation,
    /// `scripts/jetson-emu.sh` would still announce it was emulating while testing the Mac, and
    /// that failure is silent by construction.
    #[test]
    fn a_different_device_budget_produces_a_different_window() {
        /// E4B Q4_K_M, `stat -Lc %s` on the device.
        const E4B: u64 = 4_977_171_584;
        // Budgets computed the way the scheduler computes them: total RAM less
        // the fixed OS/STT/TTS reservation.
        let nano = 7620 - (1500 + 200 + 100);
        let nx = 15564 - (1500 + 200 + 100);

        let on_nano = LocalInferenceLlmAdapter::context_size_for_budget(nano, E4B, 0, None);
        let on_nx = LocalInferenceLlmAdapter::context_size_for_budget(nx, E4B, 0, None);

        assert_eq!(on_nano, 8192, "the board we actually have");
        assert!(
            on_nx > on_nano,
            "twice the RAM must buy E4B a wider window, got {on_nx} against {on_nano} -- if these \
             are equal the profile is not reaching the derivation and the emulator is theatre"
        );
    }

    /// The emulated path and the device path are the same code, so the profile
    /// for the board we own must reproduce the board's own answers exactly.
    /// This is what makes a tier-1 result worth anything.
    #[test]
    fn the_orin_profile_reproduces_the_devices_own_windows() {
        let budget = 7620 - (1500 + 200 + 100);
        assert_eq!(
            LocalInferenceLlmAdapter::context_size_for_budget(budget, 3_106_738_272, 0, None),
            16384,
            "E2B"
        );
        assert_eq!(
            LocalInferenceLlmAdapter::context_size_for_budget(budget, 4_977_171_584, 0, None),
            8192,
            "E4B"
        );
    }

    /// A budget smaller than the weights must clamp, not underflow into a huge
    /// window. Reachable from a Mac now that POND_DEVICE_TOTAL_RAM_MB exists,
    /// and previously reachable only by shipping a bigger model to the board.
    #[test]
    fn an_impossible_budget_clamps_instead_of_wrapping() {
        assert_eq!(
            LocalInferenceLlmAdapter::context_size_for_budget(512, 4_977_171_584, 0, None),
            2048,
            "a budget the model cannot fit must land on MIN_CTX; a saturating_sub that wrapped \
             would hand llama.cpp a window of billions of tokens"
        );
    }

    /// E4B at its window fits the MEASURED budget with real headroom, and doubling again does not
    /// fit at all, which is why 8192 is E4B's honest ceiling on memory grounds and not merely
    /// because `MAX_CTX` says so. The arithmetic is redone here rather than copied from the
    /// function, so a test cannot agree with the same mistake.
    #[test]
    fn e4b_fits_its_window_and_could_not_take_another_doubling() {
        /// Measured on the Orin: 128 MiB + 320 MiB at n_ctx 8192, both caches
        /// carrying n_ctx cells, so 56 KiB/token with no constant term.
        const MEASURED_KIB_PER_TOKEN: u64 = 56;
        let weights_mb = 4_640_000_000u64 / (1024 * 1024);
        let free_mb = crate::scheduler::LLM_BUDGET_MB - weights_mb - 600;

        let chosen = LocalInferenceLlmAdapter::jetson_context_size(4_640_000_000, 0, None) as u64;
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

    /// The slope itself, pinned where the ceiling cannot hide it. E4B is budget-bound and guards
    /// it too, but this survives somebody changing which models ship: a ~4.5 GB model sits where
    /// the BUDGET decides below the ceiling, giving 12,288 at the measured cost and 16,384 (the
    /// clamp) at the Mac's. The size lands MID-BAND (12,800 tokens, 512 clear of both floors).
    #[test]
    fn the_per_token_slope_is_observable_on_a_model_the_ceiling_does_not_cap() {
        let ctx = LocalInferenceLlmAdapter::jetson_context_size(4_739_563_520, 0, None);
        assert_eq!(
            ctx, 12288,
            "a 4.5 GB model got {ctx} tokens. At the device-measured cost it should get 12288; \
             16384 means the slope has been lowered towards the Mac's 16 KiB/token, which \
             describes a newer llama.cpp than the one this device ships and understates the real \
             allocation by roughly three times."
        );
    }

    /// The granularity itself: E4B IQ4_XS (4,496 MB of weights) affords 13,220 tokens, and a
    /// power-of-two floor handed back 8,192, under the 4,678-token preamble plus growth, so
    /// compaction fired on turn one. Any rounding coarser than this reintroduces that.
    #[test]
    fn rounding_does_not_discard_context_the_budget_affords() {
        // The real IQ4_XS file on the device: 4,715,416,704 bytes.
        let ctx = LocalInferenceLlmAdapter::jetson_context_size(4_715_416_704, 0, None);
        assert_eq!(
            ctx, 12288,
            "E4B IQ4_XS got {ctx}. Its budget affords 13,220 tokens, so anything at or below \
             8192 means the rounding went back to powers of two and is discarding a third of \
             the window the board can actually hold."
        );

        // And the floor still rounds DOWN, never up, at every offset.
        for bytes in [4_600_000_000u64, 4_700_000_000, 4_800_000_000] {
            let ctx = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None) as u64;
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

    /// Wiring the header-derived cost in must not move either shipped model, which is why it
    /// could land without the device: E2B computes 18 KiB/token but is `MAX_CTX`-bound either
    /// way, and both E4B quants compute exactly the 56 the constant already carries.
    #[test]
    fn header_derived_cost_is_a_no_op_for_the_shipped_models() {
        // (weights, computed KiB/token, expected window)
        let cases = [
            (3_106_738_272u64, 18u64, 16384u32), // E2B Q4_K_M
            (4_977_171_584, 56, 8192),           // E4B Q4_K_M
            (4_715_416_704, 56, 12288),          // E4B IQ4_XS
        ];
        for (bytes, kv, want) in cases {
            let fallback = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
            let derived = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(kv));
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

    /// A cheaper model gets the context its own geometry affords, which is the point of reading
    /// the header. The size matters: a light model is `MAX_CTX`-bound at BOTH costs and proves
    /// nothing. At 4,500 MB the KV budget is 720 MB: 13,166 tokens at 56 KiB (floors to 12,288)
    /// against 26,331 at 28 (ceiling-bound at 16,384).
    #[test]
    fn a_cheaper_model_is_no_longer_charged_the_widest_geometry() {
        let bytes = 4_500u64 * 1024 * 1024;
        let blanket = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
        let real = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(28));
        assert!(
            real > blanket,
            "a 28 KiB/token model should get more than the 56 KiB/token fallback allows,              got {real} against {blanket}"
        );
    }

    /// The direction that matters: a WIDER model must be charged more and get less, rather than
    /// inheriting a constant that flatters it. 168 KiB/token is the figure this file once carried
    /// for E4B before the device corrected it.
    #[test]
    fn a_wider_model_is_charged_for_it() {
        let bytes = 4_000_000_000u64;
        let blanket = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
        let real = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(168));
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
        let fallback = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
        assert_eq!(
            LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(0)),
            fallback
        );
    }

    /// The adapter's probe path end to end against real GGUFs: `probe_model` must read a
    /// `ModelProbe` off each file and the decisions must differ across the collection (a probe
    /// returning one answer for everything looks like it works). Set `GIAP_TEST_GGUF_DIR` to a
    /// `models/gguf` dir, then `cargo test -p pond-adapters-local-inference --lib -- --ignored`.
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
            LocalInferenceLlmAdapter::jetson_context_size(9_000_000_000, 0, None),
            2048
        );
    }
    /// Tests needing a real model are `#[ignore]` and gated on `GIAP_TEST_MODEL_PATH`. Run with
    /// `GIAP_TEST_MODEL_PATH=/path/to/model.gguf` set and
    /// `cargo test -p pond-adapters-local-inference -- --ignored`.
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

    /// Exercise the filename derivation inside `new_with_data_dir` without the filesystem or a
    /// model: calling it directly reaches `LocalInferenceProvider::from_env`, which downloads
    /// weights, so the pure filename logic is replicated here.
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

    /// A drafter is a second resident model, and the window has to pay for it.
    #[test]
    fn a_drafter_costs_window() {
        const E2B: u64 = 3_106_738_272;
        const DRAFTER: u64 = 59_235_648; // the real mtp-gemma-4-E2B-it.gguf
        let without = LocalInferenceLlmAdapter::jetson_context_size(E2B, 0, Some(18));
        let with = LocalInferenceLlmAdapter::jetson_context_size(E2B, DRAFTER, Some(18));
        assert!(
            with <= without,
            "attaching a drafter must not grow the window: {without} -> {with}"
        );
        assert!(
            with >= 8192,
            "E2B should still get a usable window with a drafter attached, got {with}"
        );
    }

    /// The drafter's cost is flat in `n_ctx`, so it must not be folded into the
    /// per-token slope. It shares the target's KV cache -- visible as `process`
    /// costing 0.0 ms/step in the MTP phase timings -- so charging it per token
    /// would shrink the window roughly twice over on the wider geometry.
    #[test]
    fn the_drafter_is_charged_once_not_per_token() {
        const E4B: u64 = 4_977_171_584;
        const DRAFTER: u64 = 59_678_016;
        let budget = crate::scheduler::llm_budget_mb();
        let without =
            LocalInferenceLlmAdapter::context_size_for_budget(budget, E4B, 0, Some(56)) as u64;
        let with = LocalInferenceLlmAdapter::context_size_for_budget(budget, E4B, DRAFTER, Some(56))
            as u64;
        // 57 MB of weights + a 64 MB allowance, against 56 KiB/token.
        let expected_loss = (57 + 64) * 1024 / 56;
        let actual_loss = without.saturating_sub(with);
        assert!(
            actual_loss <= expected_loss + 1024,
            "lost {actual_loss} tokens for a drafter that should cost about {expected_loss}"
        );
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

    /// The Jetson tuning block, type-checked off-device. `apply_jetson_settings` is
    /// `#[cfg(feature = "cuda")]`, so no developer machine or CI job compiles it; it once set
    /// `ModelSettings` fields that existed only in the submodule's working tree and nothing
    /// noticed. This builds the same struct literal everywhere; only hardware can check VALUES.
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
