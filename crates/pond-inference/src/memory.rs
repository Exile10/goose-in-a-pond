//! Context window estimation based on model architecture and available memory.
//!
//! Determines the maximum context length that fits in available accelerator
//! (Metal/CUDA) or CPU memory by reading the model's KV cache dimensions
//! from GGUF metadata and querying device free memory.

use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};

/// Query available inference memory in bytes.
///
/// Prefers accelerator memory (GPU, integrated GPU, Metal) over CPU RAM.
/// Returns 0 if no devices report memory information.
pub(crate) fn available_inference_memory_bytes() -> u64 {
    let devices = list_llama_ggml_backend_devices();

    // Prefer accelerator memory (Metal on macOS, CUDA on Jetson).
    let accel_memory = devices
        .iter()
        .filter(|d| {
            matches!(
                d.device_type,
                LlamaBackendDeviceType::Gpu
                    | LlamaBackendDeviceType::IntegratedGpu
                    | LlamaBackendDeviceType::Accelerator
            )
        })
        .map(|d| d.memory_free as u64)
        .max()
        .unwrap_or(0);

    if accel_memory > 0 {
        return accel_memory;
    }

    // Fall back to CPU memory.
    devices
        .iter()
        .filter(|d| d.device_type == LlamaBackendDeviceType::Cpu)
        .map(|d| d.memory_free as u64)
        .max()
        .unwrap_or(0)
}

/// Estimate the maximum context length that fits in available memory based
/// on the model's KV cache requirements.
///
/// Returns `None` if the model architecture values are unavailable or zero.
///
/// The estimate uses 50% of free memory for the KV cache, reserving the
/// rest for compute scratch buffers (attention, etc.) and other overhead.
pub(crate) fn estimate_max_context(model: &LlamaModel) -> Option<usize> {
    let available = available_inference_memory_bytes();
    if available == 0 {
        return None;
    }

    // Reserve 50% for compute scratch buffers.
    let usable = (available as f64 * 0.5) as u64;

    let n_layer = model.n_layer() as u64;
    let n_head_kv = model.n_head_kv() as u64;
    let n_head = model.n_head() as u64;
    let n_embd = model.n_embd() as u64;

    if n_head == 0 || n_layer == 0 || n_head_kv == 0 || n_embd == 0 {
        return None;
    }

    // For MLA (Multi-head Latent Attention) models like DeepSeek/GLM, the
    // actual KV cache dimensions differ from n_head_kv * head_dim. Read
    // the true dimensions from GGUF metadata when available.
    let head_dim = n_embd / n_head;
    let arch = model
        .meta_val_str("general.architecture")
        .unwrap_or_default();

    let k_per_head = model
        .meta_val_str(&format!("{arch}.attention.key_length"))
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(head_dim);

    let v_per_head = model
        .meta_val_str(&format!("{arch}.attention.value_length"))
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(head_dim);

    // Total bytes per KV token: (k + v) per head * n_kv_heads * n_layers * 2 bytes (f16).
    let bytes_per_token = (k_per_head + v_per_head) * n_head_kv * n_layer * 2;

    if bytes_per_token == 0 {
        return None;
    }

    Some((usable / bytes_per_token) as usize)
}

/// Compute the effective context size for an inference request.
///
/// Considers:
/// 1. Memory-based estimation (from [`estimate_max_context`])
/// 2. The model's training context length (`n_ctx_train`)
/// 3. Generation headroom (512 tokens reserved)
///
/// Returns the context size to use for `LlamaContextParams::with_n_ctx()`.
pub(crate) fn effective_context_size(model: &LlamaModel, prompt_token_count: usize) -> usize {
    let n_ctx_train = model.n_ctx_train() as usize;
    let memory_max = estimate_max_context(model);

    // Use the smaller of training context and memory-estimated max.
    let cap = match memory_max {
        Some(mem_max) if mem_max < n_ctx_train => {
            tracing::info!(n_ctx_train, mem_max, "capping context to memory estimate");
            mem_max
        }
        _ => n_ctx_train,
    };

    let min_generation_headroom = 512;
    let needed = prompt_token_count + min_generation_headroom;

    if needed > cap {
        tracing::warn!(
            prompt_token_count,
            cap,
            "prompt + headroom exceeds context limit, capping"
        );
    }

    needed.min(cap)
}
