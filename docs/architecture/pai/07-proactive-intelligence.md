# PAI-7 — Proactive intelligence

Requirement: *GIAP needs to be proactive, not just reactive.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (who to tell) and
[PAI-6](./06-multi-agent-orchestration.md) (what runs the background thinking).

Verified against code 2026-08-03. **Re-verified 2026-08-09**, and the substance held — every symbol
this document names still exists and compiles, which is not what the last two re-verification passes
in this programme found. What rotted was citations: eight `file:line` references had drifted by up to
a thousand lines, and rather than bump them I have replaced them with the symbol, because bumping
buys about a week. Two claims did change and are marked in place: section 3.4's device-resolution
chain does not exist in code, and the `main.rs` bus bridge has moved ~200 lines.

**Re-verified again 2026-08-11 with P2.** One claim had gone false in two days and is corrected in
place: **section 3.4's "that chain does not exist in code" is no longer true.** PAI-1 P9 landed
migration 0043 on 2026-08-11, so `devices.profile_id` and `pairing_codes.profile_id` both exist and
`DeviceAttribution` is the port over them. What is still true is the *consequence* — nothing
constructs it, so P5 remains blocked — but the prerequisite has changed shape from "build the
schema" to "wire the port", and a phase reading the old sentence would go and build a column that
is already there.

**Re-verified a third time 2026-08-11, repairing P2.** One claim in this document was false the day
it was written and inherited from P1: **`PrivacySensitivity::Sensitive` does not keep an event out
of the audit MCP reads.** `audit.rs :: MAX_SURFACEABLE` *is* `Sensitive`. Corrected in place in
3.1's repair stamp and in `event_bus.rs`; what `Sensitive` actually buys is a seven-day retention
sweep instead of thirty. The other four findings were guard-shaped rather than claim-shaped and are
recorded in the same stamp.

---

## 1. What is true today

### 1.1 There is an event bus, and P1 and P2 have widened it

**Rewritten 2026-08-10.** This section described a three-variant enum and ended "there is no time
event, no presence event, no session event". P1 falsified all three sentences and left them
standing; that is the claim-rot rule, so it is fixed here rather than noted. **Updated again
2026-08-11** for P2's variant.

`shared/ports/event_bus.rs :: BusEvent`:

```rust
pub enum BusEvent {
    Sensor(SensorReading),
    Camera(CameraEvent),
    Device(DeviceStateChanged),
    Time(TimeTick),               // PAI-7 P1 — one per local hour boundary
    Presence(ProfilePresence),    // PAI-7 P2 — a named member arrived / left
    Session(SessionLifecycle),    // PAI-7 P1 — started / idle / resumed
}
```

Still a closed enum, deliberately, so consumers "can pattern-match ergonomically instead of parsing
an attribute map". `EventBus::publish` is non-blocking and infallible; `subscribe` returns a
`Stream`. Adapter: `shared/services/in_process_event_bus.rs`.

**The first three variants are device-shaped and the last three are not**, which is why
`BusEvent::trigger_view` now returns `Option<TriggerEventView>` — `None` for `Time`, `Presence` and
`Session` — and why `rules_engine.rs :: rules_to_fire` returns an empty fire list the moment it
sees one. Presence is the one where the temptation is real, and worse than for a clock tick: a
person arriving genuinely looks like something a rule should fire on, so a "harmless" camera-family
view would hand every household member's arrival to the automations somebody already wrote. A
rule the API accepts with `device_id: None, signal: None` matches *anything* in its family, so a
placeholder view would run the automations somebody wrote about their house on the hour, every
hour. `TriggerEventView` is `#[non_exhaustive]` for that reason: outside pond-core the struct
literal does not compile, so a consumer cannot answer the `None` with a view of its own. That is a
compiler check, not a test, and it is the only guard at the rules engine's own decision point —
`rules_to_fire` still has no test that feeds it a clock or session event (recorded gap, 2026-08-10).

**Publishers (P1 and P2, `main.rs`):** `run_time_ticker` sleeps to the next wall-clock hour
(`time_tick::secs_to_next_hour_from`, which takes the clock reading rather than a minute and a
second, because two positional `u32`s swap silently inside a timer loop);
`run_session_activity_observer` polls the session store every 60s and folds it through
**both** `ActivityObserver::poll` and `PresenceObserver::observe_read`. One task, one read, two
observers — a second polling loop would read the same table on its own schedule and the two could
disagree about which conversations exist.

**A scheduled run is not a person.** Every `AgentPrompt` schedule fire and every rule
`AgentPrompt` action creates a session row (`schedule_executors.rs`, `sched-{task_id}-{unix_ts}`),
and the `sessions` table has no origin column, so P1 classifies by id:
`session_activity::SessionOrigin` / `POND_AUTHORED_SESSION_PREFIXES`, applied by `human_activity`
to **both** the arrival list and the activity clock. Without it a cron line at 3am publishes
`Started`, opens the never-at-startup gate, and fifteen minutes later publishes `Idle` — a complete
synthetic presence cycle on an empty house, which is worse than no presence signal because P4 acts
on it with confidence. The deny-list's completeness is itself a test
(`pond-core/tests/session_origin_covers_every_minted_session.rs`), which fails when a file that
mints session rows has no recorded origin.

