# Plan: GIAP Web Dashboard — Voice Loop + Full Feature Completion

## Context

The GIAP web frontend (`web/`) is ~70% complete (React 19 + TypeScript + Vite, no UI library, no tests).
The core **Wait → Listen → Think → Speak** loop exists only in CLI chat mode (`pond-server chat`).
Server mode (`pond-server serve`) exposes HTTP only — no SSE, no WebSocket, no loop state over HTTP.

**The goal:** Make the voice loop the central UX of the web app — always accessible from any page —
and complete all dashboard features (scheduler, sensors, camera, settings hydration, chat history).

---

## What the Backend Already Has (no changes needed for these)

| Endpoint | Returns |
|---|---|
| `POST /api/v1/chat` | `{ session_id, response }` — full LLM turn |
| `GET /api/v1/sessions` | `{ sessions: [{ id, title, created_at, updated_at }] }` |
| `POST /api/v1/transcribe` | `{ text }` — proxies audio to whisper.cpp |
| `GET /api/v1/settings` | Full `Settings` struct (all fields) |
| `PUT /api/v1/settings` | Saves settings |
| `GET /api/v1/schedules` | Task list |
| `POST /api/v1/schedules` | Create task |
| `DELETE /api/v1/schedules/:id` | Delete |
| `POST /api/v1/schedules/:id/pause` | Pause |
| `POST /api/v1/schedules/:id/resume` | Resume |
| `POST /api/v1/schedules/:id/run-now` | Trigger |
| `GET /api/v1/sensors/:device_id` | `{ readings: [{ device_id, sensor_type, value, unit, recorded_at }] }` |
| `GET /api/v1/camera/events` | `{ events: [{ id, camera_id, event_type, confidence, snapshot_path, acknowledged, created_at }] }` |
| `GET /api/v1/devices` | `{ devices: [...] }` |
| `POST /api/v1/test/speak` | Speaks on server speakers (kiosk only) |

## One Backend Change Required

**`GET /api/v1/sessions/{session_id}/messages` does NOT exist.**
Sessions are tracked but messages are only retrieved internally via `session_storage.get_messages()`.
Without this endpoint, the frontend cannot load chat history on refresh.

**Add to `crates/pond-api/src/routes.rs`:**
- New handler `get_session_messages(session_id: String)` — calls `state.session_storage.get_messages(&session_id)`
- Returns: `{ messages: [{ id, role, content, created_at }] }`
- Wire into protected routes: `GET /api/v1/sessions/:session_id/messages`
- Add to the `protected_routes()` function alongside existing session routes

---

## Voice Loop Architecture (Web)

The loop runs **entirely in the browser** using existing HTTP endpoints + Web Speech APIs.
No backend SSE or WebSocket needed.

```
[WAIT]  — VoiceOrb shows idle pulse
   ↓  user clicks orb (or says wake word in browser, optional)
[LISTEN] — MediaRecorder captures audio (3–5s or push-to-talk)
   ↓  POST /api/v1/transcribe → { text }
[THINK]  — POST /api/v1/chat → { session_id, response }
   ↓  response received
[SPEAK]  — window.speechSynthesis.speak(utterance) — browser TTS
   ↓  utterance ends
[WAIT]   — loop back
```

State enum in component: `"wait" | "listen" | "think" | "speak"`

### CLI Note

`pond chat` already implements the full voice loop (wake word → Whisper STT → LLM → Piper/Qwen TTS).
This plan builds the **browser-side equivalent** — it does not change the CLI.
The one backend addition (`GET /sessions/:id/messages`) benefits CLI users too (accessible via any HTTP client).

---

## Implementation Plan

### Part 1 — Backend (Rust)

**File: `crates/pond-api/src/routes.rs`**

Add handler:
```rust
async fn get_session_messages(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let messages = state.session_storage.get_messages(&session_id).await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!({ "messages": messages })))
}
```
Wire: `.route("/sessions/:session_id/messages", get(get_session_messages))` in the protected router.

---

### Part 2 — Frontend

#### 2a. Test Infrastructure Setup

Add to `web/package.json` devDependencies:
- `vitest` — test runner (Vite-native, no config overhead)
- `@testing-library/react`
- `@testing-library/user-event`
- `jsdom` — DOM environment for Vitest
- `@vitest/coverage-v8` — coverage

