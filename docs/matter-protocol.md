# The `giap-matter` protocol

GIAP's Matter controller and the Rust adapter are two implementations of the
protocol described here. Both point at this file:
`matter-server/src/protocol.ts` and
`crates/pond-adapters-matter/src/protocol.rs`. Change one, change this, change
the other.

---

## What kind of protocol this is

Domain-level, not Matter-level. The wire carries **devices, readings and control
verbs**. It never carries endpoints, clusters or attribute paths.

That is the whole design decision. Every mapping from Matter's model onto GIAP's
— which cluster a verb becomes, which endpoint it lands on, what unit a value is
in, what a node's device type is — lives in the controller, because that is where
matter.js's typed cluster models are. The Rust adapter does not know what a
cluster is, and cannot be made wrong by a Matter detail.

The predecessor protocol was the other way round, which is why the adapter used
to hold a hand-maintained table of decimal cluster ids, parse `"1/6/0"` attribute
paths, and convert units by hand.

---

## Transport

WebSocket, one JSON object per text frame, on `ws://127.0.0.1:<port>/giap`.

The controller binds **loopback only, and it is not configurable**. It holds the
fabric's operational credentials: anything that can reach the socket can drive
and unpair every device in the house, and the socket has no authentication of its
own. GIAP only ever auto-manages a controller on `127.0.0.1` (see
`local_port_from_ws_url`), so a wider bind would create exposure nothing asked
for. An operator who wants a shared controller runs their own and points
`matter_ws_url` at it — a deliberate act rather than a default.

The path names the protocol on purpose: an address left over from an earlier
release fails at the handshake instead of half-working.

---

## Greeting

The controller speaks first:

```json
{"protocol": "giap-matter", "version": 1, "fabric_id": 1, "matter_js": "0.17.9"}
```

The client refuses anything else and names what it found. `version` is bumped
when a change would break a client that has not been updated with it; a mismatch
is refused rather than guessed at, and reported as "the controller and
pond-server are from different releases".

---

## Envelope

```json
→ {"id": "giap-1", "op": "control", "params": {…}}
← {"id": "giap-1", "ok": true,  "result": {…}}
← {"id": "giap-1", "ok": false, "error": {"code": "…", "message": "…"}}
← {"event": "reading", "payload": {…}}
```

Responses correlate by `id` and may arrive out of order — a slow `control` behind
a fast `ping` is ordinary.

`ok` is the discriminant, not the presence of `result`. A successful op with
nothing to return is `{"ok": true, "result": {}}`, and keying off `result` would
read a failure carrying a null result as a success.

---

## Ops

| op | params | result |
|---|---|---|
| `subscribe` | — | `{devices: [Device], readings: [Reading]}`, and subscribes the connection |
| `discover` | — | `{commissionable: n}` |
| `commission` | `{code, name?}` | `{device: Device}` |
| `decommission` | `{device_id}` | `{}` |
| `control` | `{device_id, verb, value}` | `{applied: DeviceStatePatch}` |
| `describe` | `{device_id}` | `{description: DeviceDescription}` |
| `state` | `{device_id}` | `{state: DeviceState}` |
| `ping` | — | `{}` — liveness without a fabric round-trip |

`subscribe` returns the **whole fabric**, readings included, so a fresh
connection knows the current state without waiting for anything to change.
Without the readings a steady sensor exists in the device list while every
question about its value is answered "none recorded", which reads as "that device
is not here".

### Control verbs

Exactly GIAP's `DeviceControlPort` vocabulary:

| verb | `value` | applies |
|---|---|---|
| `power` | `true` / `false` | `on` |
| `brightness` | 0–100 | `brightness`, `on` |
| `target_temp` | degrees Celsius | `target_temp` |
| `locked` | `true` / `false` | `locked` |
| `color` | `{hue: 0–360, saturation: 0–100}` | `hue`, `saturation` |
| `color_temp` | kelvin | `color_temp` |
| `volume` | 0–100 | `volume` |
| `fan_speed` | 0–100 | `fan_speed`, `on` |
| `fan_mode` | `off`/`low`/`medium`/`high`/`on`/`auto`/`smart` | `fan_mode`, `on` |
| `position` | 0–100 percent **open** | `position` |
| `tilt` | 0–100 percent **open** | `tilt` |
| `mode` | `{setting, value}`, both as the device words them | `mode` |
| `operation` | one of the operations the device offers | `operation` |

