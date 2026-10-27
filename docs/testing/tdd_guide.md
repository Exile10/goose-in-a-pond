# Testing Guide

Write the test first. A new port gets a mock and passing tests in `pond-core` before any real
adapter exists, so the core stays fast to test and free of infrastructure.

Green unit tests are not evidence that the pond starts. Every Rust test builds its database by
applying all migrations to an empty file, in one process, with adapters constructed by hand. None
of them exercises startup ordering, migrations against a database that already has rows, route
registration, auth middleware or the wiring in `main.rs`. That is what `scripts/live-test.sh` is
for.

## The layers

| Layer | What it proves | Run | CI job |
|---|---|---|---|
| Rust unit and integration tests, fast crates | domain logic, adapters against fakes, routes in-process | `cargo test -p <crate>` | `rust` |
| Production-binary compile gate | `pond-server` and Goose build, including every test and example target | `cargo check -p pond-server -p pond-adapters-goose --all-targets` | `rust` |
| `#[ignore]`d live tests | real models, hardware, network | `cargo test -p <crate> -- --ignored` | none |
| `scripts/live-test.sh` | a real server starts and answers over HTTP, restarts on a populated database, enforces auth | `scripts/live-test.sh [--ui] [--no-build] [--keep]` | `live-test` (gated) |
| Vitest | renderer and Electron main-process units | `cd pond-desktop && npm test` | `frontend` |
| Playwright E2E | the dashboard against mocked API responses | `cd pond-desktop && npx playwright test` | `frontend` |
| Desktop smoke | the real Electron app launches and publishes the exact preload bridge | `cd pond-desktop && node scripts/smoke.mjs` | `desktop-smoke` |
| Script tests | `giap.sh` libraries against fakes, no network | `bash scripts/lib/<name>.test.sh` | one job each |

