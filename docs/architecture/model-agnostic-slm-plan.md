# Running non-Gemma SLMs: what breaks, and how to detect a model at runtime

**Status:** plan. Nothing here is implemented yet.
**Written:** 2026-08-16, from the E4B failure on the Orin.

## 1. What the transcripts actually show

The presenting complaint was "auto compaction is super aggressive for E4B". It is not a
compaction problem, and it is not a context-size problem. E4B produces **no output at all**
on this device.

From `pond_logs.db turn_metrics`, 2026-08-16:

| model | turns | reengagements | reasoning_tokens | avg completion |
|---|---|---|---|---|
| gemma-4-E2B-it | 1 | 1 | 314 | 1,237 |
| gemma-4-E2B-it-Q4_K_M | 4 | 1 | 570 | 585 |
| gemma-4-E4B-it-IQ4_XS | 5 | 0 | **0** | **40** |
| gemma-4-E4B-it-Q4_K_M | 6 | 0 | **0** | **49** |

40-49 completion tokens is the length of the error string, not an answer. And in goose's own
store the last five sessions are `2 user, 0 assistant, 0 tool` -- the second user message
being GIAP's `EMPTY_TURN_STEER` re-engagement ("Before you answer: does this fully cover
what was asked?"). The model was asked twice and said nothing twice.

Earlier sessions show the same shape degrading: `20260816_6` and `_7` are 6 user / 1
assistant -- five re-engagements to get one answer.

The user-visible error is goose's, and it is misleading:

> Failed to compact: context limit exceeded even after removing all tool responses

That is emitted at `goose/crates/goose/src/context_mgmt/mod.rs:384` only after the
*summarisation* call has also failed with `ContextLengthExceeded`. The underlying raise is
one of two checks in `llamacpp/inference_engine.rs:765-779`:

```rust
prompt_token_count > estimate_max_context_for_memory(...)  // "exceeds estimated memory capacity"
prompt_token_count >= effective_ctx                        // "exceeds context limit"
```

The first reads `backend.available_memory_bytes()` -- **live free RAM at call time**, not the
configured window. That is consistent with every observation the configured-window theory
cannot explain: E2B always works and E4B never does; it fails identically at 8192 and 12288
and on both quants; it is intermittent (14:09 failed, 14:10 succeeded, 14:17 failed); and
the board sits at ~84 MB free with swap in use once E4B is resident.

**Open:** which of the two checks fires. It needs one run with `RUST_LOG` raised --
goose carves `goose_local_inference` and `llama-cpp-2` to ERROR in `tracing_setup.rs`, so
the numbers are currently never emitted. Until that is captured, the memory-estimator
hypothesis is the best fit but is not proven.

`session_thinking` has **0 rows** for every model, so reasoning is never persisted at all.
That is a separate defect from the E4B failure and worth its own look.

## 2. Why a non-Gemma model would fare no better

Three mechanisms decide what a model can do, and none of them looks at the model.

**GIAP forces the answer.** `apply_jetson_settings` / `apply_platform_settings`
(`crates/pond-adapters-local-inference/src/lib.rs`) set `tool_calling:
ToolCallingMode::ForceNative` for **every** model unconditionally, and leave
`enable_thinking` at goose's `default_true()`. The comment above the struct says
"Thinking OFF -- GIAP handles thinking via PromptState + ThoughtFilter", but
`..Default::default()` never sets the field. The registry on the device confirms
`enable_thinking: true`. The comment describes an intention the code does not carry out.

**Goose guesses from a hardcoded table.** `default_settings_for_model`
(`goose-local-inference/src/local_model_registry.rs:261`) matches the HF repo id against a
static `FEATURED_MODELS` list. A model in the list with `native_tool_calling` gets
`ForceNative`; anything unknown gets `Auto`. GIAP then overrides it anyway.

