# Jetson inference-engine bake-off

Orin Nano 8 GB Super, JetPack 6.2.1 / L4T r36.4.7, CUDA 12.6, MAXN_SUPER with dynamic
clocks. Target `gemma-4-E4B-it-qat-UD-Q4_K_XL` (4,020 MB), `n_ctx` 16384 derived at
56 KiB/token, `tool_selection_mode=relevant`.

Harness: [`scripts/jetson/bakeoff/`](../../scripts/jetson/bakeoff/README.md).
Generated tables: [`jetson-engine-bakeoff-results.md`](jetson-engine-bakeoff-results.md).
Device prep and its measured effect: [`jetson-device-tuning.md`](jetson-device-tuning.md).

**Status: C1 and C2 measured. C3 (vLLM) and C4 (TensorRT Edge-LLM) not run.**

## Recommendation

**Bump the vendored engine. Do not adopt a sidecar.**

The two levers worth having — speculative decoding and interleaved-SWA — are both in a
current llama.cpp and both reachable in-process. Nothing measured here argues for a
second supervised process; what it argues is that `llama-cpp-2 =0.1.146` is leaving a
large amount on the table.

| | decode | peak RAM | canary | rel[all] |
|---|---:|---:|---:|---:|
| C1 — in-process `0.1.146` | 15.8 tok/s | 4,549 MB | 10/10 | 10/10 |
| C2 — llama.cpp `990e3bf` | 17.2 tok/s | 4,538 MB | 10/10 | 9/10 |
| C2 — **+ iSWA** | 18.4 tok/s | **4,249 MB** | 10/10 | 9/10 |
| C2 — **+ MTP** | **47.7 tok/s** | 4,586 MB | 10/10 | 10/10 |

All four gates pass for every row.

## The results

### MTP is a 2.8× decode win, and it is real

47.7 tok/s against C2-baseline's 17.2. That is above the 24.2 tok/s single-stream
bandwidth ceiling for 4.2 GB of weights, which is the mechanism rather than a broken
measurement — speculative decoding emits several accepted tokens per target forward
pass. Three independent things agree:

- **87 % draft acceptance**: 198 of 227 drafted tokens accepted.
- llama.cpp's own `timings` report **46.5 tok/s** against the harness's client-side 47.7.
- Quality is unchanged: canary 10/10, `reliability[all]` 10/10 — the only variant to
  score full marks on the latter.

It costs **48 MB** for the drafter and *reduces* over-current events to 0/min.

**This contradicts what this project previously recorded** — MTP measured strictly slower
here at every draft depth (E4B 15.71 → 14.57/13.81/13.22/12.43 at n_max 1–4). That was
**ik_llama.cpp with the centroid-format drafters**. This is upstream llama.cpp's MTP with
unsloth's `mtp-gemma-4-E4B-it.gguf`. Different implementation, different drafter,
opposite result. The old finding was not wrong; it was about a different thing.

Note the drafters are not interchangeable. Ours declare `gemma4_assistant` and
`gemma4_mtp` and carry `mtp.{pre_projection,centroids,token_ordering}`; upstream
registers `gemma4-assistant` and wants `nextn.{eh_proj,enorm,hnorm,shared_head_*}`. A
different tensor layout, not a rename. The working drafter ships at the root of the same
unsloth repo whose weights are already on the device.

### iSWA saves 289 MB and is slightly faster

Peak footprint 4,538 → **4,249 MB**, available minimum 1,265 → 1,566 MB, zero swap, and
decode 17.2 → 18.4 tok/s. Quality-backed: canary 10/10 in that exact configuration.

There is a catch for the in-process path. The vendored engine hardcodes `swa_full = true`
(`inference_engine.rs`) because `ReusePrefix` does a partial `seq_rm` behind the sliding
window and is unsound without it. iSWA's saving requires `swa_full = false`. llama-server
gets both via `--ctx-checkpoints`; in-process, **the saving and the KV prompt-session
cache are currently mutually exclusive**, and the cache is worth far more (it is what
turns a follow-up turn from a full re-prefill into a resume). Take the engine bump for
MTP; treat iSWA as blocked on reimplementing checkpointed reuse.

### Decode agrees, TTFT does not compare

| metric | C1 | C2-baseline | delta | tolerance |
|---|---:|---:|---:|---|
| decode tok/s | 15.8 | 17.2 | **+8.6 %** | ±10 % — **ok** |
| engine TTFT | 1,019 ms | 345 ms | −66 % | ±15 % — out |
| prompt tokens | 3,621 | 2,848 | −21 % | ±3 % — out |

Decode is the clean comparison and it is within tolerance: the two engines agree, with
the current one ~9 % ahead. **The TTFT gap is not usable as an engine result.** C2 answers
a 21 % smaller prompt, because a replayed payload carries the tools and `<system-context>`
frozen from the moment it was captured, while C1 selects tools live per question. Some of
the 66 % is engine and some is prompt size, and this run cannot separate them.

