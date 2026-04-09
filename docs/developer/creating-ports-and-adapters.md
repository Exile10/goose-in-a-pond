# Creating Ports and Adapters in Goose-in-a-Pond

This guide documents how to create a new port (trait) in `pond-core` and its
corresponding adapter in `pond-adapters-goose`. Follow these steps to keep the
hexagonal architecture consistent.

---

## 1. Define Domain Types

Create the types that your port needs in `pond-core/src/domain/`.

**File:** `crates/pond-core/src/domain/<your_types>.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YourDomainType {
    pub field: String,
}
```

Then register the module:

```rust
// crates/pond-core/src/domain/mod.rs
pub mod your_types;
```

> **Rule:** Domain types must NOT import from `goose::*`. They belong to the
> pond domain and must be adapter-agnostic.

---

## 2. Define the Port Trait

Create the trait in `pond-core/src/ports/`.

**File:** `crates/pond-core/src/ports/<your_port>.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::domain::your_types::YourDomainType;

/// Driven Port: <PortName>
///
/// Describe what this port abstracts.
#[async_trait]
pub trait YourPort: Send + Sync {
    async fn do_something(&self, input: YourDomainType) -> Result<YourDomainType>;
    fn name(&self) -> String;
}
```

Register:

```rust
// crates/pond-core/src/ports/mod.rs
pub mod your_port;
```

---

## 3. Create a Mock Implementation

Create a mock for testing in `pond-core/src/services/`.

**File:** `crates/pond-core/src/services/mock_<name>.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::domain::your_types::YourDomainType;
use crate::ports::your_port::YourPort;

pub struct MockYourPort;

impl MockYourPort {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl YourPort for MockYourPort {
    async fn do_something(&self, input: YourDomainType) -> Result<YourDomainType> {
        Ok(YourDomainType { field: format!("Mock: {}", input.field) })
    }
    fn name(&self) -> String { "mock".to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock() {
        let mock = MockYourPort::new();
        let result = mock.do_something(YourDomainType { field: "hi".into() }).await.unwrap();
        assert_eq!(result.field, "Mock: hi");
    }
}
```

Register:

```rust
// crates/pond-core/src/services/mod.rs
pub mod mock_your_port;
```

---

## 4. Create the Adapter

Adapters live in the crate that matches their dependency:

| Adapter type | Crate |
|---|---|
| Wraps Goose built-in types | `pond-adapters-goose/src/` |
| Pure HTTP (no Goose dep) | `pond-adapters-<name>/src/` or `pond-server/src/` |
| SQLite persistence | `pond-infra/src/` |

**File:** `crates/pond-adapters-goose/src/<name>_adapter.rs` (example: Goose-backed)

```rust
use anyhow::Result;
use async_trait::async_trait;
use pond_core::domain::your_types::YourDomainType;
use pond_core::ports::your_port::YourPort;
use std::sync::Arc;

use goose::some_module::GooseThing;

pub struct GooseYourPortAdapter {
    inner: Arc<GooseThing>,
}

impl GooseYourPortAdapter {
    pub fn new(inner: Arc<GooseThing>) -> Self { Self { inner } }
    fn to_goose(input: &YourDomainType) -> GooseInput { /* ... */ }
    fn from_goose(output: &GooseOutput) -> YourDomainType { /* ... */ }
}

#[async_trait]
impl YourPort for GooseYourPortAdapter {
    async fn do_something(&self, input: YourDomainType) -> Result<YourDomainType> {
        let out = self.inner.goose_method(Self::to_goose(&input)).await?;
        Ok(Self::from_goose(&out))
    }
    fn name(&self) -> String { "goose".to_string() }
}
```

> **Workspace exclusion rule:** `pond-adapters-goose` and any crate importing `goose::*` must stay in `workspace.exclude` in the root `Cargo.toml`. See `CLAUDE.md` for details.

---

## 5. Wire Into the Service Layer

Inject the port in `ChatService` or create a new service:

