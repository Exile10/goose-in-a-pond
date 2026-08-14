# Model Architecture Reference

This document covers the model management system end-to-end: domain types, ports, adapters, lifecycle, and step-by-step guides for adding a new model category or connecting a new LLM provider.

For the general port/adapter pattern see `docs/creating-ports-and-adapters.md`.

---

## Overview

All model knowledge at runtime comes from **SQLite, not source code**. There are no hardcoded URLs, filenames, or binary release tags in the codebase. On first run `run_setup` (or `run_server`) fetches a registry JSON from the configured URL and seeds the database. From that point on every subsystem queries the DB.

```
Online Registry JSON
        │  (HttpModelCatalogProvider::fetch)
        ▼
  model_role_assignments  ◄──  ModelRepository  ──►  models table
  (source of truth)                                  (full catalog)
        │
        │  sync_assignments_to_settings()
        ▼
  settings KV table  ←─ hot-cache for quick reads
  (chat_provider, chat_model, active_whisper_model, active_tts_model)
        │
        ▼
  ModelRouter  →  LlmProvider  →  chat/completions
  WhisperInput →  ASR pipeline
  PiperOutput  →  TTS pipeline
```

---

## Domain Types

All types live in `crates/pond-core/src/models/domain/model_record.rs`.

### `ModelCategory`

Six variants covering all supported model families:

| Variant | String key | Family | Local file? |
|---|---|---|---|
| `Gguf` | `"gguf"` | LLM | Yes (`models/gguf/`) |
| `Llamafile` | `"llamafile"` | LLM | Yes (`models/llm/`) |
| `Ollama` | `"ollama"` | LLM | No (server-side) |
| `Whisper` | `"whisper"` | ASR | Yes (`models/`) |
| `TtsPiper` | `"tts_piper"` | TTS | Yes (`models/tts/`) |
| `TtsHttp` | `"tts_http"` | TTS | No (HTTP endpoint) |

Helper methods: `is_llm()`, `is_asr()`, `is_tts()`.
String conversion: `ModelCategory::as_str()` / `ModelCategory::from_str()` (also accepts `"tts"` as alias for `TtsPiper`).

### `ModelRecord`

The single catalog row type. Primary key is `id = "{category}/{name}"` e.g. `"gguf/llama-3b"`.

Family-specific fields are `Option<_>` and `None` for irrelevant families:

```
Common:      id, category, name, filename, description, size_mb, url, downloaded, is_custom
LLM-only:    hf_id, ram_estimate_mb, recommended_role, context_length, quantization
ASR-only:    asr_language, asr_size
TTS-only:    tts_engine, tts_voice_name, config_filename, config_url, tts_url, sample_rate
```

`downloaded` is updated by `FilesystemModelStorage::is_present()` on startup via `seed_model_catalog`.
`is_custom = true` marks user-added rows that survive registry refreshes.

### `BinaryRecord`

Tool binaries (e.g. `whisper-server`, `piper`) that are versioned and platform-specific:

```rust
pub struct BinaryRecord {
    pub name:      String,                               // e.g. "whisper-server"
    pub version:   String,                               // e.g. "v1.8.4"
    pub platforms: HashMap<String, String>,              // "macos-arm64" -> URL
}
```

Platform key format: `"{os}-{arch}"` — `"macos-arm64"`, `"macos-x86_64"`, `"linux-x86_64"`, `"linux-aarch64"`, `"windows-x86_64"`.

`BinaryRecord::url_for_current_platform()` resolves the correct URL at compile time.

### `ModelRoleAssignment`

Maps a role to a model:

```rust
pub struct ModelRoleAssignment {
    pub role:     String,   // "chat" | "think" | "task" | "asr" | "tts"
    pub model_id: String,   // e.g. "gguf/llama-3b"
}
```

`ModelRoleAssignment::category_matches_role(category, role)` enforces that LLM roles only accept LLM categories, `"asr"` only accepts Whisper, and `"tts"` only accepts TtsPiper/TtsHttp.

---

## The Five Ports

All ports are defined in `crates/pond-core/src/<quadrant>/ports/` and exported from `ports/mod.rs`.

### `ModelRepository` — catalog + role assignment persistence

