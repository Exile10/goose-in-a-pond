# Creating Ports and Adapters

This guide shows how to add a new capability to GIAP while keeping the hexagonal architecture intact. Follow these steps in order — the mock must exist and pass tests before any real adapter is written.

---

## The Five Steps

```
0. Pick a quadrant  (user_data | models | mcp | security | shared)
1. Domain types  (pond-core/src/<quadrant>/domain/)
2. Port trait    (pond-core/src/<quadrant>/ports/)
3. Mock + tests  (pond-core/src/<quadrant>/mocks/)
4. Real adapter  (crates/pond-adapters-<name>/)
5. Wire          (pond-server/src/main.rs)
```

> The example below adds a `notification` capability, which belongs to the
> `mcp` quadrant (it is part of the tool/extension surface). Substitute your own
> quadrant — `user_data`, `models`, `mcp`, `security`, or `shared` — in the
> paths that follow.

---

## Step 1 — Domain Types

Create the pure Rust types your port will use. No external imports allowed.

**File:** `crates/pond-core/src/mcp/domain/notification.rs`  *(`<quadrant>/domain/<name>.rs`)*

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationMessage {
    pub title: String,
    pub body: String,
    pub priority: NotificationPriority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationPriority { Low, Normal, High }
```

Register in `crates/pond-core/src/mcp/domain/mod.rs`:
```rust
pub mod notification;
```

> **Rule:** Domain types must never import from `goose::*`, `sqlx::*`, `reqwest::*`, or any external framework.

---

## Step 2 — Port Trait

Define the interface the Core will use. One trait per capability.

**File:** `crates/pond-core/src/mcp/ports/notification.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::mcp::domain::notification::NotificationMessage;

/// Driven Port: push notification delivery.
///
/// Implemented by adapters that send alerts to connected clients
/// (e.g. GOTG mobile app via FCM, or a local desktop notification).
#[async_trait]
pub trait NotificationSender: Send + Sync {
    /// Send a push notification. Returns Ok(()) on delivery, Err on failure.
    async fn send(&self, msg: NotificationMessage) -> Result<()>;
}
```

Register in `crates/pond-core/src/mcp/ports/mod.rs`:
```rust
pub mod notification;
```

---

## Step 3 — Mock Implementation and Tests

Write the mock before anything else. This is the test double used by all Core tests.

**File:** `crates/pond-core/src/mcp/mocks/mock_notification.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;
use crate::mcp::domain::notification::NotificationMessage;
use crate::mcp::ports::notification::NotificationSender;

/// Test double — captures sent notifications for assertion.
pub struct MockNotificationSender {
    sent: Mutex<Vec<NotificationMessage>>,
}

impl MockNotificationSender {
    pub fn new() -> Self {
        Self { sent: Mutex::new(vec![]) }
    }

    pub fn sent(&self) -> Vec<NotificationMessage> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl NotificationSender for MockNotificationSender {
    async fn send(&self, msg: NotificationMessage) -> Result<()> {
        self.sent.lock().unwrap().push(msg);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::domain::notification::NotificationPriority;

    #[tokio::test]
    async fn send_captures_message() {
        let sender = MockNotificationSender::new();
        sender.send(NotificationMessage {
            title: "Alert".into(),
            body: "Motion detected".into(),
            priority: NotificationPriority::High,
        }).await.unwrap();

        let sent = sender.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].title, "Alert");
    }

    #[tokio::test]
    async fn trait_object_conformance() {
        use std::sync::Arc;
        let _: Arc<dyn NotificationSender> = Arc::new(MockNotificationSender::new());
    }
}
```

Register in `crates/pond-core/src/mcp/mocks/mod.rs` (gated `#[cfg(any(test, feature = "test-mocks"))]`):
```rust
pub mod mock_notification;
```

Run tests — they must pass before continuing:
```bash
cargo test -p pond-core -- notification
```

---

## Step 4 — Real Adapter

Create a new crate (or add to an existing adapter crate if it's closely related).

**`crates/pond-adapters-gotg/Cargo.toml`**
```toml
[package]
name = "pond-adapters-gotg"
version.workspace = true
edition.workspace = true

[dependencies]
pond-core   = { workspace = true }
anyhow      = { workspace = true }
async-trait = { workspace = true }
reqwest     = { workspace = true, features = ["json"] }
serde_json  = { workspace = true }

[dev-dependencies]
tokio    = { workspace = true, features = ["rt-multi-thread", "macros"] }
wiremock = { workspace = true }
```

**`crates/pond-adapters-gotg/src/notification_adapter.rs`**
```rust
use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::domain::notification::NotificationMessage;
use pond_core::mcp::ports::notification::NotificationSender;

pub struct GotgNotificationAdapter {
    endpoint: String,
    client: reqwest::Client,
}

impl GotgNotificationAdapter {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

#[async_trait]
impl NotificationSender for GotgNotificationAdapter {
    async fn send(&self, msg: NotificationMessage) -> Result<()> {
        self.client
            .post(&self.endpoint)
            .json(&serde_json::json!({
                "title": msg.title,
                "body":  msg.body,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}
```

**Integration tests — `crates/pond-adapters-gotg/tests/integration.rs`**
```rust
use pond_adapters_gotg::GotgNotificationAdapter;
use pond_core::mcp::domain::notification::{NotificationMessage, NotificationPriority};
use pond_core::mcp::ports::notification::NotificationSender;
use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{method, path};

#[tokio::test]
async fn send_posts_to_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/notify"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server).await;

    let adapter = GotgNotificationAdapter::new(&format!("{}/notify", server.uri()));
    adapter.send(NotificationMessage {
        title: "Test".into(),
        body: "Hello".into(),
        priority: NotificationPriority::Normal,
    }).await.unwrap();

    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn server_error_propagates() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/notify"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server).await;

    let adapter = GotgNotificationAdapter::new(&format!("{}/notify", server.uri()));
    let result = adapter.send(NotificationMessage {
        title: "Test".into(), body: "Hello".into(),
        priority: NotificationPriority::Normal,
    }).await;

    assert!(result.is_err(), "expected Err on HTTP 500");
}

#[test]
fn trait_object_conformance() {
    use std::sync::Arc;
    let _: Arc<dyn NotificationSender> =
        Arc::new(GotgNotificationAdapter::new("http://localhost:1234/notify"));
}
```

---

## Step 5 — Wire Into pond-server

**`crates/pond-server/src/main.rs`**
```rust
use pond_adapters_gotg::GotgNotificationAdapter;

// In the AppState construction block:
let notification_sender: Arc<dyn NotificationSender> = Arc::new(
    GotgNotificationAdapter::new("http://gotg-device:8080/notify")
);

let state = Arc::new(AppState {
    // ...
    notification_sender,
});
```

---

## Workspace Note for Goose-dependent Crates

If your adapter imports from `goose::*`, it must be listed in `workspace.exclude` in the root `Cargo.toml` and use `default-features = false` on the `goose` dependency. See `CLAUDE.md` for the full explanation of the `rmcp` version conflict.

---

## Existing Ports Reference

| Port | File | Mock | Adapter(s) |
|---|---|---|---|
| `Agent` | `ports/agent.rs` | `MockAgent` | `GooseAdapter` |
| `LlmProvider` | `ports/provider.rs` | `MockProvider` | `GooseProviderAdapter`, `LlamafileProvider`, `OllamaProvider` |
| `SessionStorage` | `ports/session_storage.rs` | `InMemorySessionStorage` | `SqliteSessionStorage`, `GooseSessionAdapter` |
| `VoiceInput` | `ports/voice_input.rs` | `StdinInput` | `WhisperInput` |
| `VoiceOutput` | `ports/voice_output.rs` | `PrintOutput` | `PiperOutput` |
| `WakeWordDetector` | `ports/wake_word.rs` | `InstantActivation` | `WhisperKeywordDetector` |
| `DeviceRegistry` | `ports/device_registry.rs` | — | `SqliteDeviceRegistry` |
| `SettingsRepository` | `ports/settings.rs` | `MockSettingsRepository` | `SqliteSettingsRepository` |
| `MemoryRepository` | `ports/memory_repository.rs` | `MockMemoryRepository` | `SqliteMemoryRepository` |
| `UserSkillRepository` | `ports/skill.rs` | `MockSkillRepository` | `SqliteSkillRepository` |
| `PromptTemplateRepository` | `ports/prompt_template.rs` | `MockPromptTemplateRepository` | `SqlitePromptTemplateRepository` |
| `SchedulerPort` | `ports/scheduler.rs` | — | `CronSchedulerAdapter` |
| `NotificationSender` | `ports/notification.rs` | — | `GotgNotificationAdapter` |
| `McpMemoryPort` | `ports/mcp_memory.rs` | — | `GooseMcpMemoryAdapter` |

---

## Key Principles

1. **Domain types are framework-agnostic** — never import `goose::*` in `pond-core`
2. **Mock first, adapt second** — tests must pass on the mock before the real adapter exists
3. **Adapters translate at the boundary** — conversion logic belongs in the adapter, not the core
4. **One port per capability** — don't bundle unrelated operations into one trait
5. **All port traits are `Send + Sync`** — required for `Arc<dyn Port>` in async code
6. **Integration tests cover error paths** — happy path + HTTP 500 + connection refused minimum
