# Context & Reasoning Roadmap

State of the world (verified against code, 2026-07-27) and the phased plan for:
relevant context (Memories, Tools, Message History), letting the model reason
across as many turns as it needs, memory consolidation hardening, and — after
that — multimodality (image, audio, video).

All file:line references are to the live GooseAdapter path
(`crates/pond-adapters-goose/src/goose_agent.rs` unless noted). The
quarantined PondAgent loop (Q2-05) is out of scope.

---

## 1. Where we are today (verified)

### Memories

- **Injection is not semantic.** Per turn (when `agent_memory_inject`, default
  on, limit 5) GIAP merges `search_recent` (created_at DESC) with
  `search_by_content` — an OR'd SQL `LIKE %kw%` over every word ≥ 3 chars of
  the user message, stopwords included (`goose_agent.rs:1064-1104`). The
  fastembed embedding provider (all-MiniLM-L6-v2) is initialized at startup but
  is only reachable through the explicit `recall_memories`/`save_memory` MCP
  tools — never on the injection path.
- **Extraction memories are unembedded.** `MemoryFragment::from_extraction`
  stores `embedding: None` (`memory.rs:191-219`) and nothing backfills, so the
  vast majority of memories are invisible to vector search. Worse: once a few
  `save_memory` (embedded) rows exist, `search_similar` considers ONLY those
  and ignores every extraction memory.
- **Dedup is lexical substring vs the 20 most recent** rows, twice
  (`llm_memory_extractor.rs:188-195`, `memory_extraction.rs:89-97`).
  Paraphrases duplicate freely.
- **Budgeting**: merged fragments sorted importance-DESC, truncated to the
  profile cap, greedily packed under `memory_token_budget` (chars/4). On the
  Jetson tier (ctx ≤ 4096) that is 200 tokens / 3 fragments — the same
  high-importance identity memories dominate every turn.
- **Placement is right**: memories render inside
  `<system-context><memories>` prepended to the user message, keeping the
  system prefix token-stable for KV reuse. Keep this invariant.
- Decay/cleanup runs by default (adaptive half-life, archive < 0.15,
  prune < 0.05). `record_access` reinforcement fires on injection.

### Tools

- **No relevance selection anywhere.** 14 `giap-*` extensions (57 tools when
  all enabled; CLAUDE.md's "12" is stale) are registered at startup from
  settings toggles; every session loads all of them; `allowed_tools` per turn
  is the full cached union (`goose_agent.rs:1452-1547`); goose's
  `prepare_tools_and_prompt` sends `list_tools(None)` wholesale. The shim's
  allow-set is a veto (drops goose self-injected tools), never a narrower.
  Cost: ~100 tok/tool through the Gemma template ≈ 5.7K prompt tokens every
  turn on an 8K-class budget.
- `giap-schedule` alone is 12 of 57 tools.
- **ShimControls is one global slot per adapter** (`provider_shim.rs:66`) —
  fine while every session gets the same set; a data race the moment per-turn
  or per-session selection exists. Must be keyed before any selection work.
- **Tool results enter history verbatim** below goose's 200K file-spill
  threshold. GIAP's `TOOL_RESULT_MAX_CHARS = 1500` truncation only affects the
  trimmer's token ESTIMATE — the rebuild keeps structured ToolResponse
  messages whole, so a 50K tool result is re-prefilled every turn until its
  turn is dropped.

### Message history

- Model context comes from goose's `sessions.db` conversation, not
  `pond_system.db`. The GIAP→goose session mapping is **in-memory only**, and
  goose session ids are auto-generated: after a pond-server restart an
  existing chat resolves to a brand-new EMPTY goose session. The model loses
  the whole conversation even though pond_system.db has every message. Nothing
  replays/hydrates.
- `hybrid_compaction_enabled` defaults false, so out of the box the only
  defenses are goose's reactive LLM auto-compaction (threshold 0.8 — an
  on-device stall) and the 200K spill. With hybrid on, the deterministic
  trimmer drops whole oldest turns and splices the idle rolling summary.
- `GOOSE_CONTEXT_LIMIT` / `GOOSE_AUTO_COMPACT_THRESHOLD` are only (re)written
  when the provider:model key changes (`ensure_provider_current` early
  returns) — toggling hybrid compaction or context_window_override does
  nothing until a model switch or restart.
- Goose's tool-pair summarization (default ON) spawns background LLM calls to
  summarize old tool pairs — spending scarce on-device tok/s even in hybrid
  mode, double-owning tool-result pruning with the GIAP trimmer.
- Token estimation is chars/4 everywhere GIAP-side despite the engine knowing
  real counts (feedback multiplier exists but corrects one turn late).

### Turn budget (reasoning length)

- The live cap is `agent_max_turns` = **20 turns text / 8 voice** (a turn =
  one provider call). Goose's own default (1000) is unreachable because GIAP
  always passes `Some(effective_max_turns())`.
- On cap: the model's stream ends with MAX_TURNS_MESSAGE ("… Would you like
  me to continue?") — but nothing is wired to actually continue, and the
  message is not persisted in goose's own store (histories diverge).
