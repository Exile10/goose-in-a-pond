# TDD Guide: Test-Driven Development in the Pond

GIAP follows a strict **test-first** discipline. All new ports must have a mock and passing tests before any real adapter is written. This keeps the Core fast to test, safe to refactor, and free from infrastructure coupling.

---

## Testing Philosophy

| Layer | What to test | How |
|---|---|---|
| `pond-core` | Domain logic, service orchestration | Unit tests with mock ports — no DB, no network |
| `pond-infra` | SQL queries and schema | Integration tests against `tempfile` SQLite |
| `pond-adapters-*` | HTTP clients, subprocess wrappers | Integration tests with `wiremock` + `#[ignore]` live tests |
| `pond-api` | Route handlers, middleware | Integration tests with `tower::ServiceExt::oneshot` |
| `pond-desktop` | State reducer, API client | vitest unit tests with `happy-dom` |

---

## 1. Core Unit Tests (Fast — ~2s)

When adding logic to `pond-core`, always write tests before the implementation.

### Pattern

```rust
// crates/pond-core/src/shared/services/chat.rs

#[tokio::test]
async fn chat_once_persists_user_and_assistant_messages() {
    // 1. Create mock ports — no real database
    let storage = Arc::new(InMemorySessionStorage::new());
    storage.create_session("test".to_string()).await.unwrap();
    let agent = Arc::new(MockAgent::new()); // echoes "Echo: <input>"

    // 2. Wire the service
    let svc = ChatService::new(agent, "test".to_string(), storage.clone());

    // 3. Execute
    svc.chat_once("hello".to_string()).await.unwrap();

    // 4. Assert
    let messages = storage.get_messages("test").await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message.role, Role::User);
    assert_eq!(messages[1].message.role, Role::Assistant);
}
```

### Run

```bash
cargo test -p pond-core          # ~2 seconds, no external deps
cargo test -p pond-core -- chat  # filter to chat tests only
```

### Available mocks

| Mock | Location | What it fakes |
|---|---|---|
| `MockAgent` | `services/mock_agent.rs` | Echoes `"Echo: <input>"` |
| `MockProvider` | `services/mock_provider.rs` | Configurable static response |
| `InMemorySessionStorage` | `services/mock_session.rs` | In-memory session store |
| `MockHandshake` | `pond-infra/src/mock_handshake.rs` | In-memory token store |
| `MockSettingsRepository` | `services/mock_settings.rs` | Default settings |
| `MockMemoryRepository` | `services/mock_memory.rs` | In-memory memory fragments |
| `MockSkillRepository` | `services/mock_skill.rs` | In-memory user skills |
| `MockPromptTemplateRepository` | `services/mock_prompt_template.rs` | Default template |

---

## 2. Adapter Integration Tests (wiremock)

Adapters that make HTTP calls are tested against a `wiremock` mock server — no real Ollama, Whisper, or OpenMeteo needed.

### Pattern

```rust
// crates/pond-adapters-ollama/tests/integration.rs

#[tokio::test]
async fn system_prompt_is_first_message_in_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_ok("ok")))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider
        .complete("You are helpful.", vec![ChatMessage::user("Hi")])
        .await
        .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();

    assert_eq!(body["messages"][0]["role"], "system");
}
```

### Live tests

Tests that require real hardware are gated with `#[ignore]` and include setup instructions:

```rust
#[tokio::test]
#[ignore = "requires ollama serve with llama3.2 pulled"]
async fn live_ollama_completion() { ... }
```

Run them explicitly:
```bash
cargo test -p pond-adapters-ollama  -- --ignored live_ollama_completion
cargo test -p pond-adapters-whisper -- --ignored live_transcription_of_jfk_wav
cargo test -p pond-adapters-piper   -- --ignored live_speak
```

---

## 3. API Integration Tests

Route handler tests use Axum's `tower::ServiceExt::oneshot` — no real HTTP server needed.

```rust
// crates/pond-api/tests/chat_integration_test.rs

#[tokio::test]
async fn post_chat_returns_echo_via_agent() {
    let (app, _tmp) = make_app().await; // pre-seeds TEST_TOKEN in MockHandshake

    let resp = app
        .oneshot(chat_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}
```

Auth in tests: `make_app()` pre-seeds `"test-token"` in `MockHandshake`. Protected route requests carry `Authorization: Bearer test-token`. Auth failure tests omit or use an unknown token.

```bash
cargo test -p pond-api
cargo test -p pond-api --test chat_integration_test
cargo test -p pond-api --test onboarding_integration_test
```

---

## 4. Desktop Frontend Tests (vitest)

The React frontend uses [vitest](https://vitest.dev) with `happy-dom`.

```typescript
// pond-desktop/src/state/reducer.test.ts
it("caps transcript at 50 messages", () => {
  let state = buildInitialState();
  for (let i = 0; i < 60; i++) {
    state = reducer(state, { type: "APPEND_TRANSCRIPT", payload: msg(i) });
  }
  expect(state.transcript).toHaveLength(50);
});
```

```bash
cd pond-desktop
npm test          # watch mode
npm run test:run  # single pass (CI)
```

---

## 5. New Port/Adapter Checklist

- [ ] Domain types in `pond-core/src/<quadrant>/domain/`
- [ ] Port trait in `pond-core/src/<quadrant>/ports/` with `async_trait`
- [ ] Mock implementation in `pond-core/src/<quadrant>/mocks/mock_<name>.rs`
- [ ] Unit tests for the mock passing
- [ ] Real adapter in `crates/pond-adapters-<name>/`
- [ ] Integration tests covering: happy path, error path (HTTP 500, connection refused), trait-object conformance, `#[ignore]` live test
- [ ] Wired into `pond-server/src/main.rs`
- [ ] `cargo test -p pond-core` still passes in ~2s
