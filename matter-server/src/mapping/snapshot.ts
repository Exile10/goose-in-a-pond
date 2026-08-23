/**
 * The pure intermediate form every mapping works over.
 *
 * matter.js's `ClientNode` is a live object wired to a network and a store; nothing
 * built on it can be unit-tested without a fabric. So `controller.ts` projects a node
 * onto this plain structure once, and `devices.ts` / `sensors.ts` / `control.ts` are
 * pure functions over it. That is the same split the Rust adapter used (a pure
 * `protocol.rs` beside an I/O-bound `bridge.rs`), and it is what lets the recorded
 * real-device fixtures keep earning their keep after the move.
 */

/**
 * Cluster state keyed by matter.js behavior id (the uncapitalized Matter cluster
 * name — `onOff`, `levelControl`, `pm25ConcentrationMeasurement`), then by attribute
 * name. Values are whatever matter.js decoded, so a caller must still check types.
 */
export type ClusterState = Record<string, Record<string, unknown>>;

export interface EndpointSnapshot {
  /** The Matter endpoint number. Endpoint 0 is the root and never the application. */
  number: number;
  /** Device type ids from the Descriptor cluster's DeviceTypeList. */
  deviceTypes: number[];
  clusters: ClusterState;
}

export interface NodeSnapshot {
  nodeId: bigint;
  online: boolean;
  endpoints: EndpointSnapshot[];
}

/** Application endpoints, lowest number first. Endpoint 0 carries utility clusters
 *  (Basic Information, Descriptor for the root node) and never says what the device is. */
export function applicationEndpoints(node: NodeSnapshot): EndpointSnapshot[] {
  return node.endpoints.filter(e => e.number !== 0).sort((a, b) => a.number - b.number);
}

/** The lowest-numbered application endpoint carrying `behaviorId`, if any. */
export function endpointWith(node: NodeSnapshot, behaviorId: string): EndpointSnapshot | undefined {
  return applicationEndpoints(node).find(e => behaviorId in e.clusters);
}

export function hasCluster(node: NodeSnapshot, behaviorId: string): boolean {
  return endpointWith(node, behaviorId) !== undefined;
}

/** An attribute on the root endpoint, where Basic Information lives. */
export function rootAttribute(
  node: NodeSnapshot,
  behaviorId: string,
  attribute: string,
): unknown {
  return node.endpoints.find(e => e.number === 0)?.clusters[behaviorId]?.[attribute];
}