- **The model never sees its budget**: goose's MOIM `<turn-context>` carries
  "N/M turns used" but the GIAP shim strips ALL turn-context blocks, so the
  model cannot pace itself or wrap up.
- Retries don't consume budget, but goal/grind nudges do.
- `/chat/stream` has an idle (silence) timeout `agent_timeout_secs` (300s
  default; resets on every event); `/agent/chat/stream` has none.
- Thinking: prompt-level `thinking_mode` renders a `<thinking>` section;
  engine-level `enable_thinking` is a registry default GIAP never sets, and
  the ThoughtFilter strips Harmony-style leakage. Prompt-off + engine-on is
  the current (inconsistent) combination when thinking is disabled.

### Memory consolidation (bonus track)

Real code, off by default (`memory_consolidation_enabled: false`):
three-stage adversarial pipeline (Proposer → Adversary → Judge, cancellable
between stages), inactivity trigger, SSE streaming UI, correction-safety
guards, audit table. Verified defects:

1. Fires ~15 min after every boot with zero activity (`last_user_activity`
   initialized to now; no startup guard, unlike the summary loop) — violates
   the "never on startup" constraint — and re-fires every ~15 min while idle.
2. Three settings are dead: `memory_consolidation_mode`,
   `_interval_hours`, `_batch_size` are persisted, UI-exposed, and never read.
   Trigger is hardcoded 15 min; mode is always adversarial; no batch cap.
3. No batch cap: ALL scoreable memories go into each of the three prompts —
   context blowup on a 3B/Jetson model as the store grows.
4. Apply-logic duplicated: `pond-core` `run_consolidation` (tested, handles
   Split/Recategorize) is production-unused; `main.rs:4182-4346` reimplements
   it inline. Drift risk.
5. Toggling the setting requires a restart (runner + loop built from a
   startup snapshot).
6. Dead code: single-pass `LlmMemoryConsolidator` (only an ignored live test
   calls it), orphaned `ports/adversarial_consolidator.rs` (references a
   nonexistent type, not in mod.rs).
7. `docs/architecture/memory_system.md` describes the old single-pass/24h
   design — stale.
8. Only chat/agent-chat/routine-run reset activity; voice-child and other API
   traffic do not cancel a running consolidation.

### Multimodality (scouted for phase F)

- The engine's mtmd vision path WORKS (mmproj auto-download, MtmdBitmap
  tokenize/eval, vision-capable Gemma E2B/E4B registry entries, no-vision
  fallback). The REST API already accepts `ChatRequest.images`. **The single
  blocking gap**: `goose_agent.rs:1588` builds the user message with
  `with_text` only — `request.images` is dropped on the floor.
  `Message::user().with_image(data, mime)` exists and types line up.
- Missing around it: desktop attachment UI + `images` in `ChatStreamRequest`;
  `/agent/chat/stream` hardcodes `images: vec![]`; images aren't persisted to
  history (a follow-up about an earlier image loses the pixels); vision turns
  forfeit KV prefix reuse (engine drops the retained session); `vision_capable`
  isn't surfaced to the UI; E4B+mmproj is at/over the 8GB Jetson budget (E2B
  is the realistic on-device vision target).
- Audio: dead-ends at goose's message types (`RawContent::Audio` →
  "[Audio content: not supported]") even though mtmd reports audio support —
  needs a fork-side content variant + extract path. Whisper ASR remains the
  transcription route meanwhile.
- Video: no path; realistic v1 is frame sampling → image path. Camera
  snapshots exist on disk (`camera_events.snapshot_path`) but vision MCP
  tools return text only.

---

## 2. The plan

Ordering principle: relevance first (biggest quality win per token), then
reasoning length, then history durability, then tool surface (KV-sensitive),
then consolidation hardening, then multimodality.

### Phase A — Relevant memories (semantic injection)

- A1 Embed at write: pass the embedding provider into the extraction
  pipeline so every new memory is embedded; startup backfill job for rows
  `WHERE embedding IS NULL` (batched, idle-friendly).
- A2 Semantic injection: embed the user message per turn (fastembed, CPU,
  ~ms) and call `search_similar`; merge recency + semantic (replace the
  naive LIKE keyword fetch; keep it only as a no-embedding fallback with a
  stopword filter).
- A3 Ranking: blend similarity, importance, and recency decay for the budget
  cut instead of importance-only, so topical memories can displace the
  standing identity block.
- A4 Semantic dedup at write: cosine against top-K similar (not substring vs
  20 recent).
- Invariants: memories stay in `<system-context>` in the user message (KV
  prefix stability); Jetson tier budgets unchanged until measured.

### Phase B — Reasoning length (turns) — LANDED (B5 deferred)

- B1 DONE `default_agent_max_turns` 20 → 50, and `0 = uncapped` (expressed to
  goose as `UNCAPPED_MAX_TURNS = 100_000`, safe to do arithmetic on unlike
  `u32::MAX`). Cancellation, idle timeout, and context-overflow abort remain the
  rails. Voice keeps its own cap: a non-zero `voice_max_turns` still binds even
  when the text budget is uncapped.
