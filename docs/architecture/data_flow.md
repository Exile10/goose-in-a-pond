# Data Flow & Lifecycle

How a request travels through GIAP from the client to the LLM and back.

---

## Voice Pipeline: Wait → Listen → Think → Speak

The core state machine is implemented in `pond-core/src/shared/services/chat.rs`:

```
┌─────────────┐   wake word    ┌─────────────┐   ASR transcript  ┌─────────────┐
│    WAIT     │ ─────────────► │    LISTEN   │ ────────────────► │    THINK    │
│             │                │             │                    │             │
│ WakeWord    │                │ VoiceInput  │                    │ GooseAdapter│
│ Detector    │                │ (Whisper)   │                    │ (LLM+Tools) │
└─────────────┘                └─────────────┘                    └──────┬──────┘
                                                                         │ response text
                                                                         ▼
                                                                  ┌─────────────┐
                                                                  │    SPEAK    │
                                                                  │             │
                                                                  │ VoiceOutput │
                                                                  │   (Piper)   │
                                                                  └─────────────┘
```

Each state is a port call — the concrete adapter is injected at startup and never known to the core.

---

## REST Chat Request

```
Mobile (GOTG) / Desktop / curl
        │
        │  POST /api/v1/chat  { "message": "...", "session_id": "..." }
        │  Authorization: Bearer <token>
        ▼
┌──────────────────────────────────────┐
│  pond-api  (Axum)                   │
│  1. Rate limiter                    │
│  2. Auth middleware                 │
│     → Handshake::validate_token()   │
│  3. Onboarding guard                │
│  4. Route handler: POST /chat       │
└──────────────────┬───────────────────┘
                   │ ChatRequest { message, session_id }
                   ▼
┌──────────────────────────────────────┐
│  pond-core  ChatService              │
│  1. Persist user message (port)     │
│  2. Load session history (port)     │
│  3. Trim to context budget          │
│  4. GooseAdapter.chat()  ◄──────────┼── agent: Arc<dyn Agent>
└──────────────────┬───────────────────┘
                   │
                   ▼
┌──────────────────────────────────────┐
│  GooseAdapter  (pond-adapters-goose) │
│  1. Load Settings from DB           │
│  2. Fetch + render prompt template  │
│  3. Inject PromptExtras + Skills    │
│  4. Inject memory fragments         │
│  5. Hot-swap LLM provider if needed │
│  6. Load "giap" MCP extension       │
│  7. Run Goose agentic loop          │
│     ↳ LLM decides to call tools     │
│     ↳ giap__get_current_weather()   │
│     ↳ giap__list_registered_devices │
│     ↳ etc.                          │
│  8. Return aggregated text          │
└──────────────────┬───────────────────┘
                   │ response_text + tool_results
                   ▼
┌──────────────────────────────────────┐
│  pond-core  ChatService              │
│  1. Persist assistant message       │
│  2. Auto-generate session title     │
│     (if first turn)                 │
│  3. Return response text            │
└──────────────────┬───────────────────┘
                   │  { response, session_id }
                   ▼
        Client receives JSON response
```

---

## Context Compaction

When the session history approaches 80% of the estimated token budget (heuristic: 4 chars/token), `ContextCompactor` triggers:

1. Takes the oldest 75% of messages
2. Summarizes them via a single LLM call
3. Replaces those messages with one summary message
4. Keeps the newest 25% verbatim

Falls back to hard truncation (`trim_to_budget()`) if the LLM call fails.

---

## Desktop App Event Flow

The Tauri desktop app communicates with the Rust backend via IPC commands and events:

```
Frontend (React)                          Backend (Rust)
     │                                         │
     │  invoke("ensure_server_running")  ──►   │  Spawns pond-server process
     │  poll invoke("server_health")    ──►   │  Returns true when HTTP ready
     │                                         │
     │  ◄──  emit("server-status", true)      │  Health monitor loop
     │  ◄──  emit("desktop-summon")           │  Global hotkey ⌘⇧V
     │  ◄──  emit("recording-started")        │  Mic open
     │  ◄──  emit("transcript", text)         │  ASR complete
     │  ◄──  emit("response-token", {token})  │  Streaming LLM token
     │  ◄──  emit("tool-result", {tool,data}) │  MCP tool called
     │  ◄──  emit("tts-start")                │  Piper speaking
     │  ◄──  emit("tts-end")                  │  Speaking done
     │  ◄──  emit("pipeline-error", msg)      │  Error in voice loop
```

All Tauri event listeners live exclusively in `AppContext.tsx`. Components dispatch Redux-style actions; they never call Tauri IPC directly.

---

## Database Writes

Every chat turn results in two SQLite writes to `pond_system.db`:

1. `session_messages` — user message (Role::User)
2. `session_messages` — assistant response (Role::Assistant)

If it is the first message in a session, a third write generates the session title via a short LLM call.

Memory fragments from `giap__save_memory` are written to `memory_fragments`. All writes are async and non-blocking to the response path.
