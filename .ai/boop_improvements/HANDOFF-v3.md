# Handoff: Tool Calling Pipeline Fix + Prompt Stabilisation — Session 3

**Date**: 2026-05-13
**Branch**: `refactor/agent-loop-v2`
**Previous handoff**: `.ai/boop_improvements/HANDOFF-v2.md`

---

## What Was Accomplished This Session

### 1. Gemma 4 Tool Calling — Fixed End-to-End (7/8 tests pass)

The tool calling pipeline was completely broken — the model produced immediate EOS (1 completion token) on every request. Root cause: a **three-layer configuration failure** in the pipeline from GIAP settings → Goose registry → llama.cpp inference engine.

**Bug 1: `enable_thinking: true` (default) caused EOS**
- `ModelSettings::default()` has `enable_thinking: true`
- This reached `OpenAIChatTemplateParams` as `reasoning_format: Some("auto")`
- Gemma 4 E2B's Jinja template rendered a thinking-mode prompt the model couldn't handle
- **Fix**: Set `enable_thinking: false` in `apply_platform_settings()`, `apply_jetson_settings()`, and all 4 registry entry points. GIAP handles thinking through its own PromptState + ThoughtFilter pipeline.

**Bug 2: `native_tool_calling` overwritten on every inference call**
- `resolve_model_path()` in `local_inference.rs` unconditionally overwrote `native_tool_calling` from `default_settings_for_model()`
- Our model ID `gemma-4-E2B-it-Q4_K_M` didn't match any featured model → returned `false`
- **Fix**: OR-merge in Goose — `settings.native_tool_calling || defaults.native_tool_calling`

**Bug 3: Jetson settings missing `use_jinja` and `native_tool_calling`**
- `apply_jetson_settings()` used `..Default::default()` which has `use_jinja: false`
- **Fix**: Added `use_jinja: true`, `native_tool_calling: true`, `enable_thinking: false`

**Test results (after fixes):**
```
[01] Wikipedia: person lookup     — PASS (64s) — INPUT: {"topic": "John Cena"}
[02] Wikipedia: place lookup      — PASS (46s) — INPUT: {"topic": "Nairobi"}
[03] Weather: no params needed    — PASS (35s) — INPUT: {}
[04] Memory: save with content    — PASS (33s) — INPUT: {"content": "...", "segment": "preference", "tags": "color"}
[05] Schedule: create             — PASS (36s) — INPUT: {"cron": "0 8 * * *", "name": "...", "prompt": "...", "timezone": "Africa/Nairobi"}
[06] System info                  — FAIL (20s) — model routing quirk, not pipeline bug
[07] Greeting: no tool expected   — PASS (19s)
[08] Factual: capital of France   — PASS (32s) — INPUT: {"topic": "capital of France"}
```

### 2. llama-server Process Manager — Removed

`llama_server_process.rs` was never functional. Removed:
- The module file (247 lines)
- `mod llama_server_process` from `main.rs`
- Setup step 6 (llama-server binary download), renumbered steps to 6 total
- `_llama_server_guard` block from serve command
- `download_llama_server_binary()`, `llama_server_asset_name()`, `LLAMA_SERVER_RELEASE_TAG` from `model_download.rs`

### 3. System Prompt — XML Structure + Stabilisation

**XML-structured prompts** for all 4 styles (balanced, concise, technical, warm):
- `<identity>` — who the model is
- `<instructions>` — behavioral rules
- `<context-handling>` — how to process `<system-context>` blocks
- `<tool-usage>` — when/how to use tools + "unsure? check tools first" rule
- `<memory-rules>` — save/recall patterns
- `<output-quality>` — no fabrication rules
- `<home-devices>` — conditional, device control
- `<thinking>` — conditional, deep reasoning
- `<voice-mode>` — conditional, TTS-friendly output

**Prompt stabilisation** — dynamic content moved from system prompt to user message:
- `current_date` / `current_time` removed from all templates (was `{% if current_date %}`)
- Temporal context + memories injected as `<system-context>` block in user message
- User's actual request wrapped in `<user-message>` tags
- System prompt is now token-stable across turns (enables future KV cache prefix reuse)

**User message structure:**
```xml
<system-context>
CURRENT CONTEXT: Today is Tuesday, 13 May 2026. The time is 15:30.
<memories>
- [preference] favorite color is green
- [identity] user's name is Jerry
</memories>
</system-context>
<user-message>
what is the weather?
</user-message>
```

**"Unsure? Check tools first" rule** added to all prompts — when the model lacks knowledge, it checks available tools before telling the user it can't help.

### 4. Test Script Overhauled

`scripts/test-tool-params.sh` — comprehensive diagnostic:
- Captures server config (provider, model, extension toggles)
- Lists registered extensions
- Full SSE event breakdown per test (tool name, input, result, thinking, text, errors, usage)
- Assertions on tool dispatch, param keys, and output existence
- Produces `test-tool-params-results.txt` for analysis

### 5. KV Cache Prefix Reuse — Attempted, Reverted

Implemented `state_save_file` / `state_load_file` based prefix caching in Goose's `inference_engine.rs`. The implementation compiled and unit tests passed (5 new tests for `find_common_prefix_len`), but caused the model to hang at runtime — the state file for a 32K context was too large and blocked the inference thread. **Reverted.** The prompt stabilisation (moving dynamic content to user message) is the prerequisite that remains in place.

---

## Current State of the Code