**And it applies to presence for a second reason (P2).** `PUT /sessions/{id}/user` binds whatever
session id it is handed, including a `sched-` one, so an *attributed* cron fire is a row production
can produce rather than a hypothetical. `PresenceObserver` refuses a non-`Human` origin before it
resolves anybody.

**Consumers: still none for the new variants.** The rules engine skips them and the bus-to-event-log
bridge records them (`main.rs`, the task that does `event_bus.subscribe()` beside the
`run_rules_engine` spawn — grep the symbol). Deciding anything about a tick or a presence
transition is P4's, and the separation is the point: an event nobody can trust is worse than no
event.

### 1.2 Rules fire, but only rules the user wrote

`SensorTriggerSpec` (`user_data/domain/schedule.rs:104`) = `{ source, condition, actions,
cooldown_secs = 60 }`. Conditions are a numeric comparison plus a local-time window that wraps
midnight and fails closed on malformed input (`:73`). Actions are **three**: `AgentPrompt`,
`DevicePower { device_id, on }`, `Notify { title, body }` (`:92-99`).

`run_rules_engine` (`pond-infra-scheduler/src/rules_engine.rs:71-104`) refreshes the rule list with a
5-second TTL cache, evaluates, and calls `scheduler.run_now(rule_id)` so rule fires reuse the cron
execution path and get run records and SSE for free. Good design.

Two limits worth naming: the debounce map is **in-memory**, so cooldowns reset on restart; and
sensor rules have no dedicated REST route — they are created by POSTing a `SensorTrigger` kind to
`/schedules` (`routes.rs :: create_schedule`, which takes a `kind` field including `SensorTrigger`), with the MCP tools (`create_sensor_rule`, `list_sensor_rules`,
`delete_sensor_rule`) as the real interface.

### 1.3 Vision detects, and nothing reasons about it

`pond-adapters-vision` decodes the stream, detects motion, optionally classifies, writes a JPEG, and
publishes `BusEvent::Camera` (persist-then-publish, the same contract as the external
`POST /camera/events` route). The **only** autonomous consumer is the rules engine. So a camera event
acts on its own **only if the user already wrote a matching rule**. There is no novelty detection, no
"this is unusual", no model in the loop on events.

### 1.4 Scheduling can prompt or POST, and nothing else

`TaskKind` (`user_data/domain/schedule.rs:16-25`) is `AgentPrompt { prompt }`,
`Webhook { webhook_url }`, or `SensorTrigger(spec)`. There is **no task kind that calls a tool
deterministically** — a cron job that should read a sensor and act must ask the model in English and
hope.

`AgentScheduleExecutor` (`pond-server/src/schedule_executors.rs:24-169`) creates an ephemeral session
`sched-{id}-{ts}` with `model_role: "task"` and calls `agent.chat()`, bounded by a `Semaphore`
(`schedule_max_concurrent`, default 2).

(`docs/architecture/scheduling.md` described two task kinds and seven MCP tools; there are three and
twelve. **Already corrected** — that fix landed with the documentation-debt pass, not with this
workstream.)

### 1.5 The outbound channel is one broadcast, and half of it is dormant

The complete list of unprompted notification producers:

| Site | Trigger |
|---|---|
| `main.rs`, the schedule-completion broadcast | schedule completion or failure |
| `schedule_executors.rs`, the `sender.broadcast(n)` in the rule executor | a fired rule's `Notify` action |
| `pond-mcp-server/src/system.rs:214` | the `send_notification` tool — model-initiated, but **only inside a user-started turn** |
| `routes.rs:542` | a device-pairing security notice |

**All four call `broadcast()`.** The durable offline queue and the FCM relay are only reachable from
the targeted `send()` path (`broadcast_notification_sender.rs :: send`, whose own doc comment records that nothing exercises it), so `sqlite_notification_queue`
and `fcm_push_relay` — both real, both tested, the latter carrying a genuinely thoughtful
content-free wake-ping design (`fcm_push_relay.rs:1-26`) — have **no production producer**.

### 1.6 GIAP never speaks first

Every `voice_output.speak()` call site is downstream of a user utterance
(`shared/services/chat.rs` -- five `voice_output.speak()` call sites, all downstream of a user utterance; the count is the durable fact, the line numbers have drifted ~130) or an explicit `/tts` request
(`routes.rs`, the `tts.speak(&text)` call). The voice child is a separate OS process and cannot reach `AppState` at all.
GIAP is visually proactive at best.

---

## 2. The gap

GIAP acts when a sensor crosses a threshold the user configured, or when a cron line the user wrote
fires. That is automation, not initiative. Nothing notices that something is *worth mentioning*;
nothing decides *whether* to interrupt; and the one channel that could reach a phone with the screen
off is switched off by having no caller.

