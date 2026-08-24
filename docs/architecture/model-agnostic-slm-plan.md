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


### Landed 2026-08-24: the probe reaches the PROMPT side (Mac only)

Steps 3-5 of the order of work below put `ModelProbe` into the ENGINE's
settings. Step 6 -- a second family -- then found what that left open: the
probe was a private helper on `LocalInferenceLlmAdapter`, consulted once at
model-registration time, and its answer went into goose's `ModelSettings` and
nowhere else. The prompt side could not reach it.

`GooseAdapter::thinking_section_applies` resolved `thinking_mode = "auto"`
through `ModelCapabilities::from_model_name`, which knows `gemma-4`, `qwen3`,
`qwq` and `deepseek-r1`. For any other reasoning model the two layers
disagreed:

| layer | source | Nemotron |
|---|---|---|
| engine `enable_thinking` | the template (gated `<think>`) | **true** |
| prompt `<thinking>` section | the filename | **false** |

A model switched into reasoning mode and given no prompt section telling it
what to do with it produced an empty first turn, was re-engaged with
`EMPTY_TURN_STEER`, and fabricated a weather report rather than calling the
weather tool. `5b04a197` pins the gap; this closes it.

**Why the filename was there, and what the replacement had to preserve.** The
doc comment on `thinking_section_applies` was right about its reason: the
`model_capabilities` cache is refreshed inside the provider-SWAP branch of
`ensure_provider_current`, which runs LATER in the turn that builds the prompt,
so on turn 1 it still held `ModelCapabilities::default()`. Turn 1 rendered a
prompt without the section and turn 2 rendered one with it -- 78 characters at
the top of the static prefix, which moved `prefix_hash` and cost every session
a full re-prefill on its second turn (3.7 s on the Orin). Any replacement had
to be synchronous, cheap, and identical on turn 1 and turn 2.

Reading the file satisfies all three. `probe_cached`
(`pond-core/src/models/domain/model_probe.rs`) memoises on
`(path, mtime, len)`: ~40 ms once per model per process, a hashmap lookup
thereafter, and a pure function of bytes that are not changing mid-session.
`model_traits` (`pond-adapters-goose`) resolves a settings model name to its
GGUF through the existing `resolve_gguf_filename` and answers three questions
-- does it reason, what marker does its template carry, what was it trained
for. HTTP providers keep the name heuristic, which is the right answer there
rather than a concession: for Ollama the name genuinely is all there is.

The capability CACHE is now overridden from the same source, so the cache and
the prompt agree by construction rather than by both guessing the same way.

Read off the real files through the new resolver:

| model | marker | trained context |
|---|---|---|
| gemma-4-E2B-it-Q4_K_M | `<\|think\|>` | 131,072 |
| NVIDIA-Nemotron3-Nano-4B-Q4_K_M | `<think>` | 1,048,576 |
| DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M | `<think>` | 131,072 |
| Nanbeige_Nanbeige4.2-3B-Q4_K_M | `<think>` | 262,144 |

The name heuristic answered "no reasoning, 4096 tokens" for Nemotron.

**A correction worth recording, because it nearly became a bug.** `ModelProbe`
returns `<|think|>` for Gemma 4, and the obvious next move -- feed the probe's
marker to `ThoughtFilter` so each family's tag is stripped -- is WRONG. Checked
against the file rather than assumed: `<|think|>` is a vocabulary token that
Gemma's template emits at the top of the first system turn to put the model
INTO reasoning mode (`{%- if enable_thinking ... -%}{{- '<|think|>' -}}`). It is
a prompt-side SWITCH. What Gemma then emits is `<|channel>thought ... <channel|>`,
which `ThoughtFilter` already strips. Nemotron, Nanbeige and DeepSeek use
`<think>` as a genuine OUTPUT tag, which `ThoughtFilter` also already covers.
So the probe's marker answers "how does this template express reasoning" and
not "what should the filter strip"; wiring it to the filter would have told the
filter to hunt for Gemma's input switch in Gemma's output. It is logged for
diagnosis and deliberately not plumbed.

**Measurement instrument.** `scripts/model-matrix.sh` drives several models
through the same three turns in isolated scratch ponds (one GGUF HARD-LINKED
in, never symlinked) and reports TTFT cold, TTFT on the reuse turn, prompt
tokens, prefill, **reengagements** and **whether a tool was actually called**.
The last two are the columns that matter: a capability mismatch presents as
slowness, and a model that answers a weather question from imagination looks
identical to one that answered it correctly unless you check.


### Measured 2026-08-24: before and after, on the Mac

`scripts/model-matrix.sh`, three turns per model in an isolated scratch pond,
`thinking_mode = "auto"`, `tool_selection_mode = "all"`, 61 tools. Both halves
driven from the SAME harness against two binaries — a kept copy of the pre-fix
one and the fixed one — rather than a rebuild between halves.

