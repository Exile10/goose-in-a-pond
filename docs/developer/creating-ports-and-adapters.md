# Creating Ports and Adapters

How to add a capability without breaking the hexagonal split: the core defines what it needs as a
trait (a port), and an adapter crate implements it. The mock exists and passes its tests before
the real adapter is written.

```
0. Pick a bounded context   crates/pond-core/src/<context>/
1. Domain types             <context>/domain/
2. Port trait               <context>/ports/
3. Mock + tests             <context>/mocks/
4. Real adapter             crates/pond-adapters-<name>/  (or pond-infra for SQLite)
5. Wire                     crates/pond-server/src/main.rs
```

## 0. Pick a bounded context

| Context | Holds |
|---|---|
| `models` | agents, LLM providers, inference, embeddings, voice in and out, wake word, model catalogue |
| `mcp` | the tool surface: extensions, tool registry and dispatch, notifications |
| `user_data` | household data: sessions, settings, memories, skills, devices, profiles, schedules, sensors |
| `security` | pairing (handshake), secrets, policy, event log, redaction, remote access |
| `shared` | agent-loop plumbing: `ChatService`, event bus, orchestrator |
| `mesh` | pond-to-pond inference and credit |
| `network` | setup code, setup radios, Wi-Fi, service advertising |
| `context` | the context index: ingest, chunking, vector index, retrieval |
| `rides` | ride booking and tracking |

Most contexts have `domain/`, `ports/`, `services/` and `mocks/` directories; `context` and
`rides` keep `domain.rs`, `ports.rs` and `mocks.rs` as single files. The example below adds a
fictional `ParcelTracker` to `user_data`.

## 1. Domain types

Plain Rust types. `pond-core` imports no framework: no `goose`, `sqlx`, `axum`, `reqwest`.

`crates/pond-core/src/user_data/domain/parcel.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parcel {
    pub tracking_number: String,
    pub status: ParcelStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParcelStatus { InTransit, OutForDelivery, Delivered }
```

Add `pub mod parcel;` to `user_data/domain/mod.rs`.

## 2. Port trait

One trait per capability, `Send + Sync` so it can live in an `Arc<dyn …>`.

`crates/pond-core/src/user_data/ports/parcel.rs`:

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::user_data::domain::parcel::Parcel;

/// Looks up a parcel's delivery status with its carrier.
#[async_trait]
pub trait ParcelTracker: Send + Sync {
    async fn track(&self, tracking_number: &str) -> Result<Parcel>;
}
```

Add `pub mod parcel;` to `user_data/ports/mod.rs`.

## 3. Mock and tests

`crates/pond-core/src/user_data/mocks/mock_parcel.rs`:

```rust
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;
use crate::user_data::domain::parcel::{Parcel, ParcelStatus};
use crate::user_data::ports::parcel::ParcelTracker;

/// Answers every lookup as in transit and records what was asked.
#[derive(Default)]
pub struct MockParcelTracker {
    pub asked: Mutex<Vec<String>>,
}

