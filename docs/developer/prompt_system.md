# GIAP Prompt System

## Overview

The prompt system controls the system prompt sent to the LLM on every chat turn. It is designed to be:

- **Privacy-first** — every template emphasises on-device operation and no data egress
- **Voice-aware** — all templates forbid Markdown and pipeline control tokens
- **DB-editable** — built-in templates are seeded to the database at `run_setup()` and editable via REST API or direct SQL; no prompt strings are hard-coded in the binary at runtime
- **Safe** — all user-supplied strings are sanitized (control chars stripped, length-bounded) before substitution

---

## How Prompts Are Built (GooseAdapter path)

All production chat routes through `GooseAdapter` (`crates/pond-adapters-goose/src/goose_agent.rs`). On every turn:

1. `SettingsRepository::get()` — loads current settings
2. `PromptTemplateRepository::get(settings.prompt_style)` — fetches the active template from DB
3. `build_system_prompt_from_template(&settings, &template_content)` — renders placeholders
4. `agent.override_system_prompt(rendered)` — injects as the Goose agent's system prompt
5. Active `PromptExtra` records → `agent.extend_system_prompt(key, instruction)` for each
6. Active `UserSkill` records → `agent.extend_system_prompt("skill:<name>", content)` for each
7. If `settings.agent_memory_inject = true` → recent memory fragments also injected as an extra

### Fallback for CLI chat

The CLI `chat` command still uses `build_system_prompt(&settings)` (`pond-core/src/prompts.rs`) as a direct fallback. This is a Rust-code path, not a DB query.

---

## Priority Chain (CLI / file-override path)

| Priority | Source | How |
|---|---|---|
| 1 | File at `$DATA_DIR/prompts/system.md` | `render_template()` with all vars — deployment/sysadmin overrides |
| 2 | `Settings.custom_system_prompt` | `render_template()` with all vars — per-user full override stored in DB |
| 3 | Active template from `prompt_templates` DB | Selected by `Settings.prompt_style` |
| 4 | `SYSTEM_PROMPT` constant | Hardcoded fallback — used when Settings unavailable |

---

## Built-in Prompt Styles

Four built-in templates are seeded at `run_setup()` via `PromptTemplateRepository::insert_if_absent()`. They are stored in `prompt_templates` with `is_system = true` (not deletable via API, but always editable). The source constants are in `pond-core/src/prompts.rs` as `pub const PROMPT_*`.

| Style | `prompt_style` value | Audience | Character |
|---|---|---|---|
| Balanced | `"balanced"` | Default — most households | Warm + practical, full safety rules, voice-safe |
| Concise | `"concise"` | Power users | 1-sentence replies, action-first, minimal |
| Technical | `"technical"` | Developers | Narrates tool use, verbose, shows reasoning |
| Warm | `"warm"` | Families | Conversational, friendly, no jargon |

All four share the same safety rules (door unlock confirmation, unknown device response, external network gate).

---

## Managing Templates via REST API

```http
# List all templates
GET /api/v1/prompts

# Get a specific template
GET /api/v1/prompts/balanced

# Edit a built-in template (content updated; is_system stays true)
PUT /api/v1/prompts/balanced
Content-Type: application/json
{ "content": "You are {{assistant_name}}. Be very brief.", "description": "Ultra-concise" }

# Create a user-defined template
PUT /api/v1/prompts/swahili
Content-Type: application/json
{ "content": "Wewe ni {{assistant_name}}. Jibu kwa Kiswahili.", "description": "Swahili mode" }

# Delete a user-defined template (fails for is_system = true templates)
DELETE /api/v1/prompts/swahili

# Switch active template
PUT /api/v1/settings
{ "prompt_style": "swahili" }
```

---

## System Prompt Extras

Per-key extra instructions are stored in `prompt_extras` and injected on every turn via `agent.extend_system_prompt(key, instruction)`. Useful for adding rules without editing a full template.

```http
# Add or update an extra
POST /api/v1/agent/extras
{ "key": "language", "instruction": "Always respond in French.", "active": true, "sort_order": 10 }

# List all extras
GET /api/v1/agent/extras

# Disable without deleting
POST /api/v1/agent/extras
{ "key": "language", "active": false }

# Delete
DELETE /api/v1/agent/extras/language
```

