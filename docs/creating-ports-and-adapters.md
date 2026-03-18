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

## 4. Create the Goose Adapter

Create the adapter in `pond-adapters-goose/src/`.

**File:** `crates/pond-adapters-goose/src/<name>_adapter.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;
use pond_core::domain::your_types::YourDomainType;
use pond_core::ports::your_port::YourPort;
use std::sync::Arc;

// Import the Goose type you're wrapping
use goose::some_module::GooseThing;

pub struct GooseYourPortAdapter {
    inner: Arc<GooseThing>,
}

impl GooseYourPortAdapter {
    pub fn new(inner: Arc<GooseThing>) -> Self {
        Self { inner }
    }

    // Conversion: pond domain → Goose
    fn to_goose(input: &YourDomainType) -> GooseInput { /* ... */ }

    // Conversion: Goose → pond domain
    fn from_goose(output: &GooseOutput) -> YourDomainType { /* ... */ }
}

#[async_trait]
impl YourPort for GooseYourPortAdapter {
    async fn do_something(&self, input: YourDomainType) -> Result<YourDomainType> {
        let goose_input = Self::to_goose(&input);
        let goose_output = self.inner.goose_method(goose_input).await?;
        Ok(Self::from_goose(&goose_output))
    }
    fn name(&self) -> String { "goose".to_string() }
}
```

Register:

```rust
// crates/pond-adapters-goose/src/lib.rs
pub mod your_port_adapter;
pub use your_port_adapter::GooseYourPortAdapter;
```

---

## 5. Wire Into the Service Layer

Inject the port in `ChatService` or create a new service:

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

| Port | Domain Types | Mock | Goose Adapter |
|---|---|---|---|
| `Agent` | `AgentRequest`, `AgentResponse` | `MockAgent` | `GooseAdapter` |
| `LlmProvider` | `ChatMessage`, `Role` | `MockProvider` | `GooseProviderAdapter` |
| `Storage` | *(empty)* | — | — |

---

## Key Principles

1. **Domain types are adapter-agnostic** — never import `goose::*` in `pond-core`
2. **Adapters convert at the boundary** — `to_goose()` / `from_goose()` methods
3. **Always create a mock first** — enables testing without Goose compilation
4. **One port per service** — don't merge unrelated capabilities
5. **Port traits are `Send + Sync`** — required for async multi-threaded use