Decode is per-token and therefore unaffected by that confound, which is why the 2.8× MTP
result stands on its own.

### Two TTFTs, and they are not the same number

C1's *turn latency* is 19.1 s; its *engine TTFT* is 1.0 s. A GIAP turn is 2–3 inferences
plus reasoning tokens plus a tool round-trip. An HTTP replay of one payload has none of
that, so its client-side first-delta **is** its engine TTFT. Ranking the two side by side
reported C2 as "98 % faster", which measured nothing. The report now ranks on engine TTFT
and prints turn latency beside it as the thing a user actually waits for.

The corollary is that **19 s of user-facing latency is not an engine problem**. Whatever
engine sits underneath, the turn structure dominates.

## What the bake-off cost, and what that says

Nine runs produced two usable candidates. Every failure was a real defect, and they share
one shape: **something initialises asynchronously, the first use races it, and the
fallback is silent and plausible.**

| # | Defect | How it presented |
|---|---|---|
| 1 | Settings PUT onto a running pond | Tuning applied at provider-build time. `n_ctx` 32768 not 16384, MemAvailable to 364 MB, swap engaged. Every turn succeeded. |
| 2 | Client-side decode rate | 43–92 tok/s against a ~24 ceiling. A GIAP turn is 2–3 inferences; `completion_tokens` sums all, the visible window covers one. |
| 3 | `relevant` could not narrow | `tool_selection_widened reason="no_embedder"` — both prompt arms were the same 61 tools. |
| 4 | `RUST_LOG` omitted `pond_adapters_local_inference` | The gate written to catch #1 read a line its own config suppressed, and reported the opposite of the truth. |
| 5 | Capture recorded the **goal check**, not the chat turn | Its last user message is "Finish anything still outstanding for this: …". Every replayed question was answered as a continuation of a failed weather conversation. C2 canary read 1/10 against C1's 10/10. |
| 6 | `pgrep -f` self-match, twice | The exclusivity gate named `run-c2-llama-server.sh` as the intruder; separately a `pkill -f` over ssh killed its own connection. |
| 7 | Canary budget 96 tokens | GIAP strips reasoning into its own frame; raw llama-server puts it in `content`. Every variant scored 3–4/10 *identically*, which was the tell. |
| 8 | G2 reserve double-counted the OS | `MemAvailable` is what remains after the OS, so adding an OS allowance charged it twice and failed the incumbent at 1,308 MB against a bar containing ~1,500 MB of already-spent memory. |

\#4 is the one worth remembering: a correctness gate that failed silently and returned a
clean, plausible, wrong answer — the same shape as the bug it existed to catch.

\#5 and #7 were caught only because C1 ran first and gave a known-good number to be
implausible against. **Run the incumbent first, always.**

## Still open

1. **C3 (vLLM) and C4 (Edge-LLM) not run.** NVIDIA's guidance is "Orin Nano: E2B only" for
   vLLM, and Edge-LLM 0.10.x ships no tool calling, so neither is likely to displace the
   recommendation — but neither has been measured.
2. **No run is a true cold start.** `drop_caches` needs root and the harness has no
   passwordless sudo, so `model_load_ms` and first-turn TTFT are unmeasured. Flagged in
   every envelope rather than quietly claimed.
3. **`n_ctx=32768` in the first two C1 runs is unexplained.** Warm-up-then-restart fixed
   it, but an empty registry derives 2048, not 32768. Recorded as resolved-by-configuration;
   no mechanism claimed.
4. **E2B-qat unmeasured.** Everything here is E4B.
5. **`must_not_call` moved 3/10 → 6/10 between two C1 runs** at nominally the same
   settings. Tool selection is per-conversation and semantic, so some variance is
   expected, but doubling is worth a look before anyone treats that metric as stable.
6. **`lfb` reached 1 MB in every run.** Fragmentation, not a failure here, but a large
   contiguous CUDA allocation late in a long uptime is not guaranteed.

## If the recommendation is taken

Path A, per `docs/goose-patch-management.md`'s breaking-sync procedure:

1. Pin `llama-cpp-2` / `llama-cpp-sys-2` to `=0.1.156` in **both** `goose/Cargo.toml` and
   the parent `Cargo.toml` — the parent wins when goose is built from GIAP.
2. Re-verify the API surface `inference_engine.rs` uses (`with_swa_full`,
   `with_flash_attention_policy`, `with_type_k/v`, `clear_kv_cache_seq`,
   `state_seq_{save,load}_file`, mtmd).
3. Keep `swa_full = true`. Record the iSWA trade-off in the patch table.
4. Wire MTP: the crate must expose a draft path, and `ModelSettings.draft_model` already
   exists but only MLX consumes it — this is new engine work, not a flag.
5. Land as one parent commit (submodule SHA + Cargo files + patch-table row) and
   fast-forward the fork with it.
6. Re-verify PAI-3/4/5 on the device afterwards: decode tok/s feeds the output-reserve
   latency budget, and prefix stability feeds compaction.