**GIAP guesses from the filename.** `ModelCapabilities::from_model_name`
(`pond-core/src/models/domain/model_capabilities.rs`) substring-matches the model name for
vision/thinking/tool support and defaults to `context_window_tokens: 4096`. It is called at
`pond-inference/src/engine.rs:341` -- **two lines after the real chat template has already
been loaded from the GGUF and then ignored.**

So dropping in Qwen3, Llama 3.2, Phi-4 or SmolLM3 today means: native tool calling forced on
whether or not the template supports it, thinking enabled whether or not the model has it, a
4096-token capability guess, and a KV cost constant measured for Gemma.

## 3. Runtime detection is not only possible, it is already in reach

Everything needed is exposed by `llama-cpp-2 0.1.146` at load time and is being discarded:

| signal | API | what it settles |
|---|---|---|
| embedded chat template | `model.chat_template(None)` -> `.to_string()` | tool use, thinking, roles |
| all GGUF metadata | `meta_val_str` / `meta_count` / `meta_key_by_index` | architecture, geometry |
| trained context | `n_ctx_train()` | the real window, not a name guess |
| vocabulary | `n_vocab()` / `token_to_str()` | special tokens (`<think>`, `<tool_call>`) |
| geometry | `n_layer` / `n_head` / `n_head_kv` | KV cost |

### Verified against the real files on the device

Reading the GGUF metadata directly (no model load), both shipped models report:

```
tools variable        YES (3 hits)      -> tool user
tool_call token       YES (15 hits)
tool_response         YES (18 hits)
enable_thinking flag  YES (5 hits)      -> thinker, gated by a template variable
<think> tags          no                -> Gemma uses channel/thought, not <think>
channel/thought       YES (4 hits)
gemma4.context_length = 131072          -> real trained window
```

The template is byte-identical (18,855 chars) between E2B and E4B, which is itself useful:
capability is a property of the *family*, not the quant, so detection results cache cleanly.

### Cross-family validation (Mac, 2026-08-16)

Run against the eleven GGUFs on the MacBook -- five architectures, no device involved:

| model | arch | `tools` var | `enable_thinking` | `<think>` | channel/thought | reading |
|---|---|---|---|---|---|---|
| DeepSeek-R1-Distill-Qwen-1.5B | qwen2 | **0** | no | 3 | no | thinker, **not** a tool user |
| NVIDIA-Nemotron3-Nano-4B | nemotron_h | 12 | 4 | 23 | 5 | tool user + gated thinker |
| Nanbeige4.2-3B | nanbeige | 8 | 2 | 11 | 8 | tool user + gated thinker |
| Gemma-4-12B-it | gemma4 | 3 | 6 | 0 | 5 | tool user + gated thinker (channel) |
| Gemma-4-E2B-it | gemma4 | 3 | 4 | 0 | 3 | tool user + gated thinker (channel) |

Three things fall out of this:

1. **The discriminator works and it matters.** DeepSeek-R1-Distill has **no `tools` variable**,
   so its template cannot render tool declarations at all. GIAP forces
   `ToolCallingMode::ForceNative` on every model unconditionally, so loading it today means
   forcing native tool calling onto a template with nowhere to put the tools. That is a
   concrete break waiting for the first non-Gemma model, and it is the same shape as the
   empty-turn failure already being seen.
2. **The thinking marker is family-specific and must be read, not assumed.** Qwen, Nemotron
   and Nanbeige use `<think>`; both Gemma sizes use channel/thought and have **zero**
   `<think>` hits. A `ThoughtFilter` hardcoded to either one is wrong half the time.
3. **Naive substring counting has false positives.** DeepSeek scores 1 hit on `tool_call`
   despite having no tool support -- almost certainly a token name rather than template
   logic. The probe must distinguish template *control flow* (`{% if tools %}`) from
   incidental string occurrences; a hit count of 1 against 8-39 is the tell. Parse the
   Jinja, or at minimum require the `tools` variable to appear in a control structure.

