/**
 * The matter.js side: fabric lifecycle, peers, subscriptions, and the four things the
 * protocol can ask a controller to do.
 *
 * Everything that touches the network lives here. The mappings under `mapping/` are
 * pure functions over a `NodeSnapshot`, so this file's job is to keep snapshots
 * current and to turn a `Plan` into real Matter traffic.
 */

import { Endpoint, Environment, Seconds, ServerNode, type ClientNode } from "@matter/main";

import { log, describeError, setupCodeKind } from "./log.js";
import { nodeToDevice } from "./mapping/devices.js";
import { planControl, type Verb } from "./mapping/control.js";
import { observedOperation } from "./mapping/settings.js";
import { describeNode } from "./mapping/describe.js";
import { stateOf } from "./mapping/state.js";
import { readingFor, sensorClusters } from "./mapping/sensors.js";
import {
  type ClusterState,
  type EndpointSnapshot,
  type NodeSnapshot,
} from "./mapping/snapshot.js";
import {
  OpError,
  deviceIdForNode,
  nodeIdFromDeviceId,
  type Device,
  type DeviceDescription,
  type DeviceState,
  type ValueSpec,
  type DeviceStatePatch,
  type Reading,
} from "./protocol.js";

/**
 * How long `discover` browses before answering.
 *
 * The probe is a local mDNS browse, so it answers in well under a second when anything
 * is advertising. Bounded low on purpose: its whole value is being cheaper than the
 * commissioning discovery timeout it saves, and a probe that hangs must not add to the
 * wait.
 */
const DISCOVER_TIMEOUT = Seconds(8);

/**
 * Which clusters a snapshot reads.
 *
 * Bounded rather than "every supported cluster": a snapshot is rebuilt on every node
 * event, and reading all the clusters a composed device may expose would make a busy
 * fabric expensive for data nothing consumes.
 *
 * The named set is the fixed vocabulary -- lighting, closures, climate, sensors. The
 * `*Mode` rule is what keeps appliances working without a list: Matter's ModeBase
 * derivatives are consistently named that way, and `settingsOf` reads them by shape,
 * so a washer, a dishwasher, an oven and whatever ships next all arrive without a
 * code change. Without that rule the promise was empty -- the snapshot dropped those
 * clusters by name before anything could look at their shape.
 */
const SNAPSHOT_CLUSTERS: ReadonlySet<string> = new Set([
  "descriptor",
  "basicInformation",
  "onOff",
  "levelControl",
  "colorControl",
  "thermostat",
  "doorLock",
  "fanControl",
  "windowCovering",
  // Selectable settings whose shape is not ModeBase, so the rule below cannot match
  // them and they are named here instead -- as they already are in settings.ts.
  "temperatureControl",
  "laundryWasherControls",
  // Start / stop / pause / resume, shared by every appliance that runs a cycle.
  "operationalState",
  ...sensorClusters(),
]);

/** Is this cluster worth putting in a snapshot? */
export function isSnapshotCluster(clusterId: string): boolean {
  // Every ModeBase derivative: laundryWasherMode, dishwasherMode, rvcRunMode,
  // ovenMode, and the ones that do not exist yet.
  return SNAPSHOT_CLUSTERS.has(clusterId) || clusterId.endsWith("Mode");
}

export interface ControllerEvents {
  deviceAdded(device: Device): void;
  deviceUpdated(device: Device): void;
  deviceRemoved(deviceId: string): void;
  availabilityChanged(deviceId: string, online: boolean): void;
  reading(reading: Reading): void;
}

export class Controller {
  #node: ServerNode;
  #events: ControllerEvents;
  /** Peers already wired for events, so a re-sync does not double-subscribe. */
  #observed = new Set<string>();

  private constructor(node: ServerNode, events: ControllerEvents) {
    this.#node = node;
    this.#events = events;
  }