---

## 3. Design

### 3.1 Widen the event spine

`BusEvent` stays a closed enum — that is a good decision — and grows:

```rust
pub enum BusEvent {
    Sensor(SensorReading),
    Camera(CameraEvent),
    Device(DeviceStateChanged),
    Time(TimeTick),                  // hourly, plus dawn/dusk/quiet-hours boundaries
    Presence(ProfilePresence),       // a profile arrived or left. AS LANDED: face + explicit.
                                     // Paired device reaches it only through the session row,
                                     // and geofence does not exist — see the P2 stamp below.
    Session(SessionLifecycle),       // started, went idle, resumed after a gap
    Ingest(SourceUpdated),           // PAI-8's hook: new mail, new calendar item
}
```

`Presence` is the one that makes proactivity feel personal rather than mechanical, and PAI-1's
identification chain is exactly what produces it. `Session` is what tells the proposer the user is
*available* — the difference between a helpful nudge and an interruption.

#### AS LANDED — P2, 2026-08-11. Publisher only; the FACE and EXPLICIT rungs, no geofence.

**What landed.** `PresenceTransition`, `ProfilePresence`, `PresenceEvidence`, `PresenceInputs` and
`PresenceObserver` in `shared/domain/session_activity.rs`; `BusEvent::Presence` with its event-log
projection and its `trigger_view` refusal; and the publisher, folded into the existing
`run_session_activity_observer` poll in `main.rs`. No new migration, no new `Settings` field, no
new port. Nothing consumes it — that is P4's, and P1's publishers-only rule is why these events
are worth consuming at all.

**It lives in P1's observer module rather than a module of its own**, because it is the same
observation: the same poll of the same store, the same `SessionOrigin` filter, the same idle
threshold. Splitting it would have produced exactly what `human_activity` was written as one
function to prevent — two places where one gets fixed and the other does not.

**Naming is delegated, not re-derived.** `PresenceObserver` never decides who anybody is: it hands
the session row to PAI-1's `identity_resolution::resolve` and publishes only for
`ProfileScope::Owner`. `Household` and `Guest` leave by the same door as an unattributed session,
so invariants 4 and 5 are the resolver's existing behaviour rather than a second copy of it that
can drift. `ProfilePresence.profile_id` is a `String` with no anonymous variant, which makes
invariant 4 structural: this type cannot express "somebody is here".

**The `paired_device` rung cannot reach a background poll, and the reason is worth writing down**
because it looks like an oversight. `ResolutionInputs::paired_device_profile` wants the member
owning the device that authenticated *this request*; a timer has no request and no token. PAI-1 P9
landing `DeviceAttribution` did not change that. The rung still reaches presence — through the
session row, the moment a handler binds one at `PairedDevice` strength — and the `source` on every
event is what says which rung it was. So presence today is the **face** and **explicit** rungs:
`POST /sessions/{id}/identify-user` and `PUT /sessions/{id}/user`.

**Geofence: there is nothing, and I did not invent one.** Section 6 defers it, GOTG has no
background location, and a `Geofence` variant on `IdentificationSource` with no producer would be
the third correct-but-unreachable mechanism this programme is carrying.

**What "arrived" and "departed" actually mean.** Presence is keyed on `sessions.updated_at` — when
somebody last *spoke* — and never on when an attribution was written, because
`set_session_identity` deliberately leaves `updated_at` alone. Three consequences, each of them a
recorded trap in a new shape:

- **A photograph is not a person in the room.** A face bound to a conversation nobody has touched
  for three hours leaves the evidence stale, so it publishes nothing. That is the strongest defence
  available from here; the identification edge itself (`/sessions/{id}/identify-user` takes a
  multipart image from any caller) is PAI-1's anti-spoofing problem, not this observer's, and until
  it is solved the `source` field is how a consumer knows to discount the claim.
- **A restart is not everybody arriving.** The first observation records the level and publishes
  nothing, exactly as `ActivityObserver::seeded_from` does for conversations. The pond restarts on
  every deploy. Cost, stated rather than hidden: a member who genuinely arrives during the first
  poll after boot is recorded rather than announced.
- **A departure is a decision, because nothing observes one.** No geofence, no door sensor bound to
  a person, no camera that reports an empty room — so absence is the evidence ageing past the same
  `INACTIVITY_THRESHOLD_SECS` that decides `SessionPhase::Idle`, evaluated on the same poll. One
  pond, one definition of gone quiet; two would have the bus saying a member is still here after it
  had already said the pond went idle. It lags by up to threshold + 60s, and `Departed` means "the
  pond stopped being able to say this member is here", including when a binding is simply released.

Re-identification is not an arrival in any of its four shapes: the same session polled again, a
second conversation opened by the same member, an upgrade from a face match to an explicit binding,
and two conversations naming one member in one poll (strongest rung wins, then the more recent).