The full list of CI jobs and their local commands is in
[CONTRIBUTING](../CONTRIBUTING.md#checks).

## 1. Rust tests

### Fast crates

`ci.yml` tests a fixed list of crates that pull neither the Goose submodule nor a heavy native
library (llama.cpp, candle, ONNX Runtime): `pond-voice`, `pond-audio`, `pond-audio-out`,
`pond-adapters-audio-devices`, `pond-ear`, `pond-core`, `pond-infra`, `pond-infra-scheduler`,
`pond-api`, `pond-adapters-llamafile`, `pond-adapters-ollama`, `pond-adapters-whisper`,
`pond-adapters-matter`, `pond-adapters-vision`, `pond-adapters-weather`, `pond-adapters-uber`,
`pond-adapters-caldav`, `pond-adapters-imap`, `pond-adapters-mcp-memory`, `pond-mesh-protocol`,
`pond-mcp-server`, `pond-hf-cache`, `pond-net`, `pond-connect`. The list also enforces the
hexagonal split: a crate on it that grows a `goose` dependency no longer builds without Goose.

CI tests `pond-inference`, `pond-adapters-vision-onnx`, `pond-adapters-mesh-libp2p`,
`pond-adapters-mesh-inference` and `pond-adapters-lightning` in their own steps.

```bash
cargo test -p pond-core                    # one crate
cargo test -p pond-core chat_once          # tests whose name contains the substring
cargo test -p pond-api --test settings_integration_test
```

`cargo check` does not compile `#[cfg(test)]` code. A deletion it accepts can still break
`cargo test`; run the tests before calling a removal done.

### Core unit tests with mocks

`pond-core` tests use in-crate mocks, no database and no network. Each bounded context has a
`mocks/` module, compiled under `cfg(test)` or the `test-mocks` feature (for downstream test
harnesses). Commonly used:

| Mock | Location (under `crates/pond-core/src/`) | Fakes |
|---|---|---|
| `MockAgent` | `shared/mocks/mock_agent.rs` | an agent that answers `Echo: <input>` |
| `MockProvider` | `models/mocks/mock_provider.rs` | an `LlmProvider` |
| `InMemorySessionStorage` | `user_data/mocks/mock_session.rs` | session storage |
| `MockSettingsRepository` | `user_data/mocks/mock_settings.rs` | settings |
| `MockMemoryRepository`, `MockEmbeddingProvider` | `user_data/mocks/mock_memory.rs` | memories, embeddings |
| `MockSkillRepository` | `user_data/mocks/mock_skill.rs` | user skills |
| `MockPromptTemplateRepository` | `user_data/mocks/mock_prompt_template.rs` | prompt templates |
| `MockEventLog` | `security/mocks/mock_event_log.rs` | the event log |
| `MockHandshake` | `crates/pond-infra/src/mock_handshake.rs` | pairing tokens |

```rust
// crates/pond-core/src/shared/services/chat.rs
#[tokio::test]
async fn chat_once_returns_echo() {
    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "test-session".to_string();
    storage.create_session(session_id.clone()).await.unwrap();

    let service = ChatService::new(agent, session_id.clone(), storage.clone());
    let result = service.chat_once("Hello!".to_string()).await.unwrap();
    assert_eq!(result, "Echo: Hello!");
}
```

Use `tokio`'s `test-util` paused clock (a `pond-core` dev-dependency) for retry and backoff
tests rather than sleeping.

### Adapter tests against fakes

HTTP adapters are tested against a `wiremock` server, persistence against SQLite in a `tempfile`
directory. Cover the happy path, the error paths (HTTP 500, connection refused) and that the
adapter works as a trait object.

```rust
// crates/pond-adapters-ollama/tests/integration.rs
#[tokio::test]
async fn system_prompt_is_first_message_in_request() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

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

### API tests

Route tests drive the Axum router in-process with `tower::ServiceExt::oneshot`; no server or
port is involved. Protected routes need `Authorization: Bearer <token>`, and auth-failure tests
omit it or send an unknown token.

For a test that crosses a real security boundary (pairing, the device registry, profiles,
sessions, onboarding), build the state with `pond_api::test_support::app_state()` (feature
`test-support`, which `pond-api`'s own dev-dependencies enable). It applies every migration to a
fresh temporary directory and uses the real SQLite adapters for those five; everything else is a
mock. Replace a field with struct update syntax before wrapping the state in `Arc`.
`test_support::pair_phone` pairs a client the way a phone does. Do not copy the `AppState`
literal into a test file: it has dozens of fields and drifts.

### Source-reading tests

Some tests read source files as text to prove wiring or forbid a pattern: the
`*_is_wired.rs` and `*_wiring.rs` tests under `crates/pond-core/tests/`,
`crates/pond-infra/tests/` and `crates/pond-server/tests/`, plus
`crates/pond-core/tests/egress_guard.rs` and `crates/pond-api/tests/stream_handler_parity.rs`. Editing comments or moving code in the files
they scan can fail them; run the crate's tests after such edits.

## 2. Live tests (`#[ignore]`)

Tests that need a model file, a running server, hardware or the network are `#[ignore]`d with
the reason, usually naming the environment variable that points at the resource:

```rust
#[tokio::test]
#[ignore = "requires local Ollama instance with llama3.2 pulled"]
async fn live_ollama_completion() { /* ... */ }
```

```bash
cargo test -p pond-adapters-ollama -- --ignored live_ollama_completion
WHISPER_TEST_MODEL=/path/ggml-base.en.bin cargo test -p pond-adapters-whisper -- --ignored
KOKORO_DIR=/path/kokoro cargo test -p pond-adapters-kokoro --test live_synthesis -- --ignored
```

Common variables: `GIAP_OLLAMA_URL`, `GIAP_LLAMAFILE_URL`, `GIAP_GGUF_MODEL_PATH`,
`GIAP_TEST_GGUF_DIR`, `WHISPER_TEST_MODEL`, `KOKORO_DIR`, `SILERO_MODEL`. Numbers that are only
true on the hardware (KV-cache cost, free memory, throughput) come from running these on the
Jetson, not the Mac.

## 3. `scripts/live-test.sh`

Required for any change to a migration, a route, a handler or startup wiring.

```bash
scripts/live-test.sh              # build if needed, run the API checks
scripts/live-test.sh --ui         # also build the web UI and run the live Playwright suite
scripts/live-test.sh --no-build   # use the existing binary
scripts/live-test.sh --keep       # leave the server running afterwards
```

It starts `pond-server` against a scratch `POND_DATA_DIR`, reads the port the server published
in `.runtime_api_port` (never assumes one) and checks the listener is its own child before
sending anything. Then it runs `scripts/live_checks.py`, claims the pond through the setup code,
restarts against the now-populated database and re-runs the checks, repeats the auth checks with
the loopback bypass off, and digs the log for errors. `--ui` runs
`pond-desktop/tests/e2e-live/` against that server with `playwright.live.config.ts`, no mocks.

CI runs it only when the diff touches `crates/pond-infra/migrations/`,
`crates/pond-api/src/routes.rs`, `crates/pond-server/src/`, the script itself or
`tests/e2e-live/`, and only when the private-fork token is configured.

## 4. Desktop tests (`pond-desktop/`)

### Vitest

`vitest.config.ts` defines two projects, and `npm test` (`vitest run`) runs both:

| Project | Environment | Files |
|---|---|---|
| `renderer` | happy-dom, `src/test-setup.ts` | `src/**/*.test.{ts,tsx}` |
| `main` | node | `electron/**/*.test.ts` |

```typescript
// pond-desktop/src/state/reducer.test.ts
it("APPEND_TRANSCRIPT caps at 50 messages (trims oldest)", () => { /* ... */ });
```

`npx vitest` gives watch mode. `src/no-emoji.test.ts` scans every `.ts`/`.tsx` file under
`src/` and `electron/`, comments included, and fails on any emoji.

### Playwright E2E

`playwright.config.ts` starts Vite on port 5173 (`npm run dev:vite`) and runs
`tests/e2e/*.spec.ts` in Chromium, one worker. Each test mocks the API with `page.route()`, so no
`pond-server` runs; outside the desktop shell the app treats the server as online.

```bash
npx playwright test                                    # headless Chromium
npx playwright test tests/e2e/settings.spec.ts         # one file
npx playwright test --config=playwright.local.config.ts  # installed Chrome instead
```

`tests/e2e-live/` is the unmocked suite; `scripts/live-test.sh --ui` runs it.

### Desktop smoke

`scripts/smoke.mjs` builds nothing; run `npm run build && npm run build:main` first. It launches
the real Electron main process, waits for the window, checks that `app://giap` serves the bundle
and that the preload publishes exactly `serverUrl`, `invoke` and `listen`. CI runs it under xvfb.
It does not cover the macOS menu, TCC prompts, a real microphone, a real sidecar or the `.dmg`.

## 5. Other test suites

| Suite | Run |
|---|---|
| `native/pondnet` (Go) | `go test -race ./...` and `go vet ./...` in `native/pondnet` |
| `matter-server` | `npx vitest run` in `matter-server` |
| `extensions/music` | `npm test` in `extensions/music`; tests that drive the real Music app need `GIAP_MUSIC_LIVE` |
| Vision pairing generator | `python3 -m unittest discover -s scripts/models -p 'test_*.py'` |
| CI credential helper | `python3 scripts/ci/test_git_askpass.py` |
| `scripts/lib/*.sh` | `bash scripts/lib/<name>.test.sh` for `node-setup`, `jetson-remote`, `setup-code`, `connect-setup`, `litert-setup`, `model-pairings` |

## New port or adapter checklist

- [ ] Domain types in `crates/pond-core/src/<context>/domain/`
- [ ] Port trait in `crates/pond-core/src/<context>/ports/`, `Send + Sync`
- [ ] Mock in `crates/pond-core/src/<context>/mocks/`, with tests that pass
- [ ] Adapter crate with tests for the happy path, the error paths and trait-object use
- [ ] Live test `#[ignore = "<what it needs>"]` if the adapter talks to something real
- [ ] Wired in `crates/pond-server/src/main.rs`; `scripts/live-test.sh` passes if wiring changed

See [Creating ports and adapters](../developer/creating-ports-and-adapters.md).
