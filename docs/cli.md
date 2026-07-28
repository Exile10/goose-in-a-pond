# Goose In A Pond — CLI Reference

The `pond-server` binary is the single entry point for all GIAP functionality: first-time setup, the HTTP server, interactive voice/text chat, and every data-management operation.

```
pond-server <SUBCOMMAND> [OPTIONS]
```

Global flags:

| Flag | Effect |
|---|---|
| `--help` / `-h` | Print help for the binary or any subcommand |
| `--version` / `-V` | Print the build version |

> **Data directory**: all commands read from and write to the GIAP data directory.
> Default: `~/Library/Application Support/goose-in-a-pond` (macOS) / `~/.local/share/goose-in-a-pond` (Linux).
> Override for testing: `POND_DATA_DIR=/tmp/mytest pond-server status`

---

## First-run workflow

```
1. pond-server setup          # initialize DBs, download ASR + TTS models
2. pond-server onboard        # fill in name, timezone, personality, model
3. pond-server serve --open   # start the server and open the dashboard
```

---

## `setup`

Initializes databases, seeds the model catalog, and downloads the required runtime binaries (Whisper ASR, Piper TTS). Run once before anything else.

```bash
pond-server setup [--model tiny|base|small]
```

| Flag | Default | Description |
|---|---|---|
| `--model` | `base` | Whisper ASR model size. `tiny` (~39 MB, fastest), `base` (~74 MB, recommended), `small` (~244 MB, most accurate) |

What `setup` does, in order:

1. Checks system dependencies (ffmpeg, etc.) — warns but continues on failure
2. Creates `pond_system.db` + `pond_logs.db` and runs all migrations
3. Fetches the model catalog from the remote registry URL stored in Settings
4. Seeds the four built-in prompt templates (`balanced`, `concise`, `technical`, `warm`) — skips if already present
5. Downloads the Whisper ASR model and whisper-server binary
6. Downloads the Piper TTS binary and voice model (`en_US-lessac-medium`)

```bash
pond-server setup
pond-server setup --model small   # higher accuracy ASR
```

---

## `serve`

Starts the HTTP server (REST API + web dashboard) on port 4000 by default.

```bash
pond-server serve [--port PORT] [--open] [--debug] [--agent goose|mock]
```

| Flag | Default | Description |
|---|---|---|
| `--static-dir PATH` | `pond-desktop/dist` | Path to the built web dashboard assets. Run `cd pond-desktop && npm run build` first. |
| `--open` | off | Open the dashboard in the default browser after startup |
| `--debug` | off | Enable `DEBUG`-level tracing (third-party crates capped at `WARN`) |
| `--agent` | `goose` | Agent backend. `goose` runs the full Block Goose agentic loop with MCP tool calls. `mock` returns instant echo responses (no LLM required — useful for development). |

```bash
# Standard production start
pond-server serve --open

# Development / testing
pond-server serve --agent mock --debug

# Custom dashboard build location
pond-server serve --static-dir /var/www/giap/dist
```

`RUST_LOG` overrides `--debug`:
```bash
RUST_LOG=pond_api=debug,sqlx=info pond-server serve
```

---

## `chat`

Interactive terminal chat loop. Cycles through the `Wait → Listen → Think → Speak` state machine. Press `Ctrl-C` or send EOF to exit.

```bash
pond-server chat [OPTIONS]
```

| Flag | Short | Default | Description |
|---|---|---|---|
| `--provider` | `-P` | from Settings | LLM backend: `mock`, `llamafile`, `ollama`, or `local` (requires `--features local-inference`) |
| `--model` | `-M` | from Settings | Model name for `--provider ollama` (e.g. `llama3.2`, `gemma2`) |
| `--input` | `-I` | `stdin` | Input source: `stdin` (keyboard) or `whisper` (microphone → ASR) |
| `--wake-word` | | from Settings | Custom wake word phrase |
| `--no-wake-word` | | off | Skip wake-word detection; activate immediately on each turn |
| `--tts` | | from Settings | TTS engine: `piper` or `none` (text only) |
| `--tts-model PATH` | | auto-detected | Path to a Piper `.onnx` voice model |