Context windows also vary far more than the 4096 default guess: 131072 (Gemma E2B, DeepSeek),
262144 (Gemma 12B, Nanbeige), **1048576** (Nemotron). All free from metadata.

### Proposed capability probe

A `ModelProbe` that runs once per GGUF and is cached by file path + mtime:

- `tool_user`: the template references a `tools` variable and a tool-call marker. Distinguish
  *native* (template renders tool schemas, as Gemma's `format_parameters` macro does) from
  *emulated* (no `tools` variable -> tools must go in the system prompt as prose).
- `thinker`: one of three shapes, and they need different handling --
  (a) template-gated (`enable_thinking` variable: Gemma 4, Qwen3),
  (b) always-on tag emitter (`<think>`/`</think>` in the vocab: DeepSeek-R1 distills),
  (c) none.
- `context_window`: `{arch}.context_length` from metadata, then `n_ctx_train()`, then the
  memory-derived cap. Never the name heuristic.
- `thinking_marker`: the actual tag pair, so `ThoughtFilter` stops needing per-family
  hardcoding.

Confidence should be recorded, and an explicit user override must still win -- detection is
evidence, not law.

## 4. The KV constant is derivable, which matters more than it sounds

`KV_KIB_PER_TOKEN = 56` in `jetson_context_size` carries a comment saying it "moves on a
measurement from the Orin and nothing less". It does not have to. From GGUF metadata alone:

```
effective_kv_layers = block_count - shared_kv_layers
global layers use  (key_length     + value_length)     x head_count_kv x 2 bytes
SWA    layers use  (key_length_swa + value_length_swa) x head_count_kv x 2 bytes
```

Against the device measurements:

| model | metadata | computed | measured |
|---|---|---|---|
| E4B | 42 blocks - 18 shared = 24 (4 global + 20 SWA), kv_heads 2, k/v 512, swa 256 | 4x4 + 20x2 = **56 KiB/tok** | **56** |
| E2B | 35 blocks - 20 shared = 15 (3 global + 12 SWA), kv_heads 1, k/v 512, swa 256 | 3x2 + 12x1 = **18 KiB/tok** | **18** |

Exact for both. This is the single highest-value item here: it turns "measure every new model
on the device or risk OOM-killing the board" into a computation that runs before the weights
are even loaded, which is precisely what a model-agnostic pond needs.

**Caveat:** the global:SWA layer split came out 1:5 for both, and I did not find a
`sliding_window_pattern` key in these files -- so that ratio is currently inferred from the
measured cache split, not read. Before trusting this for an unseen architecture, either find
the key or fall back to the conservative path. Note also that goose's own
`estimate_max_context_for_memory` computes a simpler version of this that ignores SWA
entirely, and so overestimates cost by ~2x for Gemma.

### Implementation note: where the template actually sits

`pond-core::models::domain::gguf` already parses GGUF headers, and `read_gguf_head`
(`pond-api/src/routes.rs`) feeds it the first **1 MB**. That is enough for geometry and
nowhere near enough for the template. Byte offsets of `tokenizer.chat_template`, measured
across the Mac's models:

| model | template offset |
|---|---|
| Nanbeige4.2-3B | 3.76 MB |
| DeepSeek-R1-Distill-Qwen-1.5B | 5.65 MB |
| Nemotron3-Nano-4B | 7.50 MB |
| Gemma-4 E2B / E4B / 12B | ~15.03-15.04 MB |

The cause is ordering: on gemma-4-E2B the geometry keys are at offsets 924-1,829,
`tokenizer.ggml.tokens` starts at 2,061, and the template lands at 15,764,787 -- directly
after the token array. So the split is:

- **Geometry (KV cost): reachable today.** First ~2 KB, no I/O change.
- **Template (capability detection): needs a seek**, not a bigger buffer. Raising
  `HEAD_BYTES` to 16-24 MB would work but reads tens of megabytes per file during a
  filesystem sweep. The parser already skips arrays by advancing a position, so the honest
  fix is to let it report where it stopped and have the caller do one targeted second read.

**Landed 2026-08-16** (Mac only, no device involved): `GgufInfo` now carries
`head_count_kv`, `key_length`, `value_length`, `key_length_swa`, `value_length_swa` and
`shared_kv_layers`, with `kv_bytes_per_token(swa_per_global)` / `kv_kib_per_token`. Tested
against transcribed fixtures and, via an `#[ignore]`d env-gated test, against the real files
on disk -- both paths return 18 KiB/token for E2B and 56 for E4B, matching the Orin.
**Wired into `jetson_context_size` the same day.** It now takes
`kv_kib_per_token: Option<u64>`, and `apply_jetson_settings` reads the header via
`kv_cost_from_header`. The rule there is deliberately asymmetric, because assuming more
sliding-window layers than a model has makes the cost come out LOW, which is the direction
that OOMs a board: a model with **no** `key_length_swa` is dense, the pattern cannot change
the answer, and it is trusted for any architecture; a model **with** `key_length_swa` is
trusted only for an architecture whose ratio has been confirmed against a real allocation on
the device (today: `gemma4` at 1:5). Anything else returns `None` and the caller keeps the
measured constant.

This is a **no-op for every model currently shipped** -- E2B computes 18 KiB/token but is
`MAX_CTX`-bound either way, and both E4B quants compute exactly the 56 the constant already
carried -- which is what made it safe to land from the Mac. A regression test pins that
equivalence, so if wiring ever moves a shipped model the build says so.

**Not verified on hardware.** The caller lives inside `#[cfg(feature = "cuda")]`, which
cannot compile without `nvcc`, and CI's `cargo check` does not pass that feature either --
so those few lines are reviewed, not compiled, until the next device build.

### Landed 2026-08-16: the seek fix and `ModelProbe` (Mac only)

**The seek fix.** My earlier suggestion in this document -- "let it report where it stopped
and have the caller do one targeted second read" -- was wrong, and worth recording as wrong.
You cannot compute where `tokenizer.ggml.tokens` ends: it is an array of variable-length
strings, so the only way past it is to walk its per-element length prefixes. There is no
offset to seek to.

What works is a `GgufSource` trait, so the walk reads through a buffered file rather than a
slice. The win is not the seeking, it is that **skipping stops requiring the bytes**: a
scalar or a whole string becomes position arithmetic, and a string array costs one 8-byte
length read per element instead of allocating a `String` for each of a quarter-million
tokens. `parse_gguf_file` reaches a template 15 MB in having read a few hundred kilobytes,
in about 40 ms per model.

`parse_gguf_header(&[u8])` is unchanged for callers that only want geometry.

**`ModelProbe`** (`pond-core/src/models/domain/model_probe.rs`) reads `ToolSupport`
(Native / Absent / Unknown) and `Thinking` (Gated / Always / Absent / Unknown, each carrying
the real marker) from the template. Against all eleven GGUFs it produces five distinct
classifications:

| classification | models |
|---|---|
| Native + Gated `<\|think\|>` | Gemma 4 E2B, E4B x2, 12B |
| Native + Gated `<think>` | Nemotron3-Nano-4B, Nanbeige4.2-3B |
| **Absent + Always `<think>`** | **DeepSeek-R1-Distill-Qwen** |
| Native + Absent | gemma3-270m |
| Unknown + Unknown | the three MTP/assistant drafts, which carry no template |

Three design points that the evidence forced:

1. **Control flow, not substrings.** DeepSeek mentions `tool_call` once while supporting no
   tools. The probe scans only inside `{% ... %}` and only for whole words.
2. **`Unknown` is not `Absent`.** A model with no template has not said it cannot use tools.
   `supports_native_tools()` answers false for both, but the states stay distinct so a
   caller can tell "cannot" from "did not say".
3. **The marker is read, never assumed.** Gemma 4 emits `<|think|>` -- **not** the
   `<|channel>thought` that `model_capabilities.rs` documents. That doc comment is stale.

**Wired the same day.** Both `apply_platform_settings` and `apply_jetson_settings` now read
the model before deciding, through a pure `tool_and_thinking_for(&ModelProbe)` -- pure
because the CUDA caller is compiled by nothing on a developer machine or in CI, so a decision
buried inside it would be tested by neither.

| probe | tool mode | why |
|---|---|---|
| `Native` | `ForceNative` | unchanged from the blanket behaviour |
| `Absent` | `ForceEmulated` | the template cannot carry tools; describe them in prose instead |
| `Unknown` | `Auto` | no template was readable, so leave goose its own judgement |

Run over the eleven real GGUFs through the adapter's own `probe_model`:

```
gemma-4-E2B-it-Q4_K_M          ForceNative   thinking=true
gemma-4-E4B-it-Q4_K_M/Q5_K_M   ForceNative   thinking=true
gemma-4-12b-it-IQ4_XS          ForceNative   thinking=true
NVIDIA-Nemotron3-Nano-4B       ForceNative   thinking=true
Nanbeige4.2-3B                 ForceNative   thinking=true
DeepSeek-R1-Distill-Qwen-1.5B  ForceEmulated thinking=true
old_functiongemma-270m-it       ForceNative   thinking=false
gemma-4-*-assistant (MTP x3)   Auto          thinking=false
```

**Every model in service keeps exactly what it had** -- all four Gemmas stay
`ForceNative, thinking=true`. The only rows that move are DeepSeek and the template-less
drafts, none of which is being served. That is what made this safe to land from the Mac,
and a test asserts the probe *separates* the collection rather than quietly returning one
answer for everything, which is the failure mode that looks like success.

`enable_thinking` is now stated rather than inherited. It was never set, so it took goose's
`default_true()` while the comment above it read "Thinking OFF" -- the registry on the device
sided with the code. A gated thinker still gets `true`, so nothing that currently reasons
stops; the only change is that a model with no reasoning markers gets `false` instead of a
flag it has nothing to do with. The stale comments are corrected in place.

**Still not verified on hardware.** The CUDA caller needs `nvcc`, and CI's `cargo check` does
not pass that feature, so those lines are reviewed and not compiled.

Worth noting for later: now that skipping is free, `read_gguf_head` in `pond-api` could use
`parse_gguf_file` and get templates during the catalogue sweep for ~40 ms per model, rather
than the 1 MB slice that cannot reach them.

## 5. Suggested order of work

1. **Capture the failing error string** (`RUST_LOG` run, one prompt). Everything about E4B is
   hypothesis until this exists. Cheapest item, highest information.
2. **Fix `enable_thinking`** to match its comment, or fix the comment. Currently they
   disagree and the registry sides with the code.
3. **Build `ModelProbe`** as a pure function over GGUF metadata + template text, with tests
   over checked-in template fixtures from several families. No fork patch needed to read.
4. **Wire the probe into `apply_*_settings`**, replacing the unconditional `ForceNative` and
   the `from_model_name` guess. This is where the fork patch lands, since `ModelSettings`
   needs to carry the outcome.
5. **Derive `KV_KIB_PER_TOKEN`** from the probe, keeping the constant as a fallback for
   metadata that does not parse.
6. **Then** add a second family (Qwen3-4B is the natural first: dense, ~2.5 GB at Q4_K_M
   against E4B's 4.7 GB for comparable capability, and a genuinely different thinking shape
   to prove the probe).

## 6. What this does not solve

Detection tells you what a model *can* do, not whether it *fits*. E4B's problem on a 7,620 MB
board is memory, and no amount of capability probing changes that. The two are complementary:
the probe makes an unseen model safe to try, and the memory arithmetic decides whether it can
run at all.