**Reachability, stated as a fact rather than a caveat.** The publisher is unconditionally spawned
in `run_server` and runs every 60 seconds on a default install, so unlike P3a there is no inert
layer here. Its *input* is the constraint: presence requires an attributed session, and
**no shipped surface attributes one**. `pond-desktop/src` calls exactly one profile route
(`GET /api/v1/profiles`); the two binding routes are reachable over REST by a paired client, and
the face one additionally needs a face-recognition adapter configured. So on a stock install with
the shipped app, this publishes nothing until PAI-1's identification gets a caller — which is
PAI-1's outstanding work, not a gap in this phase, and it is the same reason P4 will have nobody to
address until it lands.

**Verification.** Sixteen unit tests, and nine mutations run against them (each applied, run,
reverted): deleting the origin filter, deleting the freshness check, weakening it from `>=` to `>`,
replacing the resolver with the session row's raw `profile_id`, mapping `Household`/`Guest` to the
primary member the way `routes.rs :: profile_context_for` does, removing the baseline, giving
presence a camera-family `trigger_view`, logging it below `Sensitive`, inverting the
strongest-rung tie-break, and removing departures. Each failed with a message naming the defect.
Two things that mutation pass revealed and that a later change should know: the
`Household`/`Guest` tests are only load-bearing against a mutation that supplies a *name* for those
scopes (with an unattributed row there is no name to leak, so the raw-`profile_id` mutation slips
past them and is caught instead by `a_profile_id_with_no_source_is_not_presence`); and the
`seeded()` helper's own baseline assertion is vacuous wherever it is called with an empty session
list, so `a_restart_does_not_announce_the_household_as_arriving` is the only test that pins the
baseline.

#### REPAIRED — P2, 2026-08-11. Five findings, and three of them were in the paragraphs above

The stamp above is what P2 believed on the day it landed. A review pass reproduced five defects in
it; all five are now closed and the corrections are here rather than rewritten over the original,
because what a phase believed while shipping is the useful record.

**1. The headline property was guarded by nothing.** "Presence is keyed on `sessions.updated_at`"
is the claim this whole section rests on, and every presence fixture set `created_at ==
updated_at`, so `PresenceEvidence::of` could be pointed at `created_at` with all 1000 pond-core
tests green — including the two whose *names* state the property. The regression that hides behind
that is not cosmetic: a conversation opened three hours ago and being spoken in right now would
publish `Departed`, and could never publish `Arrived` again. The fixture helper now takes both
columns and `presence_is_keyed_on_when_somebody_last_spoke_not_on_when_the_conversation_began` is
the discriminating case. The mirror fixture (`updated_at` older than `created_at`) is deliberately
NOT written — no production path can produce that row.

**2. The publisher decided four things while its doc-comment said it decided none.** Worst of them
was the freshness window: `presence_window` was a field `main.rs` filled in, and setting it to
twenty-four hours left `cargo check -p pond-server` and the whole pond-core suite green. Nothing
exercises `run_session_activity_observer` — no file under `crates/pond-server/tests` names it, and
`ci.yml` only cargo-checks the crate. Fixed structurally rather than by comment:
`PresenceInputs`'s fields are private, `PresenceInputs::for_poll` is the only way in from another
crate, and `PRESENCE_WINDOW` is derived from `INACTIVITY_THRESHOLD_SECS` in pond-core. Writing a
window in `main.rs` is now E0451. The other three moved with it —
`household_has_multiple_members(&Result)` for the failed profile count,
`PresenceObserver::observe_read` for the failed identity read, and `attribution_candidates` for the
row pre-filter. What remains in the loop and is still guarded by nothing is now listed in the
function's own doc-comment, `PollClock::idle_threshold` included.

**3. `Sensitive` does not do what this phase thought it did.** The claim inherited from P1 — that
`Sensitive` "keeps it out of the audit MCP reads" — is false. `pond-mcp-server/src/audit.rs ::
MAX_SURFACEABLE` **is** `Sensitive` and is passed as `max_sensitivity` to all three read tools, so
`sensitivities_at_most(Sensitive)` yields Public + Internal + Sensitive: a `presence.profile` row
is readable through `recent_activity` today, and would be at `Internal` too. The half that is true
is retention — `pruning.rs` sweeps `>= Sensitive` at seven days and everything else at thirty — and
that is now the whole of what the assertion messages claim. Guests cannot reach those tools at all
(`giap-audit` is in `groups_denied_to_guests`), so the exposure is member-to-member, and the
renderer prints timestamp/category/action without `profile_id`: an Owner turn learns that somebody
arrived at 19:04, not who. **Open, and owed to whoever decides it:** if presence must be withheld
from those reads, the change is in `audit.rs` — exclude the `presence.profile` action or lower
`MAX_SURFACEABLE` — and it is a policy decision about the audit surface, not a classification.