  /**
   * Bring the controller online, storing the fabric under `storagePath`.
   *
   * The path is set on the environment explicitly rather than left to matter.js's own
   * `--storage-path` argv parsing: that parser reads the flag as a boolean, so the
   * fabric landed in a directory called `true` beside the process's cwd. A fabric in
   * the wrong place is not a cosmetic fault — it is every commissioned device lost on
   * the next start, from a working-directory change nobody would connect to it.
   */
  static async start(
    storagePath: string,
    matterPort: number,
    events: ControllerEvents,
  ): Promise<Controller> {
    Environment.default.vars.set("storage.path", storagePath);

    // NOT the default 5540.
    //
    // matter.js models a controller as a `ServerNode`, which binds the Matter
    // operational port — and 5540 is well-known precisely so that COMMISSIONABLE
    // DEVICES can be found on it. A controller squatting it means no Matter
    // device can start on the same machine: Google's Matter Virtual Device dies
    // with "OS Error 0x02000030: Address already in use ... UDP::Init
    // bind&listen port=5540" and shows an empty Controller tab, and this repo's
    // own virtual-device tool had to be moved off 5540 for the same reason.
    //
    // A controller has no need of a well-known port. It initiates the
    // connections; devices answer whatever source port it used. Verified by
    // commissioning successfully from controllers on several non-standard ports.
    const node = await ServerNode.create({
      id: "giap-controller",
      network: { port: matterPort },
    });
    await node.start();

    log.info("controller_online", "the Matter controller is online", {
      matter_port: matterPort,
    });

    const controller = new Controller(node, events);
    controller.#watchPeers();
    return controller;
  }

  async close(): Promise<void> {
    await this.#node.close();
  }

  /** The controller's fabric, for the operator reading logs. */
  fabricId(): number | null {
    for (const peer of this.#node.peers) {
      // Guarded for the same reason as `peerNodeId`: reading the address of a
      // node that has not joined a fabric throws.
      try {
        const index = peer.peerAddress?.fabricIndex;
        if (index !== undefined) return Number(index);
      } catch {
        continue;
      }
    }
    return null;
  }

  /**
   * Wire up every commissioned peer that is not already wired.
   *
   * Called on each `subscribe`, which the bridge sends on every connect and
   * reconnect. Belt and braces for the peers that never pass through the `added`
   * handler in a commissioned state: one discovered as commissionable and then
   * paired arrives as `added` before it has a node id, and is skipped there.
   */
  observeCommissioned(): void {
    for (const peer of this.#node.peers) {
      if (peerNodeId(peer) !== undefined) this.#observe(peer);
    }
  }

  /** Every commissioned node, as GIAP devices. */
  devices(): Device[] {
    return this.#peerSnapshots().map(([, snapshot]) => nodeToDevice(snapshot));
  }

  /**
   * Everything every node currently reports.
   *
   * Sent with the `subscribe` result so a sensor sitting at a steady value is knowable
   * immediately. Without it a device exists in the list while every question about its
   * reading is answered "none recorded", which reads as "that device is not here".
   */
  readings(): Reading[] {
    const out: Reading[] = [];
    for (const [, snapshot] of this.#peerSnapshots()) {
      for (const endpoint of snapshot.endpoints) {
        for (const [cluster, attributes] of Object.entries(endpoint.clusters)) {
          for (const [attribute, value] of Object.entries(attributes)) {
            const reading = readingFor(snapshot.nodeId, cluster, attribute, value);
            if (reading !== undefined) out.push(reading);
          }
        }
      }
    }
    return out;
  }

  /** How many devices are advertising themselves for commissioning right now. */
  async discover(): Promise<number> {
    const discovery = this.#node.peers.discover({ timeout: DISCOVER_TIMEOUT });
    const found = await discovery;
    return found.length;
  }

  /**
   * Pair a device by its setup code.
   *
   * Both forms find the device over mDNS. A pairing code or QR payload carries the
   * discriminator so matter.js can narrow the browse; a bare passcode cannot, so that
   * form pairs with whatever is in commissioning mode — which is how development
   * devices such as Google's Matter Virtual Device are paired, since they show only a
   * passcode.
   */
  async commission(code: string, name?: string): Promise<Device> {
    const trimmed = code.trim();
    const kind = setupCodeKind(trimmed);
    log.info("commission_started", "commissioning a device", { code_kind: kind });

    const options =
      kind === "passcode"
        ? { passcode: Number(trimmed.replace(/[\s-]/g, "")) }
        : { pairingCode: trimmed.replace(/\s/g, "") };

    let peer: ClientNode;
    try {
      peer = await this.#node.peers.commission(options);
    } catch (error) {
      // matter.js and the CHIP layer beneath it echo what they were given, so this
      // message is redacted before it becomes an error the user reads.
      throw new OpError("commission_failed", describeError(error));
    }

    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) {
      throw new OpError("commission_failed", "the device joined the fabric without a node id");
    }

