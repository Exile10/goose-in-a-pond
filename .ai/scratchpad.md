# Scratchpad

## Efficiency Improvement Plan (from Ultrareview 2026-05-16)

### Context
Audited Goose submodule usage, local inference pipeline, and tool-calling architecture.
Key finding: GIAP already has `pond-agent` + `pond-inference` that can replace the Goose
dependency for local GGUF inference — eliminating 10+ min compile, session double-bookkeeping,
and 3-4 serialization boundary crossings per message.

### Completed (this session)
- [x] Atomic schedule update (create-before-remove pattern)
- [x] Cron validation on update_schedule (looks_like_cron guard)
- [x] Wikipedia 4000-char truncation restored
- [x] rsplit → rsplit_once fix in prepend_knowledge_hint
- [x] Background memory record_access (tokio::spawn, off hot path)
- [x] Session adapter test fix (archived_at, project_id fields)

### P0 — Strategic (next sprint)

1. **Wire `pond-agent` + `pond-inference` as primary local GGUF path**
   - `pond-agent` implements Agent trait with correct tool loop
   - `pond-inference` has comprehensive multi-format tool parsing (17 tests)
   - Missing: MCP tool dispatch (marked TODO in pond-agent)
   - Gain: eliminates Goose overhead entirely for local models

2. **KV-cache persistence between turns**
   - Currently: fresh LlamaContext per inference call (10-20s TTFT on Jetson)
   - Fix: persist context in pond-inference's LlamaCppEngine, do incremental decode
   - Gain: eliminate dominant latency source on constrained devices

### P1 — This sprint

3. **Always-compact tool schemas on small context (≤4096)**
   - Skip full JSON schemas on Jetson tier; use name+description only
   - Gain: +300-600 tokens for actual conversation

4. **Cache allowed_tools + skip redundant extension stripping**
   - Currently re-queries all MCP servers and strips 10 defaults every turn
   - Gain: -20ms per turn

### P2 — Next sprint

5. Parallel tool execution in pond-agent (stream::select_all pattern)
6. Context compaction in pond-agent (summarize old messages at 80% budget)
7. Repetition detection (prevent infinite tool loops)
8. Speculative decoding / MTP via ik_llama_cpp (2-3x generation throughput)
9. Drop Goose submodule entirely once pond-agent handles local+ollama

### "Harmont Syntax" Note
No such component exists. The term maps to "Harmony tool markup" — Gemma 4's native
`<|tool_call>call:NAME{...}<tool_call|>` format. Parsing is already handled by:
1. Goose's llama-cpp-2 internal parser (current production path)
2. pond-inference/src/tool_calling.rs (most comprehensive, 17 unit tests)
3. FunctionGemma tool_caller.rs (specialist fallback for empty params)

## Completed: Canvas Bug + E2E Fixes + Visual Polish

### Canvas Navigation Bug — RESOLVED
The Canvas button in the sidebar works correctly. In Vite dev mode, `invoke("show_canvas")` throws (no Tauri runtime), the catch block dispatches `SET_SECTION: "canvas"`, and `GuiMode.tsx`'s `SectionContent` renders `<Canvas />`. Verified via Playwright — full Canvas UI renders (toolbar, chat dock, canvas pane, suggestion chips, input).

### E2E Test Fixes — ALL PASSING (58 pass, 3 skipped)

**models.spec.ts:63** — "memory status shows total MB when non-zero"
- Root cause: Models section showed only `50% memory` but test expected `8192|8 GB`
- Fix: Updated inline memory display in `Models.tsx` card header to show `22% · 6.2 / 8.0 GB` (percentage + GB values)

**models.spec.ts:80** — "memory status shows loaded model name when a model is hot"
- Root cause: `MemoryStatusBar` component existed but was unused; loaded_model not displayed
- Fix: Added `<Chip>` with `loaded_model` name to the inline memory display

**schedules.spec.ts:82** — "create schedule form submits correctly"
- Root cause: `getByLabel(/prompt/i)` matched sidebar "Prompts" button (aria-label="Prompts") before the form's textarea (aria-label="Schedule prompt")
- Fix: Changed test selector to `getByLabel("Schedule prompt")` for exact match

### Visual Polish — VERIFIED at 1280x800
All 14 sections screenshotted and verified:
1. Dashboard — Notifications accordion, Voice Mode card, Usage & Savings, Server Status, Quick Actions
2. Chat — Empty state, composer, model picker
3. Devices — Empty state with icon
4. Schedules — Calendar view with events, list/calendar toggle
5. Memory — Real data, search, filters, segment badges, tier labels
6. Skills — Empty state
7. Logs — Table layout, level filters, search, autoscroll toggle
8. Models — Role chips, memory display (% + GB + loaded model), capability badges, category tabs
9. Prompts — 4 presets, editable template, token count, variables reference
10. Settings — 8 tabs (Identity/Voice/Models/Prompts/Location/Agent/Data/Tools)
11. Extensions — Installed/Browse tabs, extension card
12. Voice Mode — Waveform, transcript area, Start Listening
13. Canvas — Split pane (chat dock + canvas rendering), suggestion chips
14. Collapsed sidebar — Icons only, consistent alignment

### Test Results
- Unit tests: 131/131 passing
- E2E tests: 58/58 passing (3 skipped = live-only tests)
