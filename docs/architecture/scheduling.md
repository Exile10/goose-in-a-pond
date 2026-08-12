# Scheduling System

Automation engine that executes LLM prompts or webhooks on recurring cron schedules, and fires sensor rules on matching EventBus traffic.

## Architecture

```
                ┌─────────────────────┐
                │  MCP Tools (12)     │ create, list, delete,
                │  REST API (8 routes)│ pause, resume, run-now,
                └────────┬────────────┘ get-runs, update, rules, clock
                         │
              ┌──────────▼──────────┐
              │   SchedulerPort     │  pond-core port trait
              │   (trait object)    │
              └──────────┬──────────┘
                         │
              ┌──────────▼──────────┐
              │ CronSchedulerAdapter│  pond-infra-scheduler
              │  tokio-cron-sched   │  JSON file persistence
              │  JsonRunHistory     │  run execution logs
              └──────────┬──────────┘
                         │ on cron fire
              ┌──────────▼──────────┐
              │  ScheduleExecutor   │  pond-core port trait
              │  (DeferredExecutor) │
              └──────────┬──────────┘
                         │
              ┌──────────▼──────────┐
              │AgentScheduleExecutor│  pond-server
              │  agent.chat()       │  ephemeral session
              │  Semaphore(N)       │  configurable concurrency
              └─────────────────────┘
```

## Domain Types (`pond-core/src/user_data/domain/schedule.rs`)

- **`TaskKind`** — `AgentPrompt { prompt }` (sends to LLM), `Webhook { webhook_url }` (HTTP POST), or `SensorTrigger(SensorTriggerSpec)` (#92 — fires on matching EventBus traffic rather than a cron cadence; `TaskKind::is_event_triggered()` tells the scheduler to skip cron registration, and the rules engine invokes it through `run_now`)
- **`SensorTriggerSpec`** — `source`, `condition` (numeric comparison plus a local-time window that wraps midnight and fails closed on malformed input), `actions`, `cooldown_secs` (default 60). Actions are `AgentPrompt`, `DevicePower { device_id, on }` and `Notify { title, body }`
- **`Schedule`** — id, label, cron (6-field), timezone (IANA), kind, paused, currently_running, last_run, next_run, created_at
- **`ScheduleRun`** — id, schedule_id, status (Running/Completed/Failed), result, error, started_at, finished_at, duration_ms
- **`ScheduleResultEvent`** — broadcast event for SSE delivery to desktop

## Cron Format

6-field: `<sec> <min> <hour> <day-of-month> <month> <day-of-week>`

Examples:
- `0 0 8 * * *` — daily at 8:00 AM
- `0 30 * * * *` — every hour at :30
- `0 0 9 * * 1` — every Monday at 9:00 AM

## Execution Flow

1. `tokio-cron-scheduler` fires the job at the scheduled time
2. Job closure marks the task as `currently_running`
3. `JsonRunHistory::record_start()` creates a run record
4. `ScheduleExecutor::execute()` dispatches based on `TaskKind`:
   - **AgentPrompt**: creates ephemeral session (`sched-{id}-{timestamp}`), calls `agent.chat()`
   - **Webhook**: HTTP POST to the configured URL
5. `JsonRunHistory::record_finish()` records result/error + duration
6. `ScheduleResultEvent` broadcast via `tokio::sync::broadcast` channel
7. Task marked as not-running, `last_run` updated

## Deferred Executor Pattern

The scheduler needs `Arc<dyn Agent>` but is created before the agent (circular dependency). Solution:

```
DeferredExecutor (empty OnceCell)
    → CronSchedulerAdapter::new(deferred_executor)
    → agent = build_goose_backend(scheduler.clone())
    → deferred_executor.init(AgentScheduleExecutor::new(agent))
```

## Natural Language Scheduling

The tool classifier routes "schedule to get weather at 10am every morning" → `create_schedule` tool → `parse_cron_from_message()` extracts:
- Time: "at 10 am" → hour=10
- Frequency: "every morning" → daily
- Prompt: "get weather" (stripped of scheduling words)
- Result: cron=`0 0 10 * * *`, prompt="Get weather"

## MCP Tools

| Tool | Description |
|------|-------------|
| `list_schedules` | List all with timezone, kind, status |
| `create_schedule` | Create from name + cron + prompt + timezone |
| `delete_schedule` | Delete by ID |
| `pause_schedule` | Pause (stops firing) |
| `resume_schedule` | Resume paused schedule |
| `run_schedule_now` | Trigger immediate execution |
| `get_schedule_runs` | Execution history with status + result |
| `update_schedule` | Modify an existing schedule in place |
| `create_sensor_rule` | Create an event-triggered rule (source, condition, actions, cooldown) |
| `list_sensor_rules` | List sensor rules |
| `delete_sensor_rule` | Delete a sensor rule by ID |
| `world_clock` | Current time in a given timezone |

Sensor rules have no dedicated REST surface today — they are created by POSTing a `SensorTrigger` kind to `/api/v1/schedules`, so the MCP tools above are the practical interface.

## REST API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/schedules` | List all schedules |
| POST | `/api/v1/schedules` | Create (accepts name, cron, prompt, timezone, kind) |
| GET | `/api/v1/schedules/upcoming` | Active schedules sorted by next fire |
| DELETE | `/api/v1/schedules/{id}` | Delete |
| POST | `/api/v1/schedules/{id}/pause` | Pause |
| POST | `/api/v1/schedules/{id}/resume` | Resume |
| POST | `/api/v1/schedules/{id}/run-now` | Fire immediately (202 Accepted) |
| GET | `/api/v1/schedules/{id}/runs` | Execution history |
| GET | `/api/v1/schedules/events` | SSE stream of completion events |

## Persistence

- **Task definitions**: `$DATA_DIR/schedules.json` — survives restarts via rehydration
- **Run history**: `$DATA_DIR/schedule_runs.json` — bounded (configurable `schedule_max_runs_per_task`, default 50)

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `schedule_result_notify` | true | Broadcast completion events via SSE |
| `schedule_max_concurrent` | 2 | Max parallel scheduled task executions |
| `schedule_max_runs_per_task` | 50 | Max retained run history entries per schedule |

## Desktop UI

The Schedules section (`pond-desktop/src/sections/Schedules.tsx`) provides:
- Create modal with frequency presets, cron input, timezone selector, recipe presets
- Schedule cards with enable/disable toggle, run-now button, delete
- Expandable run history per card with status badges (completed/failed/running)
- Live SSE notifications when scheduled tasks complete
