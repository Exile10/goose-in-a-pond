# PAI-8 — Personal context streaming

Requirement: *personal context streaming — data collected on-pond, on-mobile, and from internet
accounts (Google, OneDrive, iCloud, Slack, Telegram, WhatsApp, e-mail).* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (whose data),
[PAI-2](./02-privacy-and-security-guardrails.md) (guardrails), [PAI-7](./07-proactive-intelligence.md)
(what makes it useful).

Verified against code 2026-08-03.

---

## 0. Posture

**Ingest-only, opt-in per account.** Data flows into the pond and does not flow back out. Inference
stays entirely local; no prompt, no memory and no conversation is ever sent to a connected account.
A connector's network access is limited to the account's own API, and every request is
`record_egress`-tracked and governed by `network_mode` (PAI-2).

Writing back — sending an e-mail, posting to Slack — is **out of scope for v1** and, when it is
considered, goes through the draft-approval gate per PAI-2 §3.6.

---

## 1. What is true today

### 1.1 No personal-account integration exists

Grep across `crates/` (excluding the submodule and `vendor/`) for Gmail, Google Calendar, Drive,
OneDrive, iCloud, Telegram, WhatsApp, IMAP, SMTP and RSS returns **nothing**. The only `google`
matches are the FCM push relay and two unrelated comments.

### 1.2 But most of the machinery is already here

**OAuth 2.1 with PKCE — complete, and used once.** `pond-api/src/oauth_callback.rs` implements the
full flow: in-memory `PkceSession` keyed by a CSRF state nonce, S256 challenge (`:102-112`),
`FlowOutcome`/`OAuthOutcomes` with a 300 s TTL, and `GIAP_INTERNAL_TOKEN` / `GIAP_SERVER_URL` handed
to spawned extension subprocesses (`:120-142`). Routes at `routes.rs:216-220`.
`builtin_oauth_providers()` (`user_data/services/oauth_providers.rs:11-39`) returns **exactly one
provider: Spotify** — with a test asserting `providers.len() == 1`.

**An extension marketplace.** `pond-core/src/extensions/marketplace_registry.json`, seven curated
npx-spawned MCP servers — filesystem, github, brave-search, memory, puppeteer, **slack**, music.
Installable via `routes.rs:225-230`, with repo-relative script args anchored at construction
(`mcp/services/marketplace.rs:34-58`).

**A template for well-behaved outbound adapters.** `pond-adapters-weather` owns its own
`reqwest::Client` with explicit timeouts and wraps every request in `traced_send`, which calls
`record_egress` (`lib.rs:15-30`). Base URLs are consts with a `with_base_url()` used only by wiremock
tests.

**Secret storage.** `SecretRepository` (`security/ports/secret.rs`, the trait and nothing else) with
`SecretKind::{ApiKey, OAuthFlow, Generic}` — which lives in `security/domain/secret.rs`, a different
module. Re-verified 2026-08-09; the citation used to name one file for both and there is no enum in
the port file. Values are never returned through REST.

### 1.3 What is missing structurally

- **No inbound ingestion of any kind.** Webhooks are outbound only; there is no receiver. Grep for
  an `/ingest` or `/context/` route returns nothing — still true, re-verified 2026-08-09. **But the
  citation was pointing at dead code**, which matters because P7 will want to wire its receiver
  beside the sender: `pond-infra-scheduler`'s `WebhookTaskExecutor` is re-exported by that crate's
  `lib.rs` and constructed nowhere, and has been dead since PAI-2 P3 found it on 2026-08-05. The
  live outbound path is the `TaskKind::Webhook` arm in `pond-server/src/schedule_executors.rs`,
  which carries its own comment saying there are two webhook executors and they are not
  interchangeable.
