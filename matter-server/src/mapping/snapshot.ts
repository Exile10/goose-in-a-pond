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

/**
 * A manufacturer-specific cluster: seen, and that is the whole of it.
 *
 * An id and nothing else, because an id and nothing else is what exists. matter.js
 * builds a behavior for a cluster its model cannot name, but discovers no shape for
 * it — measured against a live commissioned device, the behavior's schema carries
 * zero attributes. Matter publishes no attribute names either, so the words for these
 * controls ("Flip-Flop", "Emoticon" on Google's Matter Virtual Device) exist only in
 * that maker's app.
 *
 * Recorded anyway, because a description that lists power and brightness for a device
 * showing three controls reads as a statement that the third does not exist.
 */
export interface VendorCluster {
  /** The 32-bit Matter cluster id, e.g. 0xfff1fc01. */
  id: number;
}

export interface EndpointSnapshot {
  /** The Matter endpoint number. Endpoint 0 is the root and never the application. */
  number: number;
  /** Device type ids from the Descriptor cluster's DeviceTypeList. */
  deviceTypes: number[];
  clusters: ClusterState;
  /** Manufacturer-specific clusters here. Empty for all but a handful of devices. */
  vendorClusters: VendorCluster[];
  /**
   * This endpoint's child endpoints, from matter.js's own resolved structure.
   *
   * NOT the Descriptor cluster's `partsList`, which is also in the snapshot and
   * looks like the same thing. Three reasons matter.js's version is the one to
   * trust. The spec gives an Aggregator's PartsList *full-family* semantics — every
   * descendant — and a composed device's *tree* semantics, so the raw attribute
   * cannot say whether a grandchild is a child. A PartsList may name endpoint 0, or
   * name endpoints cyclically, and a recursive walk over one that does either
   * either inherits the node's own name or does not terminate. matter.js has
   * already resolved all of that into a real parent-child tree to build its
   * endpoint index, so reading `endpoint.parts` gets the answer rather than the
   * evidence.
   *
   * Empty until the structure is read, like every other field here.
   */
  parts: number[];
}

/**
 * Manufacturer-specific rather than standard.
 *
 * A Matter cluster id is 32 bits with the vendor code in the upper 16, and a standard
 * cluster's is zero. Keyed off the id rather than off "the snapshot has no name for
 * it" on purpose: the snapshot also drops `identify`, `groups`, `powerSource`,
 * `timeSynchronization` and a dozen more standard utility clusters, and reporting
 * those as things the device can do would bury the one entry that is a real control
 * the user can see in the maker's app.
 */
export function isVendorCluster(id: number): boolean {
  return (id >>> 16) !== 0;
}

export interface NodeSnapshot {
  nodeId: bigint;
  online: boolean;
  endpoints: EndpointSnapshot[];
  /**
   * The endpoint this snapshot is *about*, when it is about one device of several.
   *
   * A Matter bridge is one node carrying an Aggregator whose children are separate
   * logical devices, so one node has to become several GIAP devices. Rather than
   * teach forty mapping call sites what an endpoint is, `deviceSlices` cuts the node
   * into one snapshot per device and every existing mapping runs over a slice
   * unchanged — a slice IS a `NodeSnapshot`, just a narrower one.
   *
   * Absent for an ordinary node, which is then its own single slice and keeps the
   * device id it has always had.
   */
  rootEndpoint?: number;
}

/**
 * Application endpoints: this slice's own endpoint first, then ascending. Endpoint 0
 * carries utility clusters (Basic Information, Descriptor for the root node) and
 * never says what the device is.
 *
 * Root-first, not simply ascending, and that ordering is load-bearing for bridges.
 * Everything downstream — `endpointWith`, `hasCluster`, `deviceTypeFromDescriptor`,
 * `settingsOf` — resolves ties by taking the lowest endpoint, and the comment on
 * `deviceTypeFromDescriptor` justifies that with "a composed device is reported as
 * whatever its first endpoint claims, which is what its own UI calls it". True of an
 * air purifier with a fan inside it. False behind a bridge, where the endpoint
 * numbers are allocated by the HUB in its own discovery order: a bridged air
 * purifier at endpoint 9 whose Fan part landed at endpoint 2 would be typed a fan.
 * Putting the device's own endpoint first replaces an accident of numbering with a
 * fact about the device.
 *
 * A no-op while `rootEndpoint` is absent, which it always is until `deviceSlices`
 * starts producing slices.
 */
export function applicationEndpoints(node: NodeSnapshot): EndpointSnapshot[] {
  const application = node.endpoints
    .filter(e => e.number !== 0)
    .sort((a, b) => a.number - b.number);
  if (node.rootEndpoint === undefined) return application;

  const own = application.findIndex(e => e.number === node.rootEndpoint);
  if (own <= 0) return application;
  return [application[own]!, ...application.slice(0, own), ...application.slice(own + 1)];
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