Values are in GIAP's units. The controller converts.

**`applied` is what the device did, not what was asked for.** A dimmer that
clamps to its own minimum reports that minimum. This is why the field exists: the
old adapter built its outcome from the caller's request, so a device that did
something else was still described to the user as having obeyed.

That was true of `operation` alone for a while, and of nothing else — every other verb
echoed the request straight back. A fan told to run at 85% reported 85% while the device
had quantised it onto its High mode and was sitting at 90, and the number the user read
was the number they typed. So the controller now reads the value back for every verb
whose result can differ from the request: `fan_speed`, `brightness`, `color`,
`color_temp`, `target_temp`, `position`, `tilt` — quantised onto a cluster's scale,
clamped to a device's stated limits, or still travelling.

It reads through the same inverses `state` reads with, so the number reported is one that
would put the device back where it is. It returns as soon as the reading moves rather than
waiting a fixed window, and a device already at the requested value is not waited on at
all. `power`, `locked` and `fan_mode` are deliberately not read back: a value with nowhere
else to land buys nothing for the latency. Where a device reports nothing for the verb the
request stands, because an absent reading is not evidence of a different one.

That holds only if the command's *response* is read. Matter commands do not merely
succeed or throw: Operational State answers every Start/Stop/Pause/Resume with an
`ErrorStateID`, and ModeBase answers `changeToMode` with a `status`, so a refusal
arrives as a perfectly successful invocation carrying a non-zero code. A controller
that discards the response reports a refusal as a success — which is how GIAP told
a user a washer was running while the washer sat there saying Stopped. Refusals
become `device_refused`, worded by the device where it says why.

`operation` accordingly applies the state the device reports being in, not the verb
that was sent, and the operations offered are derived from `operationalStateList`
rather than assumed: each of the four commands is optional, and the spec requires a
device to expose the states matching the commands it supports.

**Which setpoints a thermostat has comes from `controlSequenceOfOperation`**, the
mandatory attribute that states whether the device cools, heats, or both. It was not read
at all, and presence was inferred as `"occupiedCoolingSetpoint" in clusters` instead —
a KEY check, where matter.js populates a key for every attribute in the cluster model and
leaves the unsupported ones `undefined`. So every thermostat looked like it had both
setpoints, and a cooling-only air conditioner advertised a range from 7 C: the floor of a
heating setpoint it does not implement, unioned in. Claims first, evidence second, and
neither strips a control on silence — a device stating nothing keeps both, because
withholding a setpoint from an under-reporting thermostat would be the worse failure.

`systemMode` is read through the same number-or-enum-name tolerance as FanControl's mode
sequence. Compared as a raw number it never matched a device reporting `"Cool"`, so the
mode read as unsettled and the description fell back to the union of both setpoints'
ranges — which is the other half of how 7 to 32 reached the user.

`target_temp` names whichever setpoint the thermostat currently runs on. A
thermostat has two — one it heats up to, one it cools down to — and writing the
heating one to a device that is cooling moves a number nobody asked about while the
cooling carries on unchanged. Cool means the cooling setpoint, Heat means the
heating one; Auto and Off run neither exclusively, so there the requested value
picks whichever it is nearer to. Their limits differ and each bounds the other
across `minSetpointDeadBand`, which is why the range a description reports depends
on the mode, and why a thermostat advertising a 30 degree maximum can refuse 24
while heating and accept 30 while cooling.

A range that only holds in one mode says so, through `when` on the number: the
condition it is true of, and where the device still reaches beyond it. Stated bare
the number reads as a fact about the device, so the same question minutes apart
answers 7 to 23.5 and then 7 to 32 with nothing to explain either, and a reader
concludes 30 is impossible when it is one mode away.

`tilt` is a covering's second axis, not a variant of `position`. Lift is how far a
blind is lowered and tilt is how far its slats are turned, and a venetian blind is
routinely down with its slats open — which `position` alone cannot ask for. Offered
only by a covering that reports a tilt position, since a roller blind has nothing to
turn and a control a device will reject is the failure this area exists to stop.
Zero is open on both axes: the spec has `GoToTiltPercentage` treat a zero percentage
as `UpOrOpen`, so one conversion serves both.