- **GOTG pushes almost nothing.** The mobile companion has no dedicated crate and no dedicated
  endpoints — it drives the same REST API with `client_type: "gotg"`. It sends exactly two things:
  its push token (`routes.rs:140-143`) and a heartbeat. No contacts, location, calendar, photos or
  health data. **Correction, 2026-08-09 — this sentence used to say the only upload paths in the
  whole API are `/transcribe`, `/faces/*` and chat image attachments, and that was already known to
  be false**: `00-checklist.md` recorded the undercount on 2026-08-04 and the correction was never
  propagated back here. Grepping `Multipart` in `routes.rs` today finds `transcribe`,
  `calibrate_wake_word`, `read_face_multipart`, `register_face_handler` and `identify_face_handler`.
  It matters because PAI-1 and PAI-2 both lean on that absence argument, and an absence argument
  that undercounts its own surface is worth nothing. Grep the symbol before you reuse the claim.
- **Memory has no notion of provenance beyond `source`**, and no per-item retention.

---

## 2. The gap

GIAP knows what you told it and what its sensors saw. It does not know that your flight moved, that
the landlord replied, or that the meeting you are about to miss exists. That is the difference
between an assistant and a very good chat interface — and it is the requirement that most needs the
first seven workstreams to have landed first.

---

## 3. Design

### 3.1 `ContextSource` and `ContextItem`

New module `pond-core/src/context/`:

```rust
pub struct ContextSource {
    pub id: String,
    pub kind: SourceKind,          // Sensor | Camera | Voice | Mobile | Mail | Calendar |
                                   // Files | Chat
    pub provider: String,          // "google", "microsoft", "slack", "gotg", "imap", …
    pub profile_id: String,        // PAI-1: never optional. A source belongs to a person.
    pub scopes: Vec<String>,       // what the user actually granted
    pub cursor: Option<String>,    // incremental sync position
    pub last_sync: Option<DateTime<Utc>>,
    pub status: SourceStatus,      // Connected | NeedsReauth | Error | Paused
}

pub struct ContextItem {
    pub id: String,
    pub source_id: String,
    pub external_id: String,       // for idempotent re-sync
    pub profile_id: String,
    pub occurred_at: DateTime<Utc>,
    pub kind: ItemKind,            // Message | Event | Document | Location | Task
    pub title: String,
    pub body: String,
    pub participants: Vec<String>,
    pub sensitivity: PrivacySensitivity,
}
```

`profile_id` is non-optional by construction. The single largest lesson from this exploration is that
`Option<String>` for an owner becomes `None` everywhere (see PAI-1 §1.2); this type does not offer
that option.

Migration `0041_context_sources.sql` and `0042_context_items.sql`. **Re-verified 2026-08-09, and the
numbers moved:** this document was written when 0039 and 0040 were free, and PAI-5 has since taken
both — `0039_reasoning_tokens.sql` (P2) and `0040_session_thinking.sql` (P6, `bc3aa9df`). Confirm the
next free number by listing `crates/pond-infra/migrations/system/` rather than trusting this line,
which will rot again. And whatever the number, the migration must work against a database that
ALREADY HAS ROWS: every install after the first is an upgrade, and a migration that only works on an
empty file works exactly once. Retention is governed by
`retention_events_by_category` and `retention_sensitive_days` — both of which already exist and are
currently **headless** (`settings.rs` `HEADLESS_BY_DESIGN`). This workstream gives them a UI, because
"how long does GIAP keep my e-mail" is not a setting to hide.

### 3.2 The pipeline

```
source adapter  →  ContextItem  →  redact (PAI-2)  →  classify sensitivity  →
      →  persist (context_items, retention-governed)
      →  embed (fastembed, the same MiniLM used for memory)
      →  publish BusEvent::Ingest  (PAI-7's proposer wakes)
```

Items are **not** written into `memories` wholesale. Memory is a curated store with decay,
consolidation and importance; a mailbox would drown it. Instead the retrieval path gains
`context_items` as a second vector-searchable corpus, retrieved with the same
`memory_relevance.rs` blend of semantic similarity, recency and importance, and injected into
`<system-context>` under its own budget line so it can be cut independently.

### 3.3 Connectors, ordered by whether an honest implementation exists

