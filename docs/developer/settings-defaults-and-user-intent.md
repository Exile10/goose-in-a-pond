# Settings defaults, and how a default change reaches an existing install

## The failure mode

`Settings` is persisted as a flat key-value table — `settings(key, value,
updated_at)`, one row per field — and `Settings::default()` only supplies a
value when the key is **absent**. So the moment anything writes a key, that
key is pinned to whatever the default was on that day, and every later change
to that default is invisible on that install. Forever.

This bit us for real. A maintainer's `pond_system.db` held explicit rows for
`agent_max_turns = 20` and `hybrid_compaction_enabled = false` long after both
defaults had moved. The new values shipped, CI was green, fresh installs got
them — and the machine the work was being tuned on never saw either.

Why the rows existed at all: `PUT /api/v1/settings` used to save the whole
merged object, and server-side writers (`sync_assignments_to_settings`,
onboarding, model activation) still write keys the user never opened a UI for.
One save was enough to pin all of them.

## Two halves of the fix

### 1. `is_user_set` — record intent, not just a value

Migration `0035_settings_user_intent.sql` adds `settings.is_user_set INTEGER
NOT NULL DEFAULT 0`.

The presence of a row means "something wrote this". The flag means "the user
deliberately chose this". Only one writer is entitled to set it: the
`update_settings` handler, using the key set of the request **patch**. That
set is trustworthy because `PUT /api/v1/settings` now writes only the keys the
request actually carries (`update_fields(&merged, Some(&write_keys))`).

> **The client half of this is load-bearing.** The marker is only as good as
> the patch, and a caller that PUTs the whole settings object marks *every*
> key on the first Save — permanently ending default adoption for that install,
> from a click that changed nothing. `PUT /api/v1/settings` is a **patch
> endpoint**: send the keys you changed and nothing else.
>
> The hub settings views (`hub/views/settings/*`) already do per-field patches.
> The classic `sections/Settings.tsx` panel batches edits behind one Save
> button, so it keeps the last server snapshot as a baseline and PUTs only the
> keys whose value differs from it (`diffSettings`); with no edits it makes no
> request at all. That also fixes a separate standing bug — a stale full-object
> Save used to revert whatever another surface (the Models tab, the phone, a
> calibration run) had changed since the panel loaded.
>
> Server-owned fields fall out of this for free.
> `voice_wake_word_transcriptions` is appended to by
> `POST /api/v1/voice/calibrate` and cleared by `DELETE`; the classic panel
> never sends it, because a field the user did not edit never enters the diff.
>
> Writes that bypass the Save button need the same discipline. The Calibrate
> button has to persist the wake phrase before recording against it, so it goes
> through `commitField`, which sends the key only when it differs from the
> baseline and then adopts the response. Unconditional, it marked
> `voice_wake_word` from a click that changed nothing.
>
> One more thing every diffing client has to handle: the response is not
> byte-identical to the patch for a float field. See *The f32 echo is not the
> literal you sent*, below.

Deliberately excluded from the marker:

- **Server-side snapshot writes** — `update()` / `update_fields(_, None)` and
  `set_key`. They pin values without a user ever deciding anything.
- **Server-derived values** — the geocoded `weather_latitude` /
  `weather_longitude` are added to `write_keys` but not to `user_keys`.
  Derived is not chosen.

Port surface (`SettingsRepository`):

| method | meaning |
|---|---|
| `mark_user_set(&HashSet<String>)` | the user chose exactly these keys |
| `is_user_set(&str)` | was this key user-chosen? |

Both have no-op trait defaults so the several test doubles across the
workspace need not carry the flag; the SQLite adapter is the implementation
that matters, and `MockSettingsRepository` mirrors it for in-core tests.

One adapter-level consequence: every settings write is now `INSERT … ON
CONFLICT(key) DO UPDATE`, never `INSERT OR REPLACE`. Replace deletes the row
and re-inserts it, which silently resets `is_user_set` to its column default.

### 2. `DEFAULT_ADOPTIONS` — a one-time backlog pass

`pond-core/src/user_data/domain/settings.rs` carries the registry of factory
defaults that changed after installs already existed:

```rust
pub const DEFAULT_ADOPTIONS: &[DefaultAdoption] = &[ /* key, old, new, migration */ ];
```

