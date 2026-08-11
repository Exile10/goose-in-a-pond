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