```rust
pub struct YourService {
    port: Arc<dyn YourPort>,
}

impl YourService {
    pub fn new(port: Arc<dyn YourPort>) -> Self {
        Self { port }
    }
}
```

Wire in `pond-server/src/main.rs`:

```rust
let port = Arc::new(MockYourPort::new());  // or GooseYourPortAdapter
let service = YourService::new(port);
```

---

## 6. Verify

```bash
# Unit tests
cargo test -p pond-core

# Full workspace build
cargo build

# Run the server
cargo run -p pond-server
```

---

## Reference: Existing Ports

| Port | Domain Types | Mock | Adapters |
|---|---|---|---|
| `Agent` | `AgentRequest`, `AgentResponse` | `MockAgent` | `GooseAdapter` (pond-adapters-goose) |
| `LlmProvider` | `ChatMessage`, `Role` | `MockProvider`, `FallbackProvider` | `GooseProviderAdapter`, `LlamafileProvider`, `LocalInferenceProvider` |
| `SessionStorage` | `ChatSession`, `ChatMessage` | `InMemorySessionStorage` | `SqliteSessionStorage` (pond-infra), `GooseSessionAdapter` (pond-adapters-goose) |
| `VoiceInput` | — | `StdinInput` | `WhisperInput` (pond-adapters-whisper) |
| `VoiceOutput` | — | `PrintOutput` | `PiperOutput` (pond-adapters-piper), `QwenTtsOutput` (pond-adapters-qwen-tts) |
| `WakeWordDetector` | — | `InstantActivation` | `WhisperKeywordDetector` (pond-adapters-whisper) |
| `OnboardingRepository` | `OnboardingStep` | *(inline in tests)* | `SqlxOnboardingRepository` (pond-infra) |
| `DeviceRegistry` | `Device`, `RegisterDeviceRequest` | *(inline in tests)* | `SqliteDeviceRegistry` (pond-infra) |
| `Handshake` | — | `MockHandshake` | — |
| `SettingsRepository` | `Settings` | `MockSettingsRepository` | `SqliteSettingsRepository` (pond-infra) |
| `SchedulerPort` | `ScheduledTask`, `CreateTaskRequest` | — | `CronSchedulerAdapter` (pond-infra-scheduler) |
| `McpMemoryPort` | — | — | `GooseMcpMemoryAdapter` (pond-adapters-mcp-memory) |
| `ExtensionManagerPort` | — | — | `GiapGooseExtensionManager` (pond-adapters-goose) |
| `ModelRepository` | `ModelRecord`, `ModelRoleAssignment` | *(inline in tests)* | `SqliteModelRepository` (pond-infra) |
| `ModelCatalogProvider` | `ModelRecord`, `BinaryRecord` | — | `HttpModelCatalogProvider` (pond-server) |
| `ModelDownloader` | — | — | `HttpModelDownloader` (pond-server) |
| `ModelStorage` | — | — | `FilesystemModelStorage` (pond-server) |
| `ModelScheduler` | `DownloadStatus` | — | *(not yet implemented)* |
| `WeatherProvider` | `WeatherData` | — | `OpenMeteoWeatherAdapter` (pond-adapters-weather) |
| `SensorStorage` | `SensorReading` | `MockSensorStorage` | `SqliteSensorStorage` (pond-infra) |
| `CameraStorage` | `CameraImage` | `MockCameraStorage` | `SqliteCameraStorage` (pond-infra) |
| `ProfileRepository` | `Profile` | `MockProfileRepository` | *(SQLite impl in pond-infra)* |
| `MemoryRepository` | — | `MockMemoryRepository` | *(SQLite impl in pond-infra)* |

---

## Key Principles

1. **Domain types are adapter-agnostic** — never import `goose::*` in `pond-core`
2. **Adapters convert at the boundary** — `to_goose()` / `from_goose()` methods
3. **Always create a mock first** — enables testing without Goose compilation
4. **One port per service** — don't merge unrelated capabilities
5. **Port traits are `Send + Sync`** — required for async multi-threaded use