Migration 0035 adopts them with a deliberately narrow rule:

> If the stored value is still **byte-equal to the old default**, the user
> never chose anything different, so adopting the new default is safe.

```sql
UPDATE settings SET value = '50' WHERE key = 'agent_max_turns'
  AND value = '20' AND is_user_set = 0;
```

**The accepted false positive:** a user who deliberately set exactly the old
default is indistinguishable from one who never chose. Nothing in a
pre-0035 database records the difference. They get moved to the new default
once; their next explicit save marks the key and ends the ambiguity forever.
This was judged cheaper than leaving every install stuck on stale defaults.

**Not in the backlog, on purpose.** Note that the motivating database has an
explicit row for *both* of these, so "an absent key already gets the default"
is not the reason for either exclusion:

- `tool_selection_mode` — stored as `'all'`, and `default_tool_selection_mode()`
  still returns `"all"`. The default never moved, so there is nothing to adopt
  and an entry would be a no-op (the registry test rejects one).
- `embedding_provider` — stored as `'none'`, but
  `default_embedding_provider()` has returned `"fastembed"` since the field was
  introduced. `"none"` was therefore never a factory default, so no
  `old_default` this rule could key on ever matched it. It is not a declined
  adoption; it was never an eligible one. Moving that install onto embeddings
  needs a hand flip or a UI prompt.

## The f32 echo is not the literal you sent

`GET`/`PUT /api/v1/settings` serialize `Settings` with serde_json, which has no
`f32`: `serialize_f32` widens to `f64`. Every float field — `llm_temperature`,
`voice_kws_energy_threshold`, `memory_prune_threshold`,
`memory_archive_threshold`, `memory_decay_*` — therefore comes back as a
*different number* from the one the client sent. `0.7` echoes as
0.699999988079071.

This is faithful (it is exactly the `f32` the server holds) and it is not going
to be "fixed" on the server: rounding on the way out would report a value the
server does not hold.

**It is a trap for any client that diffs against the echo.** A naive panel does:
baseline ← echo, local ← what I sent. Those now disagree on every float, so the
field looks permanently edited: each Save re-sends it, and each re-send re-marks
it `is_user_set`, quietly ending default adoption for that key. Nothing about
the symptom points at floats — the field just never settles.

The rule for any such client: **after a successful PUT, fold against the patch
you sent, not the baseline you had.** In `sections/Settings.tsx`:

```ts
setSettings((prev) => foldServerState(prev, { ...baseline, ...patchBody }, updated));
```

The request succeeded, so those keys *are* what the server holds; the fold's
"has the user touched this?" question is only meaningful relative to them. The
widened echo is then adopted verbatim and the field converges on the first save.
As a bonus this is also what makes a server-side clamp or normalisation stick,
instead of the client re-asserting its rejected value forever.

What NOT to do: compare numbers with an epsilon, or with `Math.fround`. That
is a heuristic over *every* numeric field, and `Settings` holds integer counts
and durations too — `Math.fround` collapses distinct integers above 2^24, so a
real edit would silently vanish from the patch. Trading a converging float for
a dropped integer is not a fix.

The same asymmetry is why the registry's `new_default` literal cannot be checked
against a serde_json render — see the guarantee table below.

## Ordering and idempotency

- The migration runs inside `Database::init`, before anything reads settings,
  so the server's first read already sees adopted values.
- `ALTER TABLE` comes first (the UPDATEs reference the new column).
- Both UPDATEs are guarded on the stored value **and** on `is_user_set = 0`,
  so re-running the file — a restored backup, a re-applied migration — is a
  no-op after the first pass, and can never walk back a marked key.
- The marker is a **column**, not a `Settings` field, so it does not enter
  `every_settings_field_is_dispositioned` and needs no UI/TS mirror. It is
  storage metadata, not configuration.

## Adding the next default change

1. Change the `Settings::default_*()` function.
2. `every_adoption_entry_states_the_literal_the_adapter_writes` (pond-infra)
   now fails: the registry claims a value the code no longer produces. Its
   message spells out the entry to add.

   That check lives in pond-infra, not next to the registry, on purpose. It
   compares against a `Settings::default()` written through the **real SQLite
   adapter** and read back, because only the adapter knows how a field is
   rendered into the store. The domain crate cannot know: the adapter writes
   numbers with `Display`, a serde_json round-trip widens `f32` to `f64`, and
   the two disagree for every float (`0.05f32` stores as `0.05`, but renders as
   `0.05000000074505806` through JSON). A checker built on the JSON form would
   reject a correct float entry and demand a literal that no
   `WHERE value = '...'` guard could ever match — a registry that looks green
   and adopts nothing. pond-core keeps only the structural rules, which need
   field *names* and not field *rendering*.
