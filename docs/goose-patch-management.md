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
| llama.cpp prompt-session KV cache | Retains one generation context per loaded model and reuses the shared prompt prefix across turns (partial KV removal to the divergence point); small non-matching prompts run in a throwaway "sacrificial" context so side calls never clobber an expensive chat prefix. Also fixes model-slot identity (cache keyed by canonicalized file PATH, not the caller's model-name spelling — several spellings of one GGUF used to evict each other with a silent full reload per generation) and holds the global runtime through a strong Arc. Adds `ProviderStats.reused_prefix_tokens`. Measured: reuse-turn TTFT 12.4s -> 0.65s, prefill 11.6s -> 66ms (M-series, ~7K-token prompt). PR upstream planned. Commit `bfd2e854b`. | `crates/goose-local-inference/src/lib.rs`, `crates/goose-local-inference/src/llamacpp/*`, `crates/goose-provider-types/src/conversation/token_usage.rs` |

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
