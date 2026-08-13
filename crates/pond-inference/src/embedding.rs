//! GGUF-backed [`EmbeddingProvider`] over llama.cpp (llama-cpp-2).
//!
//! This exists because the only other production embedder,
//! `pond-infra`'s `FastembedEmbeddingProvider`, does not initialise on the
//! Jetson Orin: its ONNX Runtime is version-incompatible there and construction
//! times out, so retrieval silently falls back to keyword matching and PAI-3's
//! "semantic memory injection" never runs on the hardware GIAP ships to. A GGUF
//! model loaded through the same llama.cpp this pond already runs for chat has
//! no separate native runtime to be incompatible with.
//!
//! # Coexistence with Goose — solved, and NOT by load ordering
//!
//! Two llama.cpp consumers share one process here, and llama-cpp-2 tracks backend
//! initialisation in a PROCESS-GLOBAL flag that cargo shares between them. Goose
//! treats losing that CAS as `unreachable!` and PANICS
//! (`goose-local-inference/src/llamacpp/mod.rs`).
//!
//! An earlier version of this comment claimed lazy loading made that safe because
//! Goose would always go first. **That was false**, and the panic was reproduced
//! on a Mac 2026-08-13: an `ollama` pond never initialises Goose's backend at all,
//! and the memory backfill embeds at STARTUP rather than after a turn, so this
//! provider won the race and the next local chat model panicked a tokio worker.
//! Ordering could never have fixed it — any embed claims the backend, and the
//! switch to a local model can happen at any time.
//!
//! [`get_or_init_backend`] fixes it structurally instead: GIAP initialises the C
//! backend directly and never enters the CAS, so the flag is only ever set by
//! Goose and its `unreachable!` is genuinely unreachable. Read that function for
//! the full argument. Verified on a Mac: the previously-panicking sequence now
//! completes, and a real local chat turn runs in the same process as this
//! embedder with both models loaded.
//!
//! [`embed`]: EmbeddingProvider::embed
//! [`get_or_init_backend`]: crate::engine::get_or_init_backend

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A hard cap on how many tokens of a single text are embedded. The context is
/// created per call and sized to the token count, so this only bounds the
/// pathological case; it is `min`'d with the model's own trained context.
const MAX_EMBED_TOKENS: usize = 2048;

/// Immutable description of an embedding model: everything that makes one
/// model's vectors incompatible with another's.
///
/// `model_id` is the intended vector-space stamp, but **nothing persists it
/// yet** — the index's `model_id` column belongs to a later phase, and today the
/// only consumer is a log field. What actually discriminates one vector space
/// from another at read time is the WIDTH (`dims`), which is why every model here
/// is 768 and none may be 384; see [`EmbeddingModelSpec::bge_base_en_v1_5`].
#[derive(Clone, Debug)]
pub struct EmbeddingModelSpec {
    /// Stable identifier stamped onto every vector this model produces.
    pub model_id: String,
    /// GGUF filename expected under `{data_dir}/models/embedding/`.
    pub filename: String,
    /// Output dimensionality. Asserted against the loaded model's `n_embd`.
    pub dims: usize,
    /// Sequence pooling. BERT retrievers differ: nomic/MiniLM mean-pool, bge
    /// uses CLS. Wrong pooling silently produces a worse vector.
    pub pooling: LlamaPoolingType,
    /// Task prefix prepended to every text before tokenisation. nomic REQUIRES
    /// one (`search_document: `); bge uses none.
    ///
    /// **Queries get this prefix too, and for nomic they should not.** nomic is
    /// trained with asymmetric prefixes (`search_document: ` vs `search_query: `),
    /// so a query currently lands in the document manifold. Both sides carry the
    /// same prefix, so the space stays self-consistent and nothing is mis-scored
    /// across models — it is a bounded ranking-quality loss, not a correctness
    /// fault, worst where short queries meet long passages. Fixing it means adding
    /// an `embed_query` to the port (a provided method, so other implementors are
    /// untouched) and pointing the three query call sites at it:
    /// `goose_agent.rs :: topical_memories`, `context.rs :: search_context`,
    /// `memory.rs :: recall_memories`. Owed, not done.
    pub content_prefix: &'static str,
    /// GPU layers to offload. Defaults to 0 (CPU): an embedding forward pass is
    /// tiny and non-autoregressive, and CPU keeps it off the one GPU the chat
    /// model and its KV cache are fighting over.
    pub n_gpu_layers: u32,
    /// ggml threads for the forward pass. Deliberately small: llama.cpp would
    /// otherwise take 4, which on a 6-core Orin is most of the CPU the chat
    /// model needs for prefill.
    pub n_threads: i32,
    /// Canonical download URL for `filename`. The fetch is an egress point and
    /// must go through a gated downloader; this crate only names the source.
    pub download_url: String,
    /// Approximate download size, for progress display only.
    pub size_hint_mb: u64,
}

