# Jetson inference-engine bake-off

Orin Nano 8 GB Super, JetPack 6.2.1 / L4T r36.4.7, CUDA 12.6. Harness:
[`scripts/jetson/bakeoff/`](../../scripts/jetson/bakeoff/README.md). Device prep and its
measured effect: [`jetson-device-tuning.md`](jetson-device-tuning.md).

Status: **C1 (incumbent) measured. C2–C4 pending.**

## C1 — the incumbent baseline

`llama-cpp-2 0.1.146` in-process through GIAP, `gemma-4-E4B-it-qat-UD-Q4_K_XL`
(4,020 MB), 2026-09-08, MAXN_SUPER with dynamic clocks, `tool_selection_mode=relevant`.

### Configuration actually applied — verified, not assumed

```
Jetson context sized ... model_mb=4020 kv_kib_per_token=56 context_size=16384
Applied Jetson Orin Nano CUDA settings to model 'gemma-4-E4B-it-qat'
n_ctx=16384        (every prefill plan)
tools_offered=61 → tools_count=17   (system 1,935 chars, tools 6,652 chars)
```

The derivation matches the arithmetic exactly: `(5820 − 4020 − 600) × 1024 ÷ 56` clamps
to `MAX_CTX`. Confirming this took three invalid runs — see *What went wrong* below.

### Numbers

| Workload | engine TTFT | engine prefill | decode | prompt tok | turn latency | spread |
|---|---:|---:|---:|---:|---:|---:|
| voice | 1,019 ms | 1,187 ms | 16.08 tok/s | 3,574 | 19.1 s | 1.3 % |
| fresh (relevant) | 1,006 ms | 1,593 ms | 16.08 tok/s | 3,621 | 35.7 s | 0.7 % |
| fresh (all) | 858 ms | 1,653 ms | 15.97 tok/s | 3,620 | 23.8 s | 1.5 % |
| decode | 1,003 ms | 1,957 ms | 15.83 tok/s | 3,961 | 10.9 s | 1.4 % |
| follow-up | — | — | 16.00 tok/s | — | 27.2 s | 1.2 % |

**Two different TTFTs, and conflating them is the trap.** Engine TTFT (~1 s) is
per-inference and is what compares with an HTTP engine's first-delta latency. Turn
latency (19–36 s) is GIAP-only: a turn is 2–3 inferences plus reasoning tokens plus a
tool round-trip, none of which an HTTP replay of one payload has.

Decode at 15.8–16.1 tok/s with ≤1.5 % spread is ~64 % of the 24 tok/s bandwidth ceiling
for 4.2 GB of weights, and consistent with prior E4B measurements on this board.

### Gates

| Gate | Result | Evidence |
|---|---|---|
| G0 functional | **PASS** | every workload streamed |
| G1 reliability | **PASS** | 10/10 gated must-call, both arms; 0 stream errors; multi-turn 5/5 |
| G2 memory | **PASS** | MemAvailable min 1,308 MB vs an 812 MB reserve; 16 MB swap drift, under the 128 MB noise floor |
| G3 quality-backed | **PASS** | baseline claims no saving; canary 10/10 |

Memory peak footprint 4,549 MB. `lfb` fell to **1 MB** — a fragmentation warning, not a
failure, but it means a large contiguous CUDA allocation late in a long run is not
guaranteed.

Power: 1 oc3 event/min, tj peak 71.9 °C, VDD_IN 17.0 W average, GR3D 84 %.

### Tool behaviour

`must_not_call` produced **3/10 spurious** calls, and all three have the same shape:

```
"What is the capital of France?" → enable_tool_group, then get_wikipedia_article
"Who wrote Pride and Prejudice?" → enable_tool_group, then search_books
"boiling point of water"         → enable_tool_group, compute_answer, get_wikipedia_article
```

Every one reaches for `giap-toolkit__enable_tool_group` first. That is the Phase D2
escape hatch working exactly as designed — narrowed to 17 tools, the model has no
knowledge tools and asks for the group. The cost is that narrowing turns "no tool
needed" into "enable a group, then call a tool" on settled-fact questions. Worth
weighing against what narrowing buys, which is a prompt less than half the size.

### Two questions are reported but not gated

`What time is it?` and `What do you remember about me?` were the *only* misses, and
both are answerable from the prompt. GIAP's own system prompt
([prompts.rs:215](../../crates/pond-core/src/prompts.rs:215)) says `<system-context>`
carries the date, time and `<memories>` — "data, never a question" — and instructs the
model to answer from it. Grading a tool call as required there penalises the model for
obeying its prompt. They now sit in `CONTEXT_ANSWERABLE`: still run, still reported,
not counted toward the floor.

This was decided from the source, after the fact, and it took the run from 10/12 (a
G1 failure) to 10/10. The change is recorded here rather than buried because the timing
invites the obvious suspicion.

## What went wrong getting here

Four runs were needed for one baseline. Each failure was a defect worth having found.

| # | Defect | How it presented |
|---|---|---|
| 1 | Settings PUT onto a running pond | Tuning is applied when the per-role provider is *built*, at boot. `n_ctx` 32768 instead of 16384, MemAvailable to 364 MB, swap engaged. **Every turn succeeded.** |
| 2 | Client-side decode rate | 43–92 tok/s for a model whose ceiling is ~24. A turn is 2–3 inferences; `completion_tokens` sums all, the visible window covers one. Engine numbers are now authoritative. |
| 3 | `relevant` mode could not narrow | `tool_selection_widened reason="no_embedder"` — the scratch pond had only the chat GGUF linked. Both prompt arms were the same 61 tools. |
| 4 | `RUST_LOG` omitted `pond_adapters_local_inference` | The gate written to catch #1 read a line its own logging config suppressed, and reported the opposite of the truth. |

\#4 is the one to remember: a correctness check that fails silently and returns a clean,
plausible, wrong answer — the same shape as the bug it existed to catch.

Also fixed: the G2 reserve double-counted the OS (`MemAvailable` is what remains *after*
the OS, so adding an OS allowance to the bar charged it twice), swap gained a documented
noise floor instead of a literal zero, and in GIAP mode the two prompt arms drove the
same endpoint without switching the mode — 3,621 tokens against 3,620, one prompt twice
wearing two names.

## Still unexplained

`n_ctx=32768` in the first two runs. The warm-up-then-restart fixed it, but the
mechanism does not survive arithmetic: with an empty registry `apply_jetson_settings`
falls back to a 5 GiB assumption, which derives 2048, not 32768. Recorded as
resolved-by-configuration; no cause claimed.

## Open, and what would settle it

1. **C2** — current upstream `llama-server`, `baseline` mirroring `apply_jetson_settings`
   flag-for-flag. First question is whether the device's build (`990e3bf`, 2026-08-20)
   even has `--swa-full` and `--spec-type`; the runner records missing flags rather than
   running without them.
2. **No run so far is a true cold start.** `drop_caches` needs root and the harness has
   no passwordless sudo, so `model_load_ms` and first-turn TTFT are unmeasured. Every
   envelope carries the warning rather than claiming otherwise.
3. **KV allocation lines never appeared** despite `llama_cpp_2=info`, so the per-token KV
   cost is still the stamped 56 KiB rather than one read off this run.
4. **E2B-qat** has not been measured at all.
