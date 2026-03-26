# Database Schema — Goose In A Pond

> Last updated: March 2026 — reflects migrations 0001–0005 (system) and 0001–0003 (logs)

## Overview

GIAP uses **two SQLite databases** to separate concerns:

| Database | File | Purpose |
|----------|------|---------|
| **System** | `pond_system.db` | Core app data — sessions, profiles, devices, settings, memory |
| **Logs** | `pond_logs.db` | Telemetry, sensor time-series, camera events, audit trail |

Both are initialized via `sqlx::migrate!()` in `pond-infra/src/db.rs`. Migration files:
- `crates/pond-infra/migrations/system/`
- `crates/pond-infra/migrations/logs/`

The two connection pools are exposed as `db.system` and `db.logs` respectively. **Never use `db.system` for sensor or camera writes** — those tables live in `db.logs`.

---

## `pond_system.db` — Core Application Data

### `onboarding_state`

Singleton row (enforced by `CHECK(id=1)`) tracking first-run setup progress.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | INTEGER | PK, CHECK(id=1) | Forces single row |
| `current_step` | TEXT | NOT NULL | `VerifyDevice` → `CreateProfile` → `ConfigurePersonality` → `ConnectDevices` → `Completed` |
| `updated_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

---

### `sessions`

Conversation threads. Each `POST /api/v1/chat` call either creates or reuses a session.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `title` | TEXT | Nullable | Auto-generated after first LLM exchange |
| `profile_id` | TEXT | FK → profiles(id), Nullable | Household member who owns this session |
| `input_mode` | TEXT | NOT NULL, DEFAULT 'rest' | `rest` or `voice` |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |
| `updated_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

Indexes: `idx_sessions_updated_at` on `(updated_at DESC)`

---

### `session_messages`

Individual messages within a session. TTL: keep most recent **500 per session** (pruned every 6 hours by background task).

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `session_id` | TEXT | NOT NULL, FK → sessions(id) ON DELETE CASCADE | |
| `role` | TEXT | NOT NULL, CHECK IN ('user','assistant','system') | |
| `content` | TEXT | NOT NULL | |
| `embedding` | BLOB | Nullable | f32 LE bytes — reserved for future per-message semantic search |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

Indexes: `idx_session_messages_session_id`, `idx_session_messages_created_at`

> **Context budget**: `ChatService` loads the most recent 100 messages then calls `trim_to_budget()` to cap at ~9 952 chars (≈3K tokens) before sending to the LLM. This prevents silent context overflow on a 7B model.

---

### `profiles`

Household members. Each person who uses the Pond gets a profile. Sessions are linked to a profile; memory fragments are scoped to one.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `display_name` | TEXT | NOT NULL | "Jerry", "Alice" |
| `avatar_emoji` | TEXT | NOT NULL, DEFAULT 'duck' | Emoji character, e.g. 🦆 |
| `preferences` | TEXT | NOT NULL, DEFAULT '{}' | JSON object — `{"language":"sw","timezone":"Africa/Nairobi"}` |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |
| `updated_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

Indexes: `idx_profiles_created_at`

> **Note**: `preferences` is stored as JSON text in SQLite (serialized by `pond-infra`). In Rust (`pond-core`), it is typed as `HashMap<String, String>` — never `serde_json::Value`, because `pond-core` has no `serde_json` dependency.

---

### `devices`

Registered devices: GOTG mobile clients, smart home sensors, other Pond instances.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `name` | TEXT | NOT NULL | User-facing name ("Jerry's Phone") |
| `hostname` | TEXT | Nullable | mDNS hostname |
| `device_type` | TEXT | NOT NULL, DEFAULT 'gotg' | `gotg`, `sensor`, `camera`, `pond`, etc. |
| `ip_address` | TEXT | Nullable | Direct IP |
| `capabilities` | TEXT | NOT NULL, DEFAULT '[]' | JSON array — `["chat","tts","push"]` |
| `last_seen` | TEXT | Nullable | Datetime of last heartbeat (`POST /api/v1/devices/{id}/heartbeat`) |
| `is_online` | INTEGER | NOT NULL, DEFAULT 0 | Stored as hint; **computed live** at read-time from `last_seen > now − 5min` |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |
| `updated_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

> **`is_online` is computed, not trusted from DB**: the `SqliteDeviceRegistry` reads `last_seen` and derives `is_online` in Rust. The stored column is a fallback for devices that have never heartbeated.