impl EmbeddingModelSpec {
    /// `nomic-embed-text-v1.5`, 768-dim, mean-pooled. The deliberate default
    /// for blocker 0a: retrieval-tuned, well-supported in llama.cpp (so most
    /// likely to actually initialise on the Orin), and its dimension is the
    /// one-way door recorded in the personal-context design.
    pub fn nomic_embed_text_v1_5() -> Self {
        Self {
            model_id: "nomic-embed-text-v1.5".to_string(),
            filename: "nomic-embed-text-v1.5.Q8_0.gguf".to_string(),
            dims: 768,
            pooling: LlamaPoolingType::Mean,
            // nomic will not produce a good vector without a task prefix. A
            // single prefix for both stored text and queries is a deliberate
            // simplification while the port has one `embed`.
            content_prefix: "search_document: ",
            n_gpu_layers: 0,
            n_threads: 2,
            download_url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/\
                           resolve/main/nomic-embed-text-v1.5.Q8_0.gguf"
                .to_string(),
            size_hint_mb: 146,
        }
    }

    /// `bge-base-en-v1.5`, 768-dim, CLS-pooled. The fallback if nomic will not
    /// load on the device.
    ///
    /// **It is 768 on purpose, and a 384-dim model must not be added here.**
    /// Until the index carries a `model_id` column, the ONLY thing that can tell
    /// one vector space from another at read time is the vector's WIDTH: every
    /// similarity function in this workspace guards `a.len() != b.len()` and
    /// returns `0.0`. fastembed — the provider this one replaces — emits 384
    /// (`all-MiniLM-L6-v2`, `bge-small-en-v1.5`). So while every GGUF model here
    /// is 768, a 384 vector is unambiguously a fastembed leftover and the width
    /// check is a COMPLETE discriminator. Add a 384-dim GGUF model and it stops
    /// being one: two genuinely different spaces would share a width, score
    /// plausibly against each other, and be silently wrong with no guard able to
    /// see it. That is strictly worse than the mismatch this file is guarding.
    pub fn bge_base_en_v1_5() -> Self {
        Self {
            model_id: "bge-base-en-v1.5".to_string(),
            filename: "bge-base-en-v1.5-q8_0.gguf".to_string(),
            dims: 768,
            pooling: LlamaPoolingType::Cls,
            content_prefix: "",
            n_gpu_layers: 0,
            n_threads: 2,
            download_url: "https://huggingface.co/CompendiumLabs/bge-base-en-v1.5-gguf/\
                           resolve/main/bge-base-en-v1.5-q8_0.gguf"
                .to_string(),
            size_hint_mb: 117,
        }
    }

    /// Resolve a settings model-name string to a spec. The empty string and the
    /// GGUF default both mean nomic.
    ///
    /// `active_embedding_model` is shared with the fastembed path, so a name
    /// this function does not know is usually a fastembed model name left behind
    /// by a provider switch — the caller falls back to the default rather than
    /// disabling embeddings.
    pub fn resolve(name: &str) -> Result<Self> {
        match name {
            "" | "gguf" | "nomic-embed-text-v1.5" => Ok(Self::nomic_embed_text_v1_5()),
            "bge-base-en-v1.5" => Ok(Self::bge_base_en_v1_5()),
            other => Err(anyhow!(
                "unknown GGUF embedding model: '{other}'. \
                 Supported: nomic-embed-text-v1.5, bge-base-en-v1.5"
            )),
        }
    }

