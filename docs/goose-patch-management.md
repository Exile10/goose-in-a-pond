# Goose Submodule Patch Management

## Overview

GIAP pins a fork of [aaif-goose/goose](https://github.com/aaif-goose/goose) as a git submodule at
`goose/`. The fork lives at `https://github.com/jarida-io/Goose` and carries a small set of
patches on top of upstream. This document describes those patches, the branch strategy, and
how to rebase against a new upstream goose release.

---

## GIAP Patch Set

### Branch: `main` (current pin)

`jarida-io/Goose:main` = upstream goose `main` (synced 2026-07-25, 675 commits,
upstream tip `192b5db8b`) merged into the previous `giap-patches-rmcp-1.5`
history, carrying:

| Patch | Description | Files |
|--------|-------------|-------|
| ollama tool-less retry | Retry the turn tool-less when an Ollama model rejects tools with HTTP 400 ("does not support tools"). Adds `is_tools_unsupported_error` + splits `stream` into `stream` (catch/retry) and `stream_inner` (single attempt), so non-tool models (e.g. `gemma3:4b`) answer as plain chat models instead of aborting the turn. Originally `d52efa2e`; re-ported after upstream moved the provider. | `crates/goose-providers/src/ollama.rs` |
| GIAP featured Gemma models | Extra `FEATURED_MODELS` entries for the Gemma 4 family GIAP ships on-device: E1B, E2B (+mmproj), 12B-A4B (+mmproj), 27B (+mmproj). Upstream only features E4B and 26B-A4B. Featured status gives these models `ToolCallingMode::ForceNative` defaults and vision wiring. | `crates/goose-local-inference/src/local_model_registry.rs` |
| llama.cpp ProviderStats parity | The llama.cpp backend fills `ProviderStats` (TTFT, `model_load_ms`, `elapsed_ms`, `output_tokens`) like the MLX backend already does, plus two new optional fields: `prefill_ms` and `effective_context_tokens`. Additive/serde-default; PR upstream. Commit `961f1b0a5`. | `crates/goose-provider-types/src/conversation/token_usage.rs`, `crates/goose-local-inference/src/lib.rs`, `crates/goose-local-inference/src/llamacpp/*` |
| thinking-only turns count as empty | `provider_produced_content` treated a `Thinking`/`RedactedThinking` block as content, so a turn that emitted only reasoning — no text, no tool call — was not an empty turn and never reached the existing `MAX_EMPTY_TURN_RETRIES` path. The user got total silence. Small local models hit this reliably: gemma-4-E2B closes its thinking block and emits `end_of_turn` for certain phrasings (measured 5/5 on one query, gemma-4-E2B Q4_K_M). Two match arms now return `false`; safe because the flag accumulates over the chunk loop (thinking-then-text still sets it via the `Text` arm, thinking-then-tool-call is covered by `no_tools_called`). Verified: silent turn -> visible "empty response" message. Also makes the retry budget overridable via `GOOSE_MAX_EMPTY_TURN_RETRIES` (default unchanged at 3): the built-in retry re-sends an *unchanged* conversation, which a deterministic provider answers identically, so a host that varies the prompt itself sets this to `0` and owns recovery. GIAP does exactly that — see `EMPTY_TURN_STEER` in `goose_agent.rs`, which recovered a reliably-silent query into a real 3-tool answer 4/4. When the budget is 0 the `EMPTY_TURN_MESSAGE` is yielded but NOT persisted: it is a signal that the turn failed, not an answer, and persisting it left a junk assistant turn that the host's next attempt then read as context (and which moved the KV prefix). Upstream behaviour is unchanged for any budget > 0. Upstreamable — not GIAP-specific. | `crates/goose/src/agents/agent.rs` |
| session-scoped agent goal | `Agent::goal` is a single slot and an `Agent` is shared: a host serving several conversations concurrently holds one `Arc<Agent>`, so `set_goal` is process-wide mutable state and one conversation's goal is injected into another conversation's turn as a user message. For a multi-user host that is one person's request text appearing in another person's turn. GIAP bounds concurrent chat streams with `Semaphore::new(4)` and drives all four from one retained agent, so wiring the existing goal-completeness check at all required this first. Adds `session_goals` keyed by `SessionConfig::id` (already per-reply, so it cannot race), plus `set_session_goal` / `get_session_goal`; the completeness check prefers the session goal and falls back to the process-wide one, so `set_goal` is unchanged for existing callers and `None` changes nothing. Cleared alongside `set_goal(None)` on turn exit so the map does not accumulate dead sessions. Deliberately NOT a field on `SessionConfig` — cleaner conceptually, but a required field on a public struct with 38 construction sites is a rebase burden not worth it for an additive property. Upstreamable. Commit `9cf946903`. | `crates/goose/src/agents/agent.rs`, `crates/goose/tests/agent.rs` |
| the completeness check must not reach the user, in question or in answer | TWO HALVES, one unit to re-apply. **(a) Wording.** The completeness check, the grind reminder and the `/goal` kickoff each append an INVISIBLE USER message — invisible to the person, ordinary user text to the model, and the last thing in the context before generation. All three said `**Goal:** {goal}`, bolded, with the noun repeated around it. Measured 2026-08-12 on gemma-4-E2B and E4B: the models answered the household in that vocabulary ("I could not fully meet your goal", "The goal has not been fully met"). A host prompt forbidding the jargon was already in place and did not hold — it sits thousands of tokens earlier, behind the whole tool schema, and a competing suggestion at last position beats a buried instruction. The wording is therefore the fix: no label, no Markdown emphasis, no noun a model reaches for when describing a shortfall. Behaviour unchanged — same messages, same trigger points, still demanding completion. `stop_hook_denial_context_message` is deliberately left alone: same category, no measurement covering it, and every changed line is a line to re-apply at the next rebase. Guarded parent-side by `pond-adapters-goose/src/goose_nudges.rs`, which `include_str!`s this file (that guard is NOT in `pond-core`, which must stay buildable without the submodule). **(b) Visibility, and (a) alone was not enough.** The nudge is hidden, but the model's REPLY to it is an ordinary assistant message, indistinguishable at the transport layer from a reply to the person -- so it was streamed and then CONCATENATED onto the real answer. Observed on gemma-4-E4B for "Check the weather": `It is 22 degrees Celsius and sunny in Nairobi.I used the current system context ... I cannot keep working on it as I have already provided a direct answer.` -- note the missing space, two inference outputs glued together. Wording cannot fix this (the first version leaked "goal", the reworded one leaked "keep working on it"; any phrasing leaks something, because the model is being asked a question and answering it). `goal_check_pending` already carries the fact -- set when the nudge is issued, cleared the moment tools are called -- so a round with the check pending and ZERO tool requests is by construction the check's own reply, and cannot contain anything the first answer could not have. It is no longer yielded, no longer appended to `last_assistant_text`, and kept in history as not-user-visible so strict providers still see a well-formed conversation. A round WITH tool calls stays visible: that is the check doing its job (measured 4 calls -> 7 and the real answer). Also re-anchored `test_goal_nudges_agent_before_exit`, whose needle was the pre-reword wording and had gone vacuous. Upstreamable. Commits `7849bd484`, `52a7827c2`. | `crates/goose/src/agents/agent.rs` |
| llama.cpp prompt-session KV cache | Retains one generation context per loaded model and reuses the shared prompt prefix across turns (partial KV removal to the divergence point); small non-matching prompts run in a throwaway "sacrificial" context so side calls never clobber an expensive chat prefix. Also fixes model-slot identity (cache keyed by canonicalized file PATH, not the caller's model-name spelling — several spellings of one GGUF used to evict each other with a silent full reload per generation) and holds the global runtime through a strong Arc. Adds `ProviderStats.reused_prefix_tokens`. Measured: reuse-turn TTFT 12.4s -> 0.65s, prefill 11.6s -> 66ms (M-series, ~7K-token prompt). PR upstream planned. Commit `bfd2e854b`. | `crates/goose-local-inference/src/lib.rs`, `crates/goose-local-inference/src/llamacpp/*`, `crates/goose-provider-types/src/conversation/token_usage.rs` |
| llama.cpp on-disk preamble snapshot | The retained KV cache makes the SECOND turn of a session cheap and does nothing for the first, so every cold start -- a new session, a model swap, a restart -- re-decodes a byte-identical preamble. Measured on an M-series host: 6,685 prompt tokens prefill in 24.1s at 277 tok/s, against 26-40 new sessions in a day, while the same KV state reads back from disk in a fraction of a second. `prompt_snapshot.rs` persists prefix KV state to `<data>/prompt-cache/`, with the format version in the FILE NAME so an incompatible snapshot is never opened rather than opened and rejected. Uses only `state_seq_save_file`/`state_seq_load_file` already exposed by the pinned `llama-cpp-2 =0.1.146` -- no sys-crate change. Every failure path falls back to a normal prefill, holding the engine's standing rule that a bad cache never fails a turn. **The first version kept ONE snapshot per model -- the longest run every prompt had shared since process start -- and that is not what shipped.** One stable prefix is only correct when every prompt has the same shape, and a real pond interleaves chat, the scheduler and MCP side calls, whose preambles diverge early; the single prefix collapsed toward their common ancestor and stopped being worth restoring. What ships instead is one snapshot per prompt SHAPE, in per-shape buckets, chosen by a prefix ladder: incremental FNV-1a hashes at depths [256, 512, 1024, 2048, 4096, 8192, 16384], so a candidate is rejected on integers alone before any file is read. Bounded at `MAX_SNAPSHOTS = 6` (LRU), written only between `SNAPSHOT_MIN_TOKENS = 1024` and `SNAPSHOT_MAX_TOKENS = 6144`, and a snapshot the prompts have outgrown is RETIRED -- deleted -- rather than retried on every turn forever. Writes are gated on `turn_was_cold`: the turn that already paid for a full prefill is the one that can afford to serialise it, and a warm turn must not be slowed down to fill a cache it did not need. Three bugs found by running it rather than reading it: `max_tokens` on `state_seq_load_file` is an output BUFFER CAPACITY, not a filter (passing the prompt length produced `token count in sequence state file exceeded capacity! 5341 > 5033` on ordinary turns -- it must be `ctx.n_ctx()`); a stable prefix that had never been intersected over-fit the first prompt it saw; and the module's `info!` lines were invisible because `EnvFilter` matches TARGET, not module, so they now log to an explicit `giap::kv` target that crate-level carves cannot suppress. Truncating before the write is the privacy property, not an optimisation: a KV blob encodes the tokens that produced it, and a shared prefix cannot contain anything a second prompt did not also contain -- GIAP's memories and turn context ride the user message, which diverges. Never shared BETWEEN models (KV state bakes in layer count, head dims and rope), which is the point: a model swap is the most expensive cold start there is and each keeps its own file. Loaded tokens are checked to be a genuine prefix of the prompt rather than trusted, because the key cannot see a changed system prompt or tool set. **Verified on hardware** (gemma-4-E2B-it Q4_K_M, M-series, 2026-08-16): a 1,202-token snapshot is 21.2 MiB, i.e. **18.0 KiB/token** -- which independently reproduces the 18 KiB/token measured for E2B on the Orin, so the blob really is the KV cache. Restore-plus-tail-decode 132 ms against 1,898 ms for the equivalent full prefill (**14.4x**); the restored context predicts the same argmax and the same top five, with a max logit delta of 0.007 against logits spanning ~20 -- batch-shape float noise, not divergence. Bitwise equality is NOT asserted and demanding it was the first version's mistake: the two paths batch differently by construction and llama.cpp's reduction order follows the batch shape. Live tests, all `#[ignore]`d, run with `--ignored`: `a_restored_snapshot_answers_identically_to_a_full_prefill`, `two_turns_of_one_shape_leave_a_snapshot_on_disk`, `thinking_off_does_not_change_what_the_model_is_told_about_tools`, `how_much_two_chats_share_depends_on_whether_their_tools_match` (the last measures the common ground the design rests on: 100% with identical tools, 85% when the differing tools sort last, 70% -- below the floor -- when they do not, which is why the parent orders tools core-first). They share one process through a `OnceLock` backend because `LlamaCppBackend::new()` is `unreachable!` on a second init. Upstreamable, and a natural extension of the KV cache patch above. | `crates/goose-local-inference/src/llamacpp/prompt_snapshot.rs` (new), `crates/goose-local-inference/src/llamacpp/inference_engine.rs`, `crates/goose-local-inference/src/llamacpp/mod.rs` |
| model-config failures name the session and the keys | "Could not resolve model config: missing provider" named neither which session had no stored config nor which global key was consulted, so an embedder whose own provider wiring had silently not run got an engine-internal string and no way to tell which knob was at fault. Both resolution sites (Agent reply and the platform-extensions tool-call side) now name the session id, the resolved-or-missing provider and the exact keys (GOOSE_PROVIDER / GOOSE_MODEL / goose config), in deliberately identical wording so one failure cannot read as two. Parent-side companion: pond PR #303 (session rows always written or the turn refused; GOOSE_PROVIDER exported as backstop; errored turns not re-engaged). Upstreamable. Commit `6a12584af` (cherry-picked from `b1eb61792`). | `crates/goose/src/agents/agent.rs`, `crates/goose/src/agents/platform_extensions/mod.rs` |

### Patches subsumed by upstream (dropped in the 2026-07 sync)

- **MCP session panic→warning** (`5f7dceea`) — merged upstream long ago.
- **`native_tool_calling` / `use_jinja` on `ModelSettings`** — upstream adopted the
  concept with a richer design: `tool_calling: ToolCallingMode { Auto, ForceNative,
  ForceEmulated }` and `chat_template: ChatTemplate { Embedded, Builtin, CustomInline }`.
  `Embedded` (the default) uses the GGUF's embedded Jinja template — what
  `use_jinja: true` did. GIAP code now sets `ToolCallingMode::ForceNative` where it
  used to set `native_tool_calling = true`.
- **lopdf 0.40 → 0.42 security bump** (`e8afab5c`) — upstream is on 0.42.

### rmcp

Upstream now declares `rmcp = "^1.4"`; the fork no longer diverges on rmcp at all.
The GIAP parent workspace still forces `rmcp = "=1.5.0"` so the goose crates and
`pond-mcp-server` resolve to a single rmcp version.

---

## Branch Layout

```
jarida-io/Goose
├── main                   ← CURRENT PIN: upstream 2026-07 sync + patch set above
├── giap-patches           ← legacy patch branch (pre-rmcp-1.5)
└── giap-patches-rmcp-1.5  ← previous pin; still referenced by parent `main`'s CI
```

The parent repo (`goose-in-a-pond`) pins the submodule to the tip of
`jarida-io/Goose:main`. `.gitmodules` names the branch, and
`.github/workflows/ci.yml` clones that branch's HEAD directly (bypassing the
stored submodule SHA).

**Stage breaking syncs on a side branch.** CI clones the fork branch tip by
name, so moving `main` underneath parent branches whose code still targets the
old goose API fails their fresh CI runs. Prepare and verify a sync on a
temporary fork branch, then fast-forward it into fork `main` together with the
one parent commit that carries the matching API port (`.gitmodules` + `ci.yml`
+ submodule SHA + docs). Delete the temporary branch afterwards.

### Parent workspace dependency mirror

When goose crates are built as path deps from the GIAP workspace root, cargo
resolves their `workspace = true` dependency entries against the **GIAP**
`Cargo.toml`, and goose's own `[patch.crates-io]` is ignored. After any sync:

1. Mirror new/changed keys from `goose/Cargo.toml [workspace.dependencies]` into
   the parent `Cargo.toml` (union features where GIAP needs defaults goose
   disables).
2. Where goose moved to a new MAJOR a pond crate is not ready for, keep goose's
   version in the workspace key and pin the old version directly in the pond
   crate (done for rand, sha2, thiserror, dirs, tower-http in the 2026-07 sync).
3. Replicate any `[patch.crates-io]` entries goose's build graph needs (none as
   of 2026-07 — v8/cudaforge are not in GIAP's graph).

---

## Fresh Clone Setup

```bash
git clone https://github.com/jarida-io/goose-in-a-pond
cd goose-in-a-pond
git submodule update --init --recursive
cargo build -p pond-server
```

No manual submodule surgery required. The pinned SHA must be reachable on
the current patch branch — verify with:

```bash
git -C goose branch -r --contains $(git ls-tree HEAD goose | awk '{print $3}')
# expected output:  origin/main
```

---

## Rebasing Patches Against a New Upstream Release

When you want to pull in new upstream goose commits:

```bash
cd goose

# One-time: add the upstream remote
git remote add upstream https://github.com/aaif-goose/goose

git fetch upstream main
git checkout -B sync-staging origin/main

# MERGE (not rebase) upstream — the branch history contains merge commits, and
# a merge needs no force-push. Resolve conflicts, re-porting the patch set
# above where upstream moved or redesigned the touched code.
git merge upstream/main

# Compile-gate FROM THE PARENT before pushing anything (see the dependency
# mirror section above — the parent Cargo.toml usually needs updates too):
cd .. && SQLX_OFFLINE=true cargo check -p pond-server -p pond-adapters-goose

# Fast-forward fork main, then flip .gitmodules + ci.yml + submodule SHA +
# this document in ONE parent commit.
git -C goose push origin sync-staging:main
git add goose .gitmodules .github/workflows/ci.yml docs/goose-patch-management.md
git commit -m "chore: sync goose fork with upstream (<date>)"
```

After the parent commit lands, team members must run:

```bash
git submodule update --init --recursive
```

---

## Proposed patch — `Auto` mode must still honour `NeverAllow`

**Status: NOT APPLIED.** Written up here rather than committed to the submodule
because CI clones the fork branch tip directly (see `ci.yml`), so a submodule
change that exists only in a working tree makes the local build pass and every
other build fail. It needs staging on the fork and a pointer bump, exactly as
"Adding a New GIAP Patch" below describes.

**What it buys.** GIAP narrows the tool surface per session — a Guest turn does
not get the memory tools, a dormant extension group does not get its schemas.
Today that narrowing is *schema-only*: `provider_shim.rs :: enforce_tools`
filters the `&[Tool]` slice handed to the provider, but Goose keeps every
extension loaded agent-wide, `Agent::reply` collects every `ToolRequest`
regardless of whether its schema was published, and dispatch happens
before the adapter ever sees the event. So a model that names a withheld tool
anyway still runs it. `goose_agent.rs` now tracks those calls in
`suppressed_tool_ids` and drops both the call event and its result — which
closes the disclosure (a Guest turn's withheld `recall_memories` used to stream
the household's memories back as `ToolResult { tool: "" }`) — but the tool has
still executed by then.

**Why the obvious levers do not work.**

- `ToolInspectionManager` is the seam Goose provides for exactly this, and it is
  unreachable: `Agent::tool_inspection_manager` is `pub(super)` and inspectors
  are only added inside the private `Agent::create_tool_inspection_manager`.
- Writing `PermissionLevel::NeverAllow` through `PermissionManager` — which GIAP
  already passes into `AgentConfig` — looks like it should work and does not.
  `permission_inspector.rs` matches on the mode first:

  ```rust
  let action = match goose_mode {
      GooseMode::Chat => continue,
      GooseMode::Auto => InspectionAction::Allow,   // <- returns before the check below
      GooseMode::Approve | GooseMode::SmartApprove => {
          if let Some(level) = permission_manager.get_user_permission(tool_name) {
              match level {
                  PermissionLevel::NeverAllow => InspectionAction::Deny,
                  ...
  ```

  GIAP hardcodes `GooseMode::Auto`, so the `NeverAllow` arm is unreachable, and
  moving off `Auto` would turn on approval prompts for every tool call — a
  different product.

**The patch.** Honour an explicit `NeverAllow` in `Auto` mode too. "Auto" means
*do not ask me*, not *ignore the denies I configured*; a deny the user set and
the agent ignores is the worse reading of the flag, and this is upstreamable
rather than GIAP-specific.

```rust
GooseMode::Auto => match permission_manager.get_user_permission(tool_name) {
    Some(PermissionLevel::NeverAllow) => InspectionAction::Deny,
    _ => InspectionAction::Allow,
},
```

`crates/goose/src/permission/permission_inspector.rs`. The existing
`#[test_case(GooseMode::Auto, false, None, InspectionAction::Allow; "auto_allows")]`
still passes (no stored permission); add a case asserting `Auto` + `NeverAllow`
denies.

**GIAP side, once it lands.** On each turn, write `NeverAllow` for the tools
outside the session's allow-set and `AlwaysAllow` (or clear) for those inside,
before `Agent::reply`. Note `PermissionManager` is process-global and keyed by
tool name, not by session, so with concurrent sessions of differing scope the
last writer wins — either serialise the write with the turn or upstream a
session-scoped variant. Until that is settled, `suppressed_tool_ids` in
`goose_agent.rs` is the containment, and it is a disclosure gate only.

---

## Adding a New GIAP Patch

1. Checkout `main` in the submodule: `git -C goose checkout main`
2. Make and commit your change in the submodule
3. Push: `git -C goose push origin main`
4. In the parent repo, stage and commit the updated pointer:
   ```bash
   git add goose
   git commit -m "chore: bump goose to include <patch description>"
   ```
5. Add a row to the patch table in this document.

---

## Discarding a Patch (upstreamed or no longer needed)

If a patch lands in upstream `block/goose:main`:

1. After rebasing (see above), the patch will be a no-op and `git rebase` will drop it automatically.
2. Remove its row from the patch table above.
3. Update the parent repo pointer as usual.
