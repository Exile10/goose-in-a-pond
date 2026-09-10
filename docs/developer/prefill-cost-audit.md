# What prefill actually costs, measured

2026-09-10, Mac, `gemma-4-E2B-it-qat-UD-Q4_K_XL` on mistral.rs (`--paged-attn on`,
ctx 8192, `--prefix-cache-n 0`). Payloads captured with `GIAP_CAPTURE_PAYLOAD`
from the provider shim — the one place the final `(system, messages, tools)`
exists — then replayed against the engine directly, interleaved with a bare
request as a drift bookend.

## The payload

> **Superseded 2026-09-10.** Twenty of these tools were removed after this audit
> — the inventory is now 46, and a fresh `"all"` turn is ~19,000 chars rather
> than 27,443. Every number below is the measurement that motivated the cut, kept
> as it was taken. See **The cut** at the end.

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

## Below GIAP: what goose and llama.cpp already offer

Everything above treats the payload as fixed and asks how to shrink it. The
engine layer asks a different question — how often you pay for it at all — and
answers it better.

### llama.cpp's prompt cache erases the tool block

`--cache-prompt` is **on by default**. Measured against `llama-server`
(`--cache-reuse 256 --cache-ram -1`) with the captured 61-tool payload:

| request | TTFT |
|---|---:|
| 61-tool payload, cold | 12,377 ms |
| exact repeat | **117 ms** |
| same system + tools, different user message | **206 ms** |
| tool round-trip: prefix + `tool_call` + result | **721 ms** |
| repeat of the round-trip | 67 ms |
| back to the first payload | 117 ms |

Two things follow. The `inference_count × prefill` multiplier **disappears** —
the second inference of a tool round-trip costs 721 ms instead of ~12,000. And
the cache holds several branches at once (`--cache-ram -1`), so alternating
between two suffixes stays warm.

End to end through goose, same prompt, same model, same 61 tools:

| | mistral.rs (cache off) | llama.cpp (cache on) |
|---|---:|---:|
| GIAP turn TTFT, ~7,300 tok | 8,600–8,900 ms | **587–847 ms** |
| first turn on a cold server | — | 12,332 ms |

**~11× on the identical payload, from engine configuration alone.** The cache is
server-wide, not per session: a turn in a brand-new GIAP session is warm if any
earlier turn warmed the same prefix, which is what makes GIAP's boot-time prefix
prewarm pay for every session rather than only the first.

mistral.rs cannot do this today — its prefix cache corrupts on the second
request carrying a large tool payload, so it runs with `--prefix-cache-n 0`.
That single difference is most of what "goose adds a lot of overhead" was
measuring.

### `--chat-template-kwargs` fixes the thinking gap without touching goose

The body goose serializes carries no `chat_template_kwargs`, so Gemma 4 thinks
on every turn. `llama-server --chat-template-kwargs '{"enable_thinking":false}'`
sets it at the engine. Same question, same server otherwise:

| | completion tokens |
|---|---:|
| thinking on (goose's default behaviour) | 231 |
| `--chat-template-kwargs` | **54** |

No GIAP change, no fork patch.

### The other flags, and what they are for

| flag | what it buys |
|---|---|
| `--cache-reuse N` | reuse past a divergence via KV shifting, not just an exact prefix |
| `--cache-ram N` (default 8192 MiB, `-1` unlimited) | how many branches stay warm |
| `--cache-idle-slots` (default on) | idle slots are folded into the prompt cache |
| `--slot-save-path` + `/slots/{id}?action=save` | persist KV to disk — would make the 12.3 s cold cost payable once ever, not once per boot. **Untested here.** |
| `--chat-template-file` | the lean-template hook |
| `--json-schema` / `--grammar` | constrain output shape |

### goose's own facilities

| facility | verdict |
|---|---|
| `code-mode` tool disclosure (`ToolDisclosure::Catalog`) | replaces all 61 tools with three — `list_functions`, `get_function_details`, `execute_typescript` — and `Sidecar` with one. The biggest structural saving available, and almost certainly wrong here: it needs the `code-mode` feature GIAP disables for the rmcp conflict, a TypeScript runtime on the device, and it asks a 2B model to write TypeScript instead of emitting a tool call. |
| `toolshim` | the opposite of a saving — routes output through a second interpreter model |

### What this settles about the lean template

With prefill cached, a 17% smaller tool block saves ~100 ms once, on the cold
start, and nothing on any warm turn. Against a real risk to tool-call
reliability on markup the model was trained on, that is not a trade worth
making. **The lean template is dropped rather than A/B'd.**

## Ranked levers

| # | lever | saving | status |
|---|---|---|---|
| 1 | **Prompt caching on the engine** | ~11× TTFT (8,900 → 800 ms) | **on by default in llama.cpp; impossible in mistral.rs today** |
| 2 | `--chat-template-kwargs '{"enable_thinking":false}'` | 231 → 54 completion tok | engine flag, no code |
| 3 | `tool_selection_mode = "relevant"` | −68% prompt, −76% **cold** TTFT | exists, not the default; matters for the cold turn and for context headroom |
| 4 | Split `set_device_state` / `create_sensor_rule` | ~1,400 chars (5%) | schema work |
| 5 | Trim tool descriptions (6,368 chars) | up to ~1,500 chars | prose work |
| 6 | Schema minification | 455 chars (1.7%) | **not worth it** |
| 7 | Lean chat template | −14% prompt | **dropped** — see above |

**Lever 1 is the whole story, and it is a choice of engine, not of prompt.**
Everything that shrinks the payload only changes what the cold start costs;
once the prefix is warm the payload is free. That reorders the rest: `relevant`
mode is worth having for the cold turn and for context headroom, not for
steady-state latency. Core-only (`giap-draft`, `giap-memory`, `giap-system`,
`giap-toolkit` — 15 tools) is the payload floor at 5,747 chars, −79%.

## Reproducing this

```bash
GIAP_CAPTURE_PAYLOAD=/tmp/capture MODE=goose scripts/try-mistralrs.sh
```

Writes `payload-<seq>-<n>t-<hash>.json` per inference. The hash is over the
body, so two turns with a byte-identical prompt land on the same filename —
which is how the capture proves prefix stability rather than assuming it.

## The cut, 2026-09-10

Twenty tools removed, 61 → 41 offered (66 → 46 registered), −8,424 chars of tool
schema. Redundant first, then narrow-value, then two capability calls:

| group | removed |
|---|---|
| giap-knowledge | `search_wikipedia` (get_wikipedia_article auto-searches), `explore_computation`, `define_word`, `search_books` |
| giap-system | `run_shell_command`, `read_file`, `write_file` |
| giap-sensors | `create_sensor_rule`, `list_sensor_rules`, `delete_sensor_rule` |
| giap-discovery | `get_country_info`, `get_product_price` |
| giap-finance | `get_exchange_rate` (convert_currency covers it), `get_stock_quote` |
| giap-device | `get_model_assignments`, `get_recipe` |
| giap-news | `get_top_stories` |
| giap-audit | `summarize_activity` |
| giap-vision | `look_at_camera_window` |
| giap-schedule | `get_schedule_runs` |

Four things the removal changed beyond the schemas:

- **`giap-system` is no longer a write-or-execute surface.** The rationale for
  denying it to subagents and proactive proposers is now `send_notification`
  alone, and all three copies of that rationale say so.
- **`compute_answer`'s "did you mean" no longer names a tool.** It suggested
  `explore_computation` by id; it now asks the model to re-call `compute_answer`
  with the suggestion's own wording, and the Wolfram UI chip sends that wording
  instead of an id.
- **The sensor-rule dependency path is gone** — `init_sensor_rule_deps`, the
  scheduler handle on `SensorsMcpServer`, and its install in `main.rs`.
- **`recipe_repo` stopped being a tool dependency**, so it was dropped from
  `DeviceMcpServer`, `McpToolDispatcher::new` and `register_giap_extensions`.
  It still reaches the orchestrator, which is its real consumer.

`format.rs :: the_tool_inventory_parser_sees_the_tools_that_are_there` pins the
count at 46 and is the guard that will catch the next drift.
