/**
 * The `giap-matter` wire protocol — types only, no I/O.
 *
 * GIAP's Rust side and this server are the two implementations; `docs/matter-protocol.md`
 * is the specification both follow. Everything here is deliberately domain-level: the
 * wire carries devices, readings and control verbs, never endpoints, clusters or
 * attribute paths. That asymmetry is the point of the protocol — matter.js has typed
 * cluster models, so the Matter vocabulary stays on this side and the Rust adapter
 * never has to know what a cluster is.
 */

/** Bumped when a change would break a Rust client that has not been updated with it. */
export const PROTOCOL_VERSION = 1;

/** Identifies this protocol in the greeting, so an address pointing at some
 *  other server fails with a name rather than a parse error. */
export const PROTOCOL_NAME = "giap-matter";

/** The server speaks first. A client that does not recognise this refuses to proceed. */
export interface Greeting {
  protocol: typeof PROTOCOL_NAME;
  version: number;
  /** The controller's own fabric, for the operator's benefit when reading logs. */
  fabric_id: number | null;
  matter_js: string;
}

// ── Domain types ─────────────────────────────────────────────────────────────
// Both are direct projections of GIAP's own types (`device_registry::Device` and
// `sensor::SensorReading`), so the Rust side deserialises them without a mapping step.

export interface Device {
  /** `matter-<node_id>`; stable across restarts because the node id is. */
  id: string;
  name: string;
  device_type: string;
  capabilities: string[];
  online: boolean;
}

export interface Reading {
  device_id: string;
  sensor_type: string;
  value: number;
  unit: string;
  /** RFC 3339. */
  at: string;
}

/**
 * What a control op actually changed. Reported by the server rather than assumed by
 * the caller: the old adapter built its outcome from the value it had asked for, so it
 * reported success in the caller's terms whether or not the device had taken it.
 *
 * Every field is optional and only the ones the verb touched are set.
 */
export interface DeviceStatePatch {
  on?: boolean;
  brightness?: number;
  target_temp?: number;
  locked?: boolean;
  hue?: number;
  saturation?: number;
  fan_speed?: number;
  fan_mode?: string;
  position?: number;
}

// ── Envelope ─────────────────────────────────────────────────────────────────

export type OpName =
  | "subscribe"
  | "discover"
  | "commission"
  | "decommission"
  | "control"
  | "ping";

export interface Request {
  id: string;
  op: OpName;
  params?: Record<string, unknown>;
}

export interface SuccessResponse {
  id: string;
  ok: true;
  result: unknown;
}

export interface FailureResponse {
  id: string;
  ok: false;
  error: WireError;
}

export type Response = SuccessResponse | FailureResponse;

export interface WireError {
  code: ErrorCode;
  message: string;
}

/**
 * A closed set, because both the message the user reads and the decision to raise a
 * notification key off it. An open-ended string would push both back onto substring
 * matching against controller prose, which is what made "commissioning failed" the
 * only diagnosis GIAP could offer.
 */
export type ErrorCode =
  | "no_device_in_pairing_mode"
  | "invalid_setup_code"
  | "commission_failed"
  | "device_unknown"
  | "capability_unsupported"
  | "device_unreachable"
  | "bad_request"
  | "internal";


/** An error carrying a wire code, so the dispatcher does not have to guess one. */
export class OpError extends Error {
  readonly code: ErrorCode;

  constructor(code: ErrorCode, message: string) {
    super(message);
    this.name = "OpError";
    this.code = code;
  }

  toWire(): WireError {
    return { code: this.code, message: this.message };
  }
}

// ── Events ───────────────────────────────────────────────────────────────────

export type EventName =
  | "device_added"
  | "device_updated"
  | "device_removed"
  | "device_availability"
  | "reading"
  | "log";

export interface Event {
  event: EventName;
  payload: unknown;
}

export function response(id: string, result: unknown): SuccessResponse {
  return { id, ok: true, result };
}

export function failure(id: string, error: WireError): FailureResponse {
  return { id, ok: false, error };
}

export function event(name: EventName, payload: unknown): Event {
  return { event: name, payload };
}

/** `matter-<node_id>`, matching what the Rust side parses back out. */
export function deviceIdForNode(nodeId: bigint | number): string {
  return `matter-${nodeId.toString()}`;
}

export function nodeIdFromDeviceId(deviceId: string): bigint | undefined {
  if (!deviceId.startsWith("matter-")) return undefined;
  const rest = deviceId.slice("matter-".length);
  if (!/^\d+$/.test(rest)) return undefined;
  return BigInt(rest);
}
