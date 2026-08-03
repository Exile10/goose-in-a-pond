# Token Usage Tracking

Per-session token accumulation with estimated usage from the agent stream, aggregate statistics, and cost savings comparison.

## Flow

```
  Agent Stream (GooseAdapter)
        │
        │ tracks system_prompt_len + accumulated output chars
        ▼
  AgentStreamEvent::Done { usage: Some(UsageStats) }
        │
        │ chars / 4 heuristic → estimated tokens
        ▼
  routes.rs chat_stream handler
        │
        ├─► SSE done event: { usage: { prompt_tokens, completion_tokens } }
        │
        └─► session_storage.increment_usage(session_id, prompt, completion, model_name)
                │
                ▼
          sessions table (total_prompt_tokens += N, total_completion_tokens += N)
```

## Estimation

Real token counts **are** available and are the primary path. The GooseAdapter aggregates the provider's per-inference `Usage` events into `TurnStats` (`crates/pond-core/src/shared/domain/turn_stats.rs`) across every inference in a turn, and reports those counts directly:

```rust
let usage = if saw_usage {
    UsageStats { prompt_tokens: turn_stats.prompt_tokens,
                 completion_tokens: turn_stats.completion_tokens }
} else { /* heuristic fallback, below */ };
```

`prompt_tokens` is the size of the **final** inference — the turn's real context load — while `completion_tokens` is summed across the turn. Per-message counts persist via migration `0029_message_token_counts.sql`.

The characters-per-token heuristic survives only as a fallback for providers that emit no `Usage` events at all (some HTTP providers):

```
prompt_tokens  ≈ user_message.len() / 4
completion_tokens ≈ accumulated_output_chars / 4
```

That fallback is approximate (~80% accurate for English text), and the UI labels estimates with a "~" prefix. Counts sourced from real provider usage carry no "~".

## Per-Session Storage

Migration `0016_session_usage.sql` adds:
```sql
ALTER TABLE sessions ADD COLUMN total_prompt_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN total_completion_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN model_name TEXT;
```

`SessionStorage::increment_usage()` atomically adds tokens after each chat response:
```sql
UPDATE sessions SET
    total_prompt_tokens = total_prompt_tokens + ?,
    total_completion_tokens = total_completion_tokens + ?,
    model_name = COALESCE(?, model_name)
WHERE id = ?
```

## API

### `GET /api/v1/usage/summary`

Aggregates all sessions:
```json
{
  "total_prompt_tokens": 12500,
  "total_completion_tokens": 8200,
  "total_tokens": 20700,
  "session_count": 15,
  "cloud_input_price_per_million": 2.50,
  "cloud_output_price_per_million": 10.00
}
```

Pricing values come from settings (`cloud_input_price_per_million`, `cloud_output_price_per_million`).

### `GET /api/v1/sessions`

Includes per-session tokens:
```json
{
  "sessions": [{
    "id": "...",
    "title": "Weather chat",
    "total_prompt_tokens": 500,
    "total_completion_tokens": 300,
    "model_name": "gemma3:4b",
    "created_at": "...",
    "updated_at": "..."
  }]
}
```

## Cost Savings Calculation

The Dashboard "Usage & Savings" card compares on-device inference ($0) with cloud API pricing:

```
saved = (prompt_tokens / 1,000,000) * cloud_input_price
      + (completion_tokens / 1,000,000) * cloud_output_price
```

Default comparison: GPT-4o ($2.50/M input, $10.00/M output). Configurable via settings.

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `cloud_input_price_per_million` | 2.50 | Cloud API input token price per million |
| `cloud_output_price_per_million` | 10.00 | Cloud API output token price per million |