```bash
# Keyboard chat with mock LLM (no inference needed)
pond-server chat --provider mock --no-wake-word --tts none

# Keyboard chat using Ollama (must have Ollama running)
pond-server chat --provider ollama --model llama3.2 --no-wake-word --tts none

# Pipe a single message (non-interactive)
printf 'What is the weather today?\n' | pond-server chat --provider mock --no-wake-word --tts none

# Full voice loop (requires Whisper + Piper)
pond-server chat --provider llamafile --input whisper --tts piper

# Use settings from DB (provider, model, wake word all come from onboarding)
pond-server chat
```

---

## `status`

Prints a summary of the current system state: database path, assistant settings, and onboarding status.

```bash
pond-server status
```

No flags. Example output:

```
  ┌─ Goose In A Pond ──────────────────────────────────┐
  │  Database:  /home/user/.local/share/goose-in-a-pond/pond_system.db
  │  Assistant: Goose | Provider: ollama | Model: gemma-4-E4B-it-Q4_K_S
  │  Wake word: hey goose
  │  Onboarded: Some(Completed)
  └────────────────────────────────────────────────────┘
```

---

## `onboard`

Runs the interactive onboarding wizard that collects your name, timezone, personality style, wake word, and preferred model, then persists everything to the database.

```bash
pond-server onboard [--reset]
```

| Flag | Description |
|---|---|
| `--reset` | Wipe the existing onboarding record and restart all steps from the beginning |

The wizard steps (in order):

1. **Welcome** — auto-advances
2. **Basics** — your name
3. **Location** — timezone (e.g. `Africa/Nairobi`)
4. **Accessibility** — auto-advances
5. **Personality** — choose `1` balanced / `2` concise / `3` technical / `4` warm
6. **GooseIdentity** — assistant name (blank = `Goose`)
7. **WakeWord** — choose a preset or type a custom phrase
8. **Model** — pick from the downloaded model catalog
9. **Extensions** — auto-advances
10. **Completed** — settings are written to DB; main menu appears

```bash
pond-server onboard          # first run or continue incomplete wizard
pond-server onboard --reset  # start over

# Non-interactive (CI / testing) — pipe answers:
printf 'Alice\nAfrica/Nairobi\n1\n\n1\n1\n4\n' | pond-server onboard --reset
```

After onboarding, verify with `pond-server status`.

---

## `models`

Browse the model catalog, download models, delete local files, and assign models to roles.

### `models list`

```bash
pond-server models list [--category CATEGORY]
```

Lists all catalog entries with download status and active role assignment.

| Flag | Description |
|---|---|
| `--category` | Filter: `gguf`, `llamafile`, `whisper`, `tts`, `ollama` |

```bash
pond-server models list                    # all categories
pond-server models list --category gguf   # GGUF chat models only
pond-server models list --category tts    # TTS voice models only
```

Example output:

```
Category     Name                        Downloaded  Role
─────────────────────────────────────────────────────────
gguf         gemma-4-E4B-it-Q4_K_S      yes         chat
gguf         hermes-7b-gguf             no          —
whisper      tiny                       no          —
whisper      small                      yes         asr
tts          piper-lessac               yes         tts
```

### `models download`

Downloads a model file to disk. The URL and expected size come from the catalog.

```bash
pond-server models download <CATEGORY> <NAME>
```

```bash
pond-server models download gguf gemma-4-E4B-it-Q4_K_S
pond-server models download whisper small
pond-server models download tts piper-ryan
```

> **Note**: GGUF models can be multiple gigabytes. Ensure sufficient disk space.

### `models delete`

Removes the model file from disk. The catalog record is kept so the model can be re-downloaded later.

```bash
pond-server models delete <CATEGORY> <NAME>
```

```bash
pond-server models delete gguf hermes-7b-gguf
```

### `models activate`

Assigns a downloaded model to a role. The role is then used by the agent on the next turn.

```bash
pond-server models activate <CATEGORY> <NAME> --role <ROLE>
```

Available roles:

| Role | Used for |
|---|---|
| `chat` | Primary conversational LLM |
| `think` | Extended reasoning / planning LLM |
| `task` | Background task execution LLM |
| `asr` | Automatic speech recognition (Whisper) |
| `tts` | Text-to-speech voice output |

```bash
pond-server models activate gguf gemma-4-E4B-it-Q4_K_S --role chat
pond-server models activate whisper small --role asr
pond-server models activate tts piper-lessac --role tts
```

### Model workflow (end-to-end)

```bash
# 1. See what is available and what is already on disk
pond-server models list

# 2. Download a model
pond-server models download gguf hermes-7b-gguf

# 3. Assign it to the chat role
pond-server models activate gguf hermes-7b-gguf --role chat

# 4. Verify the assignment
pond-server models list --category gguf

# 5. Roll back to previous model
pond-server models activate gguf gemma-4-E4B-it-Q4_K_S --role chat
```

---

## `prompts`

Manage the DB-stored system prompt templates. Templates are injected into the agent on every turn. There are four built-in templates (`balanced`, `concise`, `technical`, `warm`) and you can add custom ones via the API.

### `prompts list`

```bash
pond-server prompts list
```

Lists all templates with their name, whether they are system-provided, and a short description.

### `prompts show`

```bash
pond-server prompts show <NAME>
```

Prints the full text of a template.

```bash
pond-server prompts show balanced
pond-server prompts show technical
```

### `prompts reset`

Re-seeds a built-in template to its factory default, overwriting any edits made via the API.

```bash
pond-server prompts reset <NAME>
```

```bash
pond-server prompts reset balanced
pond-server prompts reset warm
```

> **Tip**: Custom (non-system) templates cannot be reset; delete them via `DELETE /api/v1/prompts/{name}` instead.

---

## `skills`

User skills are short Markdown instruction snippets injected into the agent's system prompt on every turn. Use them to give the agent standing instructions (e.g. "always respond in Swahili", "call the weather tool before answering questions about outdoor plans").

### `skills list`

```bash
pond-server skills list [--all]
```

By default only active skills are shown. Pass `--all` to include disabled ones.

```bash
pond-server skills list
pond-server skills list --all
```

Example output:

```
UUID                                  Name             Active
──────────────────────────────────────────────────────────────
3f1a2b4c-…                            morning_brief    yes
9e8d7c6b-…                            light_control    no
```

### `skills add`

```bash
pond-server skills add <NAME> [--content "MARKDOWN TEXT"]
```

Creates a new skill. Omit `--content` to read the instruction text from stdin.

```bash
# Inline content
pond-server skills add morning_brief \
  --content "When asked for a briefing, call giap__get_current_weather first."

# Pipe from file
cat ~/skills/morning.md | pond-server skills add morning_brief

# Interactive stdin
pond-server skills add custom_skill
# (type content, press Ctrl-D when done)
```

Output on success:
```
✓ Skill 'morning_brief' created (id: 3f1a2b4c-...)
```

### `skills toggle`

Enables a disabled skill or disables an active one.

```bash
pond-server skills toggle <UUID>
```

```bash
pond-server skills toggle 3f1a2b4c-0000-0000-0000-000000000001
```

### `skills remove`

Permanently deletes a skill from the database.

```bash
pond-server skills remove <UUID>
```

```bash
pond-server skills remove 3f1a2b4c-0000-0000-0000-000000000001
```

> **Note**: Removing a non-existent UUID exits with code 0 (SQLite DELETE is a no-op).

### Skills CRUD cycle

```bash
# Add
pond-server skills add smoke_skill --content "Test instruction"

# Verify (note the UUID)
pond-server skills list --all

# Disable
pond-server skills toggle <UUID>

# Re-enable
pond-server skills toggle <UUID>

# Delete
pond-server skills remove <UUID>
```

---

## `memories`

Memory fragments are short text snippets persisted in the database and optionally injected into the system prompt (`settings.agent_memory_inject = true`). The agent can also create and search memories autonomously via the `giap__save_memory` / `giap__recall_memories` MCP tools.

### `memories list`

```bash
pond-server memories list [--limit N]
```

Prints recent fragments, newest last.

