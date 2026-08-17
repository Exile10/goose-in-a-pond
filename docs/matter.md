# Matter

GIAP commissions and drives Matter devices through a local controller it installs
and runs itself: a Node process running [matter.js], reachable over the
[`giap-matter` protocol](matter-protocol.md). This is the operator's and
maintainer's view: how the pieces fit, what the failure modes actually mean, and
how to check each one from the command line.

[matter.js]: https://github.com/matter-js/matter.js

---

## The pieces

```
Devices tab ─► PUT /settings ─► MatterRuntime::apply(enabled, url)
                                        │  watch channel
                                        ▼
                                  reconcile_loop            (runtime.rs)
                                        │
                    ensure_running ──────┤                  (server_setup.rs)
                    installs / starts    │
                    the controller       ▼
                                    MatterClient  ──► matter-server (Node)
                                        │  ws://…/giap
                              run_matter_supervisor         (bridge.rs)
                              reconnects, and restarts
                              the controller when that
                              stops being enough
```

- **`matter-server/`** is the controller: matter.js behind the protocol. It owns
  every Matter detail — clusters, endpoints, device types, unit conversion — and
  the fabric credentials.
- **`server_setup.rs`** owns the controller *process*: it copies the controller
  into the data dir, `npm ci`s its pinned dependencies, spawns it with storage
  inside the data dir, and waits for the port. `ensure_running` is idempotent — if
  something is already listening it is reused and nothing is installed. Only
  loopback URLs are auto-started; a remote `matter_ws_url` is someone else's
  controller and GIAP never manages it.
- **`runtime.rs`** owns the *decision*. A single reconciler task converges toward
  the last requested `(enabled, url)`, so the Devices toggle takes effect without
  a restart. It also owns the child handle and kills it on teardown.
- **`bridge.rs`** owns the *connection*: `subscribe`, syncing devices into the
  registry, putting readings on the event bus, and reconnecting with backoff.
- **`commissioning.rs`** / **`control.rs`** are the two ports the rest of GIAP
  uses — pairing devices, and driving them.
- **`notify.rs`** is what tells the user when any of this goes wrong.

Everything lives under the data dir:

```
<data_dir>/matter-server/app/          the controller and its node_modules
<data_dir>/matter-server/app/.giap-install   the lockfile the install was built from
<data_dir>/matter-server/storage-js/   the fabric — commissioned nodes live here
```

The fabric is on disk, so restarting or replacing the controller does not lose
commissioned devices. Deleting `storage-js/` does.

### Prerequisite

