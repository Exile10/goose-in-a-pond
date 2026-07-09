# Goose Submodule Patch Management

## Overview

GIAP pins a fork of [aaif-goose/goose](https://github.com/aaif-goose/goose) as a git submodule at
`goose/`. The fork lives at `https://github.com/jarida-io/Goose` and carries a small set of
patches on top of upstream. This document describes those patches, the branch strategy, and
how to rebase against a new upstream goose release.

---

## GIAP Patch Set

### Branch: `giap-patches`

`jarida-io/goose:giap-patches` = upstream goose `main` + the commits listed below.

| Commit | Description | Files |
|--------|-------------|-------|
| `5f7dceea` | Replace MCP session panic with warning to allow session switching | `crates/goose/src/agents/mcp_client.rs` |
| GIAP-local | Add `native_tool_calling` and `use_jinja` fields to `ModelSettings` | `crates/goose/src/providers/local_inference/local_model_registry.rs` |
| GIAP-local | OR-merge `native_tool_calling` flag in local inference provider | `crates/goose/src/providers/local_inference/local_inference.rs` |

### Why these patches exist

- The MCP session panic fix prevents a hard crash when users switch MCP sessions mid-conversation. Upstream has not merged it yet.
- The `native_tool_calling` / `use_jinja` fields are required for Gemma 4 on Apple Silicon and Jetson — without them the model never sees tools in its trained format and produces immediate EOS. These are GIAP-specific patches; do not drop them during a rebase.

---

## RMCP 1.5 Patch Set

### Branch: `giap-patches-rmcp-1.5`

The `giap-patches-rmcp-1.5` branch carries the GIAP patches against the rmcp 1.5
line. In addition to the base `giap-patches` set above, it includes:

| Commit | Description | Files |
|--------|-------------|-------|
| `d52efa2e` | Retry the turn tool-less when an Ollama model rejects tools with HTTP 400 ("does not support tools"). Adds `is_tools_unsupported_error` + splits `OllamaProvider::stream` into `stream` (catch/retry) and `stream_inner` (single attempt), so non-tool models (e.g. `gemma3:4b`) answer as plain chat models instead of aborting the turn. | `crates/goose/src/providers/ollama.rs` |

This fix is model-agnostic (matches on the error text) and lives entirely inside
the Ollama provider, so both the `/chat/stream` and `/agent/chat/stream` GIAP
paths get it for free.

---

## Branch Layout

```
jarida-io/goose
├── main          ← mirrors upstream block/goose main + accumulated GIAP work
└── giap-patches  ← main + any patches not yet merged into main
```

The parent repo (`goose-in-a-pond`) pins the submodule to the tip of `giap-patches`.
`.gitmodules` specifies `branch = giap-patches` so `git submodule update --remote` tracks
the right branch.

---

## Fresh Clone Setup

```bash
git clone https://github.com/jarida-io/goose-in-a-pond
cd goose-in-a-pond
git submodule update --init --recursive
cargo build -p pond-server
```

No manual submodule surgery required. The pinned SHA must be reachable on
`origin/giap-patches` — verify with:

```bash
git -C goose branch -r --contains $(git ls-tree HEAD goose | awk '{print $3}')
# expected output:  origin/giap-patches
```

---

## Rebasing Patches Against a New Upstream Release

When you want to pull in new upstream goose commits:

```bash
cd goose

# One-time: add the upstream remote
git remote add upstream https://github.com/aaif-goose/goose

git fetch upstream
git checkout giap-patches

# Rebase GIAP patches on top of the new upstream main
git rebase upstream/main

# Resolve any conflicts, then push
git push --force-with-lease origin giap-patches

cd ..

# Update the parent repo's submodule pointer
git add goose
git commit -m "chore: rebase goose giap-patches onto upstream/main $(date +%Y-%m-%d)"
```

After force-pushing `giap-patches`, all team members must run:

```bash
git submodule update --init --recursive
```

---

## Adding a New GIAP Patch

1. Checkout `giap-patches` in the submodule: `git -C goose checkout giap-patches`
2. Make and commit your change in the submodule
3. Push: `git -C goose push origin giap-patches`
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