`color_temp` is a second colour control, not a second way to reach the first. 2700K white
has no hue, so it cannot be asked for through hue and saturation at all — which is why it
earns its own verb while XY, which addresses the same perceptual space hue and saturation
already cover, does not get one.

Both are offered strictly according to the device's own `colorCapabilities`. Presence of
the ColorControl cluster used to imply hue and saturation outright, and that is wrong in a
way that shows: a tunable-white bulb has the cluster and no hue whatsoever, and was
offered a hue it rejects — the same failure as offering `tilt` to a roller blind.

The wire carries **kelvin**; the cluster takes mireds. Mireds are reciprocal megakelvin,
so the conversion inverts the bounds — the smallest mired value is the hottest colour —
and a range built without inverting has a minimum above its maximum, which reads as a
broken device rather than a broken conversion. The range itself comes from
`colorTempPhysicalMinMireds`/`MaxMireds` where the device states them, and is absent where
it does not: the spec's own default for those is 0, which converts to infinite kelvin.

`applied` reports the kelvin the device will sit at rather than the kelvin requested,
because the round trip through whole mireds is lossy — asked for 2700 a device lands on
2703, and echoing the request would overstate the precision.

`target_temp` also carries an appliance's own setpoint. Temperature Control has two
shapes: a washer names levels, which are read as a `mode`, while a dishwasher states
a number with its own minimum, maximum and `step`, taken by `setTemperature` rather
than an attribute write. Reading only the levels, GIAP reported "you cannot set a
temperature for the dishwasher" about a device showing a 49 to 82 degree slider. The
existing verb carries it rather than a new one, a per-appliance vocabulary being the
thing this area exists to avoid.

A stated `step` travels with the range, because it is as much a part of what will be
accepted: 50.5 into a dishwasher taking whole degrees is refused.

The pairing reads backwards until you know what a setpoint is: heating runs BELOW
its setpoint and cooling ABOVE its own, so heating is always the lower of the two.
They bracket a band rather than describing how hard either can work.

Reporting a state means waiting for it. A cluster's state is whatever the
subscription last reported, and the report carrying a change arrives *after* the
command returns — against the Matter Virtual Device, the command answered in 13ms
and the new state landed within 500ms. So the controller waits for the state the
verb asks for (Start for Running, Pause for Paused) before answering, returning as
soon as it appears and giving up after two seconds. Read without that wait, a
washer that started perfectly well reports as stopped, which is worse than the echo
it replaced: an echo is uninformative, while this contradicts a device that did
exactly as it was told.

---

## Events

| event | payload |
|---|---|
| `device_added` / `device_updated` | `{device: Device}` |
| `device_removed` | `{device_id}` |
| `device_availability` | `{device_id, online}` |
| `reading` | a `Reading` |
| `log` | `{level, kind, message, fields?}` |

`device_availability` is a **level report, repeated**, not an edge. Every thirty
seconds the controller names every peer it has and says whether that peer is
reachable, whether or not the answer changed. Both branches on the receiving side
are idempotent — insert into a set and heartbeat a row, or remove from a set — so
repetition costs a set operation.

That is not tidiness, it is the fix for a device that read offline while it was
working. It used to be an edge, sent only from matter.js's `lifecycle.online` /
`.offline`, which fire on a transition and only on a transition — and, per
`#retryWiring`'s own comment, *a node already online when the controller connects
never fires `online` again*. So the bridge's set of devices it vouches for was seeded
once, from a `subscribe` snapshot that reads `peer.lifecycle.isOnline`, which means
"is there a live CASE session right now" and is false until one exists. A snapshot
taken inside that window recorded a working device as offline; nothing ever said
otherwise; `last_seen` aged past the registry's five-minute threshold; and the card
went offline while readings kept arriving from matter.js's own cache. A level report
recovers from a missed, mistimed or lost transition within one tick, whichever it was.

The bridge logs the **changes** at info (`matter_availability_changed`), not the
reports. It logged them at debug before, and the tracing filter admits debug from
`pond_server` only — so a state change the user sees on a card, and gets an OS
notification for, left no trace in any log file. That absence is most of why this took
three passes to find.

`log` is the controller's own structured record, relayed into `tracing` at the
level it names. The Rust side also pipes the child's stderr, so a controller GIAP
started is audible twice over; the event is what makes a controller the operator
runs themselves — where there is no pipe to read — just as legible.

