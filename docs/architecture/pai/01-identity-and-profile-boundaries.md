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

### 1.3 Sessions have no owner — but the column has been there all along

*(Corrected 2026-08-04, while implementing P2. The original claim below was "there is no
`profile_id`", which is wrong, and wrong in a way that matters.)*

`Session` — the domain struct — was `id`, `title`, two token counters, `model_name`, `created_at`,
`updated_at`. No `profile_id` **field**.

The **column**, however, has existed since `0003_profiles.sql`, which created the `profiles` table
and bolted `profile_id TEXT REFERENCES profiles(id)` onto `sessions` in the same file. The domain
type never mapped it and `sqlite_session_storage.rs` never selected or wrote it. It was a **dead
column** for thirty-four migrations.

That is the same shape as the two other findings in this document — `memory.profile_id` written as
`None` at every call site, and an identification endpoint whose output nothing reads. The schema
keeps anticipating identity and the code never arrives. It also means P2 is smaller than specified:
wiring, not a column addition.

**And it carried a live trap.** The column was declared with no `ON DELETE` action, which SQLite
reads as `NO ACTION`, while `Database::init` sets `PRAGMA foreign_keys = ON`. Harmless only while
the column stayed NULL — the moment anything writes it, `DELETE FROM profiles WHERE id = ?` starts
failing for any member who has ever spoken to the pond. Wiring the dead column without noticing this
would have broken member deletion. See 3.2 for the fix.

### 1.4 The identification endpoint writes to a map nobody reads

*(FIXED 2026-08-04 by P2. Kept, because the shape of the mistake is the point.)*

`POST /sessions/{id}/identify-user` and `GET|DELETE /sessions/{id}/user` wrote into
`AppState.session_user_bindings: Arc<RwLock<HashMap<String, String>>>`. Grep showed the map touched
at exactly three places outside test fixtures — the three handlers themselves. **No chat, prompt,
memory or tool path consulted it**, and it was process-local, so it died on restart.

The endpoint therefore answered "who is in this session" with whatever it had last been told, by
itself, since the last reboot. It had one caller: the same map's setter.

P2 deletes the map and points all three handlers at `sessions.profile_id`. The binding is durable,
carries its provenance, and is readable by anything that can reach session storage — which is what
makes P3 able to consult it on the chat path. No consumer exists yet; the map's replacement being
persistent is not the same as it being *used*, and this document does not claim otherwise until P3
lands.

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

Migration `0037_session_identification.sql`. As landed — the `profile_id` line the original design
called for is **not** in it, because 1.3 explains the column already exists:

```sql
ALTER TABLE sessions ADD COLUMN identification_source TEXT;      -- paired_device | explicit | face | unknown
ALTER TABLE sessions ADD COLUMN identification_confidence REAL;  -- face matches only

CREATE TRIGGER IF NOT EXISTS trg_profiles_delete_releases_sessions
BEFORE DELETE ON profiles
BEGIN
    UPDATE sessions SET profile_id = NULL, identification_source = NULL,
                        identification_confidence = NULL
     WHERE profile_id = OLD.id;
END;
```

`identification_source` matters as much as `profile_id`. A session bound because a paired phone
presented that member's token is a much stronger claim than one bound by a 0.62-confidence face
match, and the policy layer must be able to tell them apart. This replaces
`AppState.session_user_bindings` entirely — the in-memory map is deleted, not wrapped.

Confidence is stored **only** for `face`. A paired-device binding is not "confidence 1.0" — it is a
different kind of claim, and giving it a number invites somebody to average the two.

**The trigger is not decoration.** It stands in for the `ON DELETE SET NULL` the original design
assumed it could declare, which SQLite will not let us add to an existing column without rebuilding
`sessions` — the hottest table in the database. A `BEFORE DELETE` trigger runs before the row is
removed, so nothing references the profile by the time the delete is applied, and the semantics are
identical. The session survives, stripped of its attribution; erasing the member's *content* is the
separate, deliberate cascade in P7.

Strength ordering is domain logic, not adapter logic: `SessionIdentity::supersedes` refuses to let a
weaker source take over a session a stronger one bound. The case it exists for is concrete — a
member's phone opens a session, then the room camera sees whoever walks past, and without the check
a face match silently overwrites a cryptographic binding with a probabilistic one about a different
person. Equal strength *does* supersede, so re-identification still works.

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

`DELETE /users/{profile_id}` today clears biometrics only (`delete_user_biometrics` in `routes.rs`).
It must cascade memories, sessions, drafts, schedules and context items owned by that profile, and
report counts per category. `Household`-scoped data survives — deleting a member does not delete the
shopping list.

Most of the cascade already exists, found while implementing P1 and P2 rather than during the design
pass. Every `profile_id` foreign key in the schema, and what it does on delete:

| Table | Migration | On delete |
|---|---|---|
| `memory_fragments` | `0005` | `CASCADE` |
| `face_embeddings` | `0013` | `CASCADE` |
| `face_profile_thresholds` | `0014` | `CASCADE` |
| `sessions` | `0003` | **nothing declared** -- so `NO ACTION`, until `0037`'s trigger |

`sessions` was the only one of the four written without an action, which is exactly why it was the
one that would have broken. A member's memories, faces and thresholds already vanish with them; P7
has to *count* those, not delete them. The session releases to NULL instead, deliberately: a
conversation is not solely the speaker's, and P7's job there is to decide what of its content goes,
not whether the row does.

So P7 is narrower than written — drafts, schedules, and the per-category counts. But the audit above
is the part to keep: three of the four behaviours were decided years ago by migrations nobody
remembered writing, and the fourth was decided by omission.

### 3.8 Speaker identification is scoped out, and said so

Voice is a shared surface. Until `pond-adapters-speaker-embed` exists, a voice session resolves to
`Household` (if a face is co-present and confident) or `Guest`. **This document does not claim
per-person voice memory works.** The speaker-ID design in `data_pipeline.md:502-577` is the
follow-on; PAI-1 makes it a drop-in by putting `identification_source` in place first.

---

## 4. Phases

- **P1 — LANDED 2026-08-04.** `ProfileScope` domain type; `MemoryRepository` signature change from
  `Option<&str>` to `&ProfileScope`; all current call sites pass `ProfileScope::Household` so
  behaviour is **byte-identical** on landing. This is the refactor that makes every later phase
  small.
- **P2 — LANDED 2026-08-04.** Migration `0037_session_identification.sql` (provenance columns and
  the delete trigger — `profile_id` was already there, see 1.3); `Session.profile_id`,
  `SessionIdentity`, `IdentificationSource`, and the two `SessionStorage` methods that read and
  write them; `AppState.session_user_bindings` deleted and its three handlers repointed at the
  column.

  P2 deliberately does **not** add a `SessionIdentity -> ProfileScope` conversion. Every session in
  every existing pond is unattributed, so such a method would have to answer "what scope is an
  unidentified session" today — and the only behaviour-preserving answer, `Household`, is precisely
  the scope-widening default invariant 2 calls a bug. That question belongs to P3, which decides it
  with the paired-device, explicit and face inputs in hand rather than from the stored row alone.
- **P3 — LANDED 2026-08-04.** Resolver in `user_data/services/identity_resolution.rs`; resolved once
  per turn at the API edge by `resolve_turn_scope` and carried on `AgentRequest.profile_scope` into
  the adapter, which uses it for all three memory searches. `PUT /sessions/{id}/user` added, so
  `Explicit` finally has a producer.

  **Resolved at the edge, carried down, never recomputed.** Two resolutions of one turn could
  disagree, and the one nearer the data would silently win. `AgentRequest` was the right channel —
  `voice_mode` and `canvas_mode` are the same pattern — and the field is non-optional in Rust with
  no `Default` on the struct, so all **twelve** construction sites had to state a scope. The
  compiler found one the recon pass had missed (`shared/services/delegation.rs`). Every failure in
  `resolve_turn_scope` narrows: a storage error reads as unidentified, a profile-count error assumes
  more than one member.

  **Deliberately not used:** the process-global `set_current_session_id` / `current_session_id` pair
  (`shared/services/egress.rs`). It is a `RwLock<String>`, not a task-local, and the SSE semaphore
  permits more than one concurrent stream — so extending it for identity would attribute one
  speaker's turn to another under load. That is precisely the failure this workstream exists to
  prevent.

  Original phase text, for the record:

- **P3 (as designed) — resolver LANDED 2026-08-04, wiring outstanding.** `user_data/services/identity_resolution.rs`
  holds the chain as one pure function with `ResolutionInputs` / `ResolvedIdentity`. No call site
  consumes it yet; threading it through `AgentRequest` into `ChatService` is the remaining half.

  **The strongest rung of the chain cannot be built.** Nothing in the schema links a paired device
  to a household member: `session_tokens` stores `device_id` and `client_id` and no profile, and
  neither do `push_tokens`, `pairing_codes` or `handshake_challenges`. The pairing flow never asks
  who is pairing. So `paired_device_profile` is an input the resolver honours and every caller feeds
  `None`.

  Deliberately **not** worked around. Falling back to `settings.primary_profile_id` would attribute
  every phone in the house to one person — the wrong-attribution failure this whole workstream
  exists to prevent, and worse than admitting we do not know. Capturing a member at pairing time is
  its own change, and it is also what [PAI-7](./07-proactive-intelligence.md) needs before it can
  address a notification to "that profile's devices"; that document assumes the link exists.

  **What an unidentified speaker resolves to.** `Household` while the pond has one member, `Guest`
  once it has more than one. This looks like a fudge and is not: `Household` and `Guest` differ only
  when there is somebody to be excluded from, so in a one-member pond they describe the same rows.
  Resolving to `Guest` unconditionally would make a working single-user assistant refuse to remember
  anything about its only user, which is a regression rather than a boundary. It does mean **adding
  a second household member is the moment a pond's privacy posture changes**, which belongs in
  release notes.

  There is also no explicit-identification route yet — `POST /sessions/{id}/identify-user` is
  face-only — so `Explicit` has no producer either. That is a small addition and it lands with the
  wiring.
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

### What P1 and P2 actually ran (2026-08-04)

`cargo fmt --check`; `pond-core` 720 lib tests; `pond-infra` 181; `pond-mcp-server` 176; `pond-api`
105 lib plus all 17 integration targets; `pond-adapters-goose` 103; `cargo check -p pond-server`;
and a live server run against a real data directory.

New tests, and what each would have to be broken for:

| Where | Asserts |
|---|---|
| `session.rs` domain | every source round-trips; an unrecognised stored source degrades to `Unknown` and never upward; the rank order; a face match cannot take over a paired-device session; equal strength still supersedes so re-identification works |
| `sqlite_session_storage.rs` | a new session is unattributed; identity round-trips including the face confidence; writing to a session that does not exist is `SessionNotFound`; reading one is not; **deleting a member with a live session succeeds and releases it**; identity cannot name a profile that does not exist |
| `agent_data_integration_test.rs` | the same, over HTTP -- and specifically that a binding survives the process that made it, which is what the deleted in-memory map could never do |

The integration tests go through the router rather than the repository on purpose. What P2 replaced
was a map that no layer below HTTP ever saw, so a test beneath that boundary would have passed
against the old code too.

The `pond-api` integration targets had to be run **one target at a time**, deleting each ~600 MB
test binary before building the next. Seventeen of them link Goose statically and together exceed
this container's disk allowance. `cargo test -p pond-api` as a single invocation fails with
`No space left on device`, which surfaces as a linker `Bus error` and reads exactly like a code
fault. It is not one.

### The plan

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