**4. `Believed::beats`'s same-rank tie-break was unguarded** despite its doc-comment naming an
ordering. Inverting `>` to `<` changed nothing any test could see, because the only other fixture
with two same-rung rows asserts emptiness.
`on_an_equal_rung_the_conversation_spoken_in_most_recently_wins` asserts it in both input orders.
The ordering is not arbitrary: `SessionIdentity::supersedes` is `rank() <= rank()`, so an
equal-rung write replaces the row, and presence breaking the tie the other way would name a
conversation the session row no longer considers current.

**5. The poll was an N+1 that grows with history.** It issued one `get_session_identity` per
attributed session every sixty seconds, forever, on rows `list_sessions` had already returned in
full — and `list_sessions` has no `LIMIT` and no time bound, so after a year every session ever
attributed was re-read once a minute to hand the observer rows the freshness gate discards
microseconds later. `attribution_candidates` now decides which rows are worth the query, from the
observer's own refusals. It is read-avoidance and not a gate: `present_members` re-applies origin
and freshness through the same `is_fresh`, and
`skipping_a_read_never_changes_who_is_published` pins that the cheap read and the expensive one
publish identical events. The alternative fix — widening `list_sessions`'s `SELECT` and building
`SessionIdentity` in `TryFrom<SessionRow>` — is the better shape and lands in `pond-infra`; it is
still worth doing and would let `attribution_candidates` become a pure filter over rows already in
hand.

**One thing the repair found that is worth carrying forward:** `household_has_multiple_members`
**cannot change a published presence event**. `identity_resolution::resolve` consults it only in
the fallback that answers `Household` or `Guest`, and presence publishes for neither, so both
values produce the same events over the same rows. The failure direction is still correct and
still pinned, but it is a tripwire rather than a guard —
`the_household_count_cannot_change_a_published_presence_event` fails the day presence grows a
`Household` path, which is the day the direction starts being load-bearing.

**Verification of the repair.** Twenty-three unit tests now, and ten further mutations run (each
applied, run, reverted byte-identical): `updated_at` swapped for `created_at`; the tie-break
inverted; `PRESENCE_WINDOW` widened to 24h; each of the three filters in `attribution_candidates`
deleted; the observer's freshness drifted away from the pre-filter's; the failed read folded in as
an empty house and as a fresh start; and the presence classification lowered to `Internal`. Each
failed with a message naming the defect. The structural claim was checked by writing the thing it
forbids: a `PresenceInputs` struct literal in `main.rs` is a compile error.

### 3.2 Propose, do not act

This is the core safety decision, and it is what keeps a proactive assistant from being a liability.

A proactive impulse does not perform an action. It produces a proposal:

```rust
pub struct Proposal {
    pub id: String,
    pub trigger: BusEventRef,
    pub rationale: String,          // why GIAP thinks this matters — always shown
    pub proposed_action: TaskKind,
    pub profile_scope: ProfileScope,
    pub confidence: f32,
    pub expires_at: DateTime<Utc>,
}
```

Proposals land in **the draft system that already exists**. `giap-draft` is unconditionally
registered as a safety extension (`giap_registration.rs :: register_builtin_extension("giap-draft", ...)`) and already models save / list /
approve / reject (`pond-mcp-server/src/draft.rs :: save_draft` / `list_drafts` / `approve_draft` / `reject_draft`). Reusing it means
proactive actions inherit an approval flow that is already built, already understood, and already
trusted — rather than a second, parallel confirmation mechanism users would have to learn.