Add `vite.config.ts` test block:
```ts
test: { environment: 'jsdom', globals: true, setupFiles: './src/test-setup.ts' }
```

New file: `web/src/test-setup.ts` — mock `window.speechSynthesis`, `MediaRecorder`, `SpeechRecognition`.

#### 2b. api.ts — Add Missing Functions

New types:
```typescript
interface SessionMessage { id: string; role: 'user' | 'assistant'; content: string; created_at: string }
interface Schedule { id: string; label: string; cron: string; enabled: boolean; payload: unknown }
interface SensorReading { device_id: string; sensor_type: string; value: number; unit: string; recorded_at: string }
interface CameraEvent { id: number; camera_id: string; event_type: string; confidence: number|null; acknowledged: boolean; created_at: string }
interface Settings {
  assistant_name: string; assistant_personality: string; user_name: string; timezone: string;
  llm_provider: string; active_llm_model: string; active_whisper_model: string; active_tts_model: string;
  voice_wake_word: string; voice_tts_voice: string; voice_recording_duration_secs: number;
  voice_whisper_url: string; voice_tts_http_url: string; voice_tts_http_voice: string;
  weather_enabled: boolean; weather_latitude: number; weather_longitude: number; weather_location_name: string;
  llm_max_tokens: number; llm_temperature: number;
  retention_event_log_days: number; retention_sensor_days: number; retention_session_messages_keep: number;
}
```

New functions (all accepting token string):
```typescript
getSettings(token)               // GET /settings
getSessionMessages(id, token)    // GET /sessions/:id/messages  ← requires new backend endpoint
listSchedules(token)             // GET /schedules
createSchedule(req, token)       // POST /schedules
deleteSchedule(id, token)        // DELETE /schedules/:id
pauseSchedule(id, token)         // POST /schedules/:id/pause
resumeSchedule(id, token)        // POST /schedules/:id/resume
runScheduleNow(id, token)        // POST /schedules/:id/run-now
getSensors(deviceId, token)      // GET /sensors/:device_id
listCameraEvents(token)          // GET /camera/events
```

#### 2c. VoiceOrb Component (NEW — most important)

**New file: `web/src/components/VoiceOrb.tsx`**

A floating button, fixed position (bottom-right), always rendered in App.tsx layout.

State machine:
```typescript
type LoopState = 'wait' | 'listen' | 'think' | 'speak'
```

Behavior:
- **Wait** — pulsing idle circle, click → start loop
- **Listen** — recording animation, MediaRecorder captures audio (push-to-talk: hold/release, or 5s auto-stop)
  - On stop → `POST /api/v1/transcribe` with WAV blob
  - On success → show transcript text briefly → enter Think
- **Think** — spinner animation, `POST /api/v1/chat` with transcribed text
  - Tracks `sessionId` in localStorage (`pond_voice_session_id`)
  - On success → enter Speak
- **Speak** — `window.speechSynthesis.speak()`, animated waveform
  - On utterance `onend` → back to Wait
- Mute toggle: if muted, skip Speak → return to Wait immediately
- Escape key or second click always returns to Wait
- Shows mini overlay with last transcript + last assistant response

Props: `token: string`

#### 2d. App.tsx — Embed VoiceOrb + Add Schedules Route

- Render `<VoiceOrb token={token} />` inside the authenticated layout (after nav bar)
- Add `"schedules"` to the `Page` type
- Add nav item for Schedules (calendar icon)
- Render `<Schedules />` when page === "schedules"

#### 2e. ChatWidget.tsx — History + TTS

- On mount: `getSessionMessages(storedSessionId, token)` → populate `messages` state
- After each assistant response: `window.speechSynthesis.speak(new SpeechSynthesisUtterance(response))`
- Add mute icon button to chat header (synced to localStorage `pond_tts_muted`)
- If `pond_tts_muted === "true"`, skip speech synthesis

#### 2f. Settings.tsx — Hydrate from Backend

- On mount: `getSettings(token)` → fill form fields
- Falls back to localStorage values if fetch fails

#### 2g. Schedules.tsx (NEW PAGE)

**New file: `web/src/pages/Schedules.tsx`**

