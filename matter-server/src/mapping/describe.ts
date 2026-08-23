/**
 * What a device can be told to do and what it measures, read from the device.
 *
 * `Device.capabilities` is a short list of verb names — enough to know a fan has a
 * speed, not enough to drive it. It cannot say which modes that fan has, what a
 * thermostat's limits are, or that an air quality sensor measures eleven separate
 * substances. A model given only the list has to guess, and finds the limits by
 * failing.
 *
 * So where a cluster states a constraint, it is read rather than assumed:
 * FanControl's mode sequence, a thermostat's setpoint limits, a concentration's
 * declared unit. Where a cluster states nothing, the conventional default stands and
 * nothing more is claimed — an invented constraint is worse than an absent one,
 * because it will be believed.
 */

import type {
  Capability,
  DeviceDescription,
  SensorSpec,
  ValueSpec,
  VendorClusterSpec,
} from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_WINDOW_COVERING,
  nodeToDevice,
} from "./devices.js";
import { declaredUnitOf, SENSORS } from "./sensors.js";
import { applianceSetpoint, reachableRange, targetSetpoint } from "./thermostat.js";
import { operationsOf, settingsOf } from "./settings.js";
import { applicationEndpoints, endpointWith, type NodeSnapshot } from "./snapshot.js";

/**
 * FanControl's `fanModeSequence` says which modes a fan really has — Off/Low/Med/High
 * is a different device from Off/High/Auto, and offering a mode the device does not
 * implement produces a failure the user cannot act on.
 */
const FAN_MODE_SEQUENCES: ReadonlyMap<number, string[]> = new Map([
  [0, ["off", "low", "medium", "high"]],
  [1, ["off", "low", "high"]],
  [2, ["off", "low", "medium", "high", "auto"]],
  [3, ["off", "low", "high", "auto"]],
  [4, ["off", "high", "auto"]],
  [5, ["off", "high"]],
]);

/** Every mode GIAP can send, for a fan that does not narrow it down. */
const ALL_FAN_MODES = ["off", "low", "medium", "high", "on", "auto", "smart"];