#[async_trait]
impl ParcelTracker for MockParcelTracker {
    async fn track(&self, tracking_number: &str) -> Result<Parcel> {
        self.asked.lock().unwrap().push(tracking_number.to_string());
        Ok(Parcel { tracking_number: tracking_number.into(), status: ParcelStatus::InTransit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn records_each_lookup() {
        let mock = MockParcelTracker::default();
        mock.track("AB123").await.unwrap();
        assert_eq!(*mock.asked.lock().unwrap(), vec!["AB123".to_string()]);
    }

    #[test]
    fn works_as_a_trait_object() {
        let _: Arc<dyn ParcelTracker> = Arc::new(MockParcelTracker::default());
    }
}
```

Add `pub mod mock_parcel;` to `user_data/mocks/mod.rs`. The `mocks` module is compiled under
`#[cfg(any(test, feature = "test-mocks"))]`, so other crates' tests reach it by enabling
`pond-core`'s `test-mocks` feature.

```bash
cargo test -p pond-core parcel
```

If a service in `pond-core` uses the port, write its tests against the mock now, before any
adapter exists.

## 4. Real adapter

A new crate under `crates/` (add it to `members` in the root `Cargo.toml`), or an existing
adapter crate when the capability is closely related. SQLite implementations live in
`pond-infra`.

`crates/pond-adapters-parcels/Cargo.toml`:

```toml
[package]
name = "pond-adapters-parcels"
version.workspace = true
edition.workspace = true

[dependencies]
pond-core   = { path = "../pond-core" }
pond-net    = { path = "../pond-net" }
anyhow      = { workspace = true }
async-trait = { workspace = true }
reqwest     = { workspace = true }
serde_json  = { workspace = true }

[dev-dependencies]
tokio    = { workspace = true }
wiremock = { workspace = true }
```

Rules for the adapter:

- **HTTP clients come from `pond_net::client()` or `pond_net::client_builder()`.**
  `crates/pond-core/tests/egress_guard.rs` fails the build when a crate builds a `reqwest` client
  any other way, and requires every file that sends HTTP to be classified: either it reports the
  request to the egress tracker (`check_egress`, `record_egress`, `traced_send`, …) or it is
  loopback-only. Add the new file to the right list there.
- **Convert at the boundary.** Translate the external API's types into the domain types inside
  the adapter; nothing from the external API leaks into `pond-core`.
- **Tests:** a `wiremock` happy path, the error paths (HTTP 500, connection refused), trait-object
  use, and an `#[ignore = "<what it needs>"]` live test against the real service. See the
  [testing guide](../testing/tdd_guide.md).

A crate that depends on Goose (`goose` with `default-features = false`, never `code-mode`) is an
ordinary workspace member but must stay off the fast-crate lists in
`.github/workflows/ci.yml`, which prove the rest of the workspace builds without Goose.

## 5. Wire into pond-server

`crates/pond-server/src/main.rs` constructs every adapter and hands it to the services or to
`pond_api::AppState` (`crates/pond-api/src/lib.rs`). Add the field to `AppState`, then to
`pond_api::test_support::app_state()` so the API tests keep compiling.

If handlers reach the port as an axum `Extension` instead, attach it in
`listeners::compose()` (`crates/pond-server/src/listeners.rs`) on the listener that should serve
it, and add an assertion to `crates/pond-server/tests/listener_wiring.rs` that the other
listeners do not. Which listener carries an extension is a security decision: the loopback
dashboard, the HTTPS companion and remote access serve different callers.

A change to wiring, routes or migrations needs `scripts/live-test.sh` before it is done.

If the model should be able to use the capability, it also needs an MCP tool in
`pond-mcp-server`; see the [MCP server standard](mcp-server-standard.md).

## Existing ports

There are about a hundred port traits; `grep -rn "^pub trait" crates/pond-core/src` lists them.
The most used:

| Port | File (under `crates/pond-core/src/`) | Mock | Adapters |
|---|---|---|---|
| `Agent` | `models/ports/agent.rs` | `MockAgent` | `GooseAdapter` (pond-adapters-goose), `MistralRsAgent` (pond-adapters-mistralrs), `PondAgent` (quarantined) |
| `LlmProvider` | `models/ports/provider.rs` | `MockProvider` | `GooseProviderAdapter`, `OllamaProvider`, `LlamafileProvider`, `LocalInferenceLlmAdapter`, `FallbackProvider` |
| `EmbeddingProvider` | `models/ports/embedding.rs` | `MockEmbeddingProvider` | `GgufEmbeddingProvider` (pond-inference), `FastembedEmbeddingProvider` (pond-infra) |
| `VoiceInput` | `models/ports/voice_input.rs` | `StdinInput` | `WhisperRsInput` (pond-adapters-whisper) |
| `VoiceOutput` | `models/ports/voice_output.rs` | `PrintOutput` | `KokoroOutput` (pond-adapters-kokoro) |
| `StreamingWakeWordDetector` | `models/ports/wake_word.rs` | `InstantActivation` | `WhisperKeywordDetector` |
| `TtsControl` | `models/ports/tts_control.rs` | `MockTtsControl` | `KokoroTtsControl` (pond-server) |
| `ModelRepository` | `models/ports/model_repository.rs` | `MockModelRepository` | `SqliteModelRepository` (pond-infra) |
| `SessionStorage` | `user_data/ports/session_storage.rs` | `InMemorySessionStorage` | `SqliteSessionStorage` (pond-infra) |
| `SettingsRepository` | `user_data/ports/settings.rs` | `MockSettingsRepository` | `SqliteSettingsRepository` (pond-infra) |
| `MemoryRepository` | `user_data/ports/memory_repository.rs` | `MockMemoryRepository` | `SqliteMemoryRepository` (pond-infra) |
| `UserSkillRepository` | `user_data/ports/skill.rs` | `MockSkillRepository` | `SqliteSkillRepository` (pond-infra) |
| `PromptTemplateRepository` | `user_data/ports/prompt_template.rs` | `MockPromptTemplateRepository` | `SqlitePromptTemplateRepository` (pond-infra) |
| `DeviceRegistry` | `user_data/ports/device_registry.rs` | `MockDeviceRegistry` | `SqliteDeviceRegistry` (pond-infra) |
| `SchedulerPort` | `user_data/ports/scheduler.rs` | none | `CronSchedulerAdapter` (pond-infra-scheduler) |
| `Handshake` | `security/ports/handshake.rs` | `MockHandshake` (pond-infra) | `SqliteHandshakeAdapter` (pond-infra) |
| `SecretRepository` | `security/ports/secret.rs` | none | `FileSecretRepository` (pond-infra) |
| `EventLog` | `security/ports/event_log.rs` | `MockEventLog` | `SqliteEventLog` (pond-infra) |
| `ExtensionManagerPort` | `mcp/ports/extension_manager.rs` | none | `GiapGooseExtensionManager` (pond-adapters-goose) |
| `NotificationSender` | `mcp/ports/notification.rs` | none | `BroadcastNotificationSender` (pond-infra) |
| `McpKnowledgePort` | `mcp/ports/mcp_knowledge.rs` | none | `GooseMcpMemoryAdapter` (pond-adapters-mcp-memory) |
| `EventBus` | `shared/ports/event_bus.rs` | none | `InProcessEventBus` (`shared/services/`) |

A few ports live in their adapter crate rather than `pond-core`: `WeatherProvider` is defined in
`pond-adapters-weather` beside its implementation, `OpenMeteoWeatherAdapter`.

## Principles

1. Domain types and ports are framework-free; `pond-core` never imports `goose`.
2. Mock first, adapter second: the mock's tests pass before the adapter exists.
3. Adapters translate at the boundary.
4. One port per capability.
5. Ports are `Send + Sync`.
6. Adapter tests cover the error paths, not just the happy path.
