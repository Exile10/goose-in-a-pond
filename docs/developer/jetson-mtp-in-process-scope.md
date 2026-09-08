# In-process MTP: scope

Measured on the Orin: upstream llama.cpp's MTP gives **47.7 tok/s against the vendored
engine's 15.8** on `gemma-4-E4B-it-qat`, at 87 % draft acceptance, quality unchanged.
See [`jetson-engine-bakeoff.md`](jetson-engine-bakeoff.md). This scopes bringing that
in-process rather than adopting a sidecar.

## BLOCKED — measured 2026-09-08, and this supersedes the scope below

The bump was attempted. **It cannot be done.** `llama-cpp-2` removed the
OpenAI-compat chat templating in `0.1.147` — the version immediately after ours — and
added MTP in `0.1.151`. There is no version that has both:

| version | `openai.rs` | `speculative.rs` (MTP) |
|---|---|---|
| **0.1.146** (pinned today) | **present** | — |
| 0.1.147 – 0.1.150 | — | — |
| 0.1.151 – 0.1.156 | — | **present** |

Bumping to reach MTP therefore *removes* the API the live serving path renders every
prompt with. `cargo check -p goose-local-inference` against `=0.1.156` fails with 13
errors; 8 of them are this one cause. On the `-sys` side, `wrapper_oai.{h,cpp}` are
deleted outright with no replacement anywhere in the crate.

### What is lost, and why it is not a port

`apply_chat_template_oaicompat` → `ChatTemplateResult` is not a formatting convenience.
It carries three things the engine depends on:

| Used for | Sites |
|---|---|
| Rendering the prompt with native tools JSON, `enable_thinking`, `parallel_tool_calls` | `inference_engine.rs:1641, 2361, 2629`; `mod.rs:81` |
| **Deciding whether a model supports native tool calling at all** — `template_result_supports_native_tool_calling` does a dry run and reads the answer off the result | `mod.rs:59, 104` |
| **Streaming tool-call parsing** — `streaming_state_oaicompat()` is what turns a token stream into `tool_calls` | `inference_native_tools.rs:37` |

`ChatTemplateResult` is also a public field of the engine's own `PreparedGeneration`
(`inference_engine.rs:305`), so the type is threaded through four modules.

Replacing it means GIAP renders the Gemma chat template itself *and* writes its own
streaming tool-call parser. The fork has `native_tool_parsing.rs` (323 lines) but it
parses text into a message — it is not the incremental parser the streaming path needs,
and `enable_thinking` plus the native-tool capability probe have no substitute at all.

This is exactly the failure mode the recorded history warns about: a `--jinja` streaming
tool-call parser breaking on GIAP's payload is what blocked the direct-llama.cpp route
once already. Re-implementing that parser to gain decode speed trades the thing that
works for the thing that is fast.

### What this means for the recommendation

The bake-off's finding stands — **upstream MTP is worth 2.8× decode on this board** — but
it is not reachable by bumping the crate. The routes that remain:

1. **Wait.** `openai.rs` may return, or the MTP API may be backported. Cheap to check
   before each attempt: the table above is two `curl`s.
2. **Fork `llama-cpp-2`.** Re-apply `openai.rs` and `wrapper_oai.{h,cpp}` from 0.1.146 on
   top of 0.1.156. Mechanically plausible — they are self-contained — but it adds a
   second vendored fork to maintain beneath the goose fork, and llama.cpp's own
   `chat.cpp` will have moved underneath them.
3. **Sidecar after all.** The bake-off measured `llama-server` doing this today with no
   GIAP changes at all. The costs are real (a second supervised process, wall-clock
   telemetry, re-solving `SacrificialContext` server-side) but they are *known*, whereas
   the cost of writing a streaming tool-call parser is not.
4. **Do nothing.** 15.8 tok/s is the current experience and nothing is broken.

My earlier recommendation — "bump the engine, do not adopt a sidecar" — assumed the bump
was available. It is not, and route 3 deserves a fresh look on that basis rather than
being dismissed for costs that are smaller than the alternative's.