---

## Types

```ts
Device  = { id: "matter-<node_id>"[-<endpoint>], name, device_type, capabilities: string[], online }
Reading = { device_id, sensor_type, value, unit, at }        // at is RFC 3339
```

Both are direct projections of GIAP's own `Device` and `SensorReading`, so the
Rust side deserialises them without a mapping step. `Device` is deliberately
smaller than GIAP's: the controller knows nothing about rooms, hostnames or when
a device was first registered, and inventing values for those would make a Matter
device look different from every other kind.

---

## One node is not always one device

A Matter **bridge** — a Hue, Aqara or Tuya hub — is a single commissioned node whose
Aggregator endpoint (device type `0x000e`) carries a Bridged Node child (`0x0013`)
per real device. So one node is a dozen devices, and GIAP mapped one device per node:
such a hub arrived as a single thing that was simultaneously a light and a lock,
whose state was whichever child held the lowest endpoint number, and which could only
ever be driven at that one child. "Turn off the hall lamp" turned off the kitchen lamp
and reported success.

`deviceSlices` cuts the node into one **slice** per device, and a slice is a
`NodeSnapshot` whose endpoints are just that device's subtree — so every mapping runs
over one unchanged. Nothing in `describe.ts`, `state.ts` or `control.ts` learned what
an endpoint is.

| | |
|---|---|
| id | `matter-<node>` for the hub, `matter-<node>-<endpoint>` for each child. An ordinary node keeps the id it always had, so existing rows and fabric state survive. |
| sliced on | **Bridged Node**, not Aggregator. The Bridged Node is what defines a device; the Aggregator only says a bridge exists, and the two descriptors arrive in independent subscription reports. |
| the hub | **always** a device, typed `bridge`, driving nothing. |
| names | `bridgedDeviceBasicInformation` `nodeLabel` → `productName` → `vendorName`, then the hub's own Basic Information, then `"<Type> <node>-<endpoint>"`. |
| `online` | the node's reachability AND `bridgedDeviceBasicInformation.reachable`, which fails **open** when unstated. |
| endpoint order | `applicationEndpoints` returns the slice's own endpoint FIRST, then ascending. |
| deletion | `matter-<node>-<endpoint>` cannot be decommissioned — Matter commissions nodes. The API refuses it and names the hub. |

Four of those are less obvious than they look.

**The hub is a device on purpose.** Clusters populate late — that is what
`#retryWiring` exists for — so the first snapshot after commissioning has no device
types and yields one slice for the whole node. If that slice were not the hub it
would register as a device and then never be removed: the peer still exists, so
`peers.deleted` never fires. The user would be left with a permanently-offline row
they could not delete without decommissioning the hub. Emitting it deliberately also
gives an empty hub something to be, gives `commission` something to return, and gives
deletion a handle.

**Endpoint order is root-first, not ascending.** Every lookup downstream resolves ties
by taking the first endpoint, and `deviceTypeFromDescriptor` justifies that with "a
composed device is reported as whatever its first endpoint claims, which is what its
own UI calls it". True of an air purifier with a fan inside it. False behind a bridge,
where the numbers are the **hub's** to allocate in its own discovery order — a bridged
video player at endpoint 7 whose Speaker part landed at 3 would be typed a speaker.

**The child tree comes from matter.js, not from `partsList`.** The Descriptor
attribute is in the snapshot and looks like the same thing. The spec gives an
Aggregator's PartsList *full-family* semantics — every descendant — and a composed
device's *tree* semantics, so it cannot say whether a grandchild is a child. It may
name endpoint 0, and a walk over one that does gives every child the hub's identity.
It may be cyclic, and a recursive walk over one that is does not terminate — inside
`subscribe`, so one bad firmware would put the bridge in a permanent reconnect loop.
matter.js has already resolved all of it into a real tree to build its endpoint index,
so `endpoint.parts` is the answer rather than the evidence.

**Endpoint numbers stay absolute.** `VendorClusterSpec.endpoint` and a control
action's endpoint are the node's own numbering, not slice-relative, so they share one
namespace with `peer.endpoints.for(...)`.

