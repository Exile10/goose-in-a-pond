# Jetson inference-engine bake-off

Envelopes: `/tmp/bakeoff-all`  ·  candidates: 4

Memory reserve for G2: **812 MB** (300 voice (fallback, not measured) + 512 NvMap contiguous slack)


## Gates

| candidate | variant | model | G0 | G1 reliability | G2 memory | G3 quality-backed |
|---|---|---|---|---|---|---|
| c1-giap | baseline | gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf | PASS <br><sub>streamed at least one turn with content</sub> | PASS <br><sub>within floors</sub> | PASS <br><sub>16 MB swap drift (under the 128 MB noise floor); largest free block fell to 1 MB (fragmentation warning, not a failure)</sub> | PASS <br><sub>baseline: nothing claimed</sub> |
| c2-llama-server | baseline | gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf | PASS <br><sub>streamed at least one turn with content</sub> | PASS <br><sub>relevant 3/10 NOT GATED (frozen payload: its tools were selected for a different ask)</sub> | PASS <br><sub>5 MB swap drift (under the 128 MB noise floor); largest free block fell to 1 MB (fragmentation warning, not a failure)</sub> | PASS <br><sub>baseline: nothing claimed</sub> |
| c2-llama-server | iswa | gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf | PASS <br><sub>streamed at least one turn with content</sub> | PASS <br><sub>relevant 3/10 NOT GATED (frozen payload: its tools were selected for a different ask)</sub> | PASS <br><sub>largest free block fell to 1 MB (fragmentation warning, not a failure)</sub> | PASS <br><sub>canary 10/10</sub> |
| c2-llama-server | mtp | gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf | PASS <br><sub>streamed at least one turn with content</sub> | PASS <br><sub>relevant 3/10 NOT GATED (frozen payload: its tools were selected for a different ask)</sub> | PASS <br><sub>largest free block fell to 1 MB (fragmentation warning, not a failure)</sub> | PASS <br><sub>canary 10/10</sub> |

## Measurements

Client-side timings throughout: from just before the request to the first chunk carrying content. Engine-reported numbers, where an engine reports any, are in the envelopes.


| candidate | variant | engine TTFT | turn latency | follow-up | decode | avail min | swap Δ | power |
|---|---|---:|---:|---:|---:|---:|---:|---|
| c1-giap | baseline | 1019 ms | 19058 ms | 27158 ms | 15.8 tok/s | 1308 MB | 16 MB | MAXN_SUPER/dynamic(gov=schedutil,gpu=306/1020MHz) |
| c2-llama-server | baseline | 345 ms | 345 ms | 766 ms | 17.2 tok/s | 1265 MB | 5 MB | MAXN_SUPER/dynamic(gov=schedutil,gpu=306/1020MHz) |
| c2-llama-server | iswa | 333 ms | 333 ms | 898 ms | 18.4 tok/s | 1566 MB | 0 MB | MAXN_SUPER/dynamic(gov=schedutil,gpu=306/1020MHz) |
| c2-llama-server | mtp | 282 ms | 282 ms | 833 ms | 47.7 tok/s | 1216 MB | 0 MB | MAXN_SUPER/dynamic(gov=schedutil,gpu=306/1020MHz) |

## C1 vs C2-baseline parity

Same model, same quant, mirrored flags. A gap here is engine vintage plus GIAP's own per-token overhead; anything larger than the tolerance means the two are not running the same configuration and no other comparison in this report is safe.


| metric | C1 | C2-baseline | delta | tolerance | verdict |
|---|---:|---:|---:|---|---|
| decode tok/s | 15.8 | 17.2 | +8.6% | ±10% | ok |
| engine TTFT | 1019.0 | 344.8 | -66.2% | ±15% | **OUT OF TOLERANCE** |
| prompt tokens | 3621.0 | 2848.0 | -21.3% | ±3% | **OUT OF TOLERANCE** |

## Ranking

Lexicographic, 10% significance band: voice TTFT → follow-up TTFT → decode → fresh TTFT → memory headroom. A rung only decides when the gap exceeds the band.

1. **c2-llama-server** (mtp) — gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf
2. **c2-llama-server** (baseline) — gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf
3. **c2-llama-server** (iswa) — gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf
4. **c1-giap** (baseline) — gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf

**Challenger check.** c2-llama-server leads, but a sidecar has a standing cost (a second supervised process, wall-clock telemetry, re-solving the sacrificial-context problem server-side). It must beat C1 by ≥10% on TTFT or decode with no rung worse by >5%. Best gain measured: **+201.1%** — clears the bar.


## Sanity checks and warnings


**c1-giap / baseline**

- over-current: 1 oc3 events/min at MAXN_SUPER

**c2-llama-server / mtp**

- decode 47.7 tok/s is above the 24.2 tok/s single-stream ceiling, explained by speculative decoding: 198/227 drafted tokens accepted (87%); the server's own timings report 46.5 tok/s independently
- over-current: 0 oc3 events/min at MAXN_SUPER
- _c1-giap_: drop_caches needs root; this run is NOT a cold start
- _c1-giap_: quality arms re-measured in a second pass after two context-answerable questions moved out of the gated set; performance figures are from the first pass, same configuration
- _c2-llama-server_: drop_caches needs root; this run is NOT a cold start
- _c2-llama-server_: canary and reliability re-measured in a second pass with max_tokens=512; performance figures are from the first pass, same flags and same payloads
- _c2-llama-server_: drop_caches needs root; this run is NOT a cold start
- _c2-llama-server_: canary and reliability re-measured in a second pass with max_tokens=512; performance figures are from the first pass, same flags and same payloads
- _c2-llama-server_: drafter arch is 'gemma4-assistant
- _c2-llama-server_: unknown'; upstream llama.cpp registers 'gemma4-assistant' and will refuse this file
- _c2-llama-server_: drop_caches needs root; this run is NOT a cold start
- _c2-llama-server_: canary and reliability re-measured in a second pass with max_tokens=512; performance figures are from the first pass, same flags and same payloads