**Corrected 2026-08-10.** This paragraph used to end "…`reject_draft`), with a UI". There is no
drafts UI. `pond-desktop/src` contains no drafts surface at all — every occurrence of "draft" in it
is local editor state (`secretDraft`, `inlineDraft`, the composer's `draft`) — and there is no
`/drafts` REST route in `pond-api` for one to call. The draft flow is model-facing only, through the
four `giap-draft` tools. The argument for reusing drafts still holds, because it is an argument
about the *approval flow*, but "reuse the surface that exists" is not one of its premises: P3's
remainder has to build that surface rather than add a card to it.

Approve-once and always-approve-this-kind are the obvious follow-ons, and they are how the system
earns autonomy incrementally instead of demanding it up front.

`rationale` is mandatory. An assistant that says "you asked me to watch for this, and the delivery
window closes at six" is helpful; one that says "I did a thing" is alarming.

#### AS LANDED — P3a, 2026-08-10. Domain and persistence only; NOTHING CONSTRUCTS IT YET.

Read the second half of this stamp before quoting the first. P3 is **not** closed: what landed is a
validated type, a port, an adapter and two migrations, with **no producer, no consumer and no
construction site anywhere in the tree**. That is a legitimate shape — PAI-6 P1 was the same shape
and says so — but it is only legitimate when it says so, and the usual tripwire cannot say it here:
every symbol is `pub` in a library crate, so `dead_code` never fires. PAI-1 P5 shipped inert for a
whole phase behind exactly that blind spot.

**What landed.** `Proposal`, `ProposalAudience`, `BusEventRef`, `ProposalPayload` and
`ProposalError` in `pond-core/src/user_data/domain/proposal.rs`; the `ProposalRepository` port;
`SqliteProposalRepository` in `pond-infra`; migrations `0041_proposals.sql` (three columns on
`drafts`, an index, and three triggers) and `0042_proposal_expiry.sql`; and — a separate, live
change — `Draft.expires_at` with the expiry refusal in `DraftMcpServer::decide`.

0042 exists because 0041 gave invariant 2 a storage layer and did not give one to invariant 7: a row
with `origin = 'proactive'` and a NULL `expires_at` was storable, and 0041's approve trigger keys on
`OLD.expires_at IS NOT NULL`, so that row was approvable forever. Writing the trigger pair surfaced
a second thing, and only running the upgrade found it: the rule is stated over a row's *state*
rather than the transition into it, so a row already in the forbidden state would be frozen —
including against the two `UPDATE`s inside 0038's `BEFORE DELETE ON profiles` trigger, which would
abort the DELETE and make a household member unremovable. 0042 therefore fixes the data before it
constrains it, and `migration_0042_does_not_freeze_a_row_that_predates_it` builds the pre-upgrade
row a fresh database cannot hold and re-runs the file over it.

**Four guards added on the repair pass, each answering a mutation that had survived.** Widening
`LIVE_PREDICATE`'s `status = 'pending'` so a decided proposal is returned left the whole pond-infra
suite green, which is invariant 7's other failure mode and the worse one — the member has already
answered. Renaming `PROPOSAL_ORIGIN` disabled every origin-keyed trigger in 0041 and 0042 for every
row the repository writes, silently, because the trigger tests wrote `'proactive'` as a SQL literal
instead of binding the constant; they bind it now, and a counting guard ties the constant to the
literal in both files. Deleting `from_parts`'s `MissingId` block left all sixteen proposal tests
passing. And 0042's data fix is what keeps a pre-upgrade row from freezing the profile-delete path.

**Respecified against the sketch above, and why.** `profile_scope: ProfileScope` became
`audience: ProposalAudience`. Of that type's three shapes, invariant 5 forbids `Guest` and
invariant 4 forbids `Household` — `Household` is not a weaker address than `Owner`, it *is* the
broadcast, and it is the value a defaulted field lands on. A type with one admissible shape out of
three should not be that type, so the audience holds a profile id in a one-type module where the
field cannot be filled from outside. `created_at` was added because the TTL ceiling needs a birth
time to measure from, and `MAX_PROPOSAL_TTL` (24h) was added because `expires_at` alone is satisfied
by the year 3000. `Proposal` derives no `Deserialize`: a derive is a second constructor that skips
every check, and the stored form is what a bug reaches first.

**What is NOT reached, stated as facts rather than as a caveat.**

- Nothing constructs `SqliteProposalRepository`. No `AppState` field, no `main.rs` line, no route,
  no test outside the adapter's own module. That is asserted rather than described:
  `nothing_outside_this_file_constructs_a_proposal_repository_yet` walks every `.rs` file under
  `crates/` on every run, and it carries its own vacuity control (the same walk must find
  `SqliteDraftRepository::new(`, which pond-server really does call, or the walk is not looking at
  the workspace and its happy answer means nothing). **That test is meant to fail one day**, and
  the day it does, this stamp is what has to change with it.
- Therefore no `drafts` row in production can carry `origin = 'proactive'` or a non-NULL
  `expires_at`. `SqliteProposalRepository::save` is the only writer of either, and `save_draft`
  binds `expires_at: None` deliberately (a draft the user is being asked to confirm in the same
  breath does not expire).
- Therefore 0041's and 0042's triggers are dormant, `list_live_for` and `get_live` would return
  nothing if called, and `expire_due` has no caller at all.
- The one exception, and it is real: the **draft** expiry half runs on every approval today.
  `DraftMcpServer::decide` evaluates `draft.is_live_at` and `SqliteDraftRepository::list_pending`
  filters on the column, for every draft, every turn. Those guards are *in* the path rather than
  beside it; they simply never see a non-NULL value yet.

**Why I did not wire it anyway.** The obvious move was a reader: teach `list_drafts` to append the
caller's live proposals, since it already resolves the caller's `ProfileScope` from the engine's
`_meta`. I rejected it. A reader with no writer is a fixture production cannot produce, which is
this programme's most-recorded test defect and not a thing to build deliberately; it would still
need an installer call in `main.rs` that this round did not own, so it would have shipped a second
inert layer under the first rather than replacing it; and PAI-6 P1 already rejected the same move in
the same words — "a no-op adapter to make the port look wired — this programme already has three
correct-but-unreachable mechanisms and each cost a later round more than the gap would have".

**The exact call site that will reach it, and the phase that owns it.** P4, the `proactive-reviewer`,
is the producer and is the first phase that can honestly construct any of this:

1. `serve()` in `crates/pond-server/src/main.rs`, on the line after
   `let draft_repo = Arc::new(SqliteDraftRepository::new(db.system.clone()));` — the same pool, the
   same shape: `let proposal_repo: Arc<dyn ProposalRepository> =
   Arc::new(SqliteProposalRepository::new(db.system.clone()));`.
2. The reviewer loop holds it and calls `save` for each accepted impulse, gated by
   `consolidation_schedule.rs :: should_run` (section 3.3).
3. The read side gets its first caller in the same phase, and the cheapest honest one is
   `DraftMcpServer::list_drafts` — it already knows who is asking, and `ProposalAudience` is exactly
   the `Owner(id)` its `owner_stamp` resolves. `expire_due` belongs on the reviewer's own tick; it
   is tidying, and nothing's correctness depends on it, because every read filters expiry in SQL.

**P3b, still owed and not started.** `GET /api/v1/proposals` and a surface to render it. See the
correction above: that surface does not exist for drafts either, so P3b is "build the approval
surface", not "add a proposal card to it". Until P3b, a proposal that P4 writes is visible only to
the model, through `giap-draft`.

**Two clocks, worth knowing before writing a test here.** The read path takes an injected `now` (the
port makes it a parameter of every method, so expiry cannot be skipped and does not need a sweeper).
0041's approve trigger cannot be handed one and compares against SQLite's own `datetime('now')`. A
fixture frozen in the past is therefore live to the reader and already unapprovable to the trigger —
which is not a defect, but it is how a test written with one clock in mind fails against the other.

### 3.3 The background reasoner

A `proactive-reviewer` `AgentRole` ([PAI-6](./06-multi-agent-orchestration.md)) that runs in idle
time over recent events, memory, and ingested context, and emits at most `max_proposals_per_day`
proposals.

**When it runs** reuses `user_data/services/consolidation_schedule.rs`'s `should_run` — a pure,
tested gate encoding *at most once per interval, only after real user activity in this process
lifetime, never at startup*. That function was written to fix exactly the failure this feature would
otherwise reproduce: a background loop firing fifteen minutes after every boot on a machine nobody
has touched. It is the right gate, it is already proven, and it should not be reinvented.

It runs as a subagent so it gets its own context window and its own tool scope — the parent
conversation is not polluted by the reasoning, and the reviewer cannot reach tools the user has not
granted it.

Budget: capped turns, capped proposals, cancelled by any user activity. On the Orin this competes
with the user for the only GPU, so "cancelled by activity" is not politeness, it is correctness.

### 3.4 Delivery: give the targeted path its first producer

Proposals are delivered to **the profile's devices**, not broadcast. That means:

- `NotificationSender::send()` with a real target, which finally exercises
  `sqlite_notification_queue` (so a phone that was off gets it on reconnect) and `fcm_push_relay`
  (so a backgrounded phone wakes and fetches the content locally, with nothing sensitive crossing
  Google's infrastructure — the existing design is already right for this).
- Device resolution comes from PAI-1: profile → paired devices → push tokens. **Corrected
  2026-08-11 — that chain now exists in storage.** It did not on 2026-08-09, and this paragraph said
  so; PAI-1 P9 landed migration 0043 two days later, adding `profile_id` to `devices` (ON DELETE SET
  NULL) and to `pairing_codes`, captured at code **issuance** so a pairing client cannot name its
  own member. `DeviceAttribution` is the port: `device_profile` for the identity direction,
  `devices_for_profile` / `push_tokens_for_profile` for the delivery one, and an unattributed device
  is deliberately returned by neither — deliver to nobody, never to everybody. `PushToken` still has
  no `profile_id` and deliberately so: one writable source of truth, reached through
  `push_tokens.device_id`.

  **What has not changed is P5's blockage, only its shape.** Nothing constructs
  `SqliteDeviceAttribution` — `crates/pond-infra/tests/device_profile_rung_is_not_wired_yet.rs`
  asserts that on every run and is written to fail the day it stops being true — and no route
  captures a member at issuance yet. So P5's prerequisite is now "wire the port and give issuance
  the question", not "build the schema", and a phase reading the old sentence would go and add a
  column that is already there. Until then invariant 4 has no mechanism at the delivery end:
  `broadcast_notification_sender.rs` says in its own doc comment that targeted `send()` is exercised
  by nothing and every production producer calls `broadcast()`. Landing P5 against unattributed
  devices would silently deliver nothing.

**Unprompted speech** is opt-in and tightly gated: only when that profile is identified as present,
only outside quiet hours, only for categories the user enabled, never mid-conversation, and never
for `Guest`. The voice child is a separate process, so the trigger arrives over the existing
notification stream rather than through `AppState`.

Category gating reuses `Notification.category` (`alert` / `info` / `action_required`), which already
exists.

### 3.5 The feedback loop

Approved and rejected proposals are written back as memories through the existing `MemoryEvent`
machinery, tagged with the trigger kind. The reviewer reads them, so "we never want to be told about
the garage door during the day" becomes a learned constraint rather than a setting nobody finds.

This is what makes proactivity converge instead of nagging, and it costs almost nothing because
memory extraction, decay and consolidation already exist.

### 3.6 Repairs this workstream also owes

- **Durable rule cooldowns.** Persist the debounce map; an in-memory cooldown means a restart loop
  can re-fire a rule indefinitely.
- **`TaskKind::ToolCall { tool, args }`.** A deterministic scheduled tool invocation, so "read the
  freezer sensor at 6am and notify if above -15" does not depend on a 3B model correctly interpreting
  an English instruction at 6am.
- **REST for sensor rules.** A first-class `/rules` surface rather than overloading `/schedules`.
- ~~`docs/architecture/scheduling.md` refreshed.~~ Done already.

---

## 4. Phases

- **P1** Widen `BusEvent` with `Time`, `Session`; publishers for both. No behaviour change.
  **LANDED 2026-08-10.** See 1.1 for what is true now. Two things a later phase must not assume:
  the observer publishes nothing about sessions the pond minted for itself (`SessionOrigin`), and
  `rules_to_fire`'s refusal to act on a viewless event is guarded by `#[non_exhaustive]` rather
  than by a test at that call site.
- **P2** `Presence` events from PAI-1's identification chain. **LANDED 2026-08-11, REPAIRED the
  same day** — read both stamps in 3.1, and the repair before the landing, because three of the
  five findings were in the landing stamp's own paragraphs. Four things a later phase must not
  assume: presence is the **face and explicit** rungs only, and there is no geofence source of any
  kind; a `Departed` is the pond's own timeout, not an observation of somebody leaving; a member who
  is present when the process starts is recorded rather than announced, so P4 must read the
  *absence* of an `Arrived` as "no edge crossed" and never as "nobody is home"; and the whole thing
  is silent on a stock install until PAI-1's identification routes get a caller. A fifth, from the
  repair: **`presence.profile` rows are readable through `giap-audit` today** — `Sensitive` bounds
  their retention, not their audience — so P6's TTS gating and anything else that treats a presence
  row as private needs the `audit.rs` decision made first.
- **P3** `Proposal` domain + persistence in the drafts table; proposal UI in the existing drafts
  surface.
- **P4** The `proactive-reviewer` role, gated by the `should_run` shape from
  `consolidation_schedule.rs`, budget-capped, cancelled by activity.
- **P5** Targeted delivery: profile → devices → queue + FCM. First production producer for both.
- **P6** Opt-in unprompted TTS with presence, quiet-hours and category gating.
- **P7** Feedback loop into memory; reviewer reads prior decisions.
- **P8** Repairs: durable cooldowns, `TaskKind::ToolCall`, `/rules` REST, docs.

---

## 5. Invariants

1. **GIAP proposes; the user disposes.** No proactive path performs a side-effecting action without
   an approval, explicit or previously granted for that kind.
2. Every proposal carries a rationale the user can read.
3. Proactive work never runs while the user is mid-turn, and is cancelled by activity.
4. Proposals are addressed to a profile, never broadcast to the household.
5. `Guest` sessions generate no proposals and receive none.
6. Quiet hours are absolute for speech.
7. A proposal expires. An assistant that surfaces yesterday's suggestion has failed twice.

---

## 6. Deliberate deferrals

- **Learned trigger discovery** (inferring new rules from behaviour). The feedback loop in 3.5 is
  the honest first step; inferring rules unsupervised is a much larger claim.
- **Cross-household coordination.** One pond, one household.
- **Geofencing.** Needs GOTG background location, which is a PAI-8 concern and a battery
  conversation. **Still true after P2, and worth saying in the positive:** there is no geofence in
  this tree — no location source, no `Geofence` identification rung, nothing that could produce one.
  A presence event never means "left the house"; it means the pond's evidence about a member went
  stale. Anything downstream that reads it as geography is reading something that was never
  measured.
- **Anomaly detection on camera events.** Genuinely valuable and genuinely a research task; the
  classifier currently emits a top label above 0.5 confidence and nothing models what is *normal*
  for this home.

---

## 7. Verification

- **Unit** — the `should_run` gate under boot-with-no-activity, activity-then-idle, and
  activity-during-run. These are the three cases that broke consolidation.
- **Integration** — a camera event outside any user rule produces a proposal, not an action; approving
  it performs the action; rejecting it writes a memory.
- **Delivery** — with a device offline, a targeted proposal is queued and delivered on reconnect.
  This is the first test that exercises `sqlite_notification_queue` end to end.
- **Speech** — assert no unprompted speech in quiet hours, with the profile absent, or in a `Guest`
  session. Three separate tests; each is a distinct way to be creepy.
- **Load** — assert `max_proposals_per_day` is enforced across restarts, not just within a process.
- **Manual, on the Orin** — start a long chat turn while the reviewer is running; assert the reviewer
  cancels within roughly 500 ms, matching the behaviour the consolidation watcher already achieves.