**No `PROTOCOL_VERSION` bump.** `commission` still returns one device — the hub — and
it can only ever honestly return that, because the children's descriptors have not
populated at the moment it answers. They arrive seconds later as `device_added`,
which the Rust side already handles. An un-updated client therefore reads the
greeting, registers the hub and receives the children exactly as it does for any
node, so the rule ("bump only if an un-updated client would break") is not met.

**Not done.** A bridged child unpaired in the vendor's own app leaves the slice set
silently; nothing yet emits `device_removed` for it. And a hub commissioned with a
dozen children still sends a dozen pairing notifications.

---

## Describing a device

`capabilities` on `Device` is a list of verb names. It is enough to know a fan has
a speed and not enough to drive one: it cannot say which modes *that* fan has,
what a thermostat's limits are, or that an air quality sensor measures eleven
separate substances. An agent given only the list guesses, and learns the limits
by failing at them in front of the user.

`describe` answers that, **read from the device rather than assumed**. Where a
cluster states a constraint it is carried: FanControl's `fanModeSequence` says
which modes the fan really has, a thermostat states its setpoint limits, a
concentration cluster declares its unit. Where a cluster states nothing, the
conventional default stands and nothing further is claimed — an invented
constraint is worse than an absent one, because it will be believed.

```ts
DeviceDescription = {
  device_id, device_type,
  capabilities: Capability[],      // what it can be told to do
  sensors: SensorSpec[],           // what it measures, reported or not
  vendor_clusters: VendorClusterSpec[],  // what it has and this cannot drive
  states: StateSpec[],             // what it reports and nothing can set
}
VendorClusterSpec = { cluster_id, endpoint }
StateSpec = { name, value: ValueSpec }
Capability = { verb: Verb, setting?: string, value: ValueSpec }
ValueSpec =
  | { kind: "boolean" }
  | { kind: "percent" }                          // 0–100
  | { kind: "number", min?, max?, unit? }        // absent key = unstated
  | { kind: "enum", values: string[] }
  | { kind: "color" }
SensorSpec = { sensor_type, unit }
```

`verb` is exactly a control verb, so a description and a `control` call cannot
drift apart: anything describable is callable, by construction.

`mode` carries a `setting` name because a device has more than one: a washer has a
wash cycle, a spin speed, a rinse count and a temperature level, and without the
name they are indistinguishable in the list. It is the same name `control` is
called with, and every layer that carries a description has to carry it — a
renderer that drops it leaves a reader four identical `mode` entries and no way to
name one, which reads as a device with no controls at all.

**Settings are found by shape, not by a list of cluster names.** Matter's appliance
controls are nearly all ModeBase derivatives, publishing `supportedModes` as
`{label, mode}` pairs the device chose. So a cluster nobody has written code for
works the day a device ships it, and the values offered are the labels that device
published — "Whites" appears because the washer said "Whites". The two that are
not ModeBase, Temperature Control and Laundry Washer Controls, are read explicitly
because their shape differs, not because they are special.

One name-shaped constraint survives, upstream of all this: a snapshot reads a
bounded set of clusters, because it is rebuilt on every node event and reading all
of them on a busy fabric costs more than the unread data is worth. The bound admits
anything named `*Mode`, which is how Matter names every ModeBase derivative, so the
promise above holds — but a control that is neither in the fixed set nor named that
way has to be added to it. Settings read by shape from a snapshot filtered by name
is a contradiction worth knowing about: it is what made a paired washer report
nothing but power while every unit test passed.

That bound is still there, but `describe` no longer hides what it drops — and the set is
now **derived from the mappings** rather than written beside them. Each module declares the
clusters it reads (`deviceClusters()`, `settingClusters()`, `sensorClusters()`), so a
cluster's name lives in the file that uses it and there is no second place to remember.

That second place is not hypothetical. Media control shipped complete and dead:
`mediaPlayback`, `mediaInput` and `audioOutput` were declared inside `settings.ts` as
module-private constants, the allowlist was not updated, and `readClusters` dropped all
three — so a television described nothing but power and volume while every unit test
passed. The fixtures build snapshots by hand and never cross the filter, which is the
identical reason a paired washer once reported nothing but power. The `*Mode` rule remains
the escape hatch that needs no list at all.

### What a device reports and nothing can set

