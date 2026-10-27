# pond-core

The domain. Pure Rust: no Axum, no SQLx, no Goose, no network clients. Every other crate depends
inward on this one; it depends on none of them.

## Layout

The crate is split by bounded context. Most contexts have the same four folders (`context/` and
`rides/` are flat modules):

| Folder | Holds |
|---|---|
| `domain/` | Types and pure rules |
| `ports/` | `async_trait` traits that adapters implement |
| `services/` | Logic that composes ports |
| `mocks/` | In-memory port implementations for tests |

| Context | Covers |
|---|---|
| `shared/` | `ChatService` (`shared/services/chat.rs`, the sole owner of turn persistence), the event bus, egress policy, turn authority, delegation |
| `models/` | Model catalogue, roles, engines, device profile and memory budget (`POND_DEVICE_PROFILE` emulation), and the inference, embedding, voice and audio-device ports |
| `mcp/` | Tools, extensions and the extension marketplace, notifications |
| `user_data/` | Memory and its batch extraction (`BatchExtractionService`), profile, settings, skills, prompts, recipes, schedules, reminders, devices, sensors, onboarding |
| `security/` | Secrets, device handshake, remote access, policy, drafts, redaction, audit and telemetry |
| `context/` | Personal-context ingest, chunking, vector index, retrieval and retention |
| `network/` | Setup codes and the setup channel, Wi-Fi and wired interfaces, service advertising |
| `mesh/` | Private mesh between ponds: peers, capabilities, credit ledger, payment rail |
| `rides/` | Ride booking and tracking |

`prompts.rs` builds the system prompt, taking the first that exists: `$DATA_DIR/prompts/system.md`,
`Settings.custom_system_prompt`, the database template, the built-in for `Settings.prompt_style`, then
`SYSTEM_PROMPT`.

## Tests

Unit tests sit next to the code and use the `mocks/`. The files in `tests/` are mostly guards that
read source as text (for example `egress_guard.rs`, `context_pipeline_is_wired.rs`,
`every_pai_row_states_its_verification.rs`): they fail when a wiring or policy invariant is broken
elsewhere in the workspace. Run them after editing comments too.

```bash
cargo test -p pond-core
```