- B2 DONE, PER-REQUEST rather than per-turn. A `<turn-budget>` note goes into
  the user message's `<system-context>` (`turn_budget_note` in pond-core). The
  ≥ 50%-of-cap trigger from the original plan needs `turns_taken` mid-loop,
  which GooseAdapter cannot see — it builds the user message once, before
  `agent.reply`. A mid-loop injection would need a fork-side seam (see below).
- B3 DONE `AgentStreamEvent::TurnLimitReached { max_turns }`, detected by
  matching goose's private `MAX_TURNS_MESSAGE` verbatim (canary test
  `goose_cap_message_is_still_verbatim` reads the fork source so a reword
  fails). Surfaced as a `turn_limit_reached` SSE event from both stream routes;
  both desktop chat surfaces render a Continue action. The cap text is still
  emitted as Text so persistence and voice are unchanged.
- B4 DONE `enable_thinking` is passed as a ModelConfig request_param for
  `local`/`gguf` (the only provider that reads it), resolved from
  `thinking_mode` + voice + capabilities. A thinking-mode change re-stamps the
  config on the retained provider instead of rebuilding it.
- B5 NOT DONE — excluding goal/grind nudges from the turn count is inside
  `goose/crates/goose/src/agents/agent.rs`, i.e. a fork change. Deferred to a
  goose-side patch.

### Phase C — History durability & budgets — LANDED

- C1 DONE The pairing is persisted in `pond_system.db` (`engine_session_map`,
  migration 0032) behind two engine-neutral `SessionStorage` methods, and
  re-validated against goose on read (its store can be wiped independently). On
  a miss with existing pond history the new goose session is hydrated via
  pond-core's `plan_replay` (recent turns + rolling summary, budgeted by the
  same trimmer, trailing user message dropped because the handler persists it
  before the stream). Text-only: pond rows cannot rebuild a valid tool
  request/response pair.
- C2 DONE `GOOSE_CONTEXT_LIMIT` / `GOOSE_AUTO_COMPACT_THRESHOLD` (plus C3's
  knob) moved into `apply_goose_env_knobs`, called on the settings path every
  turn and `set_var`-ing only when the signature changes.
- C3 DONE `truncate_head_tail` (pond-core) is applied to the retained structured
  `ToolResponse` at rebuild — cloning the message and rewriting only text bodies,
  so ids, annotations, error flags and pairing are preserved — and to the
  trimmer's estimate, so estimate and reality agree.
  `GOOSE_TOOL_PAIR_SUMMARIZATION=false` for local/gguf + hybrid: the
  deterministic trimmer owns tool-result pruning on-device.
- C4 DONE `hybrid_compaction_enabled` now defaults to true.

### Phase D — Tool relevance (KV-aware)

- D1 Key ShimControls per session (prerequisite; the global slot becomes a
  race the moment tool sets differ).
- D2 Per-SESSION tool selection: a stable core set + semantically relevant
  extensions chosen at session start (first message + memories), sticky for
  the session so the KV prefix stays reusable; a discovery escape hatch lets
  the model pull in more (accepting a one-time prefix rebuild). Never a
  keyword classifier deciding IF tools are used — the model still calls tools
  natively (working agreement).
- D3 Compress `giap-schedule` (12 tools → dispatch-style with an action enum)
  to shrink the baseline surface.

### Phase E — Consolidation hardening (bonus)

- E1 Startup guard (mirror the summary loop) + honor
  `memory_consolidation_interval_hours` as the idle re-fire floor.
- E2 Wire or remove the dead settings; wire the enable toggle without restart
  (re-read settings in the loop).
- E3 Batch cap from `memory_consolidation_batch_size` (top-N scoreable by
  age/score) so prompts fit a 3B model.
- E4 Replace the inline apply loop in main.rs with pond-core's
  `run_consolidation`; delete the orphaned port + dead single-pass
  consolidator (or wire it as the "single" mode if kept).
- E5 Refresh `docs/architecture/memory_system.md`.

### Phase F — Multimodality (after the above)

- F1 Image v1: attach `request.images` in GooseAdapter (`with_image` loop) +
  `/agent/chat/stream` parity + desktop attachment UI (`ChatStreamRequest.images`,
  picker/paste, per-model gating via a surfaced `vision_capable`).
- F2 Image history: persist attachments (pond_system.db + replay decision)
  so follow-ups about an earlier image keep working after trims/restarts.
- F3 Camera bridge: vision MCP tool variant that returns the snapshot as
  image content ("what's at the door?" feeds the frame to the model).
- F4 Audio-to-model (fork work): MessageContent variant + mtmd audio extract
  path; until then Whisper transcription remains the audio route.
- F5 Video v1: frame sampling into the image path (bounded frames/turn).
- Perf note: vision turns bypass KV retention (full prefill) — measure on
  Jetson with E2B + mmproj before enabling by default.