```rust
// Catalog
async fn list_all(&self) -> Result<Vec<ModelRecord>>;
async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>>;
async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>>;
async fn upsert(&self, model: &ModelRecord) -> Result<()>;
async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()>;

// Role assignments
async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>>;
async fn get_assignment(&self, role: &str) -> Result<Option<ModelRoleAssignment>>;
async fn set_assignment(&self, role: &str, model_id: &str) -> Result<()>;
async fn clear_assignment(&self, role: &str) -> Result<()>;
```

**Implementation:** `SqliteModelRepository` in `crates/pond-infra/src/sqlite_model_repository.rs`
**Schema:** `crates/pond-infra/migrations/system/0007_models.sql` (models table) and `0008_model_role_assignments.sql` (assignments table)

### `ModelCatalogProvider` — fetch registry JSON from a URL

```rust
async fn fetch(&self, url: &str) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)>;
```

**Implementation:** `HttpModelCatalogProvider` in `crates/pond-server/src/http_model_catalog_provider.rs`
**URL configured in:** `settings.model_registry_url`

The method is idempotent — calling twice with the same URL is safe.

### `ModelDownloader` — download a file from URL to disk

```rust
async fn download(&self, url: &str, dest: &Path, size_hint_mb: u64) -> Result<()>;
```

No-op if `dest` already exists. Parent directory must exist before calling.

**Implementation:** `HttpModelDownloader` in `crates/pond-server/src/http_model_downloader.rs`
Wraps `model_download::download_file()` which streams via `reqwest` with progress output.

### `ModelStorage` — resolve on-disk paths

```rust
fn path_for(&self, record: &ModelRecord) -> Option<PathBuf>;
fn is_present(&self, record: &ModelRecord) -> bool;   // default: path_for().exists()
fn binary_path(&self, record: &BinaryRecord) -> PathBuf;
fn binary_present(&self, record: &BinaryRecord) -> bool; // default: binary_path().exists()
```

**Implementation:** `FilesystemModelStorage` in `crates/pond-server/src/filesystem_model_storage.rs`

This is the **single place** that defines the on-disk directory layout:

| Category | Directory |
|---|---|
| `Whisper` | `<data_dir>/models/` |
| `Llamafile` | `<data_dir>/models/llm/` (`.exe` suffix on Windows) |
| `Gguf` | `<data_dir>/models/gguf/` |
| `TtsPiper` | `<data_dir>/models/tts/` |
| `TtsHttp`, `Ollama` | no local file (`path_for` returns `None`) |
| Binaries | `<data_dir>/bin/` (`.exe` suffix on Windows) |

### `ModelScheduler` — background download queue

```rust
async fn enqueue(&self, model_id: String, url: String, dest: PathBuf, size_mb: u64) -> Result<()>;
async fn status(&self, model_id: &str) -> Result<Option<DownloadStatus>>;
```

**Status:** Port defined; no real adapter yet. `AppState.model_scheduler` is `Option<Arc<dyn ModelScheduler>>` and `None` in production. The API routes fall back to synchronous download when this is `None`.

---

## On-Disk Directory Layout

```
<data_dir>/                           # default: ~/Library/Application Support/goose-in-a-pond
├── bin/
│   ├── whisper-server                # whisper.cpp HTTP server binary
│   └── piper                         # Piper TTS binary
├── models/
│   ├── ggml-base.en.bin              # Whisper models (flat, no subdir)
│   ├── gguf/
│   │   └── llama-3b.gguf            # GGUF LLM files (local inference)
│   ├── llm/
│   │   └── gemma-2b.llamafile        # Llamafile executables
│   └── tts/
│       ├── en_US-lessac-medium.onnx  # Piper TTS model
│       └── en_US-lessac-medium.onnx.json
└── pond_system.db                    # SQLite: catalog + roles + settings + sessions
```

`<data_dir>` is `dirs::data_dir()/goose-in-a-pond` — typically:
- macOS: `~/Library/Application Support/goose-in-a-pond`
- Linux: `~/.local/share/goose-in-a-pond`
- Windows: `%APPDATA%\goose-in-a-pond`

---

## Registry JSON Format

The catalog URL (default: `settings.model_registry_url`) must serve a JSON document in this format:

```json
{
  "version": "2",

  "tools": [
    {
      "name": "whisper-server",
      "version": "v1.8.4",
      "platforms": {
        "macos-arm64":   "https://example.com/whisper-server-macos-arm64",
        "linux-x86_64":  "https://example.com/whisper-server-linux-x86_64",
        "windows-x86_64":"https://example.com/whisper-server-windows-x86_64.exe"
      }
    },
    {
      "name": "piper",
      "version": "2023.11.14-2",
      "platforms": { "macos-arm64": "...", "linux-x86_64": "...", "windows-x86_64": "..." }
    }
  ],

  "whisper": [
    {
      "name":        "base",
      "filename":    "ggml-base.en.bin",
      "url":         "https://example.com/ggml-base.en.bin",
      "size_mb":     74,
      "language":    "en",
      "description": "Whisper base English-only model"
    }
  ],

  "llamafile": [
    {
      "name":             "gemma-2b",
      "filename":         "gemma-2b.llamafile",
      "url":              "https://example.com/gemma-2b.llamafile",
      "size_mb":          1500,
      "ram_estimate_mb":  2200,
      "recommended_role": "chat",
      "description":      "Gemma 2B llamafile — good for low-RAM devices"
    }
  ],

  "gguf": [
    {
      "name":             "llama-3b",
      "filename":         "llama-3b-Q4_K_M.gguf",
      "url":              "https://example.com/llama-3b-Q4_K_M.gguf",
      "id":               "meta-llama/Llama-3.2-3B-Instruct:Q4_K_M",
      "size_mb":          2000,
      "ram_estimate_mb":  3000,
      "recommended_role": "chat",
      "context_length":   128000,
      "quantization":     "Q4_K_M",
      "description":      "Llama 3.2 3B Instruct Q4_K_M"
    }
  ],

  "tts": [
    {
      "engine":           "piper",
      "name":             "en-lessac",
      "model_filename":   "en_US-lessac-medium.onnx",
      "config_filename":  "en_US-lessac-medium.onnx.json",
      "model_url":        "https://example.com/en_US-lessac-medium.onnx",
      "config_url":       "https://example.com/en_US-lessac-medium.onnx.json",
      "size_mb":          63,
      "sample_rate":      22050,
      "voice":            "lessac",
      "description":      "Piper TTS English (lessac, medium quality)"
    }
  ]
}
```

**Key points:**
- `"tools"` drives binary downloads; `"whisper"`, `"llamafile"`, `"gguf"`, `"tts"` drive `ModelRecord` rows.
- Missing or malformed entries are silently skipped — a partial catalog is always valid.
- Parsed by `HttpModelCatalogProvider` — add new top-level sections there if needed.

---

## Model Lifecycle

### 1. Seed the catalog (first run)

Called from `run_setup()` and `run_server()` startup:

```rust
seed_model_catalog(&*model_repo, &settings.model_registry_url, &data_dir).await;
```

This:
1. Calls `HttpModelCatalogProvider::fetch(url)`
2. Upserts all returned `ModelRecord`s via `model_repo.upsert()`
3. Updates `downloaded` flags by calling `FilesystemModelStorage::is_present()` for each record

### 2. Sync role assignments → settings hot-cache

Called after seeding and after CLI `models assign`:

```rust
sync_assignments_to_settings(&*model_repo, &settings_repo).await;
```

Reads all rows from `model_role_assignments` and writes:
- `chat_provider`, `chat_model` for the `"chat"` assignment
- `think_provider`, `think_model` for `"think"`
- `task_provider`, `task_model` for `"task"`
- `active_whisper_model` for `"asr"`
- `active_tts_model` for `"tts"`

The settings KV table is a **hot-cache** — always writable from the DB, never the reverse.

### 3. Assign a role

Via web UI (Settings page → save triggers `rebuild_model_router`) or CLI:

```bash
cargo run -p pond-server -- models assign --role chat --model gguf/llama-3b
```

Via REST: `POST /api/v1/models/{category}/{name}/activate` with body `{ "role": "chat" }`.