    // A user-chosen name is written to the device itself, so any controller sees it.
    // Best effort: a failed write does not unwind a successful pairing, because the
    // device is commissioned either way and GIAP's own registry still holds the name.
    if (name !== undefined && name.trim().length > 0) {
      try {
        await peer.endpoints.for(0).setStateOf("basicInformation", { nodeLabel: name.trim() });
      } catch (error) {
        log.warn("node_label_write_failed", "could not write the device's name to it", {
          device_id: deviceIdForNode(nodeId),
          error: describeError(error),
        });
      }
    }

    const snapshot = snapshotOf(peer, nodeId);
    const device = nodeToDevice(snapshot);
    if (name !== undefined && name.trim().length > 0) {
      device.name = name.trim();
    }
    // It arrived at `added` without a node id, so it was skipped there.
    this.#observe(peer);

    log.info("commission_succeeded", "device joined the fabric", {
      device_id: device.id,
      device_type: device.device_type,
    });
    return device;
  }

  /**
   * Remove a node from the fabric.
   *
   * A node the controller no longer knows is already in the desired end state, so this
   * succeeds rather than refusing — that is what lets an interrupted earlier removal be
   * cleaned up. An unreachable node falls back to a local delete: `decommission` tries
   * to tell the device, which cannot work if it is unplugged, and refusing to forget an
   * unplugged device would strand it in the list forever.
   */
  async decommission(deviceId: string): Promise<void> {
    const peer = this.#peerFor(deviceId);
    if (peer === undefined) {
      log.info("node_already_absent", "node is already off the fabric", { device_id: deviceId });
      return;
    }

    try {
      await peer.decommission();
    } catch (error) {
      log.warn("decommission_fell_back_to_delete", "device unreachable; removing it locally", {
        device_id: deviceId,
        error: describeError(error),
      });
      await peer.delete();
    }
  }

  /**
   * What a device can be told to do and what it measures.
   *
   * Read live rather than stored: a description is derived from what the device
   * currently reports, and a cached copy would go stale exactly when a device is
   * upgraded or reconfigured — the moment its description matters most.
   */
  describe(deviceId: string): DeviceDescription {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }
    return describeNode(snapshotOf(peer, nodeId));
  }

  /**
   * What the device currently is.
   *
   * The counterpart to `describe`: that says what a device can be told to do, this
   * says what it is doing, in the same names. Read from the same snapshot the
   * controller keeps current from subscription reports, so it costs no fabric
   * traffic and reflects the last thing the device said about itself.
   */
  state(deviceId: string): DeviceState {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }
    return stateOf(snapshotOf(peer, nodeId));
  }

  /** Drive a device. Returns what the device state became. */
  async control(deviceId: string, verb: Verb, value: unknown): Promise<DeviceStatePatch> {
    const peer = this.#peerFor(deviceId);
    const nodeId = peer === undefined ? undefined : peerNodeId(peer);
    if (peer === undefined || nodeId === undefined) {
      throw new OpError(
        "device_unknown",
        `Matter device '${deviceId}' is not commissioned on this fabric`,
      );
    }

    const plan = planControl(snapshotOf(peer, nodeId), deviceId, verb, value);

    for (const action of plan.actions) {
      const endpoint = peer.endpoints.for(action.endpoint);
      try {
        if (action.kind === "command") {
          const commands = endpoint.commandsOf(action.cluster);
          const command = commands[action.command];
          if (typeof command !== "function") {
            throw new OpError(
              "capability_unsupported",
              `Matter device '${deviceId}' does not accept ${action.command}`,
            );
          }
          // A command taking no fields must be invoked with NO argument. matter.js
          // validates the request against the cluster schema and rejects `{}` with
          // "Expected void, got object" — so On, Off, LockDoor and UnlockDoor all
          // failed while the commands that do take fields worked, which is a very
          // confusing half-working state to debug from the outside.
          // Cast because the untyped `commandsOf` signature demands an argument
          // while the cluster schema for these commands forbids one.
          const invoke = command as (args?: Record<string, unknown>) => Promise<unknown>;
          const hasFields = Object.keys(action.payload).length > 0;
          assertAccepted(
            deviceId,
            action.command,
            await (hasFields ? invoke(action.payload) : invoke()),
          );
        } else {
          await endpoint.setStateOf(action.cluster, { [action.attribute]: action.value });
        }
      } catch (error) {
        if (error instanceof OpError) throw error;
        throw refusalOrFault(deviceId, error, acceptedFor(snapshotOf(peer, nodeId), verb, value));
      }
    }

    // What the device is now, not what it was asked to be. The command response
    // above proves it accepted the command; this is how it describes the result.
    if (verb === "operation") {
      const observed = await settledOperation(peer, nodeId, plan.applied.operation);
      if (observed !== undefined) plan.applied.operation = observed;
    }

    return plan.applied;
  }

  // ── Peer tracking ──────────────────────────────────────────────────────────

  #peerSnapshots(): [ClientNode, NodeSnapshot][] {
    const out: [ClientNode, NodeSnapshot][] = [];
    for (const peer of this.#node.peers) {
      const nodeId = peerNodeId(peer);
      // Commissionable-but-not-commissioned nodes live in the same collection and
      // have no node id. They are not devices until they join the fabric.
      if (nodeId === undefined) continue;
      out.push([peer, snapshotOf(peer, nodeId)]);
    }
    return out;
  }

  #peerFor(deviceId: string): ClientNode | undefined {
    const wanted = nodeIdFromDeviceId(deviceId);
    if (wanted === undefined) return undefined;
    for (const peer of this.#node.peers) {
      if (peerNodeId(peer) === wanted) return peer;
    }
    return undefined;
  }

  #watchPeers(): void {
    this.observeCommissioned();
    // Both handlers are wrapped, and both run on matter.js's own callbacks: an
    // exception escaping one does not merely lose an event, it takes down the
    // discovery or subscription that fired it.
    this.#node.peers.added.on(peer =>
      guard("peer_added", () => {
        // Commissionable-but-not-commissioned nodes arrive here during every
        // discovery. They are not devices, and they are not merely uninteresting
        // — reading their structure throws, and this handler runs inside
        // matter.js's mDNS listener, so throwing here fails the discovery.
        const nodeId = peerNodeId(peer);
        if (nodeId === undefined) return;

        this.#observe(peer);
        this.#events.deviceAdded(nodeToDevice(snapshotOf(peer, nodeId)));
      }),
    );
    this.#node.peers.deleted.on(peer =>
      guard("peer_deleted", () => {
        const nodeId = peerNodeId(peer);
        if (nodeId === undefined) return;
        this.#observed.delete(peer.id);
        this.#events.deviceRemoved(deviceIdForNode(nodeId));
      }),
    );
  }

  /**
   * Wire one peer's attribute and lifecycle changes onto the protocol's events.
   *
   * Guarded by `#observed` because peers are re-walked whenever the collection changes,
   * and a second listener on the same observable would double every reading — which
   * downstream reads as a sensor that fires twice per change.
   */
  #observe(peer: ClientNode): void {
    if (this.#observed.has(peer.id)) return;
    this.#observed.add(peer.id);

    peer.lifecycle.online.on(() =>
      guard("peer_online", () => this.#announceAvailability(peer, true)),
    );
    peer.lifecycle.offline.on(() =>
      guard("peer_offline", () => this.#announceAvailability(peer, false)),
    );

    for (const endpoint of peer.endpoints) {
      for (const cluster of Object.keys(endpoint.behaviors.supported)) {
        if (!isSnapshotCluster(cluster)) continue;
        this.#observeCluster(peer, endpoint, cluster);
      }
    }
  }

  #observeCluster(peer: ClientNode, endpoint: Endpoint, cluster: string): void {
    let observables: Record<string, unknown>;
    try {
      observables = endpoint.eventsOf(cluster) as Record<string, unknown>;
    } catch {
      return; // the cluster is not present after all; nothing to watch
    }

    for (const [name, observable] of Object.entries(observables)) {
      // matter.js names attribute-change observables `<attribute>$Changed`.
      if (!name.endsWith("$Changed")) continue;
      const attribute = name.slice(0, -"$Changed".length);
      if (typeof observable !== "object" || observable === null) continue;
      const on = (observable as { on?: unknown }).on;
      if (typeof on !== "function") continue;

      (on as (handler: (value: unknown) => void) => void).call(observable, value =>
        guard("attribute_changed", () => {
          const nodeId = peerNodeId(peer);
          if (nodeId === undefined) return;

          const reading = readingFor(nodeId, cluster, attribute, value);
          if (reading !== undefined) {
            this.#events.reading(reading);
            return;
          }
          // Not a sensor value, but a change to a cluster that shapes what the
          // device IS — a name, a device type, a newly reported cluster. The
          // device is republished so the registry's typing and capabilities
          // stay true.
          if (cluster === "basicInformation" || cluster === "descriptor") {
            this.#events.deviceUpdated(nodeToDevice(snapshotOf(peer, nodeId)));
          }
        }),
      );
    }
  }

  #announceAvailability(peer: ClientNode, online: boolean): void {
    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) return;
    this.#events.availabilityChanged(deviceIdForNode(nodeId), online);
  }
}

