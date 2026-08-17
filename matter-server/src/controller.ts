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
 * The clusters a snapshot reads.
 *
 * Bounded rather than "every supported cluster": a snapshot is rebuilt on every node
 * event, and reading all ~40 clusters a composed device may expose would make a busy
 * fabric expensive for data nothing consumes. Anything not listed here is invisible to
 * GIAP by construction, which is the same contract the schema-11 adapter had.
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
  ...sensorClusters(),
]);

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
  static async start(storagePath: string, events: ControllerEvents): Promise<Controller> {
    Environment.default.vars.set("storage.path", storagePath);

    const node = await ServerNode.create({ id: "giap-controller" });
    await node.start();

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
      const index = peer.peerAddress?.fabricIndex;
      if (index !== undefined) return Number(index);
    }
    return null;
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
          await command(action.payload);
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
    for (const peer of this.#node.peers) {
      this.#observe(peer);
    }
    this.#node.peers.added.on(peer => {
      this.#observe(peer);
      const nodeId = peerNodeId(peer);
      if (nodeId !== undefined) {
        this.#events.deviceAdded(nodeToDevice(snapshotOf(peer, nodeId)));
      }
    });
    this.#node.peers.deleted.on(peer => {
      const nodeId = peerNodeId(peer);
      if (nodeId === undefined) return;
      this.#observed.delete(peer.id);
      this.#events.deviceRemoved(deviceIdForNode(nodeId));
    });
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

    peer.lifecycle.online.on(() => this.#announceAvailability(peer, true));
    peer.lifecycle.offline.on(() => this.#announceAvailability(peer, false));

    for (const endpoint of peer.endpoints) {
      for (const cluster of Object.keys(endpoint.behaviors.supported)) {
        if (!SNAPSHOT_CLUSTERS.has(cluster)) continue;
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

      (on as (handler: (value: unknown) => void) => void).call(observable, value => {
        const nodeId = peerNodeId(peer);
        if (nodeId === undefined) return;

        const reading = readingFor(nodeId, cluster, attribute, value);
        if (reading !== undefined) {
          this.#events.reading(reading);
          return;
        }
        // Not a sensor value, but a change to a cluster that shapes what the device
        // IS — a name, a device type, a newly reported cluster. The device is
        // republished so the registry's typing and capabilities stay true.
        if (cluster === "basicInformation" || cluster === "descriptor") {
          this.#events.deviceUpdated(nodeToDevice(snapshotOf(peer, nodeId)));
        }
      });
    }
  }

  #announceAvailability(peer: ClientNode, online: boolean): void {
    const nodeId = peerNodeId(peer);
    if (nodeId === undefined) return;
    this.#events.availabilityChanged(deviceIdForNode(nodeId), online);
  }
}

/** The peer's Matter node id, or `undefined` while it is only commissionable. */
function peerNodeId(peer: ClientNode): bigint | undefined {
  const nodeId = peer.peerAddress?.nodeId;
  return nodeId === undefined ? undefined : BigInt(nodeId);
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
    if (!SNAPSHOT_CLUSTERS.has(cluster)) continue;
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