---

### `settings`

Flat key-value store for GIAP configuration. 14 keys across 4 categories.

| Column | Type | Constraints |
|--------|------|-------------|
| `key` | TEXT | PK |
| `value` | TEXT | NOT NULL |
| `updated_at` | TEXT | NOT NULL, DEFAULT datetime('now') |

**Current keys and defaults:**

| Category | Key | Default |
|----------|-----|---------|
| Assistant identity | `assistant_name` | `"Goose"` |
| | `assistant_personality` | `"friendly and concise"` |
| | `user_name` | `"Friend"` |
| | `timezone` | `"UTC"` |
| LLM behaviour | `llm_max_tokens` | `"1024"` |
| | `llm_temperature` | `"0.7"` |
| | `llm_provider` | `"llamafile"` |
| Voice pipeline | `voice_wake_word` | `"goose"` |
| | `voice_tts_voice` | `"en_US-lessac-medium.onnx"` |
| | `voice_recording_duration_secs` | `"5"` |
| | `voice_whisper_url` | `"http://127.0.0.1:9000"` |
| Data retention | `retention_event_log_days` | `"30"` |
| | `retention_sensor_days` | `"7"` |
| | `retention_session_messages_keep` | `"500"` |

Missing keys fall back to Rust defaults (`Settings::default()`). The `SqliteSettingsRepository::update()` issues **one `INSERT OR REPLACE` per key** — never batched, because `sqlx::query()` with multi-statement SQL silently only executes the first statement.

---

### `memory_fragments`

Semantic/recency memory store. Stores snippets of conversation or notable facts, optionally with an embedding vector for cosine similarity search.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | |
| `profile_id` | TEXT | FK → profiles(id) ON DELETE CASCADE, Nullable | `NULL` = global memory |
| `session_id` | TEXT | FK → sessions(id) ON DELETE SET NULL, Nullable | Which conversation it came from |
| `content` | TEXT | NOT NULL | The remembered text |
| `embedding` | BLOB | Nullable | f32 LE bytes (384 dims). `NULL` until a real `EmbeddingProvider` is wired |
| `source` | TEXT | NOT NULL, DEFAULT 'chat' | `chat`, `note`, `sensor_summary` |
| `tags` | TEXT | NOT NULL, DEFAULT '[]' | JSON array |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

Indexes: `idx_memory_fragments_profile_id`, `idx_memory_fragments_created_at`

**Search strategy:**
- If embeddings exist → cosine similarity in Rust (O(n), fine at home-assistant scale)
- If no embeddings stored yet → falls back to recency (`ORDER BY created_at DESC`)
- Embeddings encoded as raw little-endian f32 BLOBs: `4 bytes × dims`

---

## `pond_logs.db` — Telemetry and Sensor Data

### `event_log`

General-purpose application event log. TTL: **30 days** (configurable via `settings.retention_event_log_days`).

| Column | Type | Constraints |
|--------|------|-------------|
| `id` | INTEGER | PK AUTOINCREMENT |
| `timestamp` | TEXT | NOT NULL, DEFAULT datetime('now') |
| `level` | TEXT | NOT NULL, DEFAULT 'INFO' — `DEBUG`, `INFO`, `WARN`, `ERROR` |
| `source` | TEXT | NOT NULL |
| `message` | TEXT | NOT NULL |
| `metadata` | TEXT | Nullable — JSON blob |

---

### `system_info`

Hardware/system telemetry snapshots (CPU temp, memory, uptime, etc.).

| Column | Type | Constraints |
|--------|------|-------------|
| `id` | INTEGER | PK AUTOINCREMENT |
| `timestamp` | TEXT | NOT NULL, DEFAULT datetime('now') |
| `key` | TEXT | NOT NULL |
| `value` | TEXT | NOT NULL |

---

### `sensor_readings`