3. **Append** a new `DefaultAdoption` entry with the new migration number.
   Never edit an existing entry — its migration has already run on real
   machines, and rewriting it would describe an adoption that never happened.
4. Add `00NN_*.sql` with the same guarded-UPDATE shape, one UPDATE per entry.
5. Run `cargo test -p pond-core -p pond-infra`. Nothing else to wire: both the
   replay tests and the SQL tie enumerate the registry and read the migration
   directory, so a new migration is picked up the moment it is registered.

### Moving a default that is already registered

Chaining is supported and is the *only* correct way to do it. The second entry
picks up where the first left off:

```rust
DefaultAdoption { key: "agent_max_turns", old_default: "20", new_default: "50", migration: "0035" },
DefaultAdoption { key: "agent_max_turns", old_default: "50", new_default: "80", migration: "0041" },
```

Migration 0041 guards on `value = '50'`, so it fires on installs that adopted
50 in 0035 *and* on installs that were created with 50 as their factory
default. Only the newest entry for a key states today's default; the pond-core
checker enforces ascending migration order and
`old_default[n] == new_default[n-1]` and reports which of those a broken
registry violates, and the pond-infra replay checks that the newest
`new_default` is what the store actually holds.

## What the tests actually guarantee

| Test | Crate | Guarantee |
|---|---|---|
| `default_adoptions_are_structurally_sound` | pond-core | Every key is a real `Settings` field, no entry is a no-op, and entries for a key chain in ascending migration order. Structure only — it says nothing about whether `new_default` is today's value, because this crate cannot render one (see step 2 above) |
| `adoption_defects_are_rejected` | pond-core | Each of those structural rules actually fires (the checker is not vacuous) |
| `every_adoption_entry_states_the_literal_the_adapter_writes` | pond-infra | Writes `Settings::default()` through the real adapter and reads the rows back: the newest entry for each key states the literal an install actually stores. This is where a moved default becomes loud |
| `every_adoption_entry_has_matching_migration_sql` | pond-infra | A **textual** tie, not a semantic one. Forward: for each entry, its named migration file contains an UPDATE whose normalised text contains `key = '<key>'`, `AND value = '<old>'`, `SET value = '<new>'` and `AND is_user_set = 0`. Backward: the file's adoption-UPDATE count equals the number of entries naming it. It does not parse SQL, so it cannot prove the clauses belong to one coherent statement — what it does prove is that no entry is unimplemented, no UPDATE is unregistered, and no guard was dropped or turned into a `SET is_user_set = 0` assignment |
| `migration_lookup_resolves_to_the_shipped_files` | pond-infra | The directory scan behind the forward tie resolves to the real migrations, so the tie cannot pass vacuously |
| `migration_adopts_defaults_the_user_never_chose` / `..._leaves_a_deliberately_different_value_alone` / `migration_is_idempotent` | pond-infra | Replay the **shipped** SQL against seeded pre-migration rows: adopts through to the newest value, skips a deliberately different one, is a no-op on a second pass, and re-adopts nothing once **every** registered key is marked (marking one key only proved that key's guard) |
| `only_a_patch_write_records_user_intent` | pond-infra | Snapshot writers claim nothing; `ON CONFLICT` writes do not reset the marker |
| `Settings save payload` suite (`sections/Settings.test.tsx`) | pond-desktop | A Save with no edits makes no request; a Save of one field sends exactly that field; a server-owned field is never re-sent; a **float** field converges despite the widened echo; Calibrate re-sends an unchanged wake phrase never, and an edited one exactly once. Without these the marker is defeated at the client, whatever the server does |
| `diffSettings` / `foldServerState` suites | pond-desktop | Arrays and maps compare by value not identity, and a background refresh moves the baseline with it so untouched fields stay out of the next patch |

Step 2 is what makes this class of bug loud instead of silent: you cannot move
a registered default without being told that existing installs will not see it.