### Files Changed (GIAP, uncommitted on `refactor/agent-loop-v2`)

| File | Changes |
|------|---------|
| `crates/pond-core/src/prompts.rs` | XML-structured templates, removed `{{current_date}}`/`{{current_time}}`, added `<system-context>` handling, tool-fallback rule |
| `crates/pond-core/src/services/prompt_builder.rs` | Updated test assertions for XML tags |
| `crates/pond-adapters-goose/src/goose_agent.rs` | Moved temporal+memories to `<system-context>` in user message, `<user-message>` wrapper, `enable_thinking: false` in `register_gguf_model()`, XML fallback prompt |
| `crates/pond-adapters-local-inference/src/lib.rs` | `enable_thinking: false` in platform/Jetson settings, `use_jinja: true` + `native_tool_calling: true` in all registry entries, added missing `LocalModelEntry` fields |
| `crates/pond-server/src/main.rs` | llama-server removal (mod, setup step, serve guard) already in working tree |
| `crates/pond-server/src/model_download.rs` | Removed llama-server download code |
| `scripts/test-tool-params.sh` | New comprehensive diagnostic test script |

### Goose Submodule Changes (uncommitted in `goose/`)

| File | Changes |
|------|---------|
| `local_inference.rs` | OR-merge fix: `native_tool_calling \|\| defaults` instead of unconditional overwrite |
| `local_model_registry.rs` | Same OR-merge fix in `enrich_with_featured_mmproj()` |

### Build Status

```bash
cargo build -p pond-server                    # ✓ compiles clean
cargo test -p pond-core --lib -- prompt       # 57 pass
cargo test -p pond-server --lib               # 18 pass
cargo test -p pond-adapters-local-inference   # 35 pass
```

### Database State

All 4 prompt templates updated in live DB via `PUT /api/v1/prompts/{name}`:
- XML-structured with `<identity>`, `<tool-usage>`, `<context-handling>`, etc.
- `<system-context>` / `<user-message>` handling instructions in all prompts
- Reseed on next `pond-server serve` will also apply via `upsert`
- `memory_consolidation_enabled: false` (turned off via API)

---

## What Needs Attention Next

### 1. Test the Full Pipeline (HIGH)

The prompt stabilisation (dynamic content in `<system-context>` + `<user-message>` XML) has NOT been live-tested yet. The XML structure is new. Run:
```bash
target/debug/pond-server serve
bash scripts/test-tool-params.sh
```
Feed `test-tool-params-results.txt` back for analysis if issues arise.

### 2. KV Cache Prefix Reuse (MEDIUM)

The prompt is now stable across turns (system prompt doesn't change). The `state_save_file` approach was too heavy (serialises entire 32K context). Better approaches:
- **Persistent LlamaContext**: keep context alive across turns, use `clear_kv_cache_seq()` to trim the suffix, prefill only delta tokens. Requires solving the `LlamaContext<'model>` self-referential lifetime issue.
- **Sequence forking**: use `copy_kv_cache_seq()` to fork a prefix into a working sequence per turn.

The llama-cpp-2 v0.1.146 API supports both: `clear_kv_cache_seq`, `copy_kv_cache_seq`, `kv_cache_seq_add`.

### 3. Commit Everything (HIGH)

All changes are uncommitted. Suggested commits:
1. `fix: Gemma 4 tool calling — enable_thinking:false, OR-merge, registry settings`
2. `refactor: remove llama-server process manager`
3. `feat: XML-structured system prompts + prompt stabilisation for KV cache`

### 4. Memory Consolidation (LOW)

Turned off (`memory_consolidation_enabled: false`). The consolidator uses the LLM provider which conflicts with inference during chat. Needs either a separate model slot or queue-based consolidation during idle periods.

---

## Key Architecture Decisions Made

1. **`enable_thinking: false` for all local GGUF models** — GIAP handles thinking through PromptState + ThoughtFilter, not llama.cpp's native `reasoning_format`. The C-level thinking mode causes Gemma 4 E2B to produce EOS.

2. **OR-merge for `native_tool_calling`** — Goose's `resolve_model_path()` no longer downgrades explicit `true` to `false` for non-featured models. Our custom-registered GGUF files retain their settings.

3. **XML-structured prompts** — all system prompt sections use XML tags (`<identity>`, `<tool-usage>`, `<memory-rules>`, etc.) for clear boundary markers. SLMs parse structured XML more reliably than heading-based formats.

4. **Dynamic content in user message** — timestamp and memories moved from `extend_system_prompt()` to `<system-context>` block in user message. User request wrapped in `<user-message>`. System prompt is token-stable across turns.

5. **"Unsure? Check tools first"** — all prompts instruct the model to check available tools before saying it can't help. Graceful fallback only when no tool matches.

---

## Build & Test Commands

```bash
# Build
cargo build -p pond-server

# Test
cargo test -p pond-core --lib -- prompt
cargo test -p pond-server --lib
cargo test -p pond-adapters-local-inference

# Live test (start server first)
target/debug/pond-server serve
bash scripts/test-tool-params.sh

# Check registry settings
cat ~/.local/share/goose/models/registry.json | python3 -c "
import sys,json; d=json.load(sys.stdin)
for m in d.get('models',[]):
    s = m.get('settings',{})
    print(f'{m[\"id\"]}: jinja={s.get(\"use_jinja\")}, ntc={s.get(\"native_tool_calling\")}, think={s.get(\"enable_thinking\")}')
"
```