### Cost of the check

Two hours, and it landed before any of the hard work in the scope below was started. That
was the point of ordering it first.

---

## Headline: smaller than it looked

I previously called this "new engine work, not a flag". That was wrong on the main
point. **`llama-cpp-2 0.1.156` already ships a safe MTP API** — I had checked
`ModelSettings.draft_model` (which only MLX consumes) and llama.cpp's
`common/speculative.cpp` (which is not part of the public C API) and concluded the loop
would have to be written. It does not.

`llama-cpp-2-0.1.156/src/speculative.rs`:

```rust
pub struct MtpSpeculativeParams { pub n_max: i32, pub n_min: i32, pub p_min: f32 }
pub struct MtpSpeculative<'model> { /* RAII over llama_rs_mtp_speculative */ }

impl<'model> MtpSpeculative<'model> {
    pub fn new(target: LlamaContext<'model>, draft: LlamaContext<'model>,
               params: MtpSpeculativeParams) -> Result<Self, MtpSpeculativeError>;
    pub fn begin(&mut self, prompt_tokens: &[LlamaToken]) -> Result<(), _>;
    pub fn process(&mut self, batch: &LlamaBatch<'_>) -> Result<(), _>;
    pub fn draft(&mut self, n_past: i32, id_last: LlamaToken,
                 prompt_tokens: &[LlamaToken]) -> Result<Vec<LlamaToken>, _>;
    pub fn accept(&mut self, n_accepted: u16) -> Result<(), _>;
}
```

That is the whole draft/verify/accept cycle, backed by `llama_rs_mtp_speculative_*` in
`llama-cpp-sys-2`'s `wrapper_common.cpp`.

## Four things gate it, in order of risk

### 1. The bump is mandatory, not optional — 0.1.146 cannot load the drafter

| | 0.1.146 (vendored today) | 0.1.156 |
|---|---|---|
| `gemma4-assistant` arch | **absent** — only `GEMMA4` | present |
| `MtpSpeculative` | absent | present |
| `llama_model_n_layer_nextn`, `_n_embd_out` | — | present |

So there is no partial path. Without the bump the drafter does not load at all, which is
also why the device's `~/llama.cpp` (a separate, current build) could run it while the
in-process engine could not.

### 2. `common` is a default feature, and GIAP disables defaults

Both `Cargo.toml` and `goose/Cargo.toml` pin:

```toml
llama-cpp-2 = { version = "=0.1.146", default-features = false, features = ["sampler", "mtmd"] }
```

`MtpSpeculative` lives behind `common` (`llama-cpp-2/common → llama-cpp-sys-2/common`),
which is in `default` but excluded by `default-features = false`. **Add `"common"`
explicitly to both files.** This pulls `wrapper_common.cpp` and llama.cpp's `common/`
into the build — a compile-time cost on a board where a cold CUDA build is 40–90 minutes,
and worth measuring before assuming it is free.

### 3. The real design problem: `MtpSpeculative` wants to own both contexts

`MtpSpeculative::new` **takes** `LlamaContext` by value for both target and draft. The
engine's `SessionKv` also owns its context, and does so carefully:

```rust
pub(super) struct SessionKv {
    /// Declared before `_model`: fields drop in declaration order, so the
    /// context is destroyed before the model allocation it points into.
    ctx: LlamaContext<'static>,
    ...
    _model: Arc<LlamaModel>,
}
```

That `'static` is a lifetime transmute over an Arc-stable address, with a hand-written
`unsafe impl Send` justified on the model-slot mutex serialising every access. Handing
that context to `MtpSpeculative` means either:

- **(a)** `SessionKv` holds `MtpSpeculative` instead of a bare context, and every existing
  path (`reuse_prefix`, `prefill_plan`, `clear_kv_cache_seq`, `state_seq_save_file`) goes
  through `spec.target_context_mut()`; or
- **(b)** MTP gets its own slot variant and sessions using it lose the prompt-session
  cache.

