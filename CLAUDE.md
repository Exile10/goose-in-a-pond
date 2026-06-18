    qw# GIAP Codebase Guide

## Agent Pipeline

### Persistence ownership

`ChatService` (`crates/pond-core/src/services/chat.rs`) is the **sole owner of turn persistence**.

Every chat turn — regardless of which engine path handles it — must go through these two methods:

| Method | When to call |
|--------|-------------|
| `persist_user_message(&str)` | Before starting the agent stream |
| `persist_assistant_turn(tool_results, text, usage, model)` | After the stream drains |

Both `/chat/stream` (`chat_stream` handler) and `/agent/chat/stream` (`agent_chat_stream` handler) build a `ChatService` and call these methods. Do not add inline persistence blocks to handlers — they will be silently dropped in future refactors.

### Engine paths

| Route | Handler | Engine |
|-------|---------|--------|
| `POST /api/v1/chat/stream` | `chat_stream` | `GooseAdapter` (via `state.agent`) |
| `POST /api/v1/agent/chat/stream` | `agent_chat_stream` | `GooseAdapter` (via `state.agent`) |

Both paths share a single authoritative history store: `pond_system.db` (`session_messages` table), readable via `GET /api/v1/sessions/{id}/messages`.

### Single source of truth

All chat history is written to and read from `pond_system.db`. Goose maintains its own `sessions.db` internally but the GIAP REST API never reads from it — `pond_system.db` is authoritative for the UI and multi-device sync.

---

## Pond-Agent Quarantine (Q2-05)

`crates/pond-agent` is compiled but **not activatable at runtime**. Setting `agent_backend = "pond"` in Settings is rejected at two layers:

1. **API** — `PUT /api/v1/settings` with `agent_backend: "pond"` returns HTTP 422.
2. **Startup** — `serve()` in `main.rs` overrides `"pond"` → `"goose"` with a `tracing::warn!` before any backend is wired.

Do not remove either guard without a dedicated stabilisation milestone. The code is preserved so it compiles and can be enabled when the engine is production-ready.

---

## Goose Submodule

See `docs/goose-patch-management.md` for the patch set carried on top of upstream (`aaif-goose/goose`) and the upstream-rebase procedure.

The submodule is pinned to `jarida-io/Goose:giap-patches`. After any force-push to that branch, teammates must run:

```bash
git submodule update --init --recursive
```