/**
 * The peer's Matter node id, or `undefined` while it is only commissionable.
 *
 * The `try` is load-bearing, and this is worth reading before anyone removes it.
 * `peerAddress` reads a private cached field, and on a node that has not joined
 * a fabric matter.js THROWS ("Cannot read private member #cachedPeerAddress…")
 * rather than returning undefined. Discovery adds exactly such nodes to the peer
 * collection, so an unguarded read here threw inside matter.js's own mDNS
 * listener — which killed the discovery that raised it. The symptom was
 * `discover` reporting nothing and every commission failing with "discovery of
 * node discovery failed", on a device that `dns-sd` could see perfectly well.
 * Commissioning could not succeed at all.
 */
function peerNodeId(peer: ClientNode): bigint | undefined {
  try {
    const nodeId = peer.peerAddress?.nodeId;
    return nodeId === undefined ? undefined : BigInt(nodeId);
  } catch {
    return undefined;
  }
}

/**
 * Run `body`, logging rather than propagating anything it throws.
 *
 * Every caller is a matter.js observer, and matter.js invokes those from inside
 * its own operations — so an exception that escapes does not just lose one
 * event, it fails the discovery or subscription that raised it. Losing an event
 * and logging why is strictly better than that.
 */
function guard(kind: string, body: () => void): void {
  try {
    body();
  } catch (error) {
    log.warn(kind, "a controller event handler failed", { error: describeError(error) });
  }
}