Table UI:
- Columns: Name | Cron | Status | Actions
- Actions per row: Run Now, Pause/Resume, Delete
- "New Schedule" button → inline form: name, cron (6-field), webhook URL
- On submit: `createSchedule({ label, cron, payload: { webhook_url } }, token)`
- Polls every 30s for schedule list refresh

Cron format reminder: `<sec> <min> <hour> <day-of-month> <month> <day-of-week>`
Example: `"0 0 8 * * *"` = 08:00:00 daily

#### 2h. DeviceList.tsx / Devices.tsx — Sensor Data

- On device card expand/click: `getSensors(device.id, token)` → show last 5 readings
- Display: sensor_type + value + unit + time ago
- Auto-refresh sensors every 30s for expanded device

#### 2i. Activity.tsx — Camera Events from Backend

- Alongside localStorage activity, call `listCameraEvents(token)` on mount + every 60s
- Merge into feed: camera events shown as type "camera" with camera_id label

---

### Part 3 — Tests

**New file: `web/src/__tests__/api.test.ts`**
- Mock `fetch` globally
- Test each new api.ts function: assert correct URL, method, headers, body
- Test error handling (non-200 responses)

**New file: `web/src/__tests__/VoiceOrb.test.tsx`**
- Mock `MediaRecorder`, `window.speechSynthesis`, `fetch`
- Test: renders in wait state
- Test: clicking orb → listen state → think state → speak state
- Test: mute toggle skips speak state
- Test: Escape key returns to wait
- Test: error in transcribe → returns to wait

**New file: `web/src/__tests__/ChatWidget.test.tsx`**
- Test: loads session history on mount (mocks getSessionMessages)
- Test: sends message → receives response → appends to messages
- Test: TTS called on response (checks speechSynthesis.speak called)
- Test: mute toggle prevents TTS

**New file: `web/src/__tests__/Schedules.test.tsx`**
- Test: renders schedule list
- Test: create schedule form submission
- Test: delete schedule

---

## File Change Summary

| File | Type | Change |
|---|---|---|
| `crates/pond-api/src/routes.rs` | **Backend** | Add `GET /sessions/:id/messages` handler + wire |
| `web/package.json` | Config | Add vitest + testing-library devDeps |
| `web/vite.config.ts` | Config | Add test block for jsdom environment |
| `web/src/test-setup.ts` | **NEW** | Mock speechSynthesis, MediaRecorder |
| `web/src/api.ts` | Frontend | Add 10 new functions + TypeScript types |
| `web/src/components/VoiceOrb.tsx` | **NEW** | Voice loop widget (Wait/Listen/Think/Speak) |
| `web/src/components/ChatWidget.tsx` | Frontend | Load history on mount + TTS output + mute toggle |
| `web/src/pages/Schedules.tsx` | **NEW** | Scheduler CRUD UI |
| `web/src/pages/Settings.tsx` | Frontend | GET /settings hydration on mount |
| `web/src/pages/Activity.tsx` | Frontend | Camera events from backend |
| `web/src/pages/Devices.tsx` | Frontend | Sensor data per device |
| `web/src/App.tsx` | Frontend | Embed VoiceOrb + add Schedules route + nav |
| `web/src/__tests__/api.test.ts` | **NEW** | API function unit tests |
| `web/src/__tests__/VoiceOrb.test.tsx` | **NEW** | Voice loop state machine tests |
| `web/src/__tests__/ChatWidget.test.tsx` | **NEW** | Chat history + TTS tests |
| `web/src/__tests__/Schedules.test.tsx` | **NEW** | Scheduler UI tests |

---

## Verification

```bash
# 1. Run frontend tests
cd web && npm test

# 2. Build frontend
cd web && npm run build

# 3. Build backend (with new route)
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server

# 4. Run server
cargo run -p pond-server -- serve --debug --open

# 5. Manual checks in browser:
#    a. Open http://localhost:4000
#    b. Click voice orb → speak → assert transcription appears + assistant responds + browser speaks aloud
#    c. Refresh page → assert chat history loads (requires new /sessions/:id/messages endpoint)
#    d. Navigate to Schedules → create schedule with cron "0 0 8 * * *" → confirm appears in list
#    e. Click device → assert sensor readings appear
#    f. Navigate to Settings → refresh → assert values persist from backend
#    g. Navigate to Activity → assert camera events appear alongside local activity
```