`states` is the third kind of thing a device has. `capabilities` are verbs `control`
accepts; `sensors` are numeric measurements, carried on the same feed as `Reading`. A
door's position is neither — a word the lock reports, writable by nobody — so it fell
through both, and a lock that can say **jammed**, **forced open** or **ajar** was
described as a device with one boolean. Asked what a door lock could do, GIAP answered
"locked or unlocked; it does not measure any data" for a device whose own app showed a
door state beside the lock state. Both attributes were in the snapshot the entire time:
`doorLock` is read whole, and there was simply nowhere in the description to put them.

`value` declares the exact words `state` will use, so the two cannot drift — for a
read-only value the list of words *is* the description, which is why the vocabulary
lives in `describe.ts` and `state.ts` imports it.

Each entry is gated on the attribute actually being present, because both belong to
optional DoorLock features: `doorState` to DoorPositionSensor, and
`requirePinForRemoteOperation` to CredentialOverTheAirAccess **and** PinCredential
together — matter.js refuses the attribute without both. A plain deadbolt has neither,
and declaring one would promise a reading that never arrives — the same failure as
offering `tilt` to a roller blind.

`doorState` is also nullable, so a lock with the sensor can have the attribute and no
value in it. The description still declares the door, exactly as `sensors` declares
what a sensor measures before it has reported; the reading stays absent rather than
being filled in, because an invented "closed" cannot be told from a real one.

**A device that is nothing but states.** A Generic Switch (device type `0x000f`,
cluster `switch`) reports which way it is thrown and takes no orders at all, so
`states` is not one slot among three for it — it is the only slot it has. Before it
existed the switch had none: `0x000f` was not a device type GIAP mapped and `switch`
was not a cluster the snapshot admitted, so a commissioned switch arrived typed
`matter` with no capabilities and answered "cannot be controlled, and it does not
measure any data" — while the maker's app showed its position plainly.

It reports `switch_position`, bounded by `numberOfPositions` **only when the device
states one**; the spec's default is 2, and a default is not a statement. It also
reports `switch_kind`, latching or momentary, when the feature map claims one — and
that is worth saying out loud rather than inferring, because the position means
different things in the two cases. A latching switch stays where it is put. A
momentary switch is a pushbutton whose `currentPosition` returns to rest on release,
and everything interesting about it — the press, the release, the double-press —
arrives as a Matter **event**. `#observeCluster` wires attribute-change observables
only (`*$Changed`), so those presses are not observed here at all. "Reports a
position, 0 to 1" describes a latching switch well and misleads about a button; a
reader told which kind they have can tell the two apart. Subscribing to Matter events
is the work that would close that gap, and it is not done.

Note the strictness. `switchKindOf` treats an unstated feature map as "the device did
not say", where `clusterHasFeature` treats one as a yes. Both are right for their own
question: withholding a reading that works is the worse mistake, and putting a word in
a device's mouth is the worse mistake.

**Read-only by construction, not by convention.** `requirePinForRemoteOperation` is a
security control: off means remote lock and unlock stop requiring a PIN. The same
cluster carries `sendPinOverTheAir`, `enableLocalProgramming`, `wrongCodeEntryLimit`,
`autoRelockTime` and `operatingMode` in the same snapshot. A verb for any of them puts a
lock's security configuration one sentence of natural language away from being turned
off, so the whole class is closed here rather than guarded case by case. `control` has
no verb that reaches them.

### Media

**Level Control means volume on a speaker and brightness on a light**, and the ENDPOINT's
device type is what says which. A Basic Video Player is composed — the player on one
endpoint, a Speaker (`0x0022`) on another — and Level Control lives on the speaker.
Searching the node for the cluster found it and reported brightness, so a television
advertised a brightness control that would have turned the sound down. Split by endpoint
type, and a device can have both: a television with a backlight is not a contradiction.

**Playback rides `operation`, and inputs ride `mode` — no new vocabulary for either.**
Play, pause and stop are what start, pause and stop already mean, so `operationsOf` simply
points the existing verb at MediaPlayback where there is no Operational State. And an
input list is exactly what `mode` was built for: a named setting whose values are labels
the DEVICE published, chosen by sending an index back. "HDMI 2" is offered because the
television said so, the same way a washer's cycles are its own.

The two clusters do not share words for the same idea — Operational State says "stopped"
where MediaPlayback says "not playing" — so the settle target for an operation is a set of
acceptable words rather than one. Without that a television's stop waited out the full
window for a word it was never going to say.