| Source | Route | Reality |
|---|---|---|
| **Google** (Gmail, Calendar, Drive) | Extend `builtin_oauth_providers()` | Clean OAuth + REST APIs. The reference implementation. |
| **Microsoft** (Outlook, OneDrive) | Same | Graph API, comparable quality. |
| **IMAP e-mail** | New adapter, app password in `SecretRepository` | Works with anything, including self-hosted. Often the best privacy answer. |
| **Slack** | Existing marketplace entry | Already registered; needs token wiring and an ingest bridge. |
| **CalDAV / CardDAV** | New adapter | Covers Fastmail, Nextcloud, and iCloud calendars in practice. |
| **iCloud** | CalDAV + app-specific passwords | **There is no general iCloud API.** Mail via IMAP, calendar via CalDAV, and that is the honest ceiling. |
| **Telegram** | Bot API, or a user-run bridge | The Bot API only sees what is sent to the bot. Full account access needs MTProto, which is a user's own decision to run. |
| **WhatsApp** | Business API, or a user-run bridge | **There is no sane official read API for a personal account.** GIAP will not ship an unofficial client. If a user runs a bridge, GIAP ingests from it like any other source. |

Stating the last two plainly is part of the design. A roadmap that lists "WhatsApp" beside "Gmail"
as though they were comparable engineering promises something that cannot be delivered without
either a business account or a terms-of-service violation.

**On-mobile (GOTG)** is a first-class source, not an afterthought: calendar, location and (opt-in)
contacts pushed to `POST /api/v1/context/ingest`. The phone is where most personal context actually
lives, and GOTG already has an authenticated, rate-limited, paired channel.

### 3.4 New surfaces

- `POST /api/v1/context/ingest` — authenticated, for GOTG and any local pusher. Idempotent on
  `(source_id, external_id)`.
- `POST /api/v1/context/webhook/{source_id}` — for push-capable providers, with per-source secret
  verification. The first inbound webhook receiver in the product.
- `GET|POST|DELETE /api/v1/context/sources` — connect, list, disconnect.
- A `giap-context` MCP extension: `search_context`, `get_recent_context`. Read-only, joining the
  tool-group catalog so [PAI-3](./03-context-governor.md)'s narrowing applies.

### 3.5 Sync

Pull-only, cursor-based, scheduled through the existing scheduler with exponential backoff and
`NeedsReauth` surfaced in the UI rather than retried into a rate limit. No realtime sockets in v1 —
a home server that holds long-lived connections to five providers is a reliability problem before it
is a feature.

Each connector owns its `reqwest::Client` and wraps requests in `traced_send`, per the weather
adapter.

### 3.6 The hard rules

1. Every `ContextItem` carries a `profile_id` and a `PrivacySensitivity`.
2. A `Guest` session sees no context items. None.
3. Redaction runs before persistence, not before retrieval — the store never holds what it does not
   need.
4. Connector tokens live in the encrypted secret store (PAI-2 §3.4), never in `Settings`.
5. `network_mode` governs which hosts a connector may reach. In `offline`, connectors report
   `Paused`, not `Error`.
6. Disconnecting a source deletes its items by default, and says how many.
7. No prompt, memory or conversation content is ever transmitted to a connected account.

---

## 4. Phases

- **P1** Domain + migrations + the ingest pipeline with **on-pond sources only** (sensor, camera,
  voice transcripts) — proves the pipeline with data GIAP already holds and no new egress.
  **CODE LANDED 2026-08-11** (`b58361e1`, `a9062615`, `468e33d8`) **and UNREACHED.**
- **P2** Retrieval: `context_items` as a second corpus in `<system-context>` with its own budget;
  `giap-context` MCP extension. **CODE LANDED 2026-08-11 and UNREACHED**, and the extension is not
  registered.

#### What "unreached" means, and what it takes to end it

Stated as its own block because the distinction cost a documentation error the day the code landed.
`pub mod context;` makes all of this compile and `cargo check` is happy; a `pub` item in a library
crate never earns a `dead_code` warning, so nothing complains. But **no production code constructs
`SqliteContextRepository`, builds an `IngestPipeline`, or calls `init_context_deps`, and
`giap-context` is absent from `giap_registration.rs`.** There is therefore no way to create a source,
so `context_items` is empty on every pond that exists, and the retrieval, retention and scope layers
are correct code operating on nothing.