function snapshotOf(peer: ClientNode, nodeId: bigint): NodeSnapshot {
  const endpoints: EndpointSnapshot[] = [];
  for (const endpoint of peer.endpoints) {
    endpoints.push({
      number: Number(endpoint.number),
      deviceTypes: readDeviceTypes(endpoint),
      clusters: readClusters(endpoint),
    });
  }
  return { nodeId, online: peer.lifecycle.isOnline, endpoints };
}

/**
 * How long to let a device's state catch up with the command it just took.
 *
 * A cluster's state here is whatever the subscription last reported, and the report
 * carrying a change arrives after the command returns -- measured against Google's
 * Matter Virtual Device, the command answered in 13ms and the new state landed
 * within 500ms. Reading straight after the invocation therefore returns the state
 * BEFORE the command, which reported a washer that started perfectly well as having
 * stayed stopped. That is a worse failure than the echo it replaced: an echo is
 * merely uninformative, while this contradicts a device that did as it was told.
 */
const OPERATION_SETTLE_MS = 2000;
const OPERATION_POLL_MS = 100;

/** The state each operation asks the device to reach. */
const INTENDED_STATE: Record<string, string> = {
  start: "running",
  resume: "running",
  stop: "stopped",
  pause: "paused",
};

/**
 * Wait for `read` to report `wanted`, or give up and return whatever it last said.
 *
 * Returns as soon as the state appears, so a device that obeys is not delayed past
 * its own report. A device that never gets there costs the full window and is then
 * reported as whatever it actually is -- which is the honest answer for one that
 * took the command and did nothing.
 */
