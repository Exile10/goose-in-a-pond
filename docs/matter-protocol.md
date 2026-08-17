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

The predecessor protocol (python-matter-server's schema 11) was the other way
round, which is why the adapter used to hold a hand-maintained table of decimal
cluster ids, parse `"1/6/0"` attribute paths, and convert units by hand.

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

The path is `/giap` rather than `/ws` on purpose: a stale address left pointing
at a python-matter-server fails at the handshake instead of half-working.

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

Values are in GIAP's units. The controller converts.

**`applied` is what the device did, not what was asked for.** A dimmer that
clamps to its own minimum reports that minimum. This is why the field exists: the
old adapter built its outcome from the caller's request, so a device that did
something else was still described to the user as having obeyed.

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