### Alarms

A smoke/CO alarm carries two separate dangers with two separate responses — one says
leave, the other says ventilate — so `smoke_alarm` and `co_alarm` are separate readings,
each gated on the cluster's own feature map. Only smoke was read for a while, which meant
an alarm sounding for carbon monoxide reported nothing about it and a CO-only alarm looked
like a device that measures nothing at all. `alarm_battery` rides alongside them: a
life-safety device with a flat battery is the failure everyone knows about and nobody was
told about.

`expressedState` is a `states` entry rather than a sensor, and the distinction is the one
`states` exists for. It is the attribute a device's own screen shows and the only one that
says WHICH alarm is sounding — a unit expressing a CO alarm while its smoke level reads
Critical is stating something neither reading does. It is categorical, not a magnitude:
"interconnected CO alarm" is not eight times worse than "normal", so an ordinal a
threshold rule could compare would be actively misleading. `alarm_service` and
`alarm_fault` join it, because an expired or faulty alarm is a decoration.

**A cluster's presence is not a sensor's presence** where features decide. `describe`
lists a sensor whether or not it has reported yet, which is right — a device that has not
spoken still measures the thing — but that let cluster presence stand in for attribute
presence. Value presence cannot separate the two cases either: an unsupported attribute
and one that has not reported are both absent. The feature map is the only thing that can,
so a reading naming a `feature` is listed only where the cluster claims it.

### Manufacturer-specific clusters

