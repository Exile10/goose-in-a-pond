# Matter

GIAP commissions and drives Matter devices through a local controller it installs
and runs itself: a Node process running [matter.js], reachable over the
[`giap-matter` protocol](matter-protocol.md). This is the operator's and
maintainer's view: how the pieces fit, what the failure modes mean, and how to
check each one from the command line.

[matter.js]: https://github.com/matter-js/matter.js

---

## The pieces

```
startup (main.rs) ─┐
PUT /settings ─────┴─► MatterRuntime::apply(MatterConfig { url, ble })
  (matter_ws_url or            │  watch channel
   matter_ble_enabled)         ▼
                         reconcile_loop                      (runtime.rs)
                               │
          ensure_running ──────┤                             (server_setup.rs)
          installs / starts    │
          the controller       ▼
                         MatterClient ──► matter-server (Node, matter.js)
                               │  ws://127.0.0.1:5580/giap
                     run_matter_supervisor                   (bridge.rs)
                     reconnects, and restarts the
                     controller when that stops being enough
```

| Piece | Owns |
|---|---|
| `matter-server/` | The controller: matter.js behind the protocol. Every Matter detail (clusters, endpoints, device types, unit conversion) and the fabric credentials live here. |
| `crates/pond-adapters-matter/src/server_setup.rs` | The controller *process*. Probes the port, installs the controller into the data dir, spawns it, waits for the port, and reaps a controller orphaned by a hard exit. Only loopback URLs (`local_port_from_ws_url`) are auto-started; a non-loopback `matter_ws_url` is someone else's controller and GIAP never manages it. |
| `runtime.rs` | The *decision*. One reconciler task converges toward the last requested `(url, ble)`, so a settings change takes effect without a restart. It owns the child handle and kills it on teardown. |
| `bridge.rs` | The *connection*: `subscribe`, syncing devices into the registry, publishing readings on the event bus (persisted by `SensorPersistingEventBus`), and reconnecting with backoff (1 s doubling to 30 s). |
| `commissioning.rs`, `control.rs` | The two ports the rest of GIAP uses: pairing devices, and driving them. |
| `notify.rs` | What tells the user when any of this goes wrong. |

The rest of GIAP reaches Matter through:

- REST: `GET /api/v1/matter/status`, `POST /api/v1/devices/commission`,
  `DELETE /api/v1/devices/{id}` (leaves the fabric first; a bridged child is refused
  with 409 naming its hub, and deleting a hub drops its children's rows).
- MCP tools in `pond-mcp-server`: `describe_device`, `get_device_state`,
  `set_device_state` (`device_control.rs`) and `get_sensor_reading`,
  `get_sensor_history`, `list_sensors` (`sensors.rs`).

Matter is wired only in builds with the `goose-agent` feature, which is the default.
Without it `/matter/status` reports `disabled` and commissioning answers that no
Matter support is compiled in.

Everything lives under the data dir:

```
<data_dir>/matter-server/app/                the controller and its node_modules
<data_dir>/matter-server/app/.giap-install   fingerprint of the dependencies and sources it was built from
<data_dir>/matter-server/controller.pid      the controller GIAP started, for orphan reaping
<data_dir>/matter-server/storage-js/         the fabric: commissioned nodes live here
<data_dir>/matter-server/storage-js/giap-controller/matter.lock, matter.pid
                                             matter.js's lock on the fabric store
<data_dir>/matter-server/storage-js/giap-controller.lock-boot.json
                                             which boot that lock was taken in (see below)
```

The fabric is on disk, so restarting or replacing the controller does not lose
commissioned devices. Deleting `storage-js/` does.

The controller sources are found at `$GIAP_ASSET_ROOT/matter-server`, then the repo's
`matter-server/`, then beside the executable (including `../Resources/matter-server`
in the macOS app bundle).

### Prerequisite: Node

matter.js 0.17's engine range is `>=20.19.0 <22.0.0 || >=22.13.0`: 20.19 or newer,
**except 22.0 through 22.12**. `server_setup.rs` enforces the same range
(`MIN_NODE`, `EXCLUDED_NODE`) and names the hole when it refuses a Node inside it.

`bash scripts/giap.sh node` finds or downloads a Node in the repo's range
(`^22.13 || ^24 || >=26`, which sits inside matter.js's) and records it.
At startup `pond-server` puts that recorded Node, or the best one it can find in
nvm, fnm, Volta, asdf or Homebrew, ahead of `PATH` (`crates/pond-server/src/node_path.rs`),
so a GUI launch with a bare `PATH` still finds it. No Python, compiler or system
package is needed.

If no suitable Node is found, Matter goes to `unreachable` with a message saying so
and naming the install command.

---

## It runs by default

There is no enable toggle. The controller ships with GIAP and installs itself the
first time it is needed, so the only thing a user does is add a device.

The first start on a fresh install runs `npm ci --omit=dev`, which takes a few
minutes. The Devices tab shows "Starting…" and polls `/matter/status` every two
seconds, a notification says the install has begun, and another says when it is
done. Startup waits at most ten seconds for Matter before serving
(`matter_startup_deferred`). Later starts reuse the install; a release that changes
only the controller's sources refreshes them by copying, without reinstalling
(`matter_sources_refreshed`).

The Devices tab shows what the runtime is doing, and a Retry when it cannot reach
the controller. Retry re-sends `matter_ws_url`, which the reconciler treats as a
reconnect. **Register device** takes a setup code and an optional name: phones pair
with a pairing code from the dashboard, so only Matter devices are registered there.

Settings for an operator live in **Settings → Vision & Cameras → Matter**:

| Setting | Default | Effect |
|---|---|---|
| `matter_ws_url` ("Controller address") | `ws://127.0.0.1:5580/giap` | Where the controller is. A loopback address is installed and run by GIAP; anything else is used as-is. Must start `ws://` or `wss://` (422 otherwise). A stored `ws://127.0.0.1:5580/ws` is read as the default. |
| `matter_ble_enabled` ("Pair over Bluetooth") | off | Restarts the controller with `--ble`. See [Bluetooth](#bluetooth-and-the-devices-that-cannot-pair-without-it). |

Only a save that touches one of these two keys reconfigures the runtime, so an
unrelated settings save never restarts a controller that is still installing.
`matter_enabled` is not a setting: a stored row of that name is ignored on read.

Verify from the API when in doubt:

```bash
curl -s "http://127.0.0.1:$(cat "<data_dir>/.runtime_api_port")/api/v1/matter/status" \
  -H "Authorization: Bearer <token>"
```

The body is `{enabled, url, state}`. `state` is one of `disabled`, `connecting`,
`connected`, `unreachable`; the last carries `error`.

---

## Testing without hardware

Google's **Matter Virtual Device** (MVD) is the device side. It advertises itself
for commissioning like any bulb or lock, and its Controller tab both shows what the
device currently is and lets you change what only the device can change (a door
position, a custom cluster's attributes).

Pair it from the Devices tab with the code it shows, then drive it from chat
("turn off the light", "set the light to 40%") and watch its Controller tab.
GIAP reporting success and the device reporting the same change are different
claims, and only the second means the command arrived.

Remove the device from the Devices tab when finished, or it shows as offline once
the app stops. MVD wants UDP 5540; see [Ports](#ports-and-why-a-device-app-may-refuse-to-start)
if it starts with an empty Controller tab.

MVD is the **independent** check and worth preferring wherever it can express the
case: it is Google's own CHIP stack, so it catches anything GIAP has quietly come to
assume about matter.js. Its form sets the device type, name, discriminator, Matter
port, vendor id and product id; because the port is editable it coexists with the
rig below.

### The virtual device

`matter-server/tools/virtual-device.ts` is a device side built on the matter.js
already installed for the controller. It needs no extra dependency, and it can build
two things MVD cannot:

```bash
cd matter-server

# an ordinary device
node --import tsx tools/virtual-device.ts --device dimmable-light

# a COMPOSED device: one device whose function lives in child endpoints
node --import tsx tools/virtual-device.ts --device oven \
  --part temperature-controlled-cabinet --part cook-surface

# a BRIDGE: one node, an Aggregator, and a separate device per child
node --import tsx tools/virtual-device.ts --name "Virtual Hub" \
  --bridged dimmable-light=Kitchen --bridged dimmable-light=Hall
```

It prints both pairing codes; paste either into Register device. While it runs,
stdin provokes a subscription report rather than waiting for one:

```
set kitchen.onOff.onOff = true
list
quit
```

Other flags: `--port` (default 5541; never 5540, see below), `--storage-dir`
(per-instance by default, so several can run at once), `--id`, `--passcode`,
`--discriminator`, `--vendor-id`, `--product-id`, `--attr`. The storage flag is
`--storage-dir` rather than `--storage` because matter.js parses the tool's argv into
its own variables: `--storage` would define `storage` as a scalar, and setting
`storage.path` then fails with *"segment storage is not a map"*. `@<number>` after a
device forces its endpoint number: `--bridged basic-video-player=Telly@7 --part speaker@3`
builds a bridged device sitting *above* its own part, which is what a real hub does
and what breaks anything reading "the lowest endpoint carrying this cluster".

**Some device types will not start bare, and that is matter.js being right.** It
enforces Matter conformance on the device side, so a Door Lock with no `lockType` is
refused rather than advertised. The tool reports which attribute matter.js wanted,
and `--attr` supplies it:

```bash
node --import tsx tools/virtual-device.ts --bridged door-lock=Front \
  --attr front.doorLock.lockType=0 --attr front.doorLock.lockState=1 \
  --attr front.doorLock.actuatorEnabled=true --attr front.doorLock.operatingMode=0 \
  --attr front.doorLock.wrongCodeEntryLimit=5 --attr front.doorLock.userCodeTemporaryDisableTime=10
```

There is deliberately no built-in table of mandatory defaults: matter.js already
knows, its message names the attribute, and a table would drift from the spec.

**What this cannot tell you.** Both sides are matter.js, so it exercises GIAP's
mappings rather than matter.js's conformance. That is the right target, since the
mappings are what is in doubt, but it is not independent evidence the way MVD is.
It uses test certificates, so attestation goes untested. And a virtual device is
spec-perfect; real hardware ships wrong feature maps and absent optional attributes
in ways nothing here reproduces.

The recorded snapshots in `matter-server/test/fixtures.ts` carry the cluster logic's
coverage without a fabric. What they cannot check is the wiring either side of them:
that a description reaches chat, that a command leaves the socket, that an attribute
a device publishes arrives decoded the way a fixture says it does. That is what MVD
and the virtual device are for.

### Ports, and why a device app may refuse to start

Matter devices are found on **UDP 5540**, so every device app wants it. The
controller does not use it: matter.js models a controller as a `ServerNode`, which
would bind 5540 by default, so `server.ts` gives it the same number as its WebSocket
port instead (UDP 5580 by default; a different protocol from the WebSocket's TCP, so
they cannot collide, and it moves with `--port`).

A controller holding 5540 would stop every Matter device on the machine from
starting, with nothing pointing back at the controller:

```
[SVR] ERROR setting up transport: OS Error 0x02000030: Address already in use
[IN]  UDP::Init bind&listen port=5540
```

An app failing this way usually shows an empty device list rather than an error.
Find what has the port:

```bash
lsof -nP -iUDP:5540
```

---

## Bluetooth, and the devices that cannot pair without it

**A device fresh out of its box has no Wi-Fi credentials, so it cannot advertise on
mDNS and IP commissioning cannot see it.** The first conversation has to happen over
Bluetooth Low Energy, and the commissioner hands the network credentials across
during it. Without BLE, GIAP can only pair a device something else has already
onboarded, which reads, from the Devices tab, as "No device found in pairing mode".

It is **off by default**, for two reasons:

- The radio comes from `@stoprocent/noble`, a native module, an optional dependency
  of `@matter/nodejs-ble`, itself optional here. `npm ci --omit=dev` on a host with no
  build toolchain installs neither, and that degrades to IP-only rather than failing
  the install. `matter-server/src/ble.ts` imports the transport **dynamically inside a
  try/catch** for the same reason: the package installs fine when noble does not
  build, and then *importing* it throws.
- It needs permission a headless service does not have.

Turn it on with **Settings → Vision & Cameras → Matter → Pair over Bluetooth**. The
controller is restarted, because `--ble` is an argument to that process.

### What it needs, per platform

**Linux and the Jetson.** The radio needs raw socket access. Either grant it to the
Node binary once:

```bash
sudo setcap cap_net_raw+eip "$(readlink -f "$(which node)")"
```

or run pond-server as root, which is worse. `setcap` is per-binary, so a Node
upgrade undoes it and BLE goes back to unavailable; the controller logs
`ble_unavailable` with the reason.

**macOS.** The bundle must declare `NSBluetoothAlwaysUsageDescription`, and the
consequence of omitting it is not a refused radio: **the OS kills the process**
(SIGKILL, with a crash report naming the missing Info.plist key).
`pond-desktop/electron-builder.yml`'s `mac.extendInfo` block carries it.

The controller is a bare `node`, so what matters is not its own bundle but the
**responsible process** macOS attributes it to: the app at the root of the process
tree. Under Electron the chain is app → pond-server → node. Launched from a terminal,
BLE works or is killed depending on whether that terminal app holds a Bluetooth
grant. The bundled app is the case GIAP controls, and the Info.plist key is what
makes it work; re-verify on a real BLE pairing after any packaging change
(unverified for the Electron bundle).

That is why `ensure_running` **falls back**. A controller that will not start with
BLE is started again without it, logging `matter_ble_start_failed` and then
`matter_ble_disabled`. A SIGKILL cannot be caught in-process, so without the
fallback the supervisor would respawn the controller and the OS would kill it again,
indefinitely.

### What the greeting says

The controller reports what it actually loaded, not what was asked for:
`{"protocol":"giap-matter", …, "ble":true}`. The Rust side needs it, because the
pre-flight "is anything in pairing mode" probe is an mDNS browse and cannot settle
the question for a device advertising over Bluetooth. With BLE active that probe is
skipped (`matter_pairing_probe_skipped`) rather than allowed to refuse a device in
pairing mode a metre away.

---

## Commissioning

A device is paired by its setup code, entered in **Register device**. A QR payload
(`MT:…`), an 11- or 21-digit manual pairing code, and a bare 8-digit passcode are
all accepted; the controller decodes the payload and decides how to find the device
(see [the three forms](matter-protocol.md#the-three-forms-of-a-setup-code)).

**A device only accepts commissioning for about 15 minutes after it boots.** It
advertises `_matterc._udp` with `CM=1` during that window and stops afterwards. Past
it, discovery finds nothing, which reads as a GIAP or network fault and is neither.
This is the most common cause of "commissioning failed"; check it first. GIAP probes
for commissionable devices *before* attempting (the `discover` op), so it says so in
seconds rather than after a discovery timeout.

Ground truth for whether anything is pairable right now:

```bash
dns-sd -B _matterc._udp local.              # macOS; an "Add" line means yes
avahi-browse -rt _matterc._udp              # Linux
```

No result means nothing is in pairing mode. Restart the device and retry promptly;
in MVD, Reboot on the device reopens the window.

Setup codes are credentials, and are redacted from every log line, error message
and API response on both sides of the socket. Finding one in a log is a bug.

---

## What a device becomes

`nodeToDevice` (`matter-server/src/mapping/devices.ts`) infers the GIAP device type
and capabilities. The node's **own** claim wins (the Descriptor cluster's
DeviceTypeList), because clusters only say what is drivable: by clusters alone an
On/Off plug looks like a lightbulb. Clusters are the fallback.

Capabilities come from the clusters present (`capabilitiesOf`):

| Cluster | Gives |
|---|---|
| On/Off | `power` |
| Fan Control | `fan_speed`, and `power` when there is no On/Off cluster |
| Level Control | `brightness` on a light, `volume` on a Speaker endpoint |
| Thermostat, or an appliance's Temperature Control setpoint | `temperature` |
| Door Lock | `lock` |
| Valve Configuration and Control | `valve`, and `position` when it has a level |
| Window Covering | `position`, and `tilt` when it reports a tilt position |
| Color Control | `color` and/or `color_temp`, per the device's `colorCapabilities` |
| any ModeBase-shaped setting | `mode` |
| Operational State, or Media Playback | `operation` |

Sensor readings come from a separate table (`matter-server/src/mapping/sensors.ts`):
occupancy; leak, freeze, rain and contact; ambient temperature, humidity,
illuminance, pressure and flow; air quality; smoke and CO alarm state and alarm
battery; ten gas and particulate concentrations (CO, CO2, NO2, ozone, formaldehyde,
PM1, PM2.5, PM10, radon, TVOC); and HEPA and activated-carbon filter condition and
change indication. That file is the whole vocabulary, one entry per line.

A device whose clusters GIAP does not map arrives typed `matter` with no
capabilities. It appears in the device list and the model has nothing it can do with
it: that is the signal a mapping is missing, not that the device is broken.

To see what a node actually exposes, ask the controller:

```bash
websocat ws://127.0.0.1:5580/giap   # then paste:
{"id":"1","op":"subscribe"}
```

---

## Failure modes

| What you see | What it means |
|---|---|
| "Starting…" for minutes on first run | The controller's dependencies are installing. A notification says when it is done. |
| "Cannot reach controller" + Retry | The socket is down or the controller could not start; `/matter/status` carries the reason. Retry re-sends the current settings, which reconnects. |
| "No device found in pairing mode" | Nothing is advertising. See the 15-minute window above. |
| "port 5580 is already in use by something that is not a giap-matter controller" | Another process holds the controller port and did not greet as `giap-matter`. It is refused rather than adopted. `lsof -nP -iTCP:5580 -sTCP:LISTEN` names it. |
| "expected a giap-matter controller but…" | `matter_ws_url` points at something else. |
| "…speaks giap-matter vN but this Pond speaks vM" | The controller and pond-server are from different releases. Restart pond-server so it reinstalls the controller. |
| "no Node 20.19+ found on PATH", or "matter.js does not support 22.0 to 22.12" | The [prerequisite](#prerequisite-node). Run `bash scripts/giap.sh node`. |
| "its fabric store is in use by another running process (pid N)" | A live Node process holds the matter.js lock on `storage-js/`, most likely a second controller. GIAP will not take the lock from it. `ps -p N -o command=` names it; stop it and press Retry. See [The fabric store lock](#the-fabric-store-lock). |
| "This build of GIAP has no Matter support compiled in" | Built without the `goose-agent` feature. |
| Repeated `matter_reconnect_attempt` | The controller is unreachable. Every third failed attempt GIAP re-runs controller setup (`RESPAWN_AFTER`), which restarts a local controller, and notifies you. |

### Reading the log

The controller's output is in GIAP's own log, not a separate file. Its stderr is
NDJSON, relayed into `tracing` at each record's own level with
`source = "controller"`; a controller GIAP did not start sends the same records as
`log` events over the socket. Everything Matter-related is tagged:

```bash
grep 'matter_' "<data_dir>/logs/pond.log.$(date +%F)"
RUST_LOG=info,pond_adapters_matter=debug cargo run -p pond-server -- serve
```

The `kind` field names the occurrence. Lifecycle: `matter_setup_started` /
`matter_setup_finished`, `matter_sources_refreshed`, `matter_controller_spawned` /
`matter_controller_ready` / `matter_controller_exited`, `matter_controller_reused`,
`matter_port_taken`, `matter_orphan_reaped`, `matter_ble_start_failed` /
`matter_ble_disabled`, `matter_state_changed`, `matter_startup_deferred`,
`matter_stale_lock_reclaimed` / `matter_stale_lock_reclaim_failed`,
`matter_storage_lock_held`, `matter_lock_boot_record_failed`.
Connection: `matter_connected` / `matter_disconnected`, `matter_reconnect_attempt`,
`matter_reconnected`, `matter_controller_revived` / `matter_controller_revive_failed`,
`matter_subscribed`. Devices: `matter_commission_started` / `_succeeded` / `_failed`,
`matter_pairing_probe_skipped`, `matter_device_command`, `matter_node_added` /
`_removed`, `matter_device_retyped`, `matter_availability_changed`.

Everything at `info` on the `giap::trace` target also lands in the operational log,
so `GET /api/v1/logs` answers the same questions without shell access.

---

## The fabric store lock

matter.js (`@matter/nodejs`, `fs/lock-utils.ts`) guards a node's storage directory
with two files, `matter.lock` and `matter.pid` (`"<pid> <token>"`), and honours the
lock for as long as `kill(pid, 0)` succeeds. A power cut leaves both behind. On the
next boot pids are handed out afresh, and if an unrelated process is given the
recorded pid, matter.js reports `StorageLockError: Storage is locked by another
process (pid N)` on every start, so the controller never comes up
(`matter_controller_exited`, then `matter_state_changed to="unreachable"`). Its token
only catches a pid recycled to matter.js itself. This happened on the Jetson on
2026-10-27.

So before matter.js opens storage, the controller (`matter-server/src/storageLock.ts`)
decides whether the lock can still belong to a running controller:

| Signal | Verdict |
|---|---|
| No `matter.lock` | Nothing to do. |
| Lock without a readable `matter.pid` | Reclaimed, `reason=no_owner_recorded`. |
| `giap-controller.lock-boot.json` names this exact lock (same pid and token) and another boot | Reclaimed, `reason=previous_boot`. |
| The pid is not running | Reclaimed, `reason=owner_gone`. |
| The pid is this process | Reclaimed, `reason=pid_is_self`. |
| The pid runs something that is not Node (and does not name this store) | Reclaimed, `reason=pid_reused`, logging only the executable's name. |
| The pid runs Node, or names this store | Left alone, `matter_storage_lock_held reason=live_controller`. |
| The pid is running but its command line cannot be read | Left alone, `reason=owner_unidentified`. |

A reclaim logs `matter_stale_lock_reclaimed` at warn with the pid and reason. Any
live Node process is respected, not only one whose command line matches GIAP's
controller: a controller run by hand may name the store by a relative path, and two
controllers on one store corrupt it. Where the lock is left alone matter.js still
runs its own check after this one, and the Pond's startup error names the holding
pid.

The boot record is written once the controller holds the lock, from Linux's
`/proc/sys/kernel/random/boot_id` or macOS's `kern.bootsessionuuid`. It is what makes
a lock from before a power cut stale even when its pid has since gone to another
Node process. No wall clock is compared, deliberately: a board without an RTC battery
boots with a wrong clock that NTP later moves forward, so a lock taken this boot can
carry an mtime earlier than the boot time, and a clock comparison would then reclaim
a live controller's lock. A store from before this change has no record, and falls
back to the process checks.

`controller.pid` is not exposed to the same pid reuse: `reap_orphan` kills only a pid
whose `ps -o command=` contains this data dir's controller entrypoint
(`<data_dir>/matter-server/app/src/server.ts`), and clears the file for anything else
(`a_reused_pid_belonging_to_something_else_is_not_killed`).

## Notes for maintainers

- `ensure_running` first reaps a controller recorded in `controller.pid` that
  outlived its Pond (matched by its per-data-dir entrypoint path, so two Ponds never
  reap each other's). It then adopts a listener only if it greets as `giap-matter`,
  so an operator's own controller on the port is reused, and anything else is refused.
- The install is idempotent against a fingerprint of `package-lock.json` and the
  sources: a dependency change reinstalls, a source-only change copies, no change is
  a no-op. Force a reinstall by deleting `<data_dir>/matter-server/app/.giap-install`.
- The lockfile must install under `npm ci` with npm 10 (shipped with Node 20) as
  well as npm 11; npm 10 refuses a lockfile missing nested platform entries that
  npm 11 tolerates.
- The adapter is testable without a controller: `tests.rs` runs an in-process mock
  speaking the protocol, so the reconnect path, the runtime state machine and every
  op are covered with no Node and no hardware (`cargo test -p pond-adapters-matter`).
  The first-run install is `tests/end_to_end_install.rs`, `#[ignore]`d because
  `npm ci` needs the network.
- The cluster mappings are pure functions over recorded devices:
  `cd matter-server && npx tsc --noEmit && npx vitest run`. CI's `matter-server` job
  runs both on Node 20.19, the oldest a Pond may have.
- Adding a sensor type without dispositioning it fails
  `cargo test -p pond-core --test context_producer_tracks_the_sensor_vocabulary`,
  deliberately. See [the protocol doc](matter-protocol.md#adding-to-the-protocol).
