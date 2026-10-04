# Goose Submodule Patch Management

## Overview

GIAP pins a fork of [aaif-goose/goose](https://github.com/aaif-goose/goose) as a git submodule at
`goose/`. The fork lives at `https://github.com/jarida-io/Goose` and carries a small set of
patches on top of upstream. This document describes those patches, the branch strategy, and
how to rebase against a new upstream goose release.

---

## GIAP Patch Set

### Branch: `main` (current pin)

`jarida-io/Goose:main` = upstream goose `main` (synced 2026-07-25, 675 commits,
upstream tip `192b5db8b`) merged into the previous `giap-patches-rmcp-1.5`
history, carrying:

| Patch | Description | Files |
|--------|-------------|-------|
| ollama tool-less retry | Retry the turn tool-less when an Ollama model rejects tools with HTTP 400 ("does not support tools"). Adds `is_tools_unsupported_error` + splits `stream` into `stream` (catch/retry) and `stream_inner` (single attempt), so non-tool models (e.g. `gemma3:4b`) answer as plain chat models instead of aborting the turn. Originally `d52efa2e`; re-ported after upstream moved the provider. | `crates/goose-providers/src/ollama.rs` |
| GIAP featured Gemma models | Extra `FEATURED_MODELS` entries for the Gemma 4 family GIAP ships on-device: E1B, E2B (+mmproj), 12B-A4B (+mmproj), 27B (+mmproj). Upstream only features E4B and 26B-A4B. Featured status gives these models `ToolCallingMode::ForceNative` defaults and vision wiring. | `crates/goose-local-inference/src/local_model_registry.rs` |
| llama.cpp ProviderStats parity | The llama.cpp backend fills `ProviderStats` (TTFT, `model_load_ms`, `elapsed_ms`, `output_tokens`) like the MLX backend already does, plus two new optional fields: `prefill_ms` and `effective_context_tokens`. Additive/serde-default; PR upstream. Commit `961f1b0a5`. | `crates/goose-provider-types/src/conversation/token_usage.rs`, `crates/goose-local-inference/src/lib.rs`, `crates/goose-local-inference/src/llamacpp/*` |
| thinking-only turns count as empty | `provider_produced_content` treated a `Thinking`/`RedactedThinking` block as content, so a turn that emitted only reasoning — no text, no tool call — was not an empty turn and never reached the existing `MAX_EMPTY_TURN_RETRIES` path. The user got total silence. Small local models hit this reliably: gemma-4-E2B closes its thinking block and emits `end_of_turn` for certain phrasings (measured 5/5 on one query, gemma-4-E2B Q4_K_M). Two match arms now return `false`; safe because the flag accumulates over the chunk loop (thinking-then-text still sets it via the `Text` arm, thinking-then-tool-call is covered by `no_tools_called`). Verified: silent turn -> visible "empty response" message. Also makes the retry budget overridable via `GOOSE_MAX_EMPTY_TURN_RETRIES` (default unchanged at 3): the built-in retry re-sends an *unchanged* conversation, which a deterministic provider answers identically, so a host that varies the prompt itself sets this to `0` and owns recovery. GIAP does exactly that — see `EMPTY_TURN_STEER` in `goose_agent.rs`, which recovered a reliably-silent query into a real 3-tool answer 4/4. When the budget is 0 the `EMPTY_TURN_MESSAGE` is yielded but NOT persisted: it is a signal that the turn failed, not an answer, and persisting it left a junk assistant turn that the host's next attempt then read as context (and which moved the KV prefix). Upstream behaviour is unchanged for any budget > 0. Upstreamable — not GIAP-specific. | `crates/goose/src/agents/agent.rs` |
| session-scoped agent goal | `Agent::goal` is a single slot and an `Agent` is shared: a host serving several conversations concurrently holds one `Arc<Agent>`, so `set_goal` is process-wide mutable state and one conversation's goal is injected into another conversation's turn as a user message. For a multi-user host that is one person's request text appearing in another person's turn. GIAP bounds concurrent chat streams with `Semaphore::new(4)` and drives all four from one retained agent, so wiring the existing goal-completeness check at all required this first. Adds `session_goals` keyed by `SessionConfig::id` (already per-reply, so it cannot race), plus `set_session_goal` / `get_session_goal`; the completeness check prefers the session goal and falls back to the process-wide one, so `set_goal` is unchanged for existing callers and `None` changes nothing. Cleared alongside `set_goal(None)` on turn exit so the map does not accumulate dead sessions. Deliberately NOT a field on `SessionConfig` — cleaner conceptually, but a required field on a public struct with 38 construction sites is a rebase burden not worth it for an additive property. Upstreamable. Commit `9cf946903`. | `crates/goose/src/agents/agent.rs`, `crates/goose/tests/agent.rs` |
| the completeness check must not reach the user, in question or in answer | TWO HALVES, one unit to re-apply. **(a) Wording.** The completeness check, the grind reminder and the `/goal` kickoff each append an INVISIBLE USER message — invisible to the person, ordinary user text to the model, and the last thing in the context before generation. All three said `**Goal:** {goal}`, bolded, with the noun repeated around it. Measured 2026-08-12 on gemma-4-E2B and E4B: the models answered the household in that vocabulary ("I could not fully meet your goal", "The goal has not been fully met"). A host prompt forbidding the jargon was already in place and did not hold — it sits thousands of tokens earlier, behind the whole tool schema, and a competing suggestion at last position beats a buried instruction. The wording is therefore the fix: no label, no Markdown emphasis, no noun a model reaches for when describing a shortfall. Behaviour unchanged — same messages, same trigger points, still demanding completion. `stop_hook_denial_context_message` is deliberately left alone: same category, no measurement covering it, and every changed line is a line to re-apply at the next rebase. Guarded parent-side by `pond-adapters-goose/src/goose_nudges.rs`, which `include_str!`s this file (that guard is NOT in `pond-core`, which must stay buildable without the submodule). **(b) Visibility, and (a) alone was not enough.** The nudge is hidden, but the model's REPLY to it is an ordinary assistant message, indistinguishable at the transport layer from a reply to the person -- so it was streamed and then CONCATENATED onto the real answer. Observed on gemma-4-E4B for "Check the weather": `It is 22 degrees Celsius and sunny in Nairobi.I used the current system context ... I cannot keep working on it as I have already provided a direct answer.` -- note the missing space, two inference outputs glued together. Wording cannot fix this (the first version leaked "goal", the reworded one leaked "keep working on it"; any phrasing leaks something, because the model is being asked a question and answering it). `goal_check_pending` already carries the fact -- set when the nudge is issued, cleared the moment tools are called -- so a round with the check pending and ZERO tool requests is by construction the check's own reply, and cannot contain anything the first answer could not have. It is no longer yielded, no longer appended to `last_assistant_text`, and kept in history as not-user-visible so strict providers still see a well-formed conversation. A round WITH tool calls stays visible: that is the check doing its job (measured 4 calls -> 7 and the real answer). Also re-anchored `test_goal_nudges_agent_before_exit`, whose needle was the pre-reword wording and had gone vacuous. Upstreamable. Commits `7849bd484`, `52a7827c2`. | `crates/goose/src/agents/agent.rs` |
| llama.cpp prompt-session KV cache | Retains one generation context per loaded model and reuses the shared prompt prefix across turns (partial KV removal to the divergence point); small non-matching prompts run in a throwaway "sacrificial" context so side calls never clobber an expensive chat prefix. Also fixes model-slot identity (cache keyed by canonicalized file PATH, not the caller's model-name spelling — several spellings of one GGUF used to evict each other with a silent full reload per generation) and holds the global runtime through a strong Arc. Adds `ProviderStats.reused_prefix_tokens`. Measured: reuse-turn TTFT 12.4s -> 0.65s, prefill 11.6s -> 66ms (M-series, ~7K-token prompt). PR upstream planned. Commit `bfd2e854b`. | `crates/goose-local-inference/src/lib.rs`, `crates/goose-local-inference/src/llamacpp/*`, `crates/goose-provider-types/src/conversation/token_usage.rs` |
| llama.cpp on-disk preamble snapshot | The retained KV cache makes the SECOND turn of a session cheap and does nothing for the first, so every cold start -- a new session, a model swap, a restart -- re-decodes a byte-identical preamble. Measured on an M-series host: 6,685 prompt tokens prefill in 24.1s at 277 tok/s, against 26-40 new sessions in a day, while the same KV state reads back from disk in a fraction of a second. `prompt_snapshot.rs` persists prefix KV state to `<data>/prompt-cache/`, with the format version in the FILE NAME so an incompatible snapshot is never opened rather than opened and rejected. Uses only `state_seq_save_file`/`state_seq_load_file` already exposed by the pinned `llama-cpp-2 =0.1.146` -- no sys-crate change. Every failure path falls back to a normal prefill, holding the engine's standing rule that a bad cache never fails a turn. **The first version kept ONE snapshot per model -- the longest run every prompt had shared since process start -- and that is not what shipped.** One stable prefix is only correct when every prompt has the same shape, and a real pond interleaves chat, the scheduler and MCP side calls, whose preambles diverge early; the single prefix collapsed toward their common ancestor and stopped being worth restoring. What ships instead is one snapshot per prompt SHAPE, in per-shape buckets, chosen by a prefix ladder: incremental FNV-1a hashes at depths [256, 512, 1024, 2048, 4096, 8192, 16384], so a candidate is rejected on integers alone before any file is read. Bounded at `MAX_SNAPSHOTS = 6` (LRU), written only between `SNAPSHOT_MIN_TOKENS = 1024` and `SNAPSHOT_MAX_TOKENS = 6144`, and a snapshot the prompts have outgrown is RETIRED -- deleted -- rather than retried on every turn forever. Writes are gated on `turn_was_cold`: the turn that already paid for a full prefill is the one that can afford to serialise it, and a warm turn must not be slowed down to fill a cache it did not need. Three bugs found by running it rather than reading it: `max_tokens` on `state_seq_load_file` is an output BUFFER CAPACITY, not a filter (passing the prompt length produced `token count in sequence state file exceeded capacity! 5341 > 5033` on ordinary turns -- it must be `ctx.n_ctx()`); a stable prefix that had never been intersected over-fit the first prompt it saw; and the module's `info!` lines were invisible because `EnvFilter` matches TARGET, not module, so they now log to an explicit `giap::kv` target that crate-level carves cannot suppress. Truncating before the write is the privacy property, not an optimisation: a KV blob encodes the tokens that produced it, and a shared prefix cannot contain anything a second prompt did not also contain -- GIAP's memories and turn context ride the user message, which diverges. Never shared BETWEEN models (KV state bakes in layer count, head dims and rope), which is the point: a model swap is the most expensive cold start there is and each keeps its own file. Loaded tokens are checked to be a genuine prefix of the prompt rather than trusted, because the key cannot see a changed system prompt or tool set. **Verified on hardware** (gemma-4-E2B-it Q4_K_M, M-series, 2026-08-16): a 1,202-token snapshot is 21.2 MiB, i.e. **18.0 KiB/token** -- which independently reproduces the 18 KiB/token measured for E2B on the Orin, so the blob really is the KV cache. Restore-plus-tail-decode 132 ms against 1,898 ms for the equivalent full prefill (**14.4x**); the restored context predicts the same argmax and the same top five, with a max logit delta of 0.007 against logits spanning ~20 -- batch-shape float noise, not divergence. Bitwise equality is NOT asserted and demanding it was the first version's mistake: the two paths batch differently by construction and llama.cpp's reduction order follows the batch shape. Live tests, all `#[ignore]`d, run with `--ignored`: `a_restored_snapshot_answers_identically_to_a_full_prefill`, `two_turns_of_one_shape_leave_a_snapshot_on_disk`, `thinking_off_does_not_change_what_the_model_is_told_about_tools`, `how_much_two_chats_share_depends_on_whether_their_tools_match` (the last measures the common ground the design rests on: 100% with identical tools, 85% when the differing tools sort last, 70% -- below the floor -- when they do not, which is why the parent orders tools core-first). They share one process through a `OnceLock` backend because `LlamaCppBackend::new()` is `unreachable!` on a second init. Upstreamable, and a natural extension of the KV cache patch above. | `crates/goose-local-inference/src/llamacpp/prompt_snapshot.rs` (new), `crates/goose-local-inference/src/llamacpp/inference_engine.rs`, `crates/goose-local-inference/src/llamacpp/mod.rs` |
| model-config failures name the session and the keys | "Could not resolve model config: missing provider" named neither which session had no stored config nor which global key was consulted, so an embedder whose own provider wiring had silently not run got an engine-internal string and no way to tell which knob was at fault. Both resolution sites (Agent reply and the platform-extensions tool-call side) now name the session id, the resolved-or-missing provider and the exact keys (GOOSE_PROVIDER / GOOSE_MODEL / goose config), in deliberately identical wording so one failure cannot read as two. Parent-side companion: pond PR #303 (session rows always written or the turn refused; GOOSE_PROVIDER exported as backstop; errored turns not re-engaged). Upstreamable. Commit `6a12584af` (cherry-picked from `b1eb61792`). | `crates/goose/src/agents/agent.rs`, `crates/goose/src/agents/platform_extensions/mod.rs` |
| llama.cpp KV cache type + physical batch | `ModelSettings` gains `type_k`/`type_v` (ggml type names, e.g. `"q8_0"`) and `n_ubatch`, wired into `build_context_params`. Upstream exposes `n_batch` but not the PHYSICAL batch, and no KV cache type at all, so neither of the two memory levers that measured as free on an 8 GB Orin was reachable from a host. Measured on gemma-4 E4B at ctx 16384 (M4/Metal, deterministic allocation, reproducible to +/-1 MiB across five runs): KV 296 -> 157 MiB and peak process footprint 437 -> 307 MB at `q8_0`; compute buffer 522 -> 129 MiB at `n_ubatch = 128` with decode unchanged (Orin sm_87: 14.4 vs 14.3 tok/s, prefill 38.3 vs 35.6). `q8_0` is quality-neutral by two independent tests -- greedy output BYTE-IDENTICAL to f16, and a paired per-chunk wikitext-2 run (n = 100, E2B Q4_K_M) giving dNLL -0.000987 +/- 0.000551, t = -1.79, i.e. indistinguishable from f16 at 95%. `q4_0` is accepted but deliberately not used: t = 0.35 on the mean, but 6.4x the per-chunk variance, so its average hides swings. `q5_1` measured WORSE than `q4_0` (t = 3.87) which is implausible on bit-count grounds and is most likely a flash-attention kernel path -- unverified, so treat that one as avoid-not-explained. A quantised V cache requires flash attention; that is checked and warned rather than left to fail opaquely at context creation. Additive and serde-default throughout, including the SDK DTO and BOTH `management.rs` mapping sites, so a settings round-trip cannot silently drop them. Upstreamable. | `crates/goose-local-inference/src/local_model_registry.rs`, `crates/goose-local-inference/src/llamacpp/inference_engine.rs`, `crates/goose-local-inference/src/management.rs`, `crates/goose-sdk-types/src/custom_requests.rs` |
| builtin spawn panic is contained to its extension | `extension_fn(reader, writer)` runs synchronously inside the extension-loading future, so a panicking spawn fn unwound the loader while sibling builtins were mid-initialize -- their duplex peers dropped and every builtin reported broken-pipe/Closed. Observed 2026-08-27 in the voice child: one uninitialised `OnceLock` in giap-context's spawn fn (`init_context_deps() not called`) killed all fourteen builtin servers for the process, presenting as a total tool outage. `catch_unwind(AssertUnwindSafe(..))` around the call converts a startup panic into that extension's own `ConfigError`; the parent separately stopped its spawn fns panicking at all (the eleven remaining `.expect("init_*_deps")` sites now log-and-return, the pattern giap-sensors already used), so this is the belt for spawn fns that panic for any other reason. Upstreamable -- not GIAP-specific. Commit `9a91cbf42`. | `crates/goose/src/agents/extension_manager.rs` |
| MOIM reaches the hardware GIAP ships to | `MIN_CONTEXT_FOR_MOIM` 32,000 -> 4,096. Goose injects `<turn-context>` into the last user message every turn -- current time, working directory, tokens remaining, turn budget, plus whatever the `todo` and `tom` extensions contribute -- and it is the only mechanism either side has for an instruction that survives compaction. The gate reads the MODEL's context limit (`get_context_limit`, falling back to `ModelConfig::context_limit`), NOT the host's prompt budget, and that is the detail that decides whether it fires: an Orin Nano running gemma-4 E2B/E4B has a 16,384 window, so upstream's 32,000 skipped the whole mechanism on the hardware GIAP ships to, while a Mac dev box at 32,768 had it on the whole time -- which is exactly why nobody noticed. Do not try to reproduce the difference locally without pinning the context limit. 4,096 is the smallest window GIAP's budget curve has an anchor for (`context_budget::PROFILE_ANCHORS`), keeping a floor below which the block's own ~60 tokens would be a meaningful fraction of the prompt. **Needs its parent-side half or it does nothing**, and the converse: GIAP's provider shim used to delete every `<turn-context>` before the provider saw it (`strip_turn_context`), so lowering this alone would have composed a block the shim then deleted, and un-stripping alone would have deleted a block that was never composed. The shim half landed in parent `47b6346f`; `strip_turn_context` is deliberately kept and still tested there as the lever to pull if the block costs more in KV churn than it returns. No stale-block problem, checked rather than assumed: `inject_moim` works on `conversation.clone()` (`agent.rs:2093`) and the result is used for that provider call only, so the stored conversation never accumulates injections. The cost is real and worth carrying: the block holds a minute-resolution timestamp and sits in the LAST user message, so the prompt TAIL changes every turn. It does not move the static prefix -- system prompt and tools sort before it, confirmed from a captured payload pair (`GIAP_CAPTURE_PAYLOAD`) -- but `goose-providers::is_turn_context_text` exists upstream so Anthropic's cache can exclude the block, and the local llama.cpp path has no equivalent exclusion. **Not upstreamable as written**: lowering a constant changes behaviour for every upstream host. The upstreamable shape is an env override (`GOOSE_MIN_CONTEXT_FOR_MOIM`) defaulting to 32,000, which is how `GOOSE_MAX_EMPTY_TURN_RETRIES` was handled in the thinking-only-turns row above. | `crates/goose/src/agents/moim.rs` **Since 2026-09-24 GIAP sets `GOOSE_DISABLE_MOIM=1` for local providers (`goose_env_knobs`), so this threshold only matters to an HTTP provider on a small-context model; measured cost of the block on a local pond was ~100 re-prefilled tokens per inference.** |
| turn-context is appended, not prepended | `inject_moim` inserted `<turn-context>` at the FRONT of the last user-role message on every provider call, on a clone that is never stored. A prefix-caching engine reuses tokens up to the first byte that differs, so the holder changed from its first byte each call. Measured on the Mac (release, gemma-4-E2B): 471 tokens re-prefilled on the completeness-check inference and 589 on the next turn's first inference; a thinking-only session whose user messages goose had merged re-prefilled the ENTIRE conversation on every call, 10-21K tokens per turn -- the shape the household pond logged all day on 2026-09-24. Appended as the last content item instead: 97 and 147 tokens, completeness-check TTFT 1.05 s -> 0.3 s (E2B) and 2.0 s -> 0.75 s (E4B). The three placement tests assert the new order. **Upstreamable as written**: prepending buys nothing a model can use, and every prompt cache in front of any provider pays for it. Commit `36413f065` (2026-09-24). See `docs/developer/realtime-inference-audit-2026-09-24.md`. | `crates/goose/src/agents/moim.rs` |
| prefill_ms waits for the GPU | Metal and CUDA return from `decode` once the graph is submitted and bill the rest to whoever reads the logits next -- the first `sample`, which lands in TTFT instead of prefill. Measured on Metal (E2B, release): a 160-token warm prefill read 13-33 ms while 250-360 ms of its work appeared as a gap between prefill end and the first token, and every rate derived from `prefill_ms` inherited it. `prepare_generation` now synchronizes the context it prefilled (transient or retained) before taking the timestamp; re-measured gap 0-2 ms, TTFT unchanged. Upstreamable. Commit `afe7adc69` (2026-09-24). | `crates/goose-local-inference/src/llamacpp/inference_engine.rs` |
| speculative decoding removed from the llama.cpp backend | The whole MTP line -- `llamacpp/mtp.rs`, `SessionCtx::Mtp`, the drafter load beside the target, the `draft` parameter through every prefill helper and the speculate branch of the decode loop -- is gone, at Jerry's request (2026-09-24). On Metal the drafter cost 6-36% of decode at depth 4 and 8 with the E2B drafter present; on the Orin it had measured 1.8x over eight real turns, at the price of a second context, the no-VMM pool, a drafter download and a model reload on every switch flip. The registry still accepts `draft_model`/`draft_n_max`/`draft_p_min` and this backend ignores them; `ResolvedModel::draft_model_path` stays for upstream's MLX backend. The same commit adds a debug `prefill breakdown` log (memory estimate / render / tokenize / validate / plan+decode) and the streaming parser's init time. The harness that measured the Orin (`target_forward_cost_by_batch_size`) lives at `afe7adc69`. Commit `743649d98`; its test target did not compile until `a0624f881` (a stale `draft` argument and a helper only the MTP tests used). | `crates/goose-local-inference/src/llamacpp/{mtp.rs (deleted), inference_engine.rs, mod.rs, inference_native_tools.rs, inference_emulated_tools.rs}` |
| `GOOSE_DISABLE_MOIM` host switch | A host that already carries time, budget and memories in the user message pays for `<turn-context>` twice: duplicate information, and ~100 tokens re-prefilled per inference because the block is injected on a clone and never stored. `GOOSE_DISABLE_MOIM=1` removes the block AND the system-prompt paragraph that announces it; default unchanged; one test pins that both halves go together. Upstreamable as an option. GIAP should set it in `goose_env_knobs` for local providers (file held by another session on 2026-09-24). Commit `d4157795d`. | `crates/goose/src/agents/moim.rs` |
| LiteRT-LM backend (`litert`) | A third `LocalInferenceBackend` beside llama.cpp and MLX that runs `.litertlm` models through the LiteRT-LM C API 1.0.0 (GIAP's LiteRT-LM fork `giap-main` at `a5dd27b5`, on upstream `b320801b`; C API 1.0.0 is in no release yet; upstream builds still load), chosen per registry entry by `backend_id: "litert"`, so the host keeps `chat_provider = "local"` and the one-resident-model eviction. **Loading.** `libloading` 0.9, absolute path, `RTLD_NOW | RTLD_LOCAL` (the library carries its own Rust std, allocator, abseil and protobuf), on a helper thread with a 30 s bound and `catch_unwind`; searched in `GOOSE_LITERT_LIB_DIR`, `<exe dir>/litert-lm`, `<exe dir>`, `<goose data>/litert-lm/lib`. A missing or broken library is a `ProviderError` at model load. A hand-written table of 51 C functions, plus giap-main's three resolved all or none and its parse-failure switch resolved alone, no bindgen, no build.rs, no shim, no feature flag: nothing native at cargo time, so CI's `cargo check` compiles it everywhere. **Prompt cache.** One LiteRT Conversation per loaded model. A request whose canonical history extends what it holds sends only the new messages (one message as an object, because LiteRT's channel filter keys on a lone user object). A different system prompt or tool set runs in a throwaway conversation when it weighs under half the retained one (the llama.cpp sacrificial rule), otherwise it takes the slot; a diverged history, changed `enable_thinking`/`max_output_tokens`, an error or a cancel recreates the conversation with the history as preface. **With giap-main** (`litert_lm_conversation_send_prefill_text_stream`, `litert_lm_conversation_save_kv_snapshot`, `litert_lm_conversation_load_kv_snapshot`) a diverged or repeated history is *rematched* instead: its whole prompt, rendered by a conversation that never runs, is prefilled into the retained conversation from step 0 and LiteRT skips the tokens its KV cache holds. Before a throwaway side call runs, the retained conversation is *set aside* (saved as a KV snapshot when it holds at least 512 tokens, then deleted), because two live conversations make LiteRT copy the whole KV cache; the next request of its family (model and engine settings, system message, tools, thinking: keying on the start of the system message alone let the proactive review, which opens like the chat, write over the chat's snapshot) restores the snapshot into a conversation without a preface and prefills its whole prompt over it. While nothing is retained, the conversation last set aside is weighed in its place, so light side calls after a quiet spell (titling, a memory-extraction pass) neither take the slot nor write snapshots. New conversations in that mode never take the history as a preface, so a rematch never sees it twice; a render or prefill that fails before anything streams falls back to the preface path. Snapshots: `<goose data>/litert-lm/kv-snapshots`, each the whole KV window (151 MB for E2B at 16384 tokens), the three most recently used kept. Tool calls get ids `lrt_<16 hex round>_<i>` so goose's per-call split messages fold back into the one message the model produced. **Rejected tool calls** (giap-main's `litert_lm_conversation_config_set_return_error_on_parse_failure`): the conversation keeps a call its parser rejects, as a text part with an "error" field, instead of failing the turn; the backend keeps that part out of the reply and reads it with a port of giap-main's lenient reader (`litert/fc_repair.rs`: a stray or missing string delimiter, a trailing comma, a missing bracket, and a bare word where a string belongs). One still unreadable is left out and logged. gemma-4-E2B's `segment:knowledge,tier:permanent` had failed a turn with INVALID_ARGUMENT. **Stats** are measured: TTFT to the first text or thought (None on a tool-call-only turn), `prefill_ms` from LiteRT's per-turn prefill rates, `reused_prefix_tokens` = KV tokens the turn did not prefill again (after = reused + prefilled + decoded, checked live; `None` on a whole-prompt turn, whose benchmark counts the skipped tokens as prefilled, LiteRT's log has the figure), `effective_context_tokens` = `max_num_tokens`, `draft: None` (MTP acceptance is not exposed). **Settings.** `ModelSettings.litert: Option<LiteRtSettings { backend, speculative_decoding }>`, serde-default, ignored by the other backends; the settings DTO does not carry it, so `update_model_settings` keeps the stored block. `max_num_tokens` is always explicit (`context_size`, then `GOOSE_CONTEXT_LIMIT`, then 4096, clamped to the file's maximum); speculative decoding defaults on and is applied only when the file supports it; benchmark info is on (it also switches LiteRT's GPU sync to busy-wait). **Verified on the Mac** (M4, gemma-4-E2B `.litertlm`, GPU and CPU, thinking off and on): tool round, a side call that leaves the retained conversation intact, a cancel that recovers on the next turn. **2026-10-04, giap-main** (GPU and CPU, thinking off and on; upstream `3dbb23e1` on the CPU as the control): GPU tool calls exact; the side call sets the chat aside, a second one while nothing is retained writes nothing, and turn 2 restores the chat (745 of 814 tokens reused, TTFT 155 ms on the GPU), turn 3 extends the restored conversation (816 reused), a regenerated turn rematches (816 of 834; 315 ms on the CPU against 1,527 ms cold upstream), a cancel recovers from the snapshot (745 of 855); `scripts/pai_bench.py` against a scratch pond on the debug binary: 12 pass, PAI-7 not run, a new session's first turn rematched 3,001 of 3,224 tokens, a 3,606-token snapshot saved in 121 ms and loaded in 109 ms; a turn after 16 quiet minutes restored the chat that titling had set aside, 3,658 of 4,072 tokens reused, TTFT 836 ms (4,497 ms when families were keyed on the head of the system message and the proactive review wrote over the chat's file). With thinking on, LiteRT's default `filter_channel_content_from_kv_cache` rewinds the second user turn of every new conversation to step 0 (measured: turn 2 reused 0 tokens, turn 3 reused 170) -- the filter-off comparison and every Orin number are still owed. **Upstreamable** as a backend: it is GIAP-agnostic and needs no build-time native code; the `lrt_` id convention and the throwaway rule are the parts upstream would review. Parent mirrors `libloading` in its workspace dependencies. Commits `f253d1330`, `e27ad7334`, `bf5d9d524`. | `crates/goose-local-inference/src/litert/{mod.rs, ffi.rs, convert.rs}` (new), `crates/goose-local-inference/src/{lib.rs, local_model_registry.rs, management.rs}`, `crates/goose-local-inference/Cargo.toml`, `Cargo.toml`, `Cargo.lock` |
| compaction keeps its order across a reload | `compact_messages` built the copy of the user's last message it keeps after the summary before running the summary, and gave it that moment's `created`. A session reloads its messages `ORDER BY created_timestamp, id`, seconds then insertion, so once a summary took a second or more the copy sorted ahead of the summary and the continuation note on the next turn, and `merge_consecutive_messages` folded it into the summary and the note into the reply after it. Every turn after an in-turn compaction reached the provider with its history rewritten: the model read its last question with the summary glued after it, and a prefix-caching engine re-prefilled the whole conversation (LiteRT-LM on the Orin, gemma-4-E4B: 950-2,200 tokens, 6-11 s, on each turn after a compaction; on the Mac, E2B, TTFT on those turns fell from 2.3-2.9 s to 0.7-1.3 s with the fix, and LiteRT's own log showed them extending instead of re-sending from step 0). The copy is now stamped when it is pushed, never earlier than the message ahead of it. A manual compaction keeps no copy and was not affected; tool-pair summaries already take the pair's `created`. Sessions compacted before the fix stay garbled until their next compaction rewrites the tail. The test's summary takes 1.1 s, as a device's does; without the fix the reload order is `[0, 1, 2, 5, 3, 4]`. **Upstreamable as written.** Commit `f06a009b8` (2026-10-04). | `crates/goose/src/context_mgmt/mod.rs` |

### Divergences NOT in the table above — recorded 2026-09-11

Found by reading the tree against `origin/main` rather than the table. An
undocumented patch is one that gets dropped at the next sync, which is what this
document exists to prevent — so these are listed even where the right answer is
"delete it".

| Divergence | Where | Disposition |
|---|---|---|
| **The whole MTP / speculative-decoding line** -- a new `llamacpp/mtp.rs`, `SessionCtx::Mtp`, drafter loading, the `draft` parameter through the prefill helpers, `cuda-no-vmm`, and a dependency-graph change (`llama-cpp-2` `=0.1.146` -> `=0.1.156` + `common`, patched to a private fork rev) | `crates/goose-local-inference/**`, both `Cargo.toml`s | **REMOVED from the engine 2026-09-24 at Jerry's request** (commit `743649d98`, row above). What stays: the `draft_n_max`/`draft_p_min` registry fields (accepted, ignored -- GIAP still writes them), the `cuda-no-vmm` feature (harmless without a second context; can go back to `cuda` when the device build is next touched), and the `llama-cpp-2 =0.1.156` fork pin, which is still needed for the OpenAI-compat templating that 0.1.156 dropped. The last commit carrying the line is `afe7adc69` |
| `crates/goose/src/prompts/giap_system.md` | goose crate | **Delete.** GIAP branding committed into a crate this table calls upstreamable, referenced by nothing, and dead by construction — it is absent from `TEMPLATE_REGISTRY`, so `render_template` returns `TemplateNotFound`. It was never needed: `render_template` already prefers `Paths::config_dir()/prompts/<name>`, so a host overrides any registered template with no patch at all |
| Four `.snap.new` insta artifacts, three under a stray nested `goose/goose/` tree | `crates/goose/src/agents/snapshots/` | **Delete.** `.snap.new` is insta's *failure* output. They are also stale fossils — they say "created by Block, the parent company of Square" against current snapshots saying "created by AAIF" — so they carry no signal and will mask the next real snapshot change |
| `mcp_client.rs:208` — upstream's cross-session `assert!` downgraded to `tracing::debug!` + overwrite | `crates/goose/src/agents/mcp_client.rs` | **Decide.** A relaxed concurrency invariant. It predates the side branch and sits on both fork branches, so it is not new — it is simply undocumented |

**A measurement caveat worth carrying with the MTP row.** `mtp.rs` records
"47.7 tok/s against 15.8, 87% draft acceptance". The only file in the repo
carrying `draft_n_accepted` is `c2-mtp.json` from the 2026-09-08 bake-off, whose
runtime is `llama-server` — **the sidecar, not the in-process path these commits
built.** Both llama.cpp generate paths hardcode `draft: None` in `ProviderStats`
(`inference_native_tools.rs`, `inference_emulated_tools.rs`) while `MtpSession`
accumulates the counters, so the in-process acceptance rate has never been
observed. Carry the feature; do not carry the number.

### Patches subsumed by upstream (dropped in the 2026-07 sync)

- **MCP session panic→warning** (`5f7dceea`) — merged upstream long ago.
- **`native_tool_calling` / `use_jinja` on `ModelSettings`** — upstream adopted the
  concept with a richer design: `tool_calling: ToolCallingMode { Auto, ForceNative,
  ForceEmulated }` and `chat_template: ChatTemplate { Embedded, Builtin, CustomInline }`.
  `Embedded` (the default) uses the GGUF's embedded Jinja template — what
  `use_jinja: true` did. GIAP code now sets `ToolCallingMode::ForceNative` where it
  used to set `native_tool_calling = true`.
- **lopdf 0.40 → 0.42 security bump** (`e8afab5c`) — upstream is on 0.42.

### rmcp

Upstream now declares `rmcp = "^1.4"`; the fork no longer diverges on rmcp at all.
The GIAP parent workspace still forces `rmcp = "=1.5.0"` so the goose crates and
`pond-mcp-server` resolve to a single rmcp version.

---

## Branch Layout

```
jarida-io/Goose
├── main                   ← CURRENT PIN: upstream 2026-07 sync + patch set above
├── giap-patches           ← legacy patch branch (pre-rmcp-1.5)
└── giap-patches-rmcp-1.5  ← previous pin; still referenced by parent `main`'s CI
```

The parent repo (`goose-in-a-pond`) pins the submodule to the tip of
`jarida-io/Goose:main`. `.gitmodules` names the branch, and
`.github/workflows/ci.yml` clones that branch's HEAD directly (bypassing the
stored submodule SHA).

> **That is not true today, 2026-09-11.** The parent pins `d43cd702c`, which is on
> `feat/llama-cpp-2-0.1.156-oai-fork` — **8 ahead of fork `main`, 0 behind**. The
> breaking-sync procedure directly below was followed up to its last step and then
> stopped: the side branch exists, the parent-side port landed (`Cargo.toml` pins
> `llama-cpp-2 =0.1.156` and patches it to `jarida-io/llama-cpp-rs-giap` rev
> `ad6e4b85`, which declares 0.1.156), but the fast-forward into fork `main` never
> happened.
>
> Consequences, both live: fork `main` still asks for `=0.1.146`, which that patch
> **cannot satisfy**, so a CI run resolves a graph no shipping build uses — and
> `cargo check` never links, so it passes. And the side branch carries
> `cuda = ["llama-cpp-2/cuda-no-vmm"]`, the flag that fixed a shipped Jetson OOM,
> while fork `main` has plain `cuda`; `.gitmodules` `branch = main` means
> `git submodule update --remote` walks back onto the broken one.
>
> The fix is the missing step: fast-forward fork `main` to `d43cd702c` (clean, 0
> behind). Until then, treat every green CI run as evidence about a goose nobody
> ships. `--locked` was added to every CI cargo step so the mismatch fails loudly.

**Stage breaking syncs on a side branch.** CI clones the fork branch tip by
name, so moving `main` underneath parent branches whose code still targets the
old goose API fails their fresh CI runs. Prepare and verify a sync on a
temporary fork branch, then fast-forward it into fork `main` together with the
one parent commit that carries the matching API port (`.gitmodules` + `ci.yml`
+ submodule SHA + docs). Delete the temporary branch afterwards.

### Parent workspace dependency mirror

When goose crates are built as path deps from the GIAP workspace root, cargo
resolves their `workspace = true` dependency entries against the **GIAP**
`Cargo.toml`, and goose's own `[patch.crates-io]` is ignored. After any sync:

1. Mirror new/changed keys from `goose/Cargo.toml [workspace.dependencies]` into
   the parent `Cargo.toml` (union features where GIAP needs defaults goose
   disables).
2. Where goose moved to a new MAJOR a pond crate is not ready for, keep goose's
   version in the workspace key and pin the old version directly in the pond
   crate (done for rand, sha2, thiserror, dirs, tower-http in the 2026-07 sync).
3. Replicate any `[patch.crates-io]` entries goose's build graph needs (none as
   of 2026-07 — v8/cudaforge are not in GIAP's graph).

---

## Fresh Clone Setup

```bash
git clone https://github.com/jarida-io/goose-in-a-pond
cd goose-in-a-pond
git submodule update --init --recursive
cargo build -p pond-server
```

No manual submodule surgery required. The pinned SHA must be reachable on
the current patch branch — verify with:

```bash
git -C goose branch -r --contains $(git ls-tree HEAD goose | awk '{print $3}')
# expected output:  origin/main
```

---

## Rebasing Patches Against a New Upstream Release

When you want to pull in new upstream goose commits:

```bash
cd goose

# One-time: add the upstream remote
git remote add upstream https://github.com/aaif-goose/goose

git fetch upstream main
git checkout -B sync-staging origin/main

# MERGE (not rebase) upstream — the branch history contains merge commits, and
# a merge needs no force-push. Resolve conflicts, re-porting the patch set
# above where upstream moved or redesigned the touched code.
git merge upstream/main

# Compile-gate FROM THE PARENT before pushing anything (see the dependency
# mirror section above — the parent Cargo.toml usually needs updates too):
cd .. && SQLX_OFFLINE=true cargo check -p pond-server -p pond-adapters-goose

# Fast-forward fork main, then flip .gitmodules + ci.yml + submodule SHA +
# this document in ONE parent commit.
git -C goose push origin sync-staging:main
git add goose .gitmodules .github/workflows/ci.yml docs/goose-patch-management.md
git commit -m "chore: sync goose fork with upstream (<date>)"
```

After the parent commit lands, team members must run:

```bash
git submodule update --init --recursive
```

---

## Proposed patch — `Auto` mode must still honour `NeverAllow`

**Status: NOT APPLIED.** Written up here rather than committed to the submodule
because CI clones the fork branch tip directly (see `ci.yml`), so a submodule
change that exists only in a working tree makes the local build pass and every
other build fail. It needs staging on the fork and a pointer bump, exactly as
"Adding a New GIAP Patch" below describes.

**What it buys.** GIAP narrows the tool surface per session — a Guest turn does
not get the memory tools, a dormant extension group does not get its schemas.
Today that narrowing is *schema-only*: `provider_shim.rs :: enforce_tools`
filters the `&[Tool]` slice handed to the provider, but Goose keeps every
extension loaded agent-wide, `Agent::reply` collects every `ToolRequest`
regardless of whether its schema was published, and dispatch happens
before the adapter ever sees the event. So a model that names a withheld tool
anyway still runs it. `goose_agent.rs` now tracks those calls in
`suppressed_tool_ids` and drops both the call event and its result — which
closes the disclosure (a Guest turn's withheld `recall_memories` used to stream
the household's memories back as `ToolResult { tool: "" }`) — but the tool has
still executed by then.

**Why the obvious levers do not work.**

- `ToolInspectionManager` is the seam Goose provides for exactly this, and it is
  unreachable: `Agent::tool_inspection_manager` is `pub(super)` and inspectors
  are only added inside the private `Agent::create_tool_inspection_manager`.
- Writing `PermissionLevel::NeverAllow` through `PermissionManager` — which GIAP
  already passes into `AgentConfig` — looks like it should work and does not.
  `permission_inspector.rs` matches on the mode first:

  ```rust
  let action = match goose_mode {
      GooseMode::Chat => continue,
      GooseMode::Auto => InspectionAction::Allow,   // <- returns before the check below
      GooseMode::Approve | GooseMode::SmartApprove => {
          if let Some(level) = permission_manager.get_user_permission(tool_name) {
              match level {
                  PermissionLevel::NeverAllow => InspectionAction::Deny,
                  ...
  ```

  GIAP hardcodes `GooseMode::Auto`, so the `NeverAllow` arm is unreachable, and
  moving off `Auto` would turn on approval prompts for every tool call — a
  different product.

**The patch.** Honour an explicit `NeverAllow` in `Auto` mode too. "Auto" means
*do not ask me*, not *ignore the denies I configured*; a deny the user set and
the agent ignores is the worse reading of the flag, and this is upstreamable
rather than GIAP-specific.

```rust
GooseMode::Auto => match permission_manager.get_user_permission(tool_name) {
    Some(PermissionLevel::NeverAllow) => InspectionAction::Deny,
    _ => InspectionAction::Allow,
},
```

`crates/goose/src/permission/permission_inspector.rs`. The existing
`#[test_case(GooseMode::Auto, false, None, InspectionAction::Allow; "auto_allows")]`
still passes (no stored permission); add a case asserting `Auto` + `NeverAllow`
denies.

**GIAP side, once it lands.** On each turn, write `NeverAllow` for the tools
outside the session's allow-set and `AlwaysAllow` (or clear) for those inside,
before `Agent::reply`. Note `PermissionManager` is process-global and keyed by
tool name, not by session, so with concurrent sessions of differing scope the
last writer wins — either serialise the write with the turn or upstream a
session-scoped variant. Until that is settled, `suppressed_tool_ids` in
`goose_agent.rs` is the containment, and it is a disclosure gate only.

---

## Adding a New GIAP Patch

1. Checkout `main` in the submodule: `git -C goose checkout main`
2. Make and commit your change in the submodule
3. Push: `git -C goose push origin main`
4. In the parent repo, stage and commit the updated pointer:
   ```bash
   git add goose
   git commit -m "chore: bump goose to include <patch description>"
   ```
5. Add a row to the patch table in this document.

---

## Discarding a Patch (upstreamed or no longer needed)

If a patch lands in upstream `block/goose:main`:

1. After rebasing (see above), the patch will be a no-op and `git rebase` will drop it automatically.
2. Remove its row from the patch table above.
3. Update the parent repo pointer as usual.
