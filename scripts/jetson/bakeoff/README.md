# Inference-engine bake-off

Measures candidate LLM engines on the Orin Nano 8 GB against **the prompt GIAP
actually sends**, and ranks them on TTFT, decode, memory headroom and tool-call
reliability — with reliability and memory as hard gates rather than columns.

Everything runs **on the device**. `bash scripts/jetson.sh bakeoff <sub>`.

## The two rules the harness enforces for you

1. **A number without its state is not a result.** Every run writes an envelope
   carrying the power mode, governor, clocks, thermal peak, over-current rate,
   swap delta and the model's sha256. `report.py` lists a row missing any of
   that as UNUSABLE rather than quoting it.
2. **A memory saving must be quality-backed.** Any variant claiming a smaller
   footprint (iSWA, KV f16, a drafter, a different quant) has to pass the
   reliability and canary workloads *in that same configuration*, or the report
   prints "unvalidated" instead of a saving.

## Order

```bash
bash scripts/jetson.sh bakeoff prep                    # root-only: swap, headless, apt holds, baseline
bash scripts/jetson.sh bakeoff capture --model gemma-4-E4B-it-qat --out ~/bakeoff-payloads
bash scripts/jetson.sh bakeoff c1 --model gemma-4-E4B-it-qat --workloads all -r 5
bash scripts/jetson.sh bakeoff c2 --model gemma-4-E4B-it-qat --payloads ~/bakeoff-payloads --variant baseline
bash scripts/jetson.sh bakeoff c2 --model gemma-4-E4B-it-qat --payloads ~/bakeoff-payloads --variant iswa
bash scripts/jetson.sh bakeoff c3 --payloads ~/bakeoff-payloads          # vLLM, E2B only per NVIDIA
bash scripts/jetson.sh bakeoff c4 --payloads ~/bakeoff-payloads          # Edge-LLM, expected to block
bash scripts/jetson.sh bakeoff report --in ~/bakeoff-results/$(date +%F) \
     --out docs/developer/jetson-engine-bakeoff.md --voice-residency-mb <measured>
```

Run **C1 first and again last** each day. A >10 % drift between the two means
the board's state moved and that day's cross-candidate comparisons are void.

## Candidates

| id | Engine | Isolates |
|---|---|---|
| C1 | `llama-cpp-2 0.1.146` in-process through GIAP | the baseline |
| C2 | current upstream `llama-server` (or NVIDIA's container) | how stale the vendored engine is; iSWA; speculative decoding |
| C3 | NVIDIA vLLM `gemma4-jetson-orin` | whether it fits at all, and its `gemma4` tool-call parser under the real payload |
| C4 | TensorRT Edge-LLM | *where* it blocks — that is what the JetPack 7.2 question turns on |

C2's `baseline` variant mirrors `apply_jetson_settings` flag-for-flag
(`-ngl 99 -fa on -b 512 -ub 128 -t 4 -ctk/-ctv q8_0 --swa-full`). That mirroring
is load-bearing: it reduces the C1↔C2 gap to one question instead of a mixture
of engine vintage and a dozen unmatched knobs. Flags are probed against
`--help` first, and a build lacking one is recorded — an `iswa` run that
silently kept `swa_full` would look like a free memory saving.

## Why payloads are captured, not reconstructed

Goose's `sessions.db` holds the raw messages but **not** the shim-enforced
system prompt, the vetoed and minified tool array, or the core-first ordering
that decides how much KV prefix survives a changed tool selection. Replaying
from it measures a prompt nobody sends. `GIAP_CAPTURE_PAYLOAD`
(`crates/pond-adapters-goose/src/provider_shim.rs`) writes the real one through
goose's own OpenAI serializer, so a replay differs from a live call only by
transport.

One turn leaves several captures — the chat inference plus the goal check and
the memory-extraction side call. The chat inference is the one carrying the tool
array, so `capture-payload.sh` takes the *widest new* file after each turn.

## Metric definitions (identical across transports)

| Metric | Definition |
|---|---|
| TTFT | request send → first chunk carrying content. A role-only opening chunk is **not** content. |
| TTFS | → first sentence-ending punctuation. The voice-relevant one. |
| decode tok/s | `completion_tokens ÷ (last content − first content)`, so prefill is excluded. `usage` when the server sends it, chunk count otherwise and labelled as such. |
| memory | `MemAvailable` minimum over the run, per-PID nvmap, `lfb` minimum, and **swap delta, which must be 0** — a swap-served KV cache reads as a slow model, never as an error. |

## Gates

- **G0** streams at least one turn with content.
- **G1** ≥90 % must-call on `relevant`, ≥83 % on `all`, zero stream errors, zero
  multi-turn breaks. A floor for elimination, not a ranking signal — 12 turns
  cannot separate 11/12 from 12/12.
- **G2** swap delta 0, and `MemAvailable` never below `1500 (OS) + voice
  residency (measured; 300 fallback) + 512 (NvMap contiguous slack)`.
- **G3** any claimed saving carries its own G1 + canary row.

Ranking is lexicographic with a 10 % significance band: voice TTFT →
follow-up TTFT → decode → fresh TTFT → memory headroom. An HTTP sidecar must
additionally beat C1 by ≥10 % on TTFT or decode with no rung worse by >5 %,
because it carries a standing cost the in-process engine does not.

## Gotchas

- `drop_caches` evicts the mmap'd GGUF, so it is the honest cold-start
  primitive — and must **never** run between warm repeats.
- nvmap is the authoritative GPU-memory source here; `nvidia-smi` is a stub on
  this stack and answers `[N/A]`. Needs root: without it the numbers are
  **absent**, which must not be read as zero.
- `nvpmodel -m 0` is 15 W on this SKU. MAXN_SUPER is id 2.
- Scratch ponds hard-link the GGUF. Never symlink `models/` — the startup
  migration MOVES real files out of `models/gguf`, and a symlinked directory
  *is* the real one. That destroyed this device's chat model once.
- `giap::trace` is INFO-rooted and goose carves `llama-cpp-2` to ERROR, so both
  need raising explicitly or the prefill plans and KV-cache sizes never appear.
