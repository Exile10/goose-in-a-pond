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
import { describeNode } from "./mapping/describe.js";
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
          await (hasFields ? invoke(action.payload) : invoke());
        } else {
          await endpoint.setStateOf(action.cluster, { [action.attribute]: action.value });
        }
      } catch (error) {
        if (error instanceof OpError) throw error;
        throw new OpError("device_unreachable", describeError(error));
      }
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