export async function settleTo(
  wanted: string | undefined,
  read: () => string | undefined,
  waitMs: number = OPERATION_SETTLE_MS,
  pollMs: number = OPERATION_POLL_MS,
): Promise<string | undefined> {
  let seen = read();
  // Nothing to wait for: a verb with no state of its own to reach.
  if (wanted === undefined) return seen;

  const deadline = Date.now() + waitMs;
  while (seen !== wanted && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, pollMs));
    seen = read();
  }
  return seen;
}

/** The device's state once it has had a chance to report the command's effect. */
async function settledOperation(
  peer: ClientNode,
  nodeId: bigint,
  requested: string | undefined,
): Promise<string | undefined> {
  const wanted = requested === undefined ? undefined : INTENDED_STATE[requested.toLowerCase()];
  return settleTo(wanted, () => observedOperation(snapshotOf(peer, nodeId)));
}

/**
 * Matter status codes a device answers a write with, rather than a fault.
 *
 * A device saying no is not a device that cannot be reached, and calling it
 * unreachable sends the reader looking at the network for a fault that is not
 * there. A thermostat answering "Constraint error" to a setpoint it will not take
 * was reported as `device_unreachable` while sitting on the same machine,
 * responding in milliseconds.
 */
const REFUSALS: ReadonlyMap<string, string> = new Map([
  ["constraint error", "the value is outside what it will accept right now"],
  ["invalid action", "it will not do that in its current state"],
  ["invalid command", "it does not accept that command"],
  ["unsupported attribute", "it has no such setting"],
  ["unsupported write", "that setting cannot be written"],
  ["invalid in state", "it will not do that in its current state"],
  ["needs timed interaction", "it requires a timed interaction"],
  ["write ignored", "it ignored the write"],
]);

/**
 * Tell a refusal from a fault, and word it as one.
 *
 * The distinction is the whole diagnostic value: a refusal means ask for something
 * else, a fault means look at the network. Anything unrecognised stays a fault
 * carrying the device's own words, because guessing that an unfamiliar error was a
 * refusal would hide a real outage.
 */
export function refusalOrFault(deviceId: string, error: unknown, accepts?: string): OpError {
  const said = describeError(error);
  const lowered = said.toLowerCase();

  for (const [needle, meaning] of REFUSALS) {
    if (lowered.includes(needle)) {
      // What it WILL take, on the refusal itself. A caller that did not read the
      // description first is exactly the caller who gets here, and telling it only
      // that the value was wrong leaves it to guess again -- which is what a
      // thermostat refusing 30 with no mention of 23.5 produced.
      const offer = accepts === undefined ? "" : ` It accepts ${accepts}.`;
      return new OpError(
        "device_refused",
        `Matter device '${deviceId}' refused that: ${meaning} (it said: ${said}).${offer}`,
      );
    }
  }
  return new OpError("device_unreachable", said);
}

/** How the device's own description words what this verb takes, if it says. */
function acceptedFor(node: NodeSnapshot, verb: Verb, value: unknown): string | undefined {
  const setting = verb === "mode" ? readSettingName(value) : undefined;
  const capability = describeNode(node).capabilities.find(
    c => c.verb === verb && (setting === undefined || c.setting === setting),
  );
  return capability === undefined ? undefined : wordValueSpec(capability.value);
}

/** The setting a `mode` request named, so its own limits are the ones quoted. */
function readSettingName(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const setting = (value as { setting?: unknown }).setting;
  return typeof setting === "string" ? setting : undefined;
}