---

## User Skills

Skills are named markdown instruction blocks stored in `user_skills`. Each active skill is injected as `skill:<name>` extra on every turn.

```http
POST /api/v1/skills
{ "name": "home_automation", "content": "When user asks about lights, call giap__list_registered_devices first." }

GET /api/v1/skills
PUT /api/v1/skills/{id}  { "active": false }
DELETE /api/v1/skills/{id}
```

---

## Template Variables

All templates (built-in, DB, file overrides) support these `{{placeholder}}` variables:

| Variable | Source | Notes |
|---|---|---|
| `{{assistant_name}}` | `Settings.assistant_name` | Sanitized, max 50 chars |
| `{{user_name}}` | `Settings.user_name` | Sanitized, max 50 chars |
| `{{personality}}` | `Settings.assistant_personality` | Sanitized, max 200 chars |
| `{{timezone}}` | `Settings.timezone` | Sanitized, max 50 chars |
| `{{location}}` | `Settings.weather_location_name` | Either `"\nLocation: <name>."` or `""` |
| `{{prompt_addendum}}` | `Settings.prompt_addendum` | Appended after the main template |

---

## Security

`sanitize_field(s, max_len)` is applied to every user-supplied value before substitution:

1. All ASCII control characters (0x00–0x1F, 0x7F) → replaced with space (prevents newline injection)
2. Whitespace runs collapsed to single space, trimmed
3. Truncated to `max_len` characters

`custom_system_prompt` itself is sanitized with `max_len = 4000` before rendering.

---

## File Override

Drop a `system.md` file at `$DATA_DIR/prompts/system.md` (defaults to `~/.local/share/goose-in-a-pond/prompts/system.md` on Linux). The CLI chat path checks for this file at startup — the REST/GooseAdapter path does not use it.

---

## Agent Settings Fields

Four settings fields control GooseAdapter's agentic loop behaviour:

| Field | Default | Description |
|---|---|---|
| `agent_goose_mode` | `"auto"` | GooseMode: `"auto"`, `"chat"`, or `"smart"` |
| `agent_max_turns` | `20` | Max loop turns per request |
| `agent_memory_inject` | `false` | Whether to inject recent memories into the system prompt |
| `agent_memory_limit` | `5` | How many memory fragments to inject (most recent) |

---

## Key Files

| File | Role |
|---|---|
| `crates/pond-core/src/prompts.rs` | `pub const PROMPT_*`, `build_system_prompt(&Settings)`, `build_system_prompt_from_template`, `sanitize_field`, `render_template` |
| `crates/pond-core/src/domain/settings.rs` | `prompt_style`, `custom_system_prompt`, `prompt_addendum`, `agent_*` fields |
| `crates/pond-core/src/ports/prompt_template.rs` | `PromptTemplateRepository` trait |
| `crates/pond-core/src/ports/prompt_extra.rs` | `PromptExtraRepository` trait |
| `crates/pond-core/src/ports/skill.rs` | `UserSkillRepository` trait |
| `crates/pond-core/src/domain/prompt_template.rs` | `PromptTemplate` domain type |
| `crates/pond-core/src/domain/prompt_extra.rs` | `PromptExtra` domain type |
| `crates/pond-core/src/domain/skill.rs` | `UserSkill` domain type |
| `crates/pond-infra/src/sqlite_prompt_template.rs` | `SqlitePromptTemplateRepository` |
| `crates/pond-infra/src/sqlite_prompt_extra.rs` | `SqlitePromptExtraRepository` |
| `crates/pond-infra/src/sqlite_skill.rs` | `SqliteSkillRepository` |
| `crates/pond-infra/migrations/system/0009_prompt_templates.sql` | DB schema |
| `crates/pond-infra/migrations/system/0010_prompt_extras.sql` | DB schema |
| `crates/pond-infra/migrations/system/0011_skills.sql` | DB schema |
| `crates/pond-adapters-goose/src/goose_agent.rs` | DB-driven prompt injection per turn |
| `crates/pond-api/src/routes.rs` | REST endpoints for prompts, extras, skills |
| `crates/pond-server/src/main.rs` | `run_setup()` seeds built-in templates via `insert_if_absent()` |
