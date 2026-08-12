# The cost of a re-prefill on the Orin Nano

Measured 2026-08-11 on the Jetson Orin Nano 8GB (`nano`, JetPack R36.4.7, `MAXN_SUPER`), with
`llama-bench` from `llama.cpp` build `0ef6f06`, CUDA backend, `-ngl 99`, against
`gemma-4-E2B-it-Q4_K_M.gguf` (2.88 GiB, 4.65 B params) — the headline on-device configuration.

This is the measurement PAI-3 P5 and PAI-4 P5 had been open on. Both bullets asked for it by name
and neither could be closed without it.

## The numbers

Two repetitions per point, model fully offloaded, GIAP's service stopped so the GPU was uncontended.

| prompt depth | prefill (tok/s) | **cold TTFT** |
|---|---|---|
| 512 | 674.59 ± 44.67 | 0.76 s |
| 2 048 | 943.51 ± 3.06 | 2.17 s |
| 4 096 | 976.48 ± 9.10 | 4.19 s |
| 8 192 | 934.89 ± 0.27 | 8.76 s |
| 12 288 | 870.70 ± 0.34 | 14.11 s |
| 16 384 | 820.55 ± 1.05 | **19.97 s** |

Decode is flat at **30.35 ± 0.39 tok/s** (`tg64`) and **30.37 ± 0.18** (`tg128`) — unchanged across
the sweep, which is what the memory-bandwidth model predicts: decode is bound by streaming the
weights, and the weights do not grow with context.

**Cold TTFT is the depth divided by the rate at that depth**, and the rate is not constant. Prefill
peaks near 4k and then falls away — 976 tok/s at 4 096, 820 at 16 384 — so the cost of a re-prefill
grows *faster than linearly* in the retained context. At the pond's real 16 384-token window a cold
prefix costs **twenty seconds** before the first visible token.

## What this settles

**PAI-4 P5's first clause — "cold-cache recompaction shows no TTFT penalty" — holds, and now has a
magnitude behind it.** When the prefix is already cold the turn pays that prefill whatever the
trimmer does, so compacting *at that moment* is free: the work is already owed. The rule's
disjunction (recompact when cold, prefer not to when warm) is the right shape.

**And the warm half is worth far more than it looked.** The reason to avoid a needless invalidation
is not a percentage, it is the difference between roughly 0.1 s and 20 s on the same turn. A single
gratuitous prefix move at full context costs the user two hundred times a warm turn's time-to-first-
token. That is the whole argument for PAI-4 P5's age rung being conditional rather than periodic,
and for PAI-3's preamble clamp not moving between turns of one session.

**PAI-3 P5's `output_reserve_tokens` gets its unit cost.** Reserving *R* tokens of output costs
`R / 30.35` seconds of generation on this hardware — 512 tokens is 17 s, 1 024 is 34 s. The reserve
is a latency budget as much as a context one, which is the argument for it being derived from the
window rather than fixed: the same 1 024-token reserve is 6 % of a 16 k window and 25 % of a 4 k one,
but it is 34 seconds either way.

## CORRECTION: this is not the llama.cpp GIAP serves turns with

Raised as soon as the numbers were published, and it is the right question. There are **three**
llama.cpp builds in play on a GIAP pond and this measurement used the one that serves nothing:

| build | what it is | serves turns? |
|---|---|---|
| `~/llama.cpp` @ `0ef6f06`, installed to `~/.local/bin` by `scripts/jetson/llama-optimization/` | upstream, built standalone for sm_87 | **no** — a benchmarking and tuning tool |
| `llama-cpp-sys-2 = 0.1.146`'s vendored tree, built by its `build.rs` with `--features cuda` | what `goose-local-inference` links | **yes — this is the live path** |
| the same crate as pulled by `pond-inference` | GIAP's own engine | no — PondAgent is quarantined (Q2-05) |

So the table above characterises **the hardware and the model**, not the engine. Two consequences,
and they are not the same size:

- **Decode (30.35 tok/s) transfers.** It is memory-bandwidth-bound: ~102 GB/s against a 2.88 GiB
  model is a ceiling near 35 tok/s, and 30.35 sits just under it. A different llama.cpp build cannot
  move that much, because the bottleneck is the bus and not the kernels. `output_reserve_tokens`'
  latency argument rests on this number and stands.
- **Prefill (820-976 tok/s) is provisional.** It is compute-bound and therefore sensitive to exactly
  the things that differ between these builds — flash-attention support, batch and ubatch defaults,
  which quantisation kernels were compiled. The vendored tree is recent enough to use the
  `llama_memory_seq_rm` API rather than `llama_kv_self_seq_rm`, but `llama-cpp-sys-2` vendors it
  without git metadata, so the exact commit is not recoverable from the checkout.

