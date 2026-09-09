# What prefill actually costs, measured

2026-09-10, Mac, `gemma-4-E2B-it-qat-UD-Q4_K_XL` on mistral.rs (`--paged-attn on`,
ctx 8192, `--prefix-cache-n 0`). Payloads captured with `GIAP_CAPTURE_PAYLOAD`
from the provider shim — the one place the final `(system, messages, tools)`
exists — then replayed against the engine directly, interleaved with a bare
request as a drift bookend.

## The payload

One fresh turn, `tool_selection_mode = "all"`, 61 tools:

| part | chars | ~tok | share |
|---|---:|---:|---:|
| **tool schemas** | **27,505** | **6,876** | **90.3%** |
| messages (incl. system prompt) | 2,864 | 716 | 9.4% |
| — of which the system prompt | 1,820 | 455 | 6.0% |
| whole body | 30,463 | 7,615 | 100% |

The system prompt is 6% of prefill. Rewriting it changes nothing that matters.

Inside the 27,505 tool chars:

| | chars | share |
|---|---:|---:|
| parameter schemas | 14,795 | 54% |
| descriptions | 6,368 | 23% |
| JSON envelope | 4,534 | 16% (74 chars × 61 tools) |
| tool names | 1,808 | 7% |

By group, the top five are two thirds of it:

| group | tools | chars | ~tok |
|---|---:|---:|---:|
| giap-device-control | 3 | 3,451 | 862 |
| giap-sensors | 6 | 3,447 | 861 |
| giap-schedule | 6 | 2,842 | 710 |
| giap-knowledge | 6 | 2,753 | 688 |
| giap-system | 6 | 1,929 | 482 |

One tool, `giap-device-control__set_device_state`, is 2,484 chars — 9% of every
turn's tool budget — because it carries 17 optional device attributes in one
schema.

## What that costs in time

Engine TTFT, three runs each, interleaved, verbatim captured bodies:

| payload | prompt tok | TTFT | implied prefill |
|---|---:|---:|---:|
| bare `"hi"` | ~10 | **73 ms** | — |
| GIAP system prompt, no tools | ~460 | **384 ms** | 1,480 tok/s |
| GIAP payload, 17 tools (`relevant`) | 2,418 | **1,975 ms** | 1,220 tok/s |
| GIAP payload, 61 tools (`all`) | 7,633 | **8,220 ms** | 930 tok/s |

Drift bookend: bare went 64 → 73 → 94 ms across the run, so the spread is real
and not the server degrading.

**Prefill throughput falls as depth grows**, so TTFT is superlinear in prompt
length: 3.16× the tokens costs 4.16× the time. Cutting tools therefore pays
*more* than proportionally — 61 → 17 tools is −68% tokens and **−76% TTFT**.

The same shape was recorded on the Orin (976 → 820 tok/s from 4096 to 16384),
so this is not a mistral.rs artifact.

## The multiplier nobody sees: `inference_count`

A live turn, `relevant` mode, "What time is it?":

| | |
|---|---:|
| GIAP `ttft_ms` (engine, first inference) | 2,066 ms |
| **client's first visible frame** | **4,873 ms** |
| client total | 15,448 ms |
| inferences | 2 |
| completion tokens | 448 |

`ttft_ms` is honest and matches the 1,975 ms probe. But the user waits 4.9 s,
because a tool round-trip runs the model **twice** and the second inference
re-prefills the whole prompt from scratch — mistral.rs's prefix cache is off,
because with a large tool payload it corrupts (see
[`mistralrs-provider.md`](mistralrs-provider.md)).

So the cost of a turn is `inference_count × prefill`, and `inference_count` is
2–3 for anything that touches a tool. **This is the strongest argument for the
in-process llama.cpp engine**, whose `ReusePrefix` plan makes the second
inference's prefill nearly free (12.4 s → 0.65 s, measured).

## 448 completion tokens for "what time is it?"

The captured body — built by goose's own request serializer, so this is what
goes on the wire — carries **no `chat_template_kwargs`**. Gemma 4's template
therefore falls to its default and thinks, on every turn, whatever GIAP's
`thinking_mode` setting says. The direct mistral.rs backend sends
`{"enable_thinking": …}` explicitly and spends 12–52 tokens on the same
question.

## What is not worth doing

Schema micro-minification. Every safe transform, measured on the real payload:

| transform | saving |
|---|---:|
| drop `"default": null` | 255 chars |
| drop `"default": ""` / `[]` | 130 chars |
| drop `"format": "float"` | 70 chars |
| drop `"additionalProperties"` | 28 chars |
| **all of them together** | **455 chars — 1.7%** |

The shim already strips `$schema`, `title` and integer-width artifacts. There is
nothing left in this direction.

## Ranked levers

| # | lever | saving | status |
|---|---|---|---|
| 1 | `tool_selection_mode = "relevant"` | −68% prompt, −76% TTFT | **exists, not the default** |
| 2 | Send `enable_thinking` on the HTTP path | ~400 completion tokens/turn | not wired |
| 3 | Cut `inference_count`, or make re-prefill free | ×2–3 on the whole turn | engine property |
| 4 | Split `set_device_state` / `create_sensor_rule` | ~1,400 chars (5%) | schema work |
| 5 | Trim tool descriptions (6,368 chars) | up to ~1,500 chars | prose work |
| 6 | Schema minification | 455 chars (1.7%) | **not worth it** |

Levers 1 and 3 are the whole story; 4 and 5 are rounding on top of 1. Core-only
(`giap-draft`, `giap-memory`, `giap-system`, `giap-toolkit` — 15 tools) is the
floor at 5,747 chars, −79%.

## Reproducing this

```bash
GIAP_CAPTURE_PAYLOAD=/tmp/capture MODE=goose scripts/try-mistralrs.sh
```

Writes `payload-<seq>-<n>t-<hash>.json` per inference. The hash is over the
body, so two turns with a byte-identical prompt land on the same filename —
which is how the capture proves prefix stability rather than assuming it.