The handler:
1. Looks up `ModelRecord` in `model_repo`
2. Validates `ModelRoleAssignment::category_matches_role(category, role)` — returns 400 on mismatch
3. Calls `model_repo.set_assignment(role, model_id)` — persists to DB
4. Calls `sync_assignments_to_settings` — updates hot-cache
5. Rebuilds the `ModelRouter` in `AppState.llm_provider`

### 4. Ensure downloaded

Before starting a subprocess (whisper, piper) or loading a GGUF, call:

```rust
let path = fs_storage.path_for(&record).ok_or_else(|| ...)?;
if !path.exists() {
    model_download::download_file(&record.url.unwrap(), &path, record.size_mb).await?;
    model_repo.set_downloaded(&record.id, true).await?;
}
```

### 5. Resolve at runtime

**LLM (chat role):** `settings.chat_provider` + `settings.chat_model` → `ModelRouter::chat_provider()`
**ASR:** `settings.active_whisper_model` → look up `"whisper/{name}"` → `fs_storage.path_for()` → passed to `whisper_process::try_start()`
**TTS:** `settings.active_tts_model` → look up `"tts_piper/{name}"` → `fs_storage.path_for()` → passed to `PiperOutput`

---

## Role System

Five roles drive runtime component selection:

| Role | Consumers | Valid categories |
|---|---|---|
| `"chat"` | `ModelRouter` (default requests) | `Gguf`, `Llamafile`, `Ollama` |
| `"think"` | `ModelRouter` (Goose reasoning calls) | `Gguf`, `Llamafile`, `Ollama` |
| `"task"` | `ModelRouter` (tool-calling tasks) | `Gguf`, `Llamafile`, `Ollama` |
| `"asr"` | `WhisperInput`, `WhisperKeywordDetector` | `Whisper` |
| `"tts"` | `PiperOutput` | `TtsPiper`, `TtsHttp` |

**If `think`/`task` roles have no assignment**, `ModelRouter` falls back to the `chat` provider for all three.

**Settings hot-cache fields** (written by `sync_assignments_to_settings`, read at startup/request time):

| Role | Provider field | Model field |
|---|---|---|
| `chat` | `settings.chat_provider` | `settings.chat_model` |
| `think` | `settings.think_provider` | `settings.think_model` |
| `task` | `settings.task_provider` | `settings.task_model` |
| `asr` | — | `settings.active_whisper_model` |
| `tts` | — | `settings.active_tts_model` |

---

## How to Add a New Model Category

**Step 1 — Domain type.** Add a variant to `ModelCategory` in `crates/pond-core/src/models/domain/model_record.rs`:

```rust
pub enum ModelCategory {
    // ... existing variants ...
    MyNewCategory,
}
```

Update `as_str()`, `from_str()`, and the appropriate family helper (`is_llm()`, `is_asr()`, `is_tts()`).

**Step 2 — DB migration.** The `models` table stores `category` as text, so no schema change is needed. If you need a new role for this category, add it to `ModelRoleAssignment::category_matches_role()`.

**Step 3 — Storage path.** Add a match arm to `FilesystemModelStorage::path_for()` in `crates/pond-server/src/filesystem_model_storage.rs`:

```rust
ModelCategory::MyNewCategory => self.data_dir.join("models").join("my-new-dir").join(filename),
```

**Step 4 — Registry parsing.** Add a top-level key to the registry JSON format and a `parse_my_new_category()` function in `crates/pond-server/src/http_model_catalog_provider.rs`. Call it from `parse_models()`.

**Step 5 — Route validation.** The `DELETE /api/v1/models/{category}/{name}` route uses `ModelCategory::from_str()` from the URL path segment. If you use a different URL-facing string than `as_str()`, update the route parameter parsing in `crates/pond-api/src/routes.rs`.

**Step 6 — Test.** Add entries to `model_integration_test.rs` with the new category.

---

## How to Connect a New LLM Provider

A "provider" here means a backend that can serve chat completions: Ollama, llamafile, OpenAI-compatible endpoint, in-process GGUF, etc.

### Option A — Reuse an existing Goose provider (recommended)

The `goose/` submodule already implements `OllamaProvider`, `OpenAiCompatibleProvider`, and `LocalInferenceProvider`. Wrap one with `GooseProviderAdapter`.

