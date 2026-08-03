# PAI-7 — Proactive intelligence

Requirement: *GIAP needs to be proactive, not just reactive.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (who to tell) and
[PAI-6](./06-multi-agent-orchestration.md) (what runs the background thinking).

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 There is an event bus, and it is narrow

`shared/ports/event_bus.rs:29-33`:

```rust
pub enum BusEvent {
    Sensor(SensorReading),
    Camera(CameraEvent),
    Device(DeviceStateChanged),
}
```

A closed enum, deliberately (`:25-28`: so consumers "can pattern-match ergonomically instead of
parsing an attribute map"). `EventBus::publish` is non-blocking and infallible; `subscribe` returns
a `Stream` (`:92-100`). Adapter: `shared/services/in_process_event_bus.rs`.

**Two consumers exist:** the rules engine, and a bridge that appends bus traffic to the durable
event log (`main.rs:2667-2678`). There is no time event, no presence event, no session event.

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
`/schedules` (`routes.rs:6498-6521`), with the MCP tools (`create_sensor_rule`, `list_sensor_rules`,
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

(`docs/architecture/scheduling.md` still describes two task kinds and seven MCP tools. There are three
and twelve. Corrected as part of this workstream.)

### 1.5 The outbound channel is one broadcast, and half of it is dormant

The complete list of unprompted notification producers:

| Site | Trigger |
|---|---|
| `main.rs:2660` | schedule completion or failure |
| `schedule_executors.rs:107` | a fired rule's `Notify` action |
| `pond-mcp-server/src/system.rs:214` | the `send_notification` tool — model-initiated, but **only inside a user-started turn** |
| `routes.rs:542` | a device-pairing security notice |

**All four call `broadcast()`.** The durable offline queue and the FCM relay are only reachable from
the targeted `send()` path (`broadcast_notification_sender.rs:68`), so `sqlite_notification_queue`
and `fcm_push_relay` — both real, both tested, the latter carrying a genuinely thoughtful
content-free wake-ping design (`fcm_push_relay.rs:1-26`) — have **no production producer**.

### 1.6 GIAP never speaks first

Every `voice_output.speak()` call site is downstream of a user utterance
(`shared/services/chat.rs:933,953,1127,1593,1604`) or an explicit `/tts` request
(`routes.rs:6395`). The voice child is a separate OS process and cannot reach `AppState` at all.
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
    Presence(ProfilePresence),       // a profile arrived or left (face, paired device, geofence)
    Session(SessionLifecycle),       // started, went idle, resumed after a gap
    Ingest(SourceUpdated),           // PAI-8's hook: new mail, new calendar item
}
```

`Presence` is the one that makes proactivity feel personal rather than mechanical, and PAI-1's
identification chain is exactly what produces it. `Session` is what tells the proposer the user is
*available* — the difference between a helpful nudge and an interruption.

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
registered as a safety extension (`giap_registration.rs:64`) and already models save / list /
approve / reject (`pond-mcp-server/src/draft.rs:97,167,200,248`), with a UI. Reusing it means
proactive actions inherit an approval flow that is already built, already understood, and already
trusted — rather than a second, parallel confirmation mechanism users would have to learn.

Approve-once and always-approve-this-kind are the obvious follow-ons, and they are how the system
earns autonomy incrementally instead of demanding it up front.

`rationale` is mandatory. An assistant that says "you asked me to watch for this, and the delivery
window closes at six" is helpful; one that says "I did a thing" is alarming.

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
- Device resolution comes from PAI-1: profile → paired devices → push tokens.

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
- **`docs/architecture/scheduling.md`** refreshed.

---

## 4. Phases

- **P1** Widen `BusEvent` with `Time`, `Session`; publishers for both. No behaviour change.
- **P2** `Presence` events from PAI-1's identification chain.
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
  conversation.
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