`vendor_clusters` is what a device has that this cannot drive. A cluster id is 32 bits
with the vendor code in the upper 16, and a non-zero one is the maker's own — outside
the snapshot's bound, unreachable by the shape rule, and **unnameable**: Matter
publishes no attribute names, so the words for these controls ("Flip-Flop",
"Emoticon" on Google's Matter Virtual Device) exist only in that maker's app.

An id and an endpoint is the whole of what is carried, because it is the whole of what
exists. matter.js builds a behavior for a cluster its model cannot name but discovers
no shape for it: measured against a live commissioned device, `cluster$fff1fc01`
carries zero attributes and a `clusterRevision` of 0. So there is not even a count to
report, and reporting a count of zero for a device showing two controls would be the
same silent falsehood this record exists to remove.

It is carried at all because the alternative reads worse than silence. Asked what a
custom light could do, GIAP answered "power and brightness" — true of everything it
could see, and taken by the user as a statement that the two controls their app was
showing did not exist. This is the invented constraint from the other direction: an
absent capability reads as a fact about the device just as readily as a wrong range
does.

Not `Capability` entries, deliberately. A capability is a `control` verb, and there is
no verb here; naming one would break the property the type rests on — anything
describable is callable — to gain a control nothing could actually work. `control`
still answers `capability_unsupported`, `state` still says nothing about them, and
reading or writing a vendor attribute by numeric address is **not** offered: an
unnamed vendor attribute can be a calibration or factory-reset control, and the caller
would be writing it on a guess.

Costs nothing to collect. matter.js builds a behavior for every cluster in the
Descriptor's ServerList, including ones its model cannot name, so the id is already in
hand — no read, no subscription, and nothing added to what the snapshot bound pays for.

It is answered live rather than cached. A description is derived from what the
device currently reports, and a stored copy goes stale exactly when a device is
upgraded or reconfigured — the moment its description matters most.

---

## Reading a device

`describe` says what a device can be told to do. `state` says what it is doing.

```ts
DeviceState = { device_id, values: StateValue[] }
StateValue  = { name, value }        // both as the device words them
```

Every `name` is one `describe` also uses — a control verb for a scalar, a setting
name for a selectable, a `states` entry for something only reported — so a reading
names the thing that changes it: "spin speed is Low" leads straight to the call that
makes it High, and "door is jammed" names something with no such call by design.
That correspondence is the point of the type being this plain, and it is asserted in
the controller's tests rather than left as an intention.

A colour reading names the mode the device is IN, not every attribute it holds: a bulb
sitting at 2700K still carries whatever hue it was last set to, and reporting both makes
the reading contradict itself.

Values are read through the inverses of the conversions `control` writes with, so a
covering reported at 40% open is the same 40% that would put it there — not
WindowCovering's percent *closed*. Anything the device does not report is absent
rather than filled in: an invented "unknown" cannot be told from a real reading one
layer up.

Without this the only way to learn a device's state was to change it. "Is the
washer running?" had no answer that did not involve starting the washer.


## Error codes

A **closed set**, because both the sentence the user reads and the decision to
raise a notification key off it. An open-ended string would push both back onto
substring-matching against controller prose, which is what made "commissioning
failed" the only diagnosis GIAP could offer.

| code | means |
|---|---|
| `no_device_in_pairing_mode` | nothing is advertising; the adapter turns this into the 15-minute-window advice |
| `invalid_setup_code` | the code is not a form the controller recognises |
| `commission_failed` | pairing was attempted and did not complete |
| `device_unknown` | no such device on this fabric |
| `capability_unsupported` | the device has no cluster for that verb |
| `device_refused` | the device answered, and said no |
| `device_unreachable` | the device is commissioned but did not answer |
| `bad_request` | the op or its params are malformed |
| `internal` | anything else |

The code is preserved into the Rust error chain (`code_of`) rather than flattened
into prose — and it is **not rendered as part of the message**. It sits at the
bottom of the chain so `code_of` can downcast to it, which means `{:#}` would print
it as if it were a sentence: `commissioning failed: Invalid pairing code:
commission_failed` is what the user actually read, and only the middle fragment
said anything. `describe` skips that frame, and the commissioner consumes the code
where it branches on it and returns prose alone across the port.

---

## The three forms of a setup code

| form | example | how the controller pairs with it |
|---|---|---|
| QR payload | `MT:Y.K9042C00KA0648G00` | decoded here with `QrPairingCodeCodec`, then `{passcode, discriminator}` |
| manual pairing code | `34970112332` (11 or 21 digits) | `{pairingCode}`; matter.js decodes it |
| passcode | `20202021` (8 digits) | `{passcode}`; pairs with whatever is in commissioning mode |

The QR payload has to be decoded **by the controller**, and that is not a stylistic
choice. matter.js's `commission({pairingCode})` runs `ManualPairingCodeCodec.decode`
unconditionally, and that codec strips every non-digit before it checks the length —
so `MT:` plus base-38 collapses to a dozen stray digits and is rejected as an
"Invalid pairing code" in about two milliseconds, before anything reaches the
network. Handing a QR payload through as a `pairingCode` therefore cannot work,
which is what `commissioningOptions` exists to prevent recurring.

A payload that will not decode is `invalid_setup_code`, not `commission_failed`:
nothing was attempted, and the two codes lead to different advice.

Only the QR form carries the **long** discriminator, so it is the only one that
narrows the mDNS browse to a single device. The manual form carries a short
discriminator and a bare passcode carries none.

---

## Setup codes are credentials

A Matter pairing code grants fabric access. **Both implementations redact them**
from anything that reaches a log line, an error message, or an API response —
`redactSetupCode` in TypeScript, `redact_setup_code` in Rust — and log which
*kind* of code it was in place of the value.

Neither side depends on the other having done it. That matters because the
strings nobody writes deliberately are the errors: matter.js and the CHIP layer
beneath it echo what they were given, and `MatterState::Unreachable { error }` is
served over HTTP by `GET /api/v1/matter/status`.

There is no `Redactor` on the tracing pipeline — `RedactingEventLog` covers the
durable event log and egress, not `tracing` — so this is a call-site duty, in the
same spirit as `wolfram.rs`'s `redact_appid`.

---

## Adding to the protocol

- A new **op** or **event**: add it to both `protocol.ts` and `protocol.rs`, and
  to the tables above. Bump `PROTOCOL_VERSION` only if an un-updated client would
  break — a new event that old clients ignore does not qualify.
- A new **sensor type**: add it to `matter-server/src/mapping/sensors.ts`, one
  entry per line with a literal string in the `sensorType` field. Then disposition
  it, or
  `crates/pond-core/tests/context_producer_tracks_the_sensor_vocabulary.rs` fails
  the build — deliberately. The producer's rule is an allow-list, so an
  undispositioned signal is discarded forever with no error anywhere.
- A new **control verb**: it has to exist on `DeviceControlPort` first. Add the
  planning arm in `mapping/control.ts` with a test against a recorded device.
