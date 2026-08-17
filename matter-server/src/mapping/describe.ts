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

import type { Capability, DeviceDescription, SensorSpec, ValueSpec } from "../protocol.js";
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
import { SENSORS } from "./sensors.js";
import { operationsOf, settingsOf } from "./settings.js";
import { endpointWith, type NodeSnapshot } from "./snapshot.js";

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

/** Matter's `MeasurementUnitEnum`, however matter.js hands it over. */
const MEASUREMENT_UNITS: ReadonlyMap<number, string> = new Map([
  [0, "ppm"],
  [1, "ppb"],
  [2, "ppt"],
  [3, "mg/m3"],
  [4, "ug/m3"],
  [5, "ng/m3"],
  [6, "/m3"],
  [7, "Bq/m3"],
]);

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
 * A thermostat's own setpoint limits, in degrees Celsius. Matter reports these in
 * hundredths; a range invented by GIAP would be a promise the device never made.
 */
function temperatureSpec(node: NodeSnapshot): ValueSpec {
  const min = asNumber(attribute(node, CLUSTER_THERMOSTAT, "absMinHeatSetpointLimit"));
  const max = asNumber(attribute(node, CLUSTER_THERMOSTAT, "absMaxHeatSetpointLimit"));

  // Keys are omitted rather than set undefined: a limit the device did not state
  // must be absent from the description, not present and empty.
  return {
    kind: "number",
    unit: "C",
    ...(min === undefined ? {} : { min: min / 100 }),
    ...(max === undefined ? {} : { max: max / 100 }),
  };
}

/** The unit a concentration cluster declares, if it declares one. */
function declaredUnit(node: NodeSnapshot, cluster: string): string | undefined {
  const raw = attribute(node, cluster, "measurementUnit");

  const numeric = asNumber(raw);
  if (numeric !== undefined) return MEASUREMENT_UNITS.get(numeric);

  if (typeof raw === "string") {
    const match = [...MEASUREMENT_UNITS.values()].find(
      unit => unit.replace("/", "").toLowerCase() === raw.replace("/", "").toLowerCase(),
    );
    return match;
  }
  return undefined;
}

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
  if (has(CLUSTER_THERMOSTAT)) add("target_temp", temperatureSpec(node));
  if (has(CLUSTER_DOOR_LOCK)) add("locked", { kind: "boolean" });
  if (has(CLUSTER_COLOR_CONTROL)) add("color", { kind: "color" });
  if (has(CLUSTER_WINDOW_COVERING)) add("position", { kind: "percent" });

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
      unit: declaredUnit(node, mapping.cluster) ?? mapping.unit,
    });
  }

  return sensors;
}

export function describeNode(node: NodeSnapshot): DeviceDescription {
  return {
    device_id: deviceIdForNode(node.nodeId),
    // The same projection the device list uses, so a description and a listing
    // can never disagree about what a device is.
    device_type: nodeToDevice(node).device_type,
    capabilities: capabilitiesOf(node),
    sensors: sensorsOf(node),
  };
}