**What survives regardless:** the SHAPE. Prefill is two orders of magnitude faster per token than
decode, it degrades with depth, and a re-prefill of a full window costs seconds while a warm turn
costs a fraction of one. The 200x warm-versus-cold argument for PAI-4 P5 holds even if the absolute
prefill rate moves by a third in either direction. What should not be quoted as an engine fact is
"19.97 s at 16 384" — quote it as "about twenty seconds on this hardware with this model, measured
on a neighbouring build".

Closing this properly means benchmarking through `goose-local-inference` itself, which is the same
deploy PAI-4 P5's second clause needs. One run answers both.

## What this does NOT settle, stated plainly

- **The warm path is arithmetic here, not a measurement.** `llama-bench` starts cold every time, and
  `llama-cli`'s `--prompt-cache` was removed from this build (`error: invalid argument`), so there
  was no way to demonstrate prefix reuse with the tools on the box. "Warm TTFT ≈ delta ÷ rate" is
  what a retained KV prefix *means*; it is not evidence that GIAP's engine retains one.
- **PAI-4 P5's second clause — "warm-cache turns show no new re-prefills" — therefore remains open.**
  That is a claim about the GIAP code path (the llama.cpp prompt-session KV cache patch, and
  `PrefixCacheState` recording the six invalidation reasons honestly), and it can only be shown by a
  turn-over-turn TTFT trace from a deployed GIAP binary. The pond on `nano` is at `445fd391`, well
  behind this branch, so that run is still owed.
- **This is one model on one board.** E4B was not measured: its KV cost is roughly 86 KiB/token
  against E2B's 18, so at any useful depth it spills, and the spill is a different phenomenon from
  the one being measured here.

## How to reproduce

```bash
ssh nano
~/.local/bin/llama-bench \
  -m ~/.local/share/goose-in-a-pond/models/gguf/gemma-4-E2B-it-Q4_K_M.gguf \
  -ngl 99 -p 512,2048,4096,8192,12288,16384 -n 128 -r 2
```

Stop the GIAP service first (`systemctl --user stop giap`) or the numbers are contended. Note that
`sudo` is password-gated on this box, so the `drop_caches` step in
`scripts/jetson/llama-optimization/` cannot be run non-interactively; the sweep above fit without it
because E2B is 2.88 GiB against ~4.5 GiB available, but a larger model needs that step and will hit
the NvMap 586 MiB wall without it.

---

## Engine-true run, 2026-08-11 (Mac): NO DEGRADATION, and PAI-4 P5's second clause closes

The correction above said the sweep used the wrong llama.cpp and that closing this properly means
benchmarking through `goose-local-inference`. Done, on the Mac, through the live serving path —
`GooseAdapter` -> `Agent::reply` -> `GiapProviderShim` -> `LocalInferenceProvider` -> llama.cpp —
against an isolated `POND_DATA_DIR` with the models symlinked in, so it is real inference and it
never touched the real pond. Two turns, one session, `gemma-4-E2B-it-Q4_K_M`.

| | turn 1 (cold) | memory extraction | turn 2 (warm) |
|---|---|---|---|
| plan | `CreateContext` | `SacrificialContext` | **`ReusePrefix(6845)`** |
| prompt tokens | 6 969 | 500 | 7 032 |
| TTFT | 12 344 ms | — | **465 ms** |
| prefill | 11 690 ms | — | **64 ms** |
| decode | 38.9 tok/s | — | — |
| reasoning tokens | **128** | — | 0 |
| model_load | 4 806 ms | — | null |

**No degradation after ~85 commits on this branch.** The skill's recorded baseline is "reuse-turn
TTFT 12.4 s -> 0.65 s, prefill 11.6 s -> 66 ms"; this run gives 12.34 s -> **0.465 s** and 11.69 s ->
**64 ms**. Marginally better, not worse. Turn-1 prompt is 6 969 tokens against the documented
~6.9K for 60 tools, so nothing in this session's work inflated the preamble.

**`session_retained=false` on turn 1 and `true` for every generation after, with the SAME
`loaded_ptr=0x86253e840` throughout.** That is the model-slot identity check from the skill: distinct
pointers across generations would mean the path-keyed slot regressed into a silent full model reload
per generation.

**PAI-4 P5's second clause — "warm-cache turns show no NEW re-prefills" — is now observed rather
than argued.** Turn 2 reused 6 845 of 7 032 tokens; only the 187-token delta was prefilled, at 64 ms
against turn 1's 11 690 ms. And the memory-extraction side call took `SacrificialContext` both times
(cached 7 111, then 7 041) rather than clobbering the chat prefix, which is the specific failure that
would have made the retained cache useless in practice.