Time-series IoT sensor data. TTL: **7 days** (configurable via `settings.retention_sensor_days`).

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | INTEGER | PK AUTOINCREMENT | |
| `device_id` | TEXT | NOT NULL | Logical reference to `devices.id` (no FK — logs DB can't FK to system DB) |
| `sensor_type` | TEXT | NOT NULL | `temperature`, `humidity`, `motion`, `door`, `energy`, `light_level` |
| `value` | REAL | NOT NULL | Numeric reading |
| `unit` | TEXT | NOT NULL | `C`, `%`, `lux`, `kWh`, `boolean` |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

Indexes: `idx_sensor_readings_device_type` on `(device_id, sensor_type)`, `idx_sensor_readings_created_at`

---

### `camera_events`

Vision detection events. TTL: **14 days for acknowledged events**; unacknowledged alerts are kept indefinitely until dismissed.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | INTEGER | PK AUTOINCREMENT | Returned by `POST /api/v1/camera/events` |
| `camera_id` | TEXT | NOT NULL | Logical reference to a device |
| `event_type` | TEXT | NOT NULL | `motion`, `person`, `pet`, `package`, `vehicle` |
| `confidence` | REAL | Nullable | Model confidence 0.0–1.0 |
| `snapshot_path` | TEXT | Nullable | Local path to captured frame |
| `metadata` | TEXT | Nullable | JSON — bounding boxes, zone info, etc. |
| `acknowledged` | INTEGER | NOT NULL, DEFAULT 0 | 0 = active alert, 1 = dismissed via `PATCH .../acknowledge` |
| `created_at` | TEXT | NOT NULL, DEFAULT datetime('now') | |

Indexes: `idx_camera_events_created_at`, `idx_camera_events_camera_id`

---

## Entity-Relationship Diagram

```
pond_system.db
─────────────────────────────────────────────────────────────
onboarding_state (singleton)

profiles ──────────────────────────────────────────┐
  │                                                 │
  ├──< sessions >──< session_messages               │
  │                                                 │
  └──< memory_fragments <───────────────────────────┘
         (session_id → sessions, nullable)

devices

settings (flat KV, 14 keys)

pond_logs.db
─────────────────────────────────────────────────────────────
event_log
system_info
sensor_readings   (device_id → devices.id, logical ref)
camera_events     (camera_id → devices.id, logical ref)
```

> Cross-DB foreign keys are **logical only** — SQLite cannot enforce FK constraints across database files. `device_id` in `sensor_readings` matches `devices.id` by convention; the application layer is responsible for consistency.

---

## TTL Pruning

A background task in `pond-infra/src/pruning.rs` runs every **6 hours** and enforces retention limits:

| Table | Retention | Rule |
|-------|-----------|------|
| `event_log` | 30 days | DELETE rows older than `retention_event_log_days` |
| `sensor_readings` | 7 days | DELETE rows older than `retention_sensor_days` |
| `camera_events` | 14 days | DELETE acknowledged rows older than 14 days; unacknowledged kept |
| `session_messages` | 500/session | DELETE oldest rows beyond `retention_session_messages_keep` per session |

All retention values are read from the `settings` table at server startup.

---

## Port → Table Mapping

| Port Trait | Tables | DB | Status |
|-----------|--------|-----|--------|
| `OnboardingRepository` | `onboarding_state` | System | ✅ Implemented |
| `SessionStorage` | `sessions`, `session_messages` | System | ✅ Implemented |
| `SettingsRepository` | `settings` | System | ✅ Implemented |
| `ProfileRepository` | `profiles` | System | ✅ Implemented |
| `DeviceRegistry` | `devices` | System | ✅ Implemented |
| `MemoryRepository` | `memory_fragments` | System | ✅ Implemented |
| `EmbeddingProvider` | — (stateless, generates BLOBs) | — | ⚪ Stub — real model deferred |
| `SensorStorage` | `sensor_readings` | Logs | ✅ Implemented |
| `CameraStorage` | `camera_events` | Logs | ✅ Implemented |
| `LlmProvider` | — (stateless HTTP calls) | — | ✅ No table needed |
| `VoiceInput` | — (real-time audio) | — | ✅ No table needed |
| `VoiceOutput` | — (real-time audio) | — | ✅ No table needed |
| `WakeWordDetector` | — (real-time audio) | — | ✅ No table needed |
| `Agent` | — (delegates to LlmProvider) | — | ✅ No table needed |
| `Handshake` | `auth_tokens` (planned) | System | 🔴 Table not yet created |
| `NotificationSender` | `notifications` (planned) | System | 🔴 Table not yet created |

---

## REST API → Table Reference

| Endpoint | Method | Table(s) |
|----------|--------|---------|
| `/api/v1/settings` | GET, PUT | `settings` |
| `/api/v1/profiles` | GET, POST | `profiles` |
| `/api/v1/profiles/{id}` | GET, PATCH, DELETE | `profiles` |
| `/api/v1/devices` | GET, POST | `devices` |
| `/api/v1/devices/{id}` | DELETE | `devices` |
| `/api/v1/devices/{id}/heartbeat` | POST | `devices` (updates `last_seen`) |
| `/api/v1/chat` | POST | `sessions`, `session_messages` |
| `/api/v1/sessions` | GET | `sessions` |
| `/api/v1/sessions/{id}` | PATCH | `sessions` (rename title) |
| `/api/v1/sensors` | POST | `sensor_readings` (logs DB) |
| `/api/v1/sensors/{device_id}` | GET | `sensor_readings` (logs DB) |
| `/api/v1/camera/events` | GET, POST | `camera_events` (logs DB) |
| `/api/v1/camera/events/{id}/acknowledge` | PATCH | `camera_events` (logs DB) |

---

## Migration History

### System DB

| File | What it does |
|------|-------------|
| `0001_initial.sql` | `onboarding_state`, `sessions`, `session_messages`, `devices`, `settings` |
| `0002_add_indexes_and_title.sql` | Adds `title` to sessions; performance indexes |
| `0003_profiles.sql` | `profiles` table; adds `profile_id` + `input_mode` to sessions |
| `0004_devices_enhanced.sql` | Adds `device_type`, `ip_address`, `capabilities`, `last_seen`, `is_online` to devices |
| `0005_memory.sql` | `memory_fragments` table; adds `embedding` BLOB to `session_messages` |

### Logs DB

| File | What it does |
|------|-------------|
| `0001_initial.sql` | `event_log`, `system_info` |
| `0002_sensor_readings.sql` | `sensor_readings` table + indexes |
| `0003_camera_events.sql` | `camera_events` table + indexes |

---

## Design Decisions

### Why SQLite (not Postgres)?
Local-first, zero config, single file, already proven for sessions and onboarding. Easy backup (`cp pond_system.db pond_system.db.bak`). Can be reconsidered for Q4 if multi-user or network deployment requirements emerge.

### Why two databases?
Separation of lifecycle: logs/sensor data are pruned on a 7–30 day TTL without touching application data. High-frequency sensor writes don't contend with system queries. The two `sqlx` pools (`db.system`, `db.logs`) make the boundary explicit at the type level.

### Why TEXT for timestamps?
`datetime('now')` produces `"YYYY-MM-DD HH:MM:SS"` — human-readable, works with all SQLite date functions, consistent with every existing table. 1-second resolution is sufficient for all current use cases. For tables with sub-second insertion rates, always secondary-sort by `rowid ASC` as a tiebreaker.

### Why JSON blobs for capabilities/preferences/tags?
These fields are read-as-blob and not queried relationally. `capabilities` on a device varies wildly by type. `preferences` on a profile is a sparse, extensible set. Using JSON avoids a new migration every time a new preference key is added. If a field starts being queried with `json_extract()` frequently, normalize it.

### Why no FK from logs DB to system DB?
SQLite cannot enforce FK constraints across attached databases. `device_id` in `sensor_readings` is a logical reference only — enforced by the application, not the database. This is an acceptable tradeoff given the performance and separation benefits of two files.

### Why is `is_online` stored but also computed?
`is_online` in `devices` is set to `1` on `register()` and `heartbeat()`. But it's never trusted at read-time — `SqliteDeviceRegistry` always computes the live value from `last_seen > now − 5min`. The stored column exists so that a raw SQL query gives a reasonable answer even without running through the adapter.

---

## Planned Additions (Not Yet Implemented)

| Table | DB | Unlocks |
|-------|----|---------|
| `auth_tokens` | System | Full GIAP ↔ GOTG handshake with token-based auth |
| `notifications` | System | Push notifications to GOTG mobile app |
| `routines` | System | Scheduled/triggered automations (Morning Routine, etc.) |
| `feedback` | System | Upvote/downvote/correction loop for self-improvement |
| `system_prompts` | System | Dynamic, versioned system prompt management |
| `device_commands` | System | Audit trail for smart device commands |
| `prompt_rewrites` | Logs | Audit trail for agent self-rewriting prompts |
| `privacy_audit_log` | Logs | Per-action privacy audit ("what did Goose do?") |
| `mcp_extensions` | System | Registry of installed MCP server extensions (Q4) |