    /// Every GGUF embedding model this crate will load. Used by the guard test
    /// that keeps the width discriminator complete.
    #[cfg(test)]
    fn all() -> Vec<Self> {
        vec![Self::nomic_embed_text_v1_5(), Self::bge_base_en_v1_5()]
    }
}

/// A loaded model held for the life of the provider. The backend is the shared
/// singleton; the model is owned here. No context is stored — one is created and
/// dropped per `embed` call, so there is no self-referential lifetime and no
/// `'static` transmute.
struct Loaded {
    backend: Arc<LlamaBackend>,
    model: LlamaModel,
    /// The per-call context is sized to at most this many tokens.
    max_ctx: usize,
}

/// [`EmbeddingProvider`] backed by a GGUF model via llama.cpp.
pub struct GgufEmbeddingProvider {
    model_path: PathBuf,
    spec: EmbeddingModelSpec,
    /// Lazily loaded on first `embed`; see the module docs for why NOT eagerly.
    loaded: Arc<Mutex<Option<Loaded>>>,
}

impl GgufEmbeddingProvider {
    /// Construct a provider for `spec`, expecting its GGUF under `embedding_dir`.
    ///
    /// This does NOT load the model or touch the llama backend — construction is
    /// cheap and infallible beyond path assembly, so it never races Goose's
    /// backend init at startup. The file need not exist yet; the first `embed`
    /// reports a clear error if it is missing (the caller is expected to have
    /// fetched it through a gated downloader).
    pub fn new(spec: EmbeddingModelSpec, embedding_dir: &Path) -> Self {
        let model_path = embedding_dir.join(&spec.filename);
        Self {
            model_path,
            spec,
            loaded: Arc::new(Mutex::new(None)),
        }
    }

    /// The path the GGUF is expected at (for a startup existence check / fetch).
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// The vector-space stamp for stored vectors.
    pub fn model_id(&self) -> &str {
        &self.spec.model_id
    }
}

/// Load the model on the calling (blocking) thread. Separated so it runs inside
/// `spawn_blocking`. Acquires the shared backend via [`get_or_init_backend`],
/// which is the graceful path when Goose has already initialised it.
fn load_sync(spec: &EmbeddingModelSpec, model_path: &Path) -> Result<Loaded> {
    if !model_path.exists() {
        return Err(anyhow!(
            "embedding model not found at {}. It must be downloaded (through a \
             gated downloader) before the provider can embed.",
            model_path.display()
        ));
    }

    // Graceful even when Goose owns the real init. See module docs.
    let backend = crate::engine::get_or_init_backend()
        .context("acquiring the shared llama backend for embeddings")?;

    let params = LlamaModelParams::default().with_n_gpu_layers(spec.n_gpu_layers);
    let model = LlamaModel::load_from_file(&backend, model_path, &params).map_err(|e| {
        anyhow!(
            "failed to load embedding model {}: {e}",
            model_path.display()
        )
    })?;

    // The dimension is a one-way door; a model that silently returns a different
    // width than we declared would poison every stored vector. Refuse loudly.
    let actual = usize::try_from(model.n_embd()).unwrap_or(0);
    if actual != spec.dims {
        return Err(anyhow!(
            "embedding model {} reports n_embd={actual} but the spec declares {} \
             dimensions — the vector space would be wrong. Refusing to load.",
            spec.model_id,
            spec.dims
        ));
    }

    let max_ctx = (model.n_ctx_train() as usize).min(MAX_EMBED_TOKENS).max(1);

    tracing::info!(
        model_id = %spec.model_id,
        dims = spec.dims,
        n_gpu_layers = spec.n_gpu_layers,
        max_ctx,
        "GGUF embedding model loaded"
    );

    Ok(Loaded {
        backend,
        model,
        max_ctx,
    })
}

