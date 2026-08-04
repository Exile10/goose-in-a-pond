# PAI-1 — Identity and hard profile boundaries

Requirement: *hard profile boundaries.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: none — this is the substrate everything else personal stands on.

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 The type exists and does almost nothing

`Profile` — `crates/pond-core/src/user_data/domain/profile.rs` — six fields (I miscounted this as
five originally; the quoted block below has always shown six):

```rust
pub struct Profile {
    pub id: String,
    pub display_name: String,
    pub avatar_emoji: String,
    pub preferences: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

No role, no PIN, no auth binding. The doc comment calls it "a household member". Port at
`user_data/ports/profile.rs:9-19`, adapter `pond-infra/src/sqlite_profile.rs`, table from
`migrations/system/0003_profiles.sql`. REST at `routes.rs:102-103,165-166` — note `POST /profiles`
and `PATCH /profiles/{id}` are on the **public** allowlist (`middleware/mod.rs:193,199-200`) for
onboarding.

### 1.2 Memory is one global pool

`MemoryFragment.profile_id: Option<String>` exists (`user_data/domain/memory.rs:100-101`) and
`sqlite_memory.rs:45,109,192-198` genuinely branches on it. Every production caller passes `None`:

| Call site | Function |
|---|---|
| `pond-adapters-goose/src/goose_agent.rs:1643` | `search_similar(&query_vector, None, limit)` |
| `pond-adapters-goose/src/goose_agent.rs:1674` | `search_by_content(&keywords, None, limit)` |
| `pond-adapters-goose/src/goose_agent.rs:2109` | `search_recent(None, limit)` |
| `pond-mcp-server/src/memory.rs:100,133,372,443` | recall / list / forget paths |
| `pond-mcp-server/src/memory.rs:292` | writes fragments with `profile_id: None` |
| `pond-core/src/user_data/services/memory_extraction.rs:84,206` | dedup window and semantic neighbours |

So the column is a schema that has never been used. Every household member's memories are visible
to every other one, and extraction attributes nothing.

### 1.3 Sessions have no owner

`Session` — `user_data/domain/session.rs:61-77` — is `id`, `title`, two token counters,
`model_name`, `created_at`, `updated_at`. There is no `profile_id`.

### 1.4 The identification endpoint writes to a map nobody reads

`POST /sessions/{id}/identify-user`, `GET|DELETE /sessions/{id}/user` (`routes.rs:296-305`,
handlers `routes.rs:11216-11272`) write into
`AppState.session_user_bindings: Arc<RwLock<HashMap<String, String>>>` (`pond-api/src/lib.rs:266`).
Grep shows the map is touched at exactly three places outside test fixtures — `routes.rs:11236`,
`:11255`, `:11268` — which are the three handlers themselves. **No chat, prompt, memory or tool
path consults it**, and it is process-local, so it dies on restart.

### 1.5 Only one profile reaches the model

`routes.rs:1191-1205` builds a `ProfileContext { preferred_name, birthday, language,
atypical_speech }` from `settings.primary_profile_id` alone, rendered by `prompts.rs:1059-1103` and
`models/services/prompt_builder.rs:148-179`. Other household members never influence the prompt,
and the primary member's context is used even when someone else is talking.

### 1.6 Face recognition is real; speaker identification is not

`pond-adapters-face-onnx` is a complete implementation — ArcFace-512 / MobileFaceNet-128 via `ort`,
SCRFD and UltraFace detectors, Umeyama alignment, and a Silent-Face MiniFASNet anti-spoofing
ensemble (`antispoof.rs`, `antispoof_onnx.rs`). Raw images are never retained; only float vectors
(`user_data/domain/face_recognition.rs:1-5,21-22`). Twelve REST routes at `routes.rs:279-296`,
per-profile thresholds in migration `0014`.

Speaker identification is designed in `docs/architecture/data_pipeline.md:502-577` — `speaker_id.rs`,
a `speaker_embeddings` table, `diarization_logs`, `biometric_audit_log`, a
`pond-adapters-speaker-embed` crate. **None of it exists.** `delete_user_biometrics` already carries
a comment saying `voice_prints_deleted` "will be populated once Phase 1 lands" (`routes.rs:11207`).

### 1.7 The enforcement hook is already designed

`security/ports/policy.rs:38-111` defines eight scope constants — `MEMORY`, `SETTINGS`, `SECRETS`,
`PROFILE`, `SESSION`, `SCHEDULE`, `SENSOR`, `CAMERA` — plus
`Principal` / `PrincipalKind { Loopback, Token, Internal }`. This is the right shape and it is
inert (see [PAI-2](./02-privacy-and-security-guardrails.md)).

---

## 2. The gap

GIAP can tell you who is in front of the camera and cannot use that fact for anything. It stores a
`profile_id` on every memory and always writes `None`. It has an identification endpoint whose
output is discarded. The product claim "your household assistant" is currently "your household's
shared assistant, and everyone reads everyone's mail".

---

## 3. Design

### 3.1 `ProfileScope` — the unit of ownership

New in `pond-core/src/user_data/domain/profile.rs`:

```rust
/// Who a piece of data belongs to.
pub enum ProfileScope {
    /// One member. Only that member (and an explicitly authorised principal) may read it.
    Owner(String),
    /// Deliberately shared with the whole household — the shopping list, the thermostat.
    Household,
    /// An unidentified speaker. Readable by nobody; discarded on session end unless promoted.
    Guest,
}
```

Carried by memory fragments, sessions, drafts, schedules, notifications and (in
[PAI-8](./08-personal-context-streaming.md)) context items. `Household` is not "no owner" — it is a
positive assertion, which is what makes `NULL` meaning "unclassified legacy row" safe to treat
conservatively during backfill.

### 3.2 Sessions get an owner and a provenance

Migration `0037_session_profile.sql`:

```sql
ALTER TABLE sessions ADD COLUMN profile_id TEXT REFERENCES profiles(id) ON DELETE SET NULL;
ALTER TABLE sessions ADD COLUMN identification_source TEXT;  -- paired_device | face | explicit | unknown
ALTER TABLE sessions ADD COLUMN identification_confidence REAL;
```

`identification_source` matters as much as `profile_id`. A session bound because a paired phone
presented that member's token is a much stronger claim than one bound by a 0.62-confidence face
match, and the policy layer must be able to tell them apart. This replaces
`AppState.session_user_bindings` entirely — the in-memory map is deleted, not wrapped.

Resolution order when a turn arrives:

1. **Paired device** — the bearer token maps to a device, the device maps to a profile. Strongest.
2. **Explicit** — the user said "this is Liz" or picked a profile in the UI.
3. **Face** — a `FaceIdentification` above the per-profile threshold, anti-spoof passed, within a
   freshness window.
4. **Unknown** → `Guest`.

### 3.3 `Principal` carries the profile

`security/ports/policy.rs` — `Principal` gains `profile_id: Option<String>` and
`identification_source`. This is the single value every enforcement decision in PAI-2 keys on.

### 3.4 The hard boundary

The rule, stated once so every implementation can be checked against it:

> A read returns rows whose scope is `Owner(current)` or `Household`. A write is stamped
> `Owner(current)`, or `Household` only when the caller says so explicitly. Any access to
> `Owner(other)` is a `SecurityPolicy` decision that is **denied by default**.

Concretely, `MemoryRepository::search_*` stop taking `Option<&str>` and start taking a
`&ProfileScope` — so passing "everything" becomes a deliberate, greppable act rather than the path
of least resistance that produced the current `None` everywhere.

Cross-profile access is the **first production call site** for `SecurityPolicy::allow` and
`::audit`. That is deliberate: it gives the inert policy port a real, testable job with a clear
failure mode.

### 3.5 Guest degradation

An unidentified speaker gets: no personal memory injection, no personal-data tool groups
(`giap-memory` reads are scoped to `Household`), no draft approval rights, and no ingested context
items. It still gets weather, time, device control and knowledge tools — the assistant stays useful
to a visitor without becoming a disclosure channel.

This is the behaviour that makes voice safe before speaker identification exists.

### 3.6 The prompt carries the current member, not the primary one

`ProfileContext` is built from the session's resolved profile. `settings.primary_profile_id` becomes
the fallback for `Household`-scope and unattributed contexts only.

**KV-prefix constraint:** profile context already rides the user message's `<system-context>` block,
not the system prefix — that placement is correct and must not change (see the programme's
invariant 1). Switching speakers mid-session therefore costs nothing in prefill.

### 3.7 Deletion actually deletes

`DELETE /users/{profile_id}` today clears biometrics only (`routes.rs:11185-11205`). It must cascade
memories, sessions, drafts, schedules and context items owned by that profile, and report counts per
category. `Household`-scoped data survives — deleting a member does not delete the shopping list.

### 3.8 Speaker identification is scoped out, and said so

Voice is a shared surface. Until `pond-adapters-speaker-embed` exists, a voice session resolves to
`Household` (if a face is co-present and confident) or `Guest`. **This document does not claim
per-person voice memory works.** The speaker-ID design in `data_pipeline.md:502-577` is the
follow-on; PAI-1 makes it a drop-in by putting `identification_source` in place first.

---

## 4. Phases

- **P1** `ProfileScope` domain type; `MemoryRepository` signature change from `Option<&str>` to
  `&ProfileScope`; all current call sites pass `ProfileScope::Household` so behaviour is
  **byte-identical** on landing. This is the refactor that makes every later phase small.
- **P2** Migration `0037`; `Session.profile_id` + `identification_source` + confidence; delete
  `AppState.session_user_bindings` and repoint its three handlers at the column.
- **P3** Resolution chain (paired device → explicit → face → guest) computed once per turn and
  threaded through `AgentRequest` into `ChatService`.
- **P4** Enforcement: reads and writes honour the scope; cross-profile access routes through
  `SecurityPolicy::allow` + `::audit`, denied by default.
- **P5** Guest degradation: memory injection, tool groups and draft rights gated.
- **P6** `ProfileContext` from the session's profile; primary becomes the fallback.
- **P7** Cascade delete with per-category counts.
- **P8** Backfill: existing rows with `profile_id IS NULL` are treated as `Household`, not as the
  primary member's. Conservative by design — a wrong attribution is worse than a shared one.

---

## 5. Invariants

1. Profile context stays in `<system-context>` on the user message. Never the system prefix.
2. A scope-widening default is a bug. Where tool selection widens on failure, access narrows.
3. `identification_source` is never dropped on the floor — a decision made on a 0.6 face match must
   be distinguishable in the audit log from one made on a paired token.
4. `Household` is a positive classification, never a synonym for `NULL`.
5. Deleting a member never deletes `Household` data.

---

## 6. Deliberate deferrals

- **Speaker identification.** Real work, separate crate, biometric-consent implications. PAI-1 makes
  it additive.
- **Per-profile PINs or passwords.** Household members are not adversaries in the threat model; the
  boundary protects against accidental disclosure and cross-contamination of memory, not against a
  determined member with physical access. If that changes, `Principal` is the place to add it.
- **Per-profile model or prompt-style selection.** Interesting, not load-bearing.

---

## 7. Verification

- **Unit** — `ProfileScope` read/write matrix: `Owner(a)` cannot see `Owner(b)`; both see
  `Household`; `Guest` sees neither.
- **Regression** — a test asserting no production call site passes a "everything" scope, in the
  spirit of `every_settings_field_is_dispositioned`: grep-based, build-breaking.
- **Integration** — two profiles, two sessions, a memory written in each; assert isolation through
  the REST API, not just the repository.
- **End to end** — pair a phone as member A, ask it to recall something only member B told the pond,
  assert refusal plus one `Auth` audit event with `ok = false`.
- **Migration** — restore a pre-`0037` database, confirm every legacy memory resolves as
  `Household` and no row is attributed to the primary member.