`crates/pond-core/tests/context_pipeline_is_not_wired_yet.rs` asserts this on every run, walks the
whole workspace to do it, carries a control proving the walk can see the files it excludes, and
fails with instructions naming the three documents to correct. PAI-7 P3a shipped with a guard of that
shape and it worked; this phase shipped without one, and within a day the checklist said `DESIGNED`
while the master roadmap repeated it.

Four things stand between here and reachable, and none is large:

1. **Register `giap-context`.** The count is asserted in three places that
   `registration_matches_the_catalog.rs` ties together — CLAUDE.md's sentence, the registration list,
   and `TOOL_GROUPS`. An uncatalogued builtin is treated as a *user-added* MCP server that selection
   never narrows, which for a personal-context reader is the wrong direction.
2. **A settings toggle**, off by default, with its own named `default_*` fn — the convention PAI-6 P5
   and PAI-7 P4 and P6 each established for a capability nobody asked for by upgrading.
3. **Construct the repository and install the deps** in `main.rs`, next to the other
   `init_*_deps` calls.
4. **Give ingest an on-pond producer**, which is what P1's own sentence asks for and the only part
   with real design left in it: a sensor reading is not a `ContextItem` until somebody decides which
   readings are worth keeping and what their `external_id` is.

**One thing this unblocks that the ledger had wrong.** PAI-2 P6b's third part — P3's redaction
chokepoint — was recorded as circularly blocked on PAI-8. It is not: `IngestPipeline::new` takes a
`Redactor` that is deliberately **not** an `Option`, so the chokepoint exists in the type as soon as
anything constructs a pipeline. The dependency runs one way.
- **P3** `POST /context/ingest` + GOTG calendar and location.
- **P4** Google connector as the OAuth reference implementation, extending
  `builtin_oauth_providers()`.
- **P5** IMAP and CalDAV — the self-hosted and iCloud path.
- **P6** Microsoft Graph; Slack ingest bridge over the existing marketplace entry.
- **P7** Webhook receiver with per-source secret verification.
- **P8** `BusEvent::Ingest` wired to PAI-7's proposer — the phase where this stops being a database
  and starts being an assistant.
- **DEFERRED** Telegram and WhatsApp bridges; write-back of any kind.

---

## 5. Invariants

Restated because they are the ones most likely to erode: every item has an owner and a sensitivity;
`Guest` sees nothing; redaction precedes persistence; tokens are encrypted; egress is tracked and
gated; disconnect deletes; nothing leaves.

---

## 6. Deliberate deferrals

- **Write-back.** A separate design with a separate risk profile.
- **Realtime push subscriptions** (Gmail watch, Graph subscriptions). Meaningful latency win,
  meaningful reliability cost. Revisit once P4-P6 have run in a real home for a while.
- **Full-text search over attachments.** Needs a document extraction pipeline; PDF and DOCX parsing
  is its own project.
- **Cross-source entity resolution** (recognising that an e-mail sender and a contact are the same
  person). Wanted, and much easier once several sources are live.

---

## 7. Verification

- **Unit** — idempotent ingest on `(source_id, external_id)`; cursor advance and resume; redaction
  applied before persistence, asserted by inspecting the stored row.
- **Isolation** — two profiles, two mail sources; assert profile A's assistant cannot retrieve
  profile B's items through chat, memory recall, or the `giap-context` tools. Three paths, three
  tests.
- **Guest** — assert zero context items reach a `Guest` session.
- **Egress** — every connector request appears in the event log with a classified host; with
  `network_mode = "allowlist"`, an unlisted host is refused with an actionable message.
- **Token handling** — assert connector tokens never appear in `GET /settings`, any log line, any
  event attribute, or any prompt.
- **Deletion** — disconnect a source, assert items are gone and the count reported matches.
- **Integration** — a wiremock Gmail returning three messages produces three `ContextItem`s, one
  `BusEvent::Ingest`, and a retrievable answer to a question about their content — with the
  assertion that no outbound request carried any conversation text.