**Node 20.19+** (matter.js's own floor; 22.13+ and 24+ also qualify). Nothing
else — no Python, no compiler, no system packages.

```bash
node --version                 # must be >= v20.19
# Debian / Jetson:
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo bash - && sudo apt-get install -y nodejs
# macOS:
brew install node
```

If Node is missing or too old, enabling Matter fails with a message saying so and
giving the command above. It does not fail silently.

---

## Upgrading from an earlier release

**Commissioned devices must be paired again.** The fabric store is matter.js's
own and cannot be read from the previous controller's, so there is no migration
to run — pair each device once more from the Devices tab.

The default address moved from `ws://127.0.0.1:5580/ws` to
`ws://127.0.0.1:5580/giap`, and an install still holding the old default is
migrated on read, so a Pond whose address was never touched follows it. An
address you typed yourself is left alone.

An earlier controller left running will hold port 5580 and be refused rather than
adopted, with a message naming the process. Stop it, and delete any
`venv/` or `storage/` left beside the controller in the data dir — nothing reads
them now.

---

## Turning it on

You do not. Matter runs by default: the controller ships with GIAP and installs
itself the first time it is needed, so the only thing a user does is add a
device. There is no enable toggle, because its only honest advice would have been
"leave it on", and no controller address on the Devices tab, because a Pond
running its own controller has nothing to point anywhere.

The first start on a fresh install downloads the controller's dependencies, which
legitimately takes a few minutes — the Devices tab shows "Starting…" throughout,
a notification says the install has begun, and another says when it is done.
After that it is always ready.

The Devices tab shows only what the runtime is actually doing, and a Retry when
it cannot reach the controller. **Register device** takes a setup code and
nothing else: phones pair with a pairing code from the dashboard, and the desktop
app is the app, so a "what kind of device" chooser had one real option in it.

`matter_ws_url` remains in Settings for an operator running their own
controller. `matter_enabled` remains settable through the settings API, for a
Pond that will never see a Matter device — it is not in the UI and defaults on.

Verify from the API rather than the UI when in doubt:

```bash
curl -s "http://127.0.0.1:$(cat "<data_dir>/.runtime_api_port")/api/v1/matter/status"
```

`state` is one of `disabled`, `connecting`, `connected`, `unreachable`. The last
carries the underlying error.

---

## Testing without hardware

`matter-server/` can be a Matter *device* as well as drive one, so testing needs
nothing external — no Google Matter Virtual Device, no bulb:

```bash
cd matter-server && npm run virtual-device
```

It prints a pairing code and waits. Pair it from the Devices tab, then drive it
from chat ("turn off the light", "set the light to 40%") and watch the device
process report what it was told:

```
  [device] power  -> on
  [device] level  -> 102 (40%)
```

Two lines, one from each side, is the check worth making: GIAP reporting success
and the device reporting the same change are different claims, and only the
second one means the command arrived.

Its fabric is temporary, so stopping and restarting gives you a fresh, unpaired
device. It binds Matter port 5541, leaving the standard 5540 for a real device
app. Remove it from the Devices tab when finished, or it will show as offline
once stopped.

Google's Matter Virtual Device works too, and wants UDP 5540.

### Ports, and why a device app may refuse to start

Matter devices are found on **UDP 5540**, so every device app wants it. The
controller does not use it: matter.js models a controller as a `ServerNode`,
which would bind 5540 by default, so GIAP gives it the same NUMBER as its
WebSocket port instead (UDP 5580 by default — a different protocol from the
TCP the WebSocket uses, so they cannot collide).

That matters because a controller holding 5540 stops every Matter device on the
machine from starting, with no clue pointing back at the controller:

```
[SVR] ERROR setting up transport: OS Error 0x02000030: Address already in use
[IN]  UDP::Init bind&listen port=5540
```

An app failing this way usually shows an empty device list rather than an error.
If you see it, find what has the port:

```bash
lsof -nP -iUDP:5540
```

---

## Commissioning

A device is paired by its setup code, entered in **Register device**. A QR
payload (`MT:…`), an 11- or 21-digit manual pairing code, and a bare 8-digit
passcode are all accepted; the controller decodes the payload and decides how to
find the device.

**A device only accepts commissioning for about 15 minutes after it boots.** It
advertises `_matterc._udp` with `CM=1` during that window and stops afterwards.
Past it, discovery finds nothing — which reads as a GIAP or network fault and is
neither. This is the single most common cause of "commissioning failed"; check it
first. GIAP probes for commissionable devices *before* attempting, so it says so
in seconds rather than after a discovery timeout.

Ground truth for whether anything is pairable right now:

```bash
dns-sd -B _matterc._udp local.              # macOS; an "Add" line means yes
avahi-browse -rt _matterc._udp              # Linux
```

No result means nothing is in pairing mode. Restart the device and retry
promptly — for the bundled virtual device, that is Ctrl-C and `npm run
virtual-device` again.

Setup codes are credentials, and are redacted out of every log line, error
message and API response on both sides of the socket. If you ever find one in a
log, that is a bug worth reporting.

---

## What a device becomes

`nodeToDevice` (`matter-server/src/mapping/devices.ts`) infers the GIAP device
type and capabilities. The node's **own** claim wins — the Descriptor cluster's
DeviceTypeList — because clusters can only say what is drivable, which is why an
On/Off plug used to arrive wearing a lightbulb. Clusters are the fallback.

Capabilities come from the clusters present:

| Cluster | Gives |
|---|---|
| On/Off | `power` |
| Fan Control | `fan_speed`, and `power` when there is no On/Off cluster |
| Level Control | `brightness` |
| Thermostat | `temperature` |
| Door Lock | `lock` |

Sensor readings come from a separate table
(`matter-server/src/mapping/sensors.ts`) covering presence and contact, ambient
temperature/humidity/illuminance/pressure/flow, air quality and the smoke alarm,
ten gas and particulate concentrations, and the two air-purifier filters. That
file is the whole vocabulary, one entry per line.

A device whose clusters GIAP does not map arrives typed `matter` with no
capabilities. It appears in the device list and the model has nothing it can do
with it — that is the signal a mapping is missing, not that the device is broken.

To see what a node actually exposes, ask the controller:

```bash
websocat ws://127.0.0.1:5580/giap   # then paste:
{"id":"1","op":"subscribe"}
```

---

## Failure modes

| What you see | What it means |
|---|---|
| "Matter is off" on a device command | The toggle is off. Matter devices are refused rather than silently succeeding. |
| "Cannot reach controller" + Retry | The socket is down. Retry re-sends the current settings, which reconnects. |
| "No device found in pairing mode" | Nothing is advertising. See the 15-minute window above. |
| "expected a giap-matter controller but…" | The address points at something else, or at the old `/ws` path from an earlier release. |
| "…speaks giap-matter v1 but this Pond speaks v2" | The controller and pond-server are from different releases. Restart pond-server so it reinstalls the controller. |
| "no Node 20.19+ found on PATH" | The prerequisite above. |
| Repeated `matter_reconnect_attempt` | The controller is unreachable. After three in a row GIAP restarts it itself and notifies you. |

### Reading the log

The controller is not silent any more, and its output is in GIAP's own log rather
than a separate file. Everything Matter-related is tagged:

```bash
grep 'matter_' "<data_dir>/logs/pond.log.$(date +%F)"
```

```bash
RUST_LOG=info,pond_adapters_matter=debug cargo run -p pond-server -- serve
```

The `kind` field names the occurrence: `matter_setup_started` /
`matter_setup_finished`, `matter_controller_spawned` / `matter_controller_exited`,
`matter_connected` / `matter_disconnected`, `matter_reconnect_attempt`,
`matter_controller_revived`, `matter_subscribed`, `matter_commission_started` /
`_succeeded` / `_failed`, `matter_device_command`, `matter_node_added` /
`_removed`, `matter_state_changed`.

Everything at `info` on the `giap::trace` target also lands in the operational
log, so `GET /api/v1/logs` answers the same questions without shell access.

Records from the controller itself carry `source = "controller"` and keep the
level they were written at — that is why it logs NDJSON rather than prose.

---

## Notes for maintainers

- `ensure_running` reusing a live port means a controller left over from an
  earlier run is adopted rather than replaced. That is deliberate (the user may
  run their own), but a stale process can be inherited — if the controller behaves
  oddly and has been up far longer than pond-server, restart it deliberately.
- The install is idempotent against the committed `package-lock.json`: a release
  that changes dependencies reinstalls, one that does not is a no-op. Force a
  reinstall by deleting `<data_dir>/matter-server/app/.giap-install`.
- The adapter is fully testable without a controller: `tests.rs` runs an
  in-process mock speaking the native protocol, so the reconnect path, the runtime
  state machine and every op are covered in CI with no Node and no hardware.
  `cargo test -p pond-adapters-matter` is the whole suite.
- The cluster mappings are pure functions over recorded devices:
  `cd matter-server && npx vitest run`. The recorded fixtures came across from the
  Rust tests when the mapping moved, so the coverage moved with the code.
- Adding a sensor type without dispositioning it fails
  `cargo test -p pond-core --test context_producer_tracks_the_sensor_vocabulary`,
  deliberately. See [the protocol doc](matter-protocol.md#adding-to-the-protocol).