Debug build on an M4. The absolute numbers are therefore not release
performance; the comparison is what they are for.

| | Nemotron 3 Nano 4B | | gemma-4-E2B (control) | |
|---|---|---|---|---|
| | before | after | before | after |
| turn-1 TTFT | 187.25 s | **38.93 s** | 14.94 s | 13.96 s |
| prompt tokens | 17,376 | **10,092** | 7,837 | 7,837 |
| completion tokens (t1/t2/t3) | 2 / 18 / 3 | **517 / 858 / 425** | 89 / 236 / 41 | 89 / 225 / 41 |
| reasoning tokens | 0 / 0 / 0 | **54 / 654 / 160** | 0 | 0 |
| called the weather tool | **no** | **yes** | yes | yes |

Nemotron before was not slow so much as absent: two, eighteen and three
completion tokens are the length of a stub, it never reasoned once, and it
answered "what is the weather right now?" without calling the weather tool.
After, it reasons and calls it. The control is unchanged within noise, which is
the other half of the claim — the models whose classification does not move
must not move.

**The before behaviour is INTERMITTENT, and that bounds what the table above
can claim.** A second run of the same pre-fix binary DID call the weather tool
(13 / 51 / 14 completion tokens, still 0 reasoning). So "before never calls the
tool" is not a safe statement from n = 1; "before is unreliable" is. What
reproduces across both pre-fix runs, and is therefore the honest claim:

| | before (n = 2) | after |
|---|---|---|
| reasoning tokens | 0, every turn, both runs | 54 / 654 / 160 |
| prompt tokens | 17,376 and 17,377 | 10,092 |
| completion tokens | 2-51 (stubs) | 425-858 |
| turn-1 TTFT | 187.25 s, 69.34 s | 38.93 s |
| weather tool | once in two runs | called |

The deterministic, log-confirmed change is `capabilities: thinking=false` →
`thinking=true` and reasoning tokens going from exactly zero to real. The
tool-call reliability claim needs repetitions and is being measured; the
latency figures have a wide before-spread and should be read as directional.

**Why the prompt SHRANK, which was not the intent.** The shim's accounting says
the system prompt went from 30,848 chars to 2,170, with
`tools_json_chars = 28,534` unchanged in both, and its `system_rebuilt` flag
reads `false` before and `true` after.

The arithmetic settles what was in there: 2,170 + 28,534 = 30,704, against an
observed 30,848. The tool JSON was inlined into the system prompt AND passed
as 61 native tool declarations — the model was handed the whole tool surface
twice, in two formats. 28,534 chars of JSON is ~7,284 tokens, and the prompt
difference is 17,376 - 10,092 = **7,284**. Exact.

So GIAP's system-prompt veto was not firing for this model, and the cost of it
not firing was a duplicated tool surface. WHY it declined to fire is still
open: `enforce_system` returns `None` when the incoming prompt neither starts
with GIAP's prefix nor carries the goose default marker, which means goose
handed over something GIAP did not recognise as its own. Worth chasing on its
own account — a veto that silently declines is a larger problem than the token
count that exposed it — and worth recording that two plausible explanations
(a prose-tools section keyed on `caps.tool_calling`, and the context governor
resolving a different window) were both checked against the source and are
wrong.

### Llama 3.2 3B: the family the pond had never seen

Downloaded fresh and run through the same harness. The probe reads it
correctly — `tools=Native, thinking=Absent`, which is right on both counts —
and it called `giap-system__get_current_time` unprompted on its first turn.

But its turn-1 TTFT is **145 s**, on a prompt of **18,001 tokens**, against
Gemma's 7,837 for the same 61 tools and the same 28,534 chars of tool JSON
handed to both by the shim.

That gap is a property of the chat template, not of the pond: Gemma's renders
tool declarations in its own reduced form and Llama's renders them close to
verbatim. So the tool surface the pond was tuned around costs Llama roughly
2.6x what it costs Gemma, and 16K of its 18K-token prompt is spent before the
user says anything.

The budgeting cannot see this. `ContextGovernor::prompt_window` clamps the
preamble GIAP writes to `LOCAL_PROMPT_CLAMP` = 8192 for local providers, but
tool schemas are rendered by the model's own template downstream of that clamp
and are never counted against it. The reasoning recorded above
`COMPACT_SHAPE_CEILING` in `prompts.rs` rests on "at `all` the schemas are
~6,500 tokens" — Gemma's number, treated as every model's.

Turn 3 prefills in 148 ms, so the KV prompt-session cache is working exactly as
designed. This is a cold-turn cost. It is also the first thing a user meets.

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
