# Dual-wielding Gemma

A standalone experiment: **Gemma 4 E2B reasons, FunctionGemma 270M calls the tools.**
No Goose, no `pond-server`, no crates from this repo. Two `llama-server`
processes and about 900 lines of dependency-free Python.

```
user -> [ E2B: plan ] -> [ FunctionGemma: emit call ] -> [ tool ] -> [ E2B: read result ] -> user
              ^                                                            |
              +------------------------------------------------------------+
```

## The question

Does splitting "decide what to do" from "write the call" produce better tool
use than one model doing both? The experiment runs three arms over the same 22
cases, the same 8 tools, the same loop and the same step budget. Only the
author of the tool call changes.

| arm | who plans | interface | who writes the call(s) |
| --- | --- | --- | --- |
| `dual` | Gemma 4 E2B | one strict imperative, one tool per step | FunctionGemma 270M |
| `solo` | Gemma 4 E2B | JSON, one call per step | Gemma 4 E2B |
| `natural` | Gemma 4 E2B (thinking on) | free-form request, several actions at once | FunctionGemma 270M, fanned out |

## Why the split might help

FunctionGemma 270M was probed directly before any of this was built, and it
behaves like a **transducer, not a reasoner**. Given the same three tools:

| input | output |
| --- | --- |
| `What's the weather like in Nairobi?` | `call:get_weather{city:Nairobi}` correct |
| `How much is a fifth of eighteen thousand?` | `call:get_weather{city:New York}` wrong tool, invented argument |
| `Wake me in 10 minutes for the standup` | *"I cannot assist with scheduling wake-ups or setting timers."* refusal |
| `Call set_timer with seconds 600 and label standup` | `call:set_timer{seconds:600,label:standup}` correct |
| `Call calculate with the expression 18000 / 5` | `call:calculate{expression:18000 / 5}` correct |

Two out of four raw utterances, four out of four once the intent was made
literal. It cannot convert ten minutes into six hundred seconds and it will
not admit that it cannot; it refuses or hallucinates instead. Everything it
needs must already be on the page.

That is the bet: E2B is a competent reasoner that is mediocre at emitting
rigid syntax, and FunctionGemma is the reverse. The planner's whole job is to
turn a human request into a sentence FunctionGemma can transduce without
thinking.

It also has one mechanical quirk worth knowing: **it never stops.** After the
first well-formed call it keeps emitting plausible garbage until it hits the
token limit. `<end_function_call>` is a hard stop sequence and only the first
call is ever read.

## Running it

```bash
./serve.sh start          # two llama-servers: planner :8091, picker :8092
python3 web.py            # console on http://localhost:8093
python3 chat.py           # same thing in the terminal
python3 eval.py           # score both arms over cases.json
./serve.sh stop
```

Requires `llama-server` on PATH (`brew install llama.cpp`) and the two GGUFs
under `~/Library/Application Support/goose-in-a-pond/models/gguf/`. Override
with `PLANNER_GGUF` / `PICKER_GGUF`. Python 3.9, stdlib only.

```bash
python3 chat.py --both "How far is Kisumu in miles?"   # one question, both arms
python3 chat.py --solo                                  # control arm only
python3 eval.py --case timer_minutes --arm dual         # one case
python3 eval.py --repeat 3 --out results/r3.json        # average over repeats
python3 web.py --no-think                               # 5x faster turns; see below
```

`--no-think` (on every driver) disables the planner's thinking channel for
the session -- see *Latency, TTFT and memory* for what that trades. The
natural arm ignores it and always thinks: its design is deliberation plus
a forgiving interface.

## Latency, TTFT and memory

Where the time goes, measured across the full suite: **FunctionGemma is ~2%
of wall clock** (4.2s of 217s; 0.25s per call, since its ~600-token
declaration block stays KV-cached and each call prefills only the
instruction line). E2B is the other 98%, and most of what it generates is
not output: 4,047 tokens generated for roughly 700 tokens of actual
one-line replies. The latency problem is the thinking channel, nothing else.