/// Run one text through a freshly-created embedding context and return the
/// pooled, L2-normalised vector. Blocking; called inside `spawn_blocking`.
fn embed_sync(loaded: &Loaded, spec: &EmbeddingModelSpec, text: &str) -> Result<Vec<f32>> {
    let prefixed = format!("{}{}", spec.content_prefix, text);

    let mut tokens = loaded
        .model
        .str_to_token(&prefixed, AddBos::Always)
        .map_err(|e| anyhow!("tokenising text for embedding failed: {e}"))?;
    if tokens.is_empty() {
        return Err(anyhow!("text produced no tokens to embed"));
    }
    if tokens.len() > loaded.max_ctx {
        // Truncation is silent data loss: the row is then marked embedded and
        // never revisited, so the tail is unsearchable forever. Say so.
        tracing::warn!(
            model_id = %spec.model_id,
            tokens = tokens.len(),
            kept = loaded.max_ctx,
            "embedding input truncated; the discarded tail is not searchable"
        );
        tokens.truncate(loaded.max_ctx);
    }
    let n = tokens.len();
    let n_u32 = u32::try_from(n).expect("token count exceeds u32");

    // Non-causal pooled embeddings require n_ubatch >= n_tokens, so size the
    // whole context to this one input rather than to a fixed maximum.
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_u32))
        .with_n_batch(n_u32)
        .with_n_ubatch(n_u32)
        .with_embeddings(true)
        .with_pooling_type(spec.pooling)
        // Bounded on purpose. llama.cpp defaults to 4 ggml threads; on a 6-core
        // Orin that is most of the CPU taken from the chat model's prefill for a
        // forward pass this small.
        .with_n_threads(spec.n_threads)
        .with_n_threads_batch(spec.n_threads);

    let mut ctx = loaded
        .model
        .new_context(&loaded.backend, ctx_params)
        .map_err(|e| anyhow!("creating embedding context failed: {e}"))?;

    let mut batch = LlamaBatch::new(n, 1);
    for (pos, token) in tokens.iter().enumerate() {
        // logits=false: seq pooling reads the whole sequence, not per-token
        // logits. Matches llama.cpp's own embedding example.
        batch
            .add(*token, pos as i32, &[0], false)
            .map_err(|e| anyhow!("adding token to embedding batch failed: {e}"))?;
    }

    ctx.clear_kv_cache();
    ctx.decode(&mut batch)
        .map_err(|e| anyhow!("decoding embedding batch failed: {e}"))?;

    let raw = ctx
        .embeddings_seq_ith(0)
        .map_err(|e| anyhow!("reading pooled embedding failed: {e:?}"))?;

    if raw.len() != spec.dims {
        return Err(anyhow!(
            "embedding width {} != declared {} for {}",
            raw.len(),
            spec.dims,
            spec.model_id
        ));
    }
    // A NaN or infinity here would be stored and then poison every comparison it
    // takes part in: `partial_cmp` on a NaN score returns None, which the sorts in
    // sqlite_memory/sqlite_context turn into `Ordering::Equal`, so a single bad
    // row silently scrambles ranking. Refuse it instead of storing it.
    if !raw.iter().all(|x| x.is_finite()) {
        return Err(anyhow!(
            "embedding for {} contains non-finite values; refusing to store it",
            spec.model_id
        ));
    }

    Ok(l2_normalize(raw))
}

/// L2-normalise so cosine similarity is a plain dot product. A zero vector is
/// returned unchanged rather than dividing by zero.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter().map(|x| x / norm).collect()
    } else {
        v.to_vec()
    }
}