**(a) is the only acceptable one.** The KV cache is worth more than MTP on the metric that
matters most here: it turns a follow-up turn from a full re-prefill into a resume
(measured 12.4 s → 0.65 s TTFT). Losing it to gain decode would be a bad trade, and the
bake-off's own follow-up numbers would have caught it.

The wrapper exposes `target_context()`, `target_context_mut()` and `draft_context_mut()`,
so (a) is mechanically available. The work is threading it through, plus re-justifying the
`Send` impl for a struct that now owns two contexts.

### 4. Memory and the second context

The drafter is 57 MB on disk (`mtp-gemma-4-E4B-it.gguf`), but it needs its own
`LlamaContext` and therefore its own KV allocation. Measured cost of the whole MTP
configuration on the device: **peak footprint 4,586 MB vs the baseline's 4,538 — +48 MB**,
with `MemAvailable` bottoming at 1,216 MB against an 812 MB reserve. It fits, and it fits
because llama.cpp's MTP drafter shares the target's KV cache rather than duplicating it.

That is measured through llama-server, not through this integration. **Re-measure after
wiring**, because an in-process arrangement that accidentally gives the drafter a full
`n_ctx` of its own would cost ~896 MiB at 16384 and blow the budget.

## Work items

| # | Item | Files | Risk |
|---|---|---|---|
| 1 | Pin `=0.1.156` + add `"common"` | `Cargo.toml`, `goose/Cargo.toml` (parent wins) | low; API drift across a 10-patch jump needs checking |
| 2 | Re-verify the engine's API surface | `llamacpp/inference_engine.rs` | low–medium |
| 3 | Resolve the drafter path | `ModelSettings.draft_model` exists and `resolve_model_path` already honours `GOOSE_LOCAL_DRAFT_MODEL`; only the MLX backend consumes it today | low |
| 4 | `SessionKv` owns `MtpSpeculative` | `inference_engine.rs` — the `'static` transmute, drop order, and `unsafe impl Send` all need re-justifying for two contexts | **high** |
| 5 | Speculative generation loop | `generation_loop` currently samples one token and decodes one batch; MTP replaces that with draft → verify → accept | medium |
| 6 | `DraftStats` telemetry | `ProviderStats.draft` is already `Option<DraftStats>` and hardcoded `None` at both call sites (`inference_native_tools.rs:250`, `inference_emulated_tools.rs:493`) — wire acceptance rate through | low |
| 7 | Registry plumbing | `apply_jetson_settings` stamps a `draft_model` and the derived contexts for both | low |
| 8 | Fork patch table | `docs/goose-patch-management.md` | low |

Items 4 and 5 are the work. Everything else is wiring.

## What would make this not worth doing

- **The bump regresses the KV cache.** Patches #7/#8/#10 (prompt-session cache, on-disk
  snapshot, KV type + `n_ubatch`) all live in `inference_engine.rs`. If 0.1.156 moved the
  API under them, the re-port cost could exceed the gain.
- **`common` inflates the device build materially.** Measure it on the first build.
- **The drafter cannot be made to share KV in-process.** Then item 4 collapses into
  option (b), and the trade is decode against prefix reuse — which the measured numbers
  say is a bad trade.

## Order

1. Bump and add `common` on a fork branch. Build on the **Mac** first — `cargo check -p
   pond-server -p pond-adapters-goose --all-targets` — and record what the API drift costs
   before touching the device.
2. `cargo test -p goose-local-inference` (three known host-memory context-cap failures are
   pre-existing; do not chase them).
3. Device build, then C1 re-run **without** MTP. That isolates the bump: does the vendored
   engine reach C2-baseline's 17.2 tok/s once it is current? If it does not, the gap was
   never engine vintage and item 4 is premature.
4. Only then items 4–6, and re-run C1 with MTP against the same payloads.
5. Re-verify PAI-3/4/5: decode tok/s feeds the output-reserve latency budget, and the
   prefix-stability rules assume the cache behaves as it does today.

Step 3 is the decision point and it is cheap. It is also the one this scope could be
wrong about: everything above assumes the 2.8× survives moving from llama-server into
GIAP's loop, and nothing measured so far demonstrates that.