So the lever is thinking, and it was measured at three settings:

| configuration | dual tool/args | solo tool/args | s/case |
| --- | --- | --- | --- |
| thinking unbounded | 94 / 94 | 94 / 94 | 12.1 / 11.1 |
| `--reasoning-budget 96` | 83 / 78 | 83 / 83 | 10.3 / 11.6 |
| thinking off | 89 / 83 | **100 / 94** | **2.5 / 2.4** |

Three conclusions worth the table:

- **Thinking off is 5x faster and, for solo, better.** Unable to reason
  internally, solo stops doing arithmetic in its head and delegates --
  the `1234 * 17` case that it hallucinated with thinking on becomes a
  clean `calculate` call. Dual pays a small planning-precision cost
  (its instruction quality drops when it cannot deliberate).
- **A bounded budget is the worst of both.** Truncated mid-thought, the
  model loses output quality *and* still pays ~5s of reasoning decode.
- The residual no-think failure class is implicit arithmetic ("an hour
  and a half" became `seconds: 90`).

Run any driver with `--no-think` to use it; the console header shows which
mode the session is in.

### The KV rule: decide thinking once per session

The chat template injects the think token at the very top of the first
system turn. Toggling thinking therefore moves the **entire token prefix**
and invalidates the whole KV cache -- measured directly: 2,967ms full
re-prefill on the flip, ~250ms warm otherwise. This is the same landmine as
the `thinking_mode` KV fix in the main repo. `--no-think` is a session
flag, not a per-request one, for exactly this reason.

Everything else about the loop is prefix-friendly by construction:

- The system prompt (tool catalogue included) is byte-stable, and history
  is append-only, so every turn reuses ~96% of its prompt from cache
  (`589/619 cached`, ~300ms prefill on a warm turn -- the console shows
  this per row).
- The picker is **stateless**: a fixed ~600-token declaration prefix plus
  one instruction line, forever. Its context never grows, so it has no
  compaction problem and no KV growth. Across 17 eval calls it prefilled
  281 tokens total.
- Anything that rewrites history (summarisation, compaction) moves the
  prefix and buys a full re-prefill. If compaction ever becomes necessary
  here, it should be rare and batched, not incremental.

### TTFT to the user

`web.py` streams the planner over SSE (`chat_stream` in
`dualgemma/llama.py`): reasoning deltas render dim and live, content
deltas render as they decode, so perceived TTFT is the first token, not
the finished turn. With thinking off, a single-tool exchange completes in
~4s end to end and a two-hop chain in ~6s on the M-series MacBook.

### Keeping both resident

Measured co-residency on the Mac: **3.5GB total** -- the E2B server at
3.26GB (2.9GB weights + KV at ctx 8192 + ~550MB compute buffers) and
FunctionGemma at 310MB (241MB weights, ctx 2048). Both servers stay up
across sessions; `serve.sh start` is idempotent and reuses running
instances, so model load cost is paid once, not per conversation.

On the Jetson (7,620MB real): E2B at the registry's ctx 16384 costs
288MB of KV (18KiB/token, measured on device previously), putting the
pair at roughly 3.8GB + 0.3GB. That fits, but two things must be
confirmed on the hardware per the Mac-first rule: whether the picker
belongs on GPU or CPU (`-ngl` placement interacts with the NvMap
586MiB wall and drop_caches behaviour), and real prefill/decode rates.
Nothing in this section's Jetson numbers has been re-measured for this
experiment.

## The natural arm: FunctionGemma as a tool

The third arm inverts the framing: E2B is not given eight tools, it is
given ONE -- a small helper it addresses in plain language, which turns
the request into however many calls it implies.

```
ASK: Get the weather for Nairobi and set a timer for 300 seconds labelled tea
FGEMMA   get_weather({"city": "Nairobi"})
FGEMMA   set_timer({"seconds": 300, "label": "tea"})
```

Thinking stays ON for the planner in this arm by design: the bet is that
deliberation plus a forgiving interface beats deliberation plus a rigid
one, and that fanning several calls out of one request claws back the
round-trips that make thinking expensive.

Multi-call parsing rests on a measured property of FunctionGemma's
runaway tail: **genuine calls each open with `<start_function_call>`,
babble does not.** Probed directly with compound instructions:

- "weather for Nairobi, Kisumu and Mombasa" -> three tagged calls, all
  three cities correct, then an untagged repeat
- "weather for Nairobi + timer 300 tea" -> two tagged calls, then
  untagged `call:list_timers` garbage
- a single-intent instruction -> one tagged call, then untagged garbage

`parse_calls` therefore accepts the contiguous head-run of tagged,
back-to-back calls and stops at the first untagged `call:` (with a
dedupe and a cap of five as belts). The one genuine failure the probe
found: "convert 340 km to mi and calculate 18000 / 5" produced a second
call of `calculate{340 / 5}` -- the model blended the two intents'
numbers. Cross-intent argument bleed is the natural arm's
characteristic risk, and the eval's compound cases exist to price it.

The planner, told to speak "plain natural language", promptly wrote
pseudo-code anyway (`get_weather(city="Nairobi") get_weather(city="Mombasa")`)
-- and the picker transduced it regardless. The forgiving interface
absorbs both registers.

### Natural-arm results

Thinking ON everywhere, 22 cases (18 single-intent + 4 compound):

|  | tool | args | answer | s/case | compound tools | compound args |
| --- | --- | --- | --- | --- | --- | --- |
| **dual** (strict line) | 20/22 (91%) | 19/22 (86%) | 11/12 (92%) | 14.0 | 3/4 | 2/4 |
| **solo** (JSON) | 20/22 (91%) | 19/22 (86%) | 10/12 (83%) | 14.3 | 3/4 | 2/4 |
| **natural** (prose, fan-out) | 21/22 (95%) | 18/22 (82%) | 10/12 (83%) | 17.6 | **4/4** | **3/4** |

**Multi-tool calls work, and the natural arm is the best at them.** It
satisfied every compound case's tool set in a single fan-out, where the
sequential arms managed 3 of 4 across multiple loop steps -- both dropped
the second call of "convert 340 km AND calculate 18000/5". One ASK line
became `get_weather + set_timer`, or three weather calls for three
cities, with one planner round-trip instead of two or three.

The price is argument fidelity on single-intent cases (82% vs 86%): even
in prose, the picker occasionally corrupts a number it was handed
literally (a "600 seconds" ask transduced as `seconds: 100` in one run,
`seconds: 0` in another -- flaky, not systematic), and it still cannot
tell `get_time` from `read_note`. The strict one-line protocol remains
the safest way to move an argument through FunctionGemma; the natural
interface is the strongest way to move an *intent list* through it.

Getting here took two measured prompt corrections, both worth recording:

1. **E2B drifts into pseudo-code, and code corrupts the picker.** Told to
   speak plain language, the planner wrote `set_timer(600, standup)` --
   with every number correctly resolved -- and FunctionGemma turned it
   into `seconds: 16400`. Keyword form fared no better (`seconds=600` ->
   `100`). The identical intent as an English sentence transduces
   perfectly. FunctionGemma is a prose transducer: any function-syntax
   register in its input corrupts numeric arguments.
2. **"Plain words" was the wrong instruction.** The first fix told the
   planner to state amounts "in plain words", and it obeyed too well:
   `kilometers` for a tool that accepts `km`, answer numbers spelled out
   as words. The working phrasing: an English sentence, numbers as
   digits, unit codes and names exactly as the gadget list shows them.

## Driving the Jetson from the Mac

```bash
./nano-bench.sh start     # nano llama-servers + ssh tunnel + console
./nano-bench.sh stop      # tears all of it down, board included
./nano-bench.sh status
```

Same console, same code -- but planner and picker run on the Orin's CUDA
build, reached over an ssh tunnel (Mac 18091/18092 -> nano 8091/8092).
The header shows `inference: Jetson Orin Nano` so there is no mistaking
whose silicon is answering. Measured on-device: E2B decodes at 28.9
tok/s (faster than this MacBook's Metal), a no-think planner turn is
0.7-1.3s, a complete dual exchange lands in ~2-4s, and the picker's
held-out benchmark scores an identical 12/18 to the Mac -- same weights,
same quant, temperature 0.

Needs the `ssh nano` alias, the CUDA `llama-server` at
`~/llama.cpp/build/bin/`, E2B in the pond data dir, and FunctionGemma at
`~/bench-models/functiongemma-270m-it-Q4_K_M.gguf` (copied once, outside
the pond's models dir so the registry never sees it). The script reuses
servers that are already up, so `start` is idempotent.

## The console

`python3 web.py` serves a page on :8093 that streams the loop live over
server-sent events. Each hand-off is one row, colour-coded by who spoke:

```
E2B      Call search_notes with query Kisumu     [names search_notes]
FGEMMA   search_notes({"query": "Kisumu"})       [honoured]
TOOL     Matching notes: travel -- Nairobi to Kisumu is 340 km ...
ANSWER   That is 340 km, about 211 miles.
```

The badges are the point. Every planner instruction is scanned for a tool
name; when it names one, the picker's choice is compared against it and
marked `honoured` or `overruled`. The running percentage in the sidebar is
the cooperation rate for the session, so a disagreement is visible the
moment it happens rather than buried in an eval summary.

The arm selector runs the same question through `dual`, `solo`, or `both`
back to back, which is the quickest way to see where the two diverge on a
prompt of your own.

It is a local development console: it binds to 127.0.0.1, has no auth, and
executes the sandbox toolbox. Do not expose it.

## The protocol

E2B is held to one line per turn:

```
TOOL: Call convert_units with value 340, from_unit km, to_unit mi
ANSWER: That is about 211 miles.
```

The `TOOL:` line is prose, not JSON -- E2B is never asked to produce rigid
syntax. It is instructed to resolve every unit, sum and reference *before*
writing the line, because the model reading it cannot. FunctionGemma then
sees all eight declarations and that one sentence, and picks.

The control arm gets the identical loop and an equally careful prompt, but
emits `CALL: {"tool": ..., "args": {...}}` itself.

Failures feed back rather than aborting: an unparseable pick, an unknown tool
or a missing required argument becomes a `TOOL_ERROR:` observation, and the
planner gets another turn to rephrase. That recovery path is the reason this
is a loop and not a pipeline.

## The toolbox

Eight tools, all real code over a local sandbox, no network, deterministic so
runs are comparable: `get_weather` (fixed fixture table, and its output says
so), `calculate` (AST-walking arithmetic, no `eval`), `set_timer`,
`list_timers` (the zero-argument case), `get_time`, `search_notes`,
`read_note` (over `sandbox/notes/`), `convert_units`.

Schemas are declared once in `dualgemma/tools.py` and rendered two ways: as
FunctionGemma declarations for the picker, as prose for the planner.

## Scoring

Per case: did the first call name the **right tool**, did the **required
arguments** come out right, and does the final answer **contain the fact** it
should. Plus a count of malformed calls the loop had to reject. Cases where
the correct behaviour is to call nothing at all are scored for restraint.

Compound cases carry an `expect_calls` list instead of a single expected
tool: every listed call must appear somewhere in the run, order-blind,
expected arguments matching as a subset. The metric is indifferent to
whether an arm satisfies it in one fan-out (natural) or across
sequential steps (dual, solo) -- both are legitimate ways to do
multi-tool work, and pricing them against each other is the point.

The 18 single-intent cases deliberately include the failure modes probed above: implicit
unit conversion (`10 minutes`, `an hour and a half`), word arithmetic (`a
fifth of eighteen thousand`), near-miss tool pairs (`calculate` vs
`convert_units`), a zero-argument tool, a two-hop case that must read a note
before converting what it found, and two cases whose right answer is no tool
call at all.

## Results

18 cases, both arms, temperature 0, Apple silicon, `llama-server` with Metal.

|  | tool | args | answer | malformed | s/case |
| --- | --- | --- | --- | --- | --- |
| **dual** (E2B + FunctionGemma) | 17/18 (94%) | 17/18 (94%) | 7/8 (88%) | 0 | 12.1 |
| **solo** (E2B alone) | 17/18 (94%) | 17/18 (94%) | 6/8 (75%) | 0 | 11.1 |

**On aggregate score the two arms tie.** Dual is one case better on answer
content out of eight scored, which is one case, not a result. Dual costs
about 9% more wall clock for the extra hop.

The interesting part is that they do not fail in the same way. Each arm lost
exactly one case, and the two failures are not comparable:

**Solo lost `math_direct`: it skipped the calculator.** Asked for 1234 times
17, it answered `209708` without calling anything. The correct answer is
20978 -- it did the arithmetic in its head and inserted a digit. Re-run five
times, solo scores 0/5 and dual scores 5/5. This is deterministic, not noise,
and it is exactly the failure class tool calling exists to prevent: a
confident wrong number with no call to audit.

**Dual lost `time_timezone`: FunctionGemma mis-picked.** E2B wrote the correct
instruction, `Call get_time with timezone JST`, and the picker returned
`read_note{name:get_time}` -- it read the tool name as a note name. At
temperature 0 rewording does not help, because the planner's instruction was
already right and the planner is the only participant that can see the
observation. The loop cannot repair a confident mis-pick.

So the honest summary is a trade, not a win: dual replaces *silent wrong
answers* with *visible wrong calls*. A bad call is bounded, diagnosable and
logged; a hallucinated number is none of those. Whether that trade is worth
an extra resident model and 9% latency depends on how much the caller values
auditability over simplicity.

One caveat worth stating plainly. The two arms cannot share a system prompt,
and dual's prompt necessarily explains that another model does the calling,
which primes delegation. The calculator result may therefore be a prompt
effect rather than an architectural one. Testing solo under a prompt that
mandates tool use as strongly is the obvious next experiment.

`notes_then_convert` failed identically in both arms, so it separates
nothing: `search_notes` does naive substring matching and the planners both
queried `road distance to Kisumu`, which appears in no note verbatim. That is
a limitation of the bench's toolbox, not of either arm.

### Are they cooperating?

Yes, and the disagreements are concentrated rather than scattered. Every
planner instruction is scanned for a tool name; when it names one, the
picker's choice is compared against it.

```
picker honoured the tool the planner named: 15/17 (88%)
```

Both misses are the **same case and the same tool**: `Call get_time with
timezone JST` came back as `read_note{name:get_time}` twice, the picker
reading the tool name as a note name. Across every other hand-off in the
suite -- weather, arithmetic, timers, unit conversion, note search, the
zero-argument tool -- FunctionGemma called exactly what E2B asked for, with
the arguments E2B spelled out.

So the division of labour holds. The planner's instructions are followed
almost all of the time, and the failure is not "the small model is
unreliable" but "the small model cannot distinguish one specific pair of
tools." That is a much more tractable problem, and it is what constrained
decoding over the declared tool names would remove outright.

Reproduce with `python3 eval.py --arm dual`, or watch it live in the console,
where each hand-off is tagged `honoured` or `overruled`.

## The first run was wrong, and why

The first full run scored dual at 83% tool / 72% args / 62% answer against
solo at 94/78/88, which looked like a clear defeat for dual. Both gaps were
bugs in this harness, not properties of the architecture:

1. **Gemma 4 E2B is a thinking model.** It spends tokens in
   `reasoning_content` before writing any `content`. A 320-token budget
   truncated it *inside the reasoning*, so `content` came back as the empty
   string with `finish_reason: length`. On `timer_compound` the transcript
   shows it had already computed `3600 + 1800 = 5400` and simply never got to
   say so. An empty planner reply reads as "no tool call" and scores as a
   miss. The budget is now 1024 and truncation is counted, not swallowed.

2. **FunctionGemma copies parameter descriptions into argument values.** A
   description reading `Unit to convert from: km mi kg lb c f m ft` came back
   as `from_unit: "km mi kg lb c f m ft"`. Descriptions the picker sees are
   now terse; anything the *planner* needs but the transducer must not echo
   lives in a `hint` field that only `prose_catalogue` renders.

Fixing those two moved dual from 83/72/62 to 94/94/88 and changed the
conclusion from "clearly worse" to "tied". The harness was worth more than
the architecture, which is the main methodological lesson of this bench: a
two-model system has twice the surface for the measurement itself to be
broken, and a negative result from a small local model deserves a look at the
transcript before it is believed.

A third fix was applied to **both** arms so the comparison stayed fair: a
repetition guard. A mis-pick is not an error -- the call succeeds and returns
something useless -- so nothing in the loop noticed, and at temperature 0 the
planner reproduced the same instruction until the step budget ran out.
`time_timezone` burned all four steps that way. The guard cuts it to two and
the model gives up honestly instead.

## What would be worth trying next

- Constrained decoding on the picker (GBNF over the declared tool names)
  would make `read_note{name:get_time}` unrepresentable, which is dual's only
  remaining loss here.
- Let the picker see the failed observation. Today only the planner does, so
  the participant that made the mistake never learns of it.
- A larger case set. At n=18 with one-case margins, this bench can detect a
  catastrophe but not an improvement.
- Solo under a delegation-mandating prompt, to separate the architecture from
  the prompt framing.

## Layout

```
serve.sh              start/stop/status the two llama-servers (Mac)
nano-bench.sh         the same bench on the Jetson, tunnelled to the Mac
web.py                web console: SSE server, live hand-off trace
web/index.html        the console page
chat.py               interactive driver, prints the hand-off trace
eval.py               scores both arms, writes JSON
cases.json            the 22 labelled cases (18 single + 4 compound)
dualgemma/llama.py    llama-server HTTP client (stdlib only)
dualgemma/fgemma.py   FunctionGemma prompt format + output parser
dualgemma/tools.py    the eight tools and their schemas
dualgemma/agent.py    the three arms
finetune/             LoRA pipeline for the picker (see finetune/README.md)
sandbox/notes/        fixture notes the note tools read
```

`dualgemma/fgemma.py` is a port of the declaration builder and parser in
`crates/pond-adapters-local-inference/src/tool_caller.rs`, extended to declare
several tools at once so the model *picks* rather than only filling arguments
for a tool something else already chose.

## Fine-tuning the picker

`finetune/` is a complete LoRA pipeline targeting the picker's measured
failure classes -- data generator, isolated before/after benchmark
(baseline: 12/18 on the held-out set), MLX training, GGUF conversion.
One manual step stands between it and a training run: the base weights
are licence-gated on Hugging Face (see `finetune/README.md`).

## Relationship to GIAP

None, by construction. This does not import from `crates/`, does not touch
`pond_system.db`, and is not wired into any binary. It is a bench for one
question, kept separate so the answer can be negative without anything
needing to be unpicked.

Note that it deliberately tests an architecture the main project has ruled
out -- GIAP's standing position is that the main LLM calls tools natively via
MCP, with no separate tool-caller model in the loop. This bench exists to put
a number on what that position costs or saves.
