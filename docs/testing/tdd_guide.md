# TDD Guide: Test-Driven Development in the Pond

We use TDD to ensure that our **Core** logic remains pure and that our **Adapters** are correctly wired.

---

## 1. Unit Testing the Core (The Brain)

When writing logic in `pond-core`, you should never need a real database or a real AI framework.

### The Process
1.  **Define a Port**: Identify what external capability you need (e.g., `MemoryStore`).
2.  **Write a Test**: Create a test in `src/services/chat_test.rs` that uses a **Mock** implementation of that port.
3.  **Implement**: Write just enough code in `src/services/chat.rs` to make the test pass.

### Example (Conceptual)
```rust
#[tokio::test]
async fn test_orchestrator_saves_message() {
    // 1. Setup mock storage
    let mock_storage = MockStorage::new();
    
    // 2. Setup service with mock
    let service = ChatService::new(mock_storage);
    
    // 3. Execute
    service.handle("Hello").await.unwrap();
    
    // 4. Assert
    assert!(mock_storage.was_called());
}
```

---

## 2. Integration Testing the Adapters

Adapters are where we test our code against the "Real World" (or a simulated version of it).

- **`pond-infra`**: Tests actual SQLite queries against `:memory:` databases to ensure SQL syntax and schema are correct.
- **`pond-adapters-goose`**: Tests that our mapping logic correctly converts Goose events into Pond domain objects.

---

## 3. Running Tests

Run all tests across the entire workspace:
```powershell
cargo test --workspace
```

Run tests for a single crate:
```powershell
cargo test -p pond-core
```