**PAI-5 P5 gets a real number too:** 128 reasoning tokens on the reasoning turn, 0 on the recall
turn, measured through the live path rather than inferred. That is consistent with the 306-token
figure in `context_budget.rs`'s Jetson anchor comment being a ceiling rather than a typical value,
and it means the 768-token reserve at that tier holds reasoning plus a real answer with room to
spare.

**Caveat that keeps this honest: this is the Mac, not the Orin.** Metal prefill here is 596 tok/s
against the Orin's CUDA 820-976 in the standalone sweep, so the two are not interchangeable and the
absolute latencies do not transfer. What transfers is the *mechanism*: the prefix is token-stable
across turns, the cache is reused, and the side call does not evict it. Running this same probe on
the Jetson needs the branch deployed there.

---

## Why a turn is slow, M4 Mac, 2026-08-11: it is the tool schemas, not the engine

"12 seconds to answer *what time is it?*" is a fair complaint, so here is where the time goes. Three
measurements on the same machine (Apple M4, 24 GB, `gemma-4-E2B-it-Q4_K_M`) separate the causes.

### 1. The engine is already at the hardware ceiling

Standalone `llama-bench` (Homebrew, Metal) against GIAP's live path on the same model:

| prompt depth | standalone llama.cpp | GIAP via `goose-local-inference` |
|---|---|---|
| 512 | 670 tok/s | — |
| 2 048 | 630 tok/s | — |
| 4 096 | 596 tok/s | — |
| 6 912 | 538 tok/s | — |
| **~6 969 (real turn)** | — | **596 tok/s** |

**GIAP's prefill matches or beats the standalone build at the same depth.** There is no engine
overhead to remove: the retained-KV path, the provider shim and the tool filtering cost nothing
measurable on the prefill side. Anyone hunting for a slow wrapper here will not find one.

Note also that prefill degrades with depth on Metal exactly as it does on CUDA — 670 to 538 across
this range — so a longer prompt is punished twice: more tokens, and a worse rate for all of them.

### 2. The prompt is the problem, and 88 % of it is tools

`provider payload size` for the turn above: `system_chars=2548`, `tools_count=61`,
`tools_json_chars=25449`.

That is 61 tool schemas on a question that needs one. Of 6 969 prompt tokens, roughly 6 100 are tool
definitions — **about 88 %** — and at ~550 tok/s they cost about ten of the eleven-and-a-half seconds
of prefill. The conversation itself is under a second.

### 3. The lever already exists and is off by default

`tool_selection_mode` defaults to `"all"`. Setting it to `"relevant"`, same machine, same model,
same question:

| | `"all"` | `"relevant"` | change |
|---|---|---|---|
| tools sent | 61 | **17** | -72 % |
| tools JSON | 25 449 chars | **6 761 chars** | -73 % |
| prompt tokens | 6 969 | **2 495** | **-64 %** |
| prefill | 11 690 ms | **3 326 ms** | **-72 %** |
| **TTFT** | **12 344 ms** | **4 065 ms** | **3.0x faster** |
| prefill rate | 596 tok/s | 750 tok/s | +26 % (shallower prompt, better rate) |
| reasoning tokens | 128 | 122 | unchanged in substance |

Three times faster to first token, and the model still reasoned (122 tokens against 128) and still
answered correctly. The reasoning budget is untouched because what was cut is schema, not thinking.

**This is the single largest available win on the on-device path, it is already implemented, and it
is one setting.** The reason it is not the default is that `"relevant"` has not had burn-in — a
mis-scored turn silently lacks a tool it needed, which is worse than being slow. That is a real
objection and the answer is burn-in, not permanent avoidance.

### 4. One gap worth chasing separately: decode

| | standalone | GIAP |
|---|---|---|
| decode | 50.61 tok/s | 38.9-40.0 tok/s |

GIAP decodes about **22 % slower** than the same model under `llama-bench` on the same box. That is
not the prefill story and is not explained by prompt size, since decode is per-token. Candidates, in
the order I would check them: the thought filter running per token, SSE frame construction per token,
sampler configuration differences, and the reasoning-token accounting added by PAI-5 P2. It is worth
about 10 tok/s on every answer the pond ever gives.

### And a genuinely surprising cross-machine result

The Orin Nano's CUDA prefill (976 tok/s at 4 096) **beats this M4's Metal prefill** (596 tok/s at the
same depth), while the M4 wins decode (50.6 against 30.4). Prefill is compute-bound and the Orin's
CUDA kernels are strong for it; decode is memory-bandwidth-bound and the M4's unified memory is
faster. So a long-prompt turn is *relatively* better on the Jetson and a long-answer turn is better
on the Mac — which is the opposite of the intuition that the laptop is simply the faster machine.