function attribute(node: NodeSnapshot, cluster: string, name: string): unknown {
  return endpointWith(node, cluster)?.clusters[cluster]?.[name];
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/**
 * The modes this fan accepts. matter.js may decode the sequence as its enum name
 * rather than its number, so both are read; anything unrecognised falls back to the
 * full set, which is what GIAP assumed before it asked at all.
 */
function fanModes(node: NodeSnapshot): string[] {
  const raw = attribute(node, CLUSTER_FAN_CONTROL, "fanModeSequence");

  const numeric = asNumber(raw);
  if (numeric !== undefined) return FAN_MODE_SEQUENCES.get(numeric) ?? ALL_FAN_MODES;

  if (typeof raw === "string") {
    // e.g. "OffLowMedHighAuto" — the shape matter.js uses for enum names.
    const name = raw.toLowerCase();
    const modes = ["off"];
    if (name.includes("low")) modes.push("low");
    if (name.includes("med")) modes.push("medium");
    if (name.includes("high")) modes.push("high");
    if (name.includes("auto")) modes.push("auto");
    return modes.length > 1 ? modes : ALL_FAN_MODES;
  }

  return ALL_FAN_MODES;
}

/**
 * A thermostat's setpoint limits, in degrees Celsius.
 *
 * The limits belong to whichever setpoint a request would land on, which the mode
 * decides. So the same device answers 7 to 23.5 while heating and 16 to 32 while
 * cooling, and a bare number is misleading twice over: it looks like a device fact
 * when it is a mode fact, and it hides that the device reaches higher in another
 * mode. Both the condition and the wider span are stated, so an answer read back
 * later carries the circumstances it was true under.
 *
 * The pairing is not arbitrary, and it reads backwards until you know what a
 * setpoint is. The heating setpoint is the temperature heat runs BELOW, so it is
 * the lower of the pair; the cooling setpoint is the one cooling runs ABOVE, so it
 * is the higher. They bracket a band and must stay `minSetpointDeadBand` apart,
 * which is what caps heating at 23.5 while this device cools at 26 — not a limit
 * on how warm it can make a room.
 */
function temperatureSpec(node: NodeSnapshot): ValueSpec {
  // An appliance keeps one setpoint, with its own limits and increment, and no mode
  // to qualify it.
  const appliance = applianceSetpoint(node);
  if (appliance !== undefined) {
    return {
      kind: "number",
      unit: "C",
      ...(appliance.min === undefined ? {} : { min: appliance.min / 100 }),
      ...(appliance.max === undefined ? {} : { max: appliance.max / 100 }),
      ...(appliance.step === undefined ? {} : { step: appliance.step / 100 }),
    };
  }

  const mode = attribute(node, CLUSTER_THERMOSTAT, "systemMode");
  // Auto (1) and Off (0) do not name a setpoint; the requested value would.
  const settled = mode === MODE_COOL || mode === MODE_HEAT || mode === MODE_EMERGENCY_HEAT;
  const live = settled ? targetSetpoint(node) : undefined;
  const range = live ?? reachableRange(node);

  return {
    kind: "number",
    unit: "C",
    ...(range?.min === undefined ? {} : { min: range.min / 100 }),
    ...(range?.max === undefined ? {} : { max: range.max / 100 }),
    ...(live === undefined ? {} : { when: conditionFor(live, reachableRange(node)) }),
  };
}

/** Matter's SystemModeEnum, for the modes that settle which setpoint is meant. */
const MODE_COOL = 3;
const MODE_HEAT = 4;
const MODE_EMERGENCY_HEAT = 5;

/**
 * What a mode-bound range is true of, and what the device can do outside it.
 *
 * Without the second half, "7 to 23.5" reads as this thermostat's ceiling, and a
 * reader concludes it cannot be asked for 30 — when 30 is reachable the moment its
 * mode changes.
 */
function conditionFor(
  live: { which: "heating" | "cooling"; min?: number; max?: number },
  overall: { min?: number; max?: number } | undefined,
): string {
  const doing = live.which === "heating" ? "while heating" : "while cooling";
  if (overall === undefined) return doing;

  const wider = (overall.min !== undefined && overall.min !== live.min)
    || (overall.max !== undefined && overall.max !== live.max);
  if (!wider) return doing;

  const from = overall.min === undefined ? "" : `${overall.min / 100} to `;
  const to = overall.max === undefined ? "" : `${overall.max / 100}`;
  return `${doing}; this device reaches ${from}${to} C across its modes`;
}

/** The unit a concentration cluster declares, if it declares one. */

function capabilitiesOf(node: NodeSnapshot): Capability[] {
  const capabilities: Capability[] = [];
  const has = (cluster: string) => endpointWith(node, cluster) !== undefined;
  const add = (verb: Capability["verb"], value: ValueSpec) => capabilities.push({ verb, value });

  const hasOnOff = has(CLUSTER_ON_OFF);
  const hasFan = has(CLUSTER_FAN_CONTROL);

  // A fan need not implement On/Off at all; FanMode is its power switch.
  if (hasOnOff || hasFan) add("power", { kind: "boolean" });
  if (has(CLUSTER_LEVEL_CONTROL)) add("brightness", { kind: "percent" });
  if (hasFan) {
    add("fan_speed", { kind: "percent" });
    add("fan_mode", { kind: "enum", values: fanModes(node) });
  }
  // Either source of a temperature target: a thermostat's setpoints, or an
  // appliance's own. Gating on the thermostat alone is why a dishwasher showing a
  // 49 to 82 degree slider was described as having no temperature at all.
  if (has(CLUSTER_THERMOSTAT) || applianceSetpoint(node) !== undefined) {
    add("target_temp", temperatureSpec(node));
  }
  if (has(CLUSTER_DOOR_LOCK)) add("locked", { kind: "boolean" });
  if (has(CLUSTER_COLOR_CONTROL)) add("color", { kind: "color" });
  if (has(CLUSTER_WINDOW_COVERING)) add("position", { kind: "percent" });
  // The second axis, offered only by a covering that has it. A roller blind has no
  // slats to turn, and offering a control the device will reject is the failure this
  // whole area exists to stop.
  if (attribute(node, CLUSTER_WINDOW_COVERING, "currentPositionTiltPercent100ths") !== undefined) {
    add("tilt", { kind: "percent" });
  }

  // Everything the device says can be chosen, named as it names it. This is what
  // makes an appliance drivable without a verb per appliance: a washer's cycle,
  // its spin speed and its rinse count are three settings, not three new verbs.
  for (const setting of settingsOf(node)) {
    capabilities.push({
      verb: "mode",
      setting: setting.name,
      value: { kind: "enum", values: setting.values },
    });
  }

  const operations = operationsOf(node);
  if (operations !== undefined) {
    capabilities.push({ verb: "operation", value: { kind: "enum", values: operations.values } });
  }

  return capabilities;
}

/**
 * What the device measures — whether or not it has reported yet.
 *
 * Derived from the same table the readings come from, so a sensor cannot be
 * describable and unreadable, or the reverse.
 */
function sensorsOf(node: NodeSnapshot): SensorSpec[] {
  const seen = new Set<string>();
  const sensors: SensorSpec[] = [];

  for (const mapping of SENSORS) {
    if (endpointWith(node, mapping.cluster) === undefined) continue;
    if (seen.has(mapping.sensorType)) continue;
    seen.add(mapping.sensorType);
    sensors.push({
      sensor_type: mapping.sensorType,
      // What the device says it measures in, falling back to the substance's
      // conventional unit only where it says nothing.
      unit:
        declaredUnitOf(attribute(node, mapping.cluster, "measurementUnit")) ?? mapping.unit,
    });
  }

  return sensors;
}

/**
 * The manufacturer-specific clusters this device has, which is all that can be said
 * about them.
 *
 * Deliberately not folded into `capabilities`: a capability is a verb `control`
 * accepts, and there is no verb here. Naming one would break the property the whole
 * type rests on -- that anything describable is callable -- to gain a control the
 * agent still could not work.
 *
 * Application endpoints only, for the reason `applicationEndpoints` exists: endpoint 0
 * is the node's own plumbing and never something the device does.
 */
function vendorClustersOf(node: NodeSnapshot): VendorClusterSpec[] {
  const vendor: VendorClusterSpec[] = [];
  for (const endpoint of applicationEndpoints(node)) {
    for (const cluster of endpoint.vendorClusters) {
      vendor.push({ cluster_id: cluster.id, endpoint: endpoint.number });
    }
  }
  return vendor;
}

export function describeNode(node: NodeSnapshot): DeviceDescription {
  return {
    device_id: deviceIdForNode(node.nodeId),
    // The same projection the device list uses, so a description and a listing
    // can never disagree about what a device is.
    device_type: nodeToDevice(node).device_type,
    capabilities: capabilitiesOf(node),
    sensors: sensorsOf(node),
    vendor_clusters: vendorClustersOf(node),
  };
}