| Flag | Default | Description |
|---|---|---|
| `--limit` | `20` | Maximum number of fragments to show |

```bash
pond-server memories list
pond-server memories list --limit 50
```

### `memories add`

```bash
pond-server memories add "<TEXT>"
```

```bash
pond-server memories add "User prefers temperatures in Celsius"
pond-server memories add "Family dinner is every Sunday at 7pm"
```

Output:
```
✓ Memory saved (id: 7c3e9f01-...)
```

### `memories remove`

```bash
pond-server memories remove <UUID>
```

```bash
pond-server memories remove 7c3e9f01-0000-0000-0000-000000000001
```

> **Note**: Removing a non-existent UUID exits with code 0.

---

## `recipes`

Recipes are Goose YAML automations stored in the database. Import a recipe file, inspect its content, and remove it when no longer needed. Recipes can be triggered by the agent via the `giap__get_recipe` MCP tool.

### `recipes list`

```bash
pond-server recipes list
```

Example output:

```
Name              Description
──────────────────────────────────────────
morning_brief     Daily briefing automation
```

### `recipes import`

```bash
pond-server recipes import <NAME> <FILE> [--description "TEXT"]
```

```bash
pond-server recipes import morning_brief ~/recipes/brief.yaml \
  --description "Fetch weather and calendar, then read a summary"
```

Output:
```
✓ Recipe 'morning_brief' imported
```

### `recipes show`

```bash
pond-server recipes show <NAME>
```

Prints the raw YAML content of the recipe.

```bash
pond-server recipes show morning_brief
```

### `recipes remove`

```bash
pond-server recipes remove <NAME>
```

```bash
pond-server recipes remove morning_brief
```

Output:
```
✓ Recipe 'morning_brief' deleted
```

> **Note**: Removing a recipe that does not exist exits with code 1.

---

## `agent`

One-shot commands that go through the full Goose agentic loop without starting the HTTP server. Useful for scripting or quick manual tests.

### `agent chat`

```bash
pond-server agent chat "<MESSAGE>" [--session SESSION_ID]
```

| Flag | Default | Description |
|---|---|---|
| `--session` | `cli-agent` | Session ID for conversation continuity. Reuse the same ID across calls to maintain history. |

```bash
# Single question
pond-server agent chat "What is the weather today?"

# Stateful conversation
pond-server agent chat "Remember: I am vegetarian" --session user-prefs
pond-server agent chat "Suggest a dinner recipe"    --session user-prefs

# Quick test (verify the agent loop is wired)
pond-server agent chat "What tools do you have?" --session smoke-test
```

The agent has access to all nine GIAP MCP tools (`giap__get_current_weather`, `giap__list_registered_devices`, etc.) and any additional MCP extensions loaded in Settings.

### `agent tools`

Lists the MCP tool extensions currently known to the agent, grouped by extension name.

```bash
pond-server agent tools
```

Example output:

```
Extension: giap (9 tools)
  giap__get_current_weather
  giap__list_registered_devices
  giap__list_schedules
  giap__get_user_profile
  giap__get_model_assignments
  giap__recall_memories
  giap__save_memory
  giap__get_recipe
  giap__list_skills
```

### `agent extras`

Lists all system prompt extras stored in the database (key/instruction pairs injected on every agent turn).

```bash
pond-server agent extras
```

---

## Environment variables

| Variable | Description |
|---|---|
| `POND_DATA_DIR` | Override the data directory (useful for testing and multi-instance setups) |
| `RUST_LOG` | Tracing filter (e.g. `debug`, `pond_api=debug,sqlx=warn`) |
| `OLLAMA_HOST` | Ollama server URL (default: `http://127.0.0.1:11434`) |

```bash
# Isolated test environment
POND_DATA_DIR=/tmp/test-giap pond-server setup
POND_DATA_DIR=/tmp/test-giap pond-server status

# Verbose logging
RUST_LOG=debug pond-server serve --agent mock
```

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success (or silent no-op, e.g. deleting a non-existent memory) |
| `1` | Command-level error printed to stderr (e.g. `recipes remove` on unknown name, DB init failure) |
| `101` | Rust panic (should not happen in production) |