#[async_trait]
impl EmbeddingProvider for GgufEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let loaded = Arc::clone(&self.loaded);
        let spec = self.spec.clone();
        let model_path = self.model_path.clone();
        let owned = text.to_string();

        tokio::task::spawn_blocking(move || {
            // Lock across load+embed: llama contexts are not Send and the model
            // load must not race itself. Cheap for GIAP's 1-3 embeds per turn.
            let mut guard = loaded.blocking_lock();
            if guard.is_none() {
                *guard = Some(load_sync(&spec, &model_path)?);
            }
            let loaded_ref = guard.as_ref().expect("just loaded");
            embed_sync(loaded_ref, &spec, &owned)
        })
        .await
        .map_err(|e| anyhow!("embedding spawn_blocking join error: {e}"))?
    }

    fn dimensions(&self) -> usize {
        self.spec.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_maps_names_and_defaults_to_nomic() {
        for name in ["", "gguf", "nomic-embed-text-v1.5"] {
            let s = EmbeddingModelSpec::resolve(name).unwrap();
            assert_eq!(s.model_id, "nomic-embed-text-v1.5");
            assert_eq!(s.dims, 768);
            assert!(matches!(s.pooling, LlamaPoolingType::Mean));
        }
        let bge = EmbeddingModelSpec::resolve("bge-base-en-v1.5").unwrap();
        assert_eq!(bge.dims, 768);
        assert!(matches!(bge.pooling, LlamaPoolingType::Cls));

        assert!(EmbeddingModelSpec::resolve("does-not-exist").is_err());
    }

    /// The width IS the vector-space discriminator until the index carries a
    /// `model_id` column: every similarity function in this workspace guards
    /// `a.len() != b.len()` and returns 0.0, and fastembed — the provider this
    /// one replaces — emits 384. So no GGUF model here may be 384: that would
    /// give two different vector spaces the same width, where they score
    /// plausibly against each other and NOTHING can detect it.
    ///
    /// If this fires because you added a model, do not change the number. Either
    /// pick a non-384 model, or land the `model_id` column first and replace the
    /// width check everywhere it is relied on.
    #[test]
    fn no_gguf_model_shares_a_width_with_the_fastembed_provider() {
        const FASTEMBED_DIMS: usize = 384;
        for spec in EmbeddingModelSpec::all() {
            assert_ne!(
                spec.dims, FASTEMBED_DIMS,
                "{} is {}-dim, which collides with fastembed's width and makes a \
                 mixed store undetectable",
                spec.model_id, spec.dims
            );
        }
    }

    #[test]
    fn dimensions_are_declared_without_loading_a_model() {
        // Construction must not touch the backend or the filesystem model — that
        // is what keeps it from racing Goose's backend init at startup.
        let p = GgufEmbeddingProvider::new(
            EmbeddingModelSpec::nomic_embed_text_v1_5(),
            Path::new("/nonexistent"),
        );
        assert_eq!(p.dimensions(), 768);
        assert_eq!(p.model_id(), "nomic-embed-text-v1.5");
    }

    #[test]
    fn l2_normalize_is_unit_length_and_zero_safe() {
        let out = l2_normalize(&[3.0, 4.0]);
        let norm = (out[0] * out[0] + out[1] * out[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm was {norm}");
        assert_eq!(l2_normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    }

    /// Live end-to-end embed. Ignored by default: needs the real GGUF present at
    /// `$POND_EMBED_MODEL_DIR/nomic-embed-text-v1.5.Q8_0.gguf`. This is the Mac
    /// verification for blocker 0a; the Orin run is the acceptance gate.
    #[tokio::test]
    #[ignore]
    async fn live_embed_produces_a_unit_vector_and_ranks_related_text_higher() {
        let dir = std::env::var("POND_EMBED_MODEL_DIR")
            .expect("set POND_EMBED_MODEL_DIR to the folder holding the GGUF");
        let provider = GgufEmbeddingProvider::new(
            EmbeddingModelSpec::nomic_embed_text_v1_5(),
            Path::new(&dir),
        );

        let cat = provider.embed("the cat sat on the mat").await.unwrap();
        assert_eq!(cat.len(), 768);
        let norm = cat.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "not unit length: {norm}");

        let kitten = provider.embed("a kitten rested on the rug").await.unwrap();
        let finance = provider
            .embed("quarterly interest rate policy")
            .await
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(
            dot(&cat, &kitten) > dot(&cat, &finance),
            "related text should score higher than unrelated"
        );
    }
}