```rust
// In run_server() or run_chat() — after the provider is needed
let goose_provider = OllamaProvider::from_env(model_config).await?;
let provider: Arc<dyn LlmProvider> =
    Arc::new(GooseProviderAdapter::new(Arc::new(goose_provider), session_id));
```

Add a new `"my-provider"` string to the `build_llm_provider()` match in `main.rs` and return the wrapped provider.

See `AGENTS.md` § "Goose Built-in Providers" for the full list and `OLLAMA_HOST` trick for llamafile.

### Option B — Write a new `LlmProvider` adapter

Use this when Goose has no matching provider (e.g. a completely custom HTTP protocol).

**1. New crate or module** implementing the `LlmProvider` port:

```rust
// crates/pond-adapters-myprovider/src/lib.rs
use async_trait::async_trait;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::domain::message::ChatMessage;

pub struct MyProvider { base_url: String }

#[async_trait]
impl LlmProvider for MyProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> anyhow::Result<ChatMessage> {
        // ... call your HTTP endpoint ...
    }

    fn model_name(&self) -> String {
        "my-provider".to_string()
    }
}
```

**2. Wire into `build_llm_provider()`** in `crates/pond-server/src/main.rs`:

```rust
"my-provider" => Arc::new(MyProvider::new(&base_url)),
```

**3. Settings hot-cache:** Assign the model via the web UI or CLI — `chat_provider = "my-provider"` and `chat_model = "my-model-name"` will be written to settings. The `ModelRouter` reads these at startup and on hot-reload.

**4. Model category in DB** (optional): If you want your model to appear in the Models page, add a `ModelRecord` with `category = ModelCategory::Ollama` (for remote servers) or a new category following the steps above. Set `downloaded = true` since there is no local file to check.

**5. Test the provider in isolation** (no Goose compile):

```bash
cargo test -p pond-core -- provider
```

---

## AppState Fields Related to Models

In `crates/pond-api/src/lib.rs`:

```rust
pub llm_provider:           Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,  // runtime ModelRouter
pub model_repo:             Option<Arc<dyn ModelRepository>>,           // catalog + roles DB
pub model_catalog_provider: Option<Arc<dyn ModelCatalogProvider>>,      // HTTP fetch impl
pub model_storage_dir:      Option<PathBuf>,                            // data_dir for path resolution
pub download_tracker:       Arc<RwLock<HashMap<String, f32>>>,          // progress % by model_id
pub model_scheduler:        Option<Arc<dyn ModelScheduler>>,            // background queue (future)
```

`model_catalog_provider` and `model_storage_dir` are `None` in tests (use `None` when constructing `AppState` in test fixtures).

---

## Settings Reference

All settings live in `crates/pond-core/src/user_data/domain/settings.rs` and are persisted as a flat key-value store in `pond_system.db`. Read/write via `GET /api/v1/settings` and `PUT /api/v1/settings` (or `settings_repo.get()` / `settings_repo.set_key()` in Rust).

### Assistant Identity

| Field | Type | Default | Description |
|---|---|---|---|
| `assistant_name` | `String` | `"Goose"` | Name the assistant uses for itself |
| `assistant_personality` | `String` | `"friendly and concise"` | Personality hint injected into the system prompt |
| `user_name` | `String` | `"Friend"` | Primary user's name, personalises responses |
| `timezone` | `String` | `"UTC"` | IANA timezone string, e.g. `"Africa/Nairobi"` |
| `primary_profile_id` | `Option<String>` | `None` | UUID of the primary profile created during onboarding |

### Prompt System

| Field | Type | Default | Description |
|---|---|---|---|
| `prompt_style` | `String` | `"balanced"` | Built-in template. Values: `balanced` \| `concise` \| `technical` \| `warm` |
| `custom_system_prompt` | `Option<String>` | `None` | Full prompt override. Supports `{{assistant_name}}`, `{{user_name}}`, `{{personality}}`, `{{timezone}}`, `{{location}}`, `{{prompt_addendum}}` placeholders. Max 4000 chars. |
| `prompt_addendum` | `String` | `""` | Extra instructions appended after the main prompt. Max 500 chars. |

### Model Roles (LLM routing)

These are the **authoritative** fields read at runtime. Written by `sync_assignments_to_settings()` from the `model_role_assignments` table; also written directly when the web UI saves the Settings page.