function wordValueSpec(spec: ValueSpec): string | undefined {
  switch (spec.kind) {
    case "enum":
      return spec.values.join(", ");
    case "percent":
      return "0 to 100 percent";
    case "number": {
      const unit = spec.unit === undefined ? "" : ` ${spec.unit}`;
      if (spec.min !== undefined && spec.max !== undefined) {
        return `${spec.min} to ${spec.max}${unit}`;
      }
      if (spec.max !== undefined) return `up to ${spec.max}${unit}`;
      if (spec.min !== undefined) return `from ${spec.min}${unit}`;
      return undefined;
    }
    // Nothing a refusal could usefully narrow.
    case "boolean":
    case "color":
      return undefined;
  }
}

/** ErrorStateEnum, for a device that sends an id without a label. */
const OPERATIONAL_ERRORS: Record<number, string> = {
  1: "it could not start or resume",
  2: "it could not complete the operation",
  3: "that command is not valid in its current state",
};

/**
 * Fail if the device refused the command it just answered.
 *
 * Matter commands do not only succeed or throw. Operational State answers every
 * Start/Stop/Pause/Resume with an `ErrorStateID`, and ModeBase answers
 * `changeToMode` with a `status` — and a refusal comes back as a perfectly
 * successful invocation carrying a non-zero code. Discarding that response is why
 * a washer that never started was reported as running: nothing threw, so nothing
 * looked. The device's own `errorStateLabel` or `statusText` is preferred over
 * anything we could word ourselves, because it knows why it said no.
 */
export function assertAccepted(deviceId: string, command: string, response: unknown): void {
  if (typeof response !== "object" || response === null) return;

  const state = (response as { commandResponseState?: unknown }).commandResponseState;
  if (typeof state === "object" && state !== null) {
    const id = (state as { errorStateId?: unknown }).errorStateId;
    const label = (state as { errorStateLabel?: unknown }).errorStateLabel;
    const details = (state as { errorStateDetails?: unknown }).errorStateDetails;
    if (typeof id === "number" && id !== 0) {
      const said =
        typeof details === "string" && details !== ""
          ? details
          : typeof label === "string" && label !== ""
            ? label
            : OPERATIONAL_ERRORS[id] ?? `it answered with error state ${id}`;
      throw new OpError(
        "device_refused",
        `Matter device '${deviceId}' refused ${command}: ${said}`,
      );
    }
  }

  const status = (response as { status?: unknown }).status;
  const statusText = (response as { statusText?: unknown }).statusText;
  if (typeof status === "number" && status !== 0) {
    const said =
      typeof statusText === "string" && statusText !== ""
        ? statusText
        : `it answered with status ${status}`;
    throw new OpError(
      "device_refused",
      `Matter device '${deviceId}' refused ${command}: ${said}`,
    );
  }
}

function readClusters(endpoint: Endpoint): ClusterState {
  const clusters: ClusterState = {};
  for (const cluster of Object.keys(endpoint.behaviors.supported)) {
    if (!isSnapshotCluster(cluster)) continue;
    try {
      clusters[cluster] = { ...endpoint.stateOf(cluster) } as Record<string, unknown>;
    } catch {
      // A cluster present in `supported` but not yet populated by the subscription.
      // Recording it empty keeps "this endpoint has this cluster" true, which is what
      // the capability and endpoint lookups actually ask.
      clusters[cluster] = {};
    }
  }
  return clusters;
}

/**
 * The Matter device type ids this endpoint claims, from the Descriptor cluster's
 * DeviceTypeList. Empty when the endpoint has no Descriptor or has not been read yet,
 * in which case the cluster-based fallback decides the type.
 */
function readDeviceTypes(endpoint: Endpoint): number[] {
  const descriptor = endpoint.maybeStateOf("descriptor");
  const list = descriptor?.deviceTypeList;
  if (!Array.isArray(list)) return [];
  return list
    .map(entry => {
      if (typeof entry !== "object" || entry === null) return undefined;
      const id = (entry as { deviceType?: unknown }).deviceType;
      return typeof id === "number" ? id : undefined;
    })
    .filter((id): id is number => id !== undefined);
}
