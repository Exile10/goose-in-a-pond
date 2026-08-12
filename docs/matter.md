# Matter

GIAP commissions and drives Matter devices through a local
[python-matter-server] controller. This is the operator's and maintainer's view:
how the pieces fit, what the failure modes actually mean, and how to check each
one from the command line.

[python-matter-server]: https://github.com/home-assistant-libs/python-matter-server

---

## The pieces

```
Devices tab ─► PUT /settings ─► MatterRuntime::apply(enabled, url)
                                        │  watch channel
                                        ▼
                                  reconcile_loop            (runtime.rs)
                                        │
                    ensure_running ──────┤                  (server_setup.rs)
                    starts / reuses      │
                    the controller       ▼
                                    MatterClient  ──► matter-server (Python)
                                        │
                              run_matter_supervisor         (bridge.rs)
                              reconnects on drop
```

- **`server_setup.rs`** owns the controller *process*: it creates a private venv
  under the data dir, `pip install`s a pinned python-matter-server, spawns it
  with storage inside the data dir, and waits for the port. `ensure_running` is
  idempotent — if something is already listening it is reused and nothing is
  installed. Only loopback URLs are auto-started; a remote `matter_ws_url` is
  someone else's controller and GIAP never manages it.
- **`runtime.rs`** owns the *decision*. A single reconciler task converges toward
  the last requested `(enabled, url)`, so the Devices toggle takes effect without
  a restart. It also owns the child handle and kills it on teardown.
- **`bridge.rs`** owns the *connection*: `start_listening`, syncing nodes into the
  device registry, translating sensor updates onto the event bus, and
  reconnecting with backoff when the socket drops.
- **`commissioning.rs`** / **`control.rs`** are the two ports the rest of GIAP
  uses — pairing devices, and driving them.

Everything lives under the data dir:

```
<data_dir>/matter-server/venv/       the controller's Python environment
<data_dir>/matter-server/storage/    the fabric — commissioned nodes live here
<data_dir>/matter-server/matter-server.log   the controller's own log
```

The fabric is on disk, so restarting or replacing the controller does not lose
commissioned devices. Deleting `storage/` does.

---

## Turning it on

The Devices tab has the toggle and the controller address. The address must be a
WebSocket URL; `ws://127.0.0.1:5580/ws` is the default and means "GIAP runs the
controller itself". The first enable on a fresh install downloads and installs
python-matter-server, which legitimately takes minutes — the panel shows
"Starting…" throughout.

Verify from the API rather than the UI when in doubt:

```bash
curl -s "http://127.0.0.1:$(cat "<data_dir>/.runtime_api_port")/api/v1/matter/status"
```

`state` is one of `disabled`, `connecting`, `connected`, `unreachable`. The last
carries the underlying error.

---

## Commissioning

A device is paired by its setup code, entered in **Register device**:

| Input | Command | How it finds the device |
|---|---|---|
| 11-digit pairing code or `MT:` QR payload | `commission_with_code` | decodes the payload |
| bare 8-digit passcode | `commission_on_network` | mDNS discovery |

**A device only accepts commissioning for about 15 minutes after it boots.** It
advertises `_matterc._udp` with `CM=1` during that window and stops afterwards.
Past it, discovery finds nothing and commissioning fails with a 30-second mDNS
timeout — which reads as a GIAP or network fault and is neither. This is the
single most common cause of "commissioning failed"; check it first.

Ground truth for whether anything is pairable right now:

```bash
dns-sd -B _matterc._udp local.        # an "Add" line means a device is advertising
dns-sd -L <instance> _matterc._udp local.   # TXT shows D=<discriminator> CM=1
```

No `Add` line means nothing is in pairing mode. Reboot the device (for Google's
Matter Virtual Device, the Reboot button on its Controller tab) and retry
promptly.

---

## What a device becomes

`node_to_device` (`protocol.rs`) infers the GIAP device type and capabilities
from the application clusters present on the node. Nothing is vendor-specific.

| Cluster | Id | Gives |
|---|---|---|
| On/Off | 6 | `power` |
| Level Control | 8 | `brightness` |
| Thermostat | 513 | `temperature` |
| Door Lock | 257 | `lock` |
| Occupancy / Boolean State / Temperature / Humidity | 1030 / 69 / 1026 / 1029 | sensor readings on the event bus |

A cluster GIAP does not map leaves the device with no capabilities and type
`matter` — it appears in the device list but the model has nothing it can do
with it. That is the signal that a mapping is missing, not that the device is
broken.

To see what a node actually exposes, read the fabric store:

```bash
python3 -c "
import json;d=json.load(open('<data_dir>/matter-server/storage/<fabric-id>.json'))
n=d['nodes']['<node-id>']
print(sorted({int(k.split('/')[1]) for k in n['attributes'] if k.count('/')==2}))"
```

---

## Failure modes

| What you see | What it means |
|---|---|
| "Matter is off" on a device command | The toggle is off. Matter devices are refused rather than silently succeeding. |
| "Cannot reach controller" + Retry | The socket is down. Retry re-sends the current settings, which reconnects. |
| `commissioning failed`, mDNS timeout in `matter-server.log` | Nothing was in pairing mode. See the 15-minute window above. |
| `reconnect attempt failed; will retry` repeating forever | The controller process is gone. Toggling Matter off and on re-runs `ensure_running`. |
| Controller address looks filled but saving fails | An empty stored value renders as the placeholder. Type the URL in explicitly. |

The controller's own log is the first place to look for anything commissioning-
or CHIP-related; GIAP's log only carries what crossed the WebSocket:

```bash
tail -f "<data_dir>/matter-server/matter-server.log"
```

---

## Notes for maintainers

- `ensure_running` reusing a live port means a controller left over from an
  earlier run is adopted rather than replaced. That is deliberate (the user may
  run their own), but it also means a stale process can be inherited — if the
  controller behaves oddly and has been up far longer than pond-server, restart
  it deliberately.
- The adapter is fully testable without a controller: `tests.rs` runs an
  in-process mock matter-server speaking schema 11, so every mapping and the
  reconnect path are covered in CI with no Python and no hardware.
- `cargo test -p pond-adapters-matter` is the whole suite; nothing in it needs
  the network.