| Field | Type | Default | Description |
|---|---|---|---|
| `chat_provider` | `String` | `""` | Provider for the Chat role: `"llamafile"` \| `"ollama"` \| `"gguf"` \| `"mock"` |
| `chat_model` | `String` | `""` | Model name for the Chat role (e.g. `"llama-3b"`) |
| `think_provider` | `Option<String>` | `None` | Provider for the Think role. `None` = fallback to `chat_provider` |
| `think_model` | `Option<String>` | `None` | Model for the Think role. `None` = fallback to `chat_model` |
| `task_provider` | `Option<String>` | `None` | Provider for the Task role. `None` = fallback to `chat_provider` |
| `task_model` | `Option<String>` | `None` | Model for the Task role. `None` = fallback to `chat_model` |

### LLM Behaviour

| Field | Type | Default | Description |
|---|---|---|---|
| `llm_max_tokens` | `u32` | `1024` | Maximum tokens the LLM may generate per response |
| `llm_temperature` | `f32` | `0.7` | Sampling temperature (0.0 = deterministic, 1.0 = creative) |

### Voice Pipeline

| Field | Type | Default | Description |
|---|---|---|---|
| `voice_wake_word` | `String` | `"goose"` | Wake word / phrase detected by `WhisperKeywordDetector` (case-insensitive substring) |
| `voice_recording_duration_secs` | `u32` | `5` | Microphone capture duration per Whisper inference call |
| `voice_whisper_url` | `String` | `"http://127.0.0.1:9000"` | Custom remote whisper.cpp server URL |
| `voice_tts_voice` | `String` | `""` | Piper ONNX voice filename (e.g. `"en_US-lessac-medium.onnx"`) |

### Active Model Selection (hot-cache)

These are written by `sync_assignments_to_settings()` from `model_role_assignments`. Do not write them directly from application code — use the role assignment system instead.

| Field | Type | Default | Description |
|---|---|---|---|
| `active_whisper_model` | `String` | `""` | Model name for the `"asr"` role (e.g. `"base"`, `"small"`) |
| `active_tts_model` | `String` | `""` | Model name for the `"tts"` role (e.g. `"en-lessac"`) |

### Model Catalog

| Field | Type | Default | Description |
|---|---|---|---|
| `model_registry_url` | `String` | GitHub raw URL | URL of the registry JSON fetched by `HttpModelCatalogProvider` |

### Weather

| Field | Type | Default | Description |
|---|---|---|---|
| `weather_enabled` | `bool` | `false` | Whether to make weather available to the LLM via the `giap__get_current_weather` MCP tool |
| `weather_latitude` | `f64` | `0.0` | Decimal latitude for weather lookups (e.g. `-1.286` for Nairobi) |
| `weather_longitude` | `f64` | `0.0` | Decimal longitude (e.g. `36.817` for Nairobi) |
| `weather_location_name` | `String` | `""` | Human-readable location injected into the system prompt |

### Data Retention

| Field | Type | Default | Description |
|---|---|---|---|
| `retention_event_log_days` | `u32` | `30` | Days to keep rows in `event_log` (0 = keep forever) |
| `retention_sensor_days` | `u32` | `7` | Days to keep rows in `sensor_readings` |
| `retention_session_messages_keep` | `u32` | `500` | Maximum session messages to keep per session |

### Legacy fields (kept for DB compat, not used for routing)

| Field | Status | Notes |
|---|---|---|
| `llm_provider` | Serde-compatible stub | Old single-provider field. DB rows with this key are harmless. Code reads `chat_provider` instead. |
| `active_llm_model` | Serde-compatible stub | Old model name field. Code reads `chat_model` instead. |

---

## Verification Checklist

```bash
# No hardcoded model URLs/filenames remain in source (besides registry JSON comments)
grep -r "ggml-\|llamafile\|lessac\|piper-\|WHISPER_MODELS\|LLAMAFILE_MODELS" crates/ --include="*.rs"

# No llm_provider fallbacks in non-struct code
grep -rn "llm_provider" crates/ --include="*.rs" | grep -v "struct\|pub \|//\|settings\.rs"

# Full build passes
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server

# All tests green
cargo test -p pond-core -p pond-infra -p pond-api
```
