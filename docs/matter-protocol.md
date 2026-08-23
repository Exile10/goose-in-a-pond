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

`log` is the controller's own structured record, relayed into `tracing` at the
level it names. The Rust side also pipes the child's stderr, so a controller GIAP
started is audible twice over; the event is what makes a controller the operator
runs themselves — where there is no pipe to read — just as legible.

---

## Types

```ts
Device  = { id: "matter-<node_id>", name, device_type, capabilities: string[], online }
Reading = { device_id, sensor_type, value, unit, at }        // at is RFC 3339
```

Both are direct projections of GIAP's own `Device` and `SensorReading`, so the
Rust side deserialises them without a mapping step. `Device` is deliberately
smaller than GIAP's: the controller knows nothing about rooms, hostnames or when
a device was first registered, and inventing values for those would make a Matter
device look different from every other kind.

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
}
VendorClusterSpec = { cluster_id, endpoint }
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

That bound is still there, but `describe` no longer hides what it drops.

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
name for a selectable — so a reading names the thing that changes it: "spin speed
is Low" leads straight to the call that makes it High. That correspondence is the
point of the type being this plain, and it is asserted in the controller's tests
rather than left as an intention.

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
into prose.

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
