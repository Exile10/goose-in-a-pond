# pond-infra

SQLite adapters for `pond-core`'s storage ports, plus the few other local-infrastructure adapters
(secret encryption, push relay, embeddings).

## Databases

`src/db.rs` opens three files in the data directory, each with its own migration set:

| Database | Migrations | Holds |
|---|---|---|
| `pond_system.db` | `migrations/system/` | Everything authoritative: settings, chat history, memory, profile, skills, schedules, devices, pairing |
| `pond_logs.db` | `migrations/logs/` | Event and operational logs, sensor readings, camera events, turn metrics |
| `pond_vectors.db` | `migrations/vectors/` | Embedding chunks for retrieval |

Queries use runtime `sqlx::query`, not the compile-time macros, so the build needs no database, there
is no `.sqlx/` directory, and `cargo sqlx prepare` and `SQLX_OFFLINE` are never needed.

Settings are one row per field in a key-value table. A new field needs both an `upsert!` in
`update_fields` and an arm in `apply_key` in `src/sqlite_settings.rs`, or it saves and then vanishes on
the next read; `roundtrip_persists_every_field` guards this.

A sidecar binary older than the database's applied migrations refuses to start. Restage it with
`bash scripts/stage-server-sidecar.sh` after any migration change.

## Tests

Each test applies every migration to a fresh file. That does not exercise migrating a database that
already has rows; `scripts/live-test.sh` does.

```bash
cargo test -p pond-infra
```
