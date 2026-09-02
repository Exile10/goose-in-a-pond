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
  StateSpec,
  ValueSpec,
  VendorClusterSpec,
} from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  colorSupport,
  levelIsBrightness,
  speakerEndpoint,
  valveHasLevel,
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_SMOKE_CO_ALARM,
  CLUSTER_SWITCH,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_VALVE,
  CLUSTER_WINDOW_COVERING,
  nodeToDevice,
} from "./devices.js";
import { miredsToKelvin } from "./control.js";
import { clusterHasFeature, declaredUnitOf, sensorApplies, SENSORS } from "./sensors.js";
import {
  applianceSetpoint,
  reachableRange,
  systemMode,
  targetSetpoint,
} from "./thermostat.js";
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

/**
 * DoorLock's `doorState`, in the order Matter numbers it.
 *
 * Three of these six are the reason the attribute is worth reading at all: a lock can
 * say jammed, forced open, or ajar, and none of them is answerable from `lockState`.
 * A bolt thrown into a frame that is standing open reports "locked" perfectly happily.
 *
 * Declared here rather than in `state.ts` because for a read-only value the list of
 * words *is* the description — `state` imports it so the two cannot drift.
 */
export const DOOR_STATES = [
  "open",
  "closed",
  "jammed",
  "forced open",
  "unspecified error",
  "ajar",
] as const;

/** The same six by matter.js's enum name, which it may hand over instead of the number. */
const DOOR_STATE_NAMES: ReadonlyMap<string, string> = new Map(
  DOOR_STATES.map(word => [`door${word.replace(/ /g, "")}`, word]),
);

/**
 * Where a valve is, in its own three words.
 *
 * "Transitioning" is not padding: a motorised valve takes seconds to travel, and a
 * caller that has just asked for it to open needs to be able to tell "moving" from
 * "refused". Declared here for the same reason `DOOR_STATES` is -- `state` imports
 * it, so a description and a reading cannot use different words.
 */
export const VALVE_STATES = ["closed", "open", "transitioning"] as const;

/** The same three by matter.js's enum name, which it may hand over instead. */
const VALVE_STATE_NAMES: ReadonlyMap<string, string> = new Map(
  VALVE_STATES.map(word => [word, word]),
);

/** Both encodings, for the reason `doorStateWord` reads both. */
export function valveStateWord(raw: unknown): string | undefined {
  const numeric = asNumber(raw);
  if (numeric !== undefined) return VALVE_STATES[numeric];
  if (typeof raw === "string") return VALVE_STATE_NAMES.get(raw.toLowerCase().trim());
  return undefined;
}

/** What PIN enforcement reads as. Both words, so `state` cannot invent a third. */
export const PIN_REQUIREMENTS = ["required", "not required"] as const;

/**
 * The two kinds of Generic Switch, and the reason the distinction is reported.
 *
 * A latching switch stays where it is put, so its position is a lasting fact about
 * the device — MVD's Generic Switch is one, and its own screen shows nothing but
 * "Current position". A momentary switch is a pushbutton: `currentPosition` returns
 * to rest the instant it is released, and everything interesting about it — the
 * press, the release, the double-press — arrives as a Matter EVENT rather than an
 * attribute. The controller subscribes to attribute changes only, so those presses
 * are not observed here at all.
 *
 * Which is exactly why the kind is worth saying out loud. "Reports a position, 0 to
 * 1" is a true and useful description of a latching switch and a misleading one of a
 * button, and a reader who is told which kind it is can tell the difference.
 */
export const SWITCH_KINDS = ["latching", "momentary"] as const;

/**
 * Which kind of switch this is, or undefined if the device did not say.
 *
 * Stricter than `clusterHasFeature`, on purpose. That helper answers "may this
 * reading exist" and treats an unstated feature map as a yes, which is right when
 * withholding a working reading is the worse mistake. Here an unstated feature map
 * means the device has not told us which kind of switch it is, and inventing an
 * answer would put a word in its mouth.
 */
export function switchKindOf(node: NodeSnapshot): (typeof SWITCH_KINDS)[number] | undefined {
  const features = attribute(node, CLUSTER_SWITCH, "featureMap");
  if (typeof features !== "object" || features === null) return undefined;
  const claimed = features as Record<string, unknown>;
  if (claimed["latchingSwitch"] === true) return "latching";
  if (claimed["momentarySwitch"] === true) return "momentary";
  return undefined;
}

function attribute(node: NodeSnapshot, cluster: string, name: string): unknown {
  return endpointWith(node, cluster)?.clusters[cluster]?.[name];
}

/**
 * A lock's `doorState` as a word, or undefined if it does not have one.
 *
 * Both encodings, for the reason `fanModes` reads both: matter.js may decode an enum
 * to its name rather than its number, and a door reported as "DoorJammed" must not
 * come out the same as a door that said nothing.
 */
export function doorStateWord(raw: unknown): string | undefined {
  const numeric = asNumber(raw);
  if (numeric !== undefined) return DOOR_STATES[numeric];
  if (typeof raw === "string") {
    return DOOR_STATE_NAMES.get(raw.toLowerCase().replace(/[\s_-]/g, ""));
  }
  return undefined;
}

/**
 * The colour temperatures this device can actually reach, in kelvin.
 *
 * Mireds are reciprocal megakelvin, so the conversion inverts the bounds: the SMALLEST
 * mired value is the HOTTEST colour. Getting that backwards yields a range whose
 * minimum exceeds its maximum, which reads as a broken device rather than a broken
 * conversion.
 *
 * Zero is not a temperature. The spec's default for `colorTempPhysicalMinMireds` is 0,
 * which converts to infinite kelvin — so a device that has not stated a real bound gets
 * no bound stated for it, and the capability stands without an invented range.
 */
function colorTemperatureSpec(node: NodeSnapshot): ValueSpec {
  const coolestMireds = asNumber(attribute(node, CLUSTER_COLOR_CONTROL, "colorTempPhysicalMinMireds"));
  const warmestMireds = asNumber(attribute(node, CLUSTER_COLOR_CONTROL, "colorTempPhysicalMaxMireds"));

  const spec: ValueSpec = { kind: "number", unit: "K" };
  if (warmestMireds !== undefined && warmestMireds > 0) {
    spec.min = miredsToKelvin(warmestMireds);
  }
  if (coolestMireds !== undefined && coolestMireds > 0) {
    spec.max = miredsToKelvin(coolestMireds);
  }
  return spec;
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

  // Through the tolerant reader: compared as a raw number this failed for every device
  // that reported its mode as a NAME, and a cool-only air conditioner in Cool mode fell
  // through to the union of both setpoints -- 7 to 32 C on a device that cannot reach 7.
  const endpoint = endpointWith(node, CLUSTER_THERMOSTAT);
  const mode = endpoint === undefined ? undefined : systemMode(endpoint);
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
  // Whose level it is decides which control it is. A composed television carries Level
  // Control on its SPEAKER endpoint, and describing that as brightness offered a control
  // that would have turned the sound down instead.
  if (speakerEndpoint(node) !== undefined) add("volume", { kind: "percent" });
  if (levelIsBrightness(node)) add("brightness", { kind: "percent" });
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
  // Gated on what the device claims, not on the cluster being present. A tunable-white
  // bulb has ColorControl with no hue, and offering it one is the failure this whole
  // area exists to stop — the same reason `tilt` is offered only to a covering that
  // reports a tilt position.
  if (has(CLUSTER_COLOR_CONTROL)) {
    const colour = colorSupport(node);
    if (colour.hueSaturation) add("color", { kind: "color" });
    if (colour.temperature) add("color_temp", colorTemperatureSpec(node));
  }
  if (has(CLUSTER_VALVE)) {
    add("valve", { kind: "boolean" });
    // Only a valve that says it has a level. A plain solenoid is open or shut with
    // nothing in between, and offering it a percentage is a control it would reject.
    if (valveHasLevel(node)) add("position", { kind: "percent" });
  }
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
    const endpoint = endpointWith(node, mapping.cluster);
    if (endpoint === undefined) continue;
    // Boolean State is one bit whose meaning is the endpoint's device type, so only
    // one of its four mappings describes any given device. Without this a leak
    // detector was described as having a `leak` AND a `contact`.
    if (!sensorApplies(mapping, endpoint.deviceTypes)) continue;
    // Cluster presence is not sensor presence where the cluster's own features decide.
    if (
      mapping.feature !== undefined &&
      !clusterHasFeature(endpoint.clusters[mapping.cluster], mapping.feature)
    ) {
      continue;
    }
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

/** SmokeCoAlarm's ExpressedStateEnum: WHICH alarm the device is currently sounding. */
export const EXPRESSED_STATES = [
  "normal",
  "smoke alarm",
  "co alarm",
  "battery alert",
  "testing",
  "hardware fault",
  "end of service",
  "interconnected smoke alarm",
  "interconnected co alarm",
] as const;

/** The same nine by matter.js's enum name, which it may send instead of the number. */
const EXPRESSED_STATE_NAMES: ReadonlyMap<string, string> = new Map([
  ["normal", "normal"],
  ["smokealarm", "smoke alarm"],
  ["coalarm", "co alarm"],
  ["batteryalert", "battery alert"],
  ["testing", "testing"],
  ["hardwarefault", "hardware fault"],
  ["endofservice", "end of service"],
  ["interconnectsmoke", "interconnected smoke alarm"],
  ["interconnectco", "interconnected co alarm"],
]);

/** What the alarm says it is expressing, or undefined if it does not say. */
export function expressedStateWord(raw: unknown): string | undefined {
  const numeric = asNumber(raw);
  if (numeric !== undefined) return EXPRESSED_STATES[numeric];
  if (typeof raw === "string") {
    return EXPRESSED_STATE_NAMES.get(raw.toLowerCase().replace(/[\s_-]/g, ""));
  }
  return undefined;
}

/** SmokeCoAlarm's EndOfServiceEnum. An expired alarm is a decoration. */
export const SERVICE_STATES = ["normal", "expired"] as const;

/**
 * What the device reports and nothing can set.
 *
 * Both of these sat in the snapshot already — `doorLock` is read whole — and fell
 * through every slot there was: not a verb, so not a capability, and not a number, so
 * not a sensor. Asked what a door lock could do, GIAP answered "locked or unlocked"
 * for a device whose own app showed a door position and a PIN requirement beside it.
 *
 * Each is gated on the attribute actually being there, because both belong to optional
 * DoorLock features: a lock with no position sensor has no `doorState`, and declaring
 * one would promise a reading that never arrives.
 */
function statesOf(node: NodeSnapshot): StateSpec[] {
  const states: StateSpec[] = [];

  // Read whether or not the value decodes: a lock with the DoorPositionSensor feature
  // has the attribute, and an encoding this does not recognise is still a device that
  // reports its door.
  if (attribute(node, CLUSTER_DOOR_LOCK, "doorState") !== undefined) {
    states.push({ name: "door", value: { kind: "enum", values: [...DOOR_STATES] } });
  }

  // Whether remote lock and unlock require a PIN. Reported, never written — and not a
  // `mode` for that reason. Every writable attribute on this cluster is a security
  // control (`sendPinOverTheAir`, `enableLocalProgramming`, `wrongCodeEntryLimit`), and
  // a verb for one of them puts a lock's security configuration one sentence of natural
  // language away from being turned off.
  // Gated on CredentialOverTheAirAccess *and* PinCredential, not PIN alone -- measured
  // against a live lock, matter.js refuses the attribute without both. So a PIN lock
  // with no over-the-air credential access has no such setting to report.
  if (typeof attribute(node, CLUSTER_DOOR_LOCK, "requirePinForRemoteOperation") === "boolean") {
    states.push({ name: "pin_required", value: { kind: "enum", values: [...PIN_REQUIREMENTS] } });
  }

  // Whether a valve is shut, open, or moving between the two, and whether it has
  // faulted. Reported rather than driven: `valve` and `position` ask for a state,
  // and this is the device saying where it actually is -- which for a motorised
  // valve is a third thing for several seconds, and a fourth if it jams.
  if (attribute(node, CLUSTER_VALVE, "currentState") !== undefined) {
    states.push({ name: "valve_state", value: { kind: "enum", values: [...VALVE_STATES] } });
  }
  if (attribute(node, CLUSTER_VALVE, "valveFault") !== undefined) {
    states.push({ name: "valve_fault", value: { kind: "boolean" } });
  }

  // A smoke/CO alarm's summary of what it is doing, and whether it can still do it.
  //
  // `expressedState` is the one the device's own screen shows, and it is the only
  // attribute that says WHICH alarm is sounding — smoke and CO have separate readings
  // but a device expressing a CO alarm while its smoke reading sits at Critical is
  // telling you something neither reading does. Categorical, not a magnitude:
  // "interconnected CO alarm" is not eight times worse than "normal", so it is a state
  // rather than a sensor with an ordinal a rule could compare.
  if (attribute(node, CLUSTER_SMOKE_CO_ALARM, "expressedState") !== undefined) {
    states.push({ name: "alarm", value: { kind: "enum", values: [...EXPRESSED_STATES] } });
  }
  // Whether the unit is past its service life. Not an ordinal either — expired is not a
  // worse Normal, it is a different fact about the device.
  if (attribute(node, CLUSTER_SMOKE_CO_ALARM, "endOfServiceAlert") !== undefined) {
    states.push({ name: "alarm_service", value: { kind: "enum", values: [...SERVICE_STATES] } });
  }
  // A fault means the alarm may not sound at all, which is the one thing a smoke alarm
  // exists to do.
  if (typeof attribute(node, CLUSTER_SMOKE_CO_ALARM, "hardwareFaultAlert") === "boolean") {
    states.push({ name: "alarm_fault", value: { kind: "enum", values: ["ok", "faulty"] } });
  }

  // Which way a Generic Switch is thrown. Reported, and settable by nobody -- a
  // switch is a thing a person moves, which is the whole of what it is for.
  //
  // Described as nothing at all until now: 0x000f is not a device type GIAP mapped
  // and `switch` is not a cluster it read, so a commissioned Generic Switch arrived
  // typed `matter` with no capabilities and answered "cannot be controlled, and does
  // not measure any data" -- while the maker's app showed its position plainly.
  if (attribute(node, CLUSTER_SWITCH, "currentPosition") !== undefined) {
    // `numberOfPositions` is what the device says it has. Bounded only when it said
    // so: the spec's default is 2, but a default is not a statement, and an invented
    // bound is worse than an absent one because it will be believed.
    const positions = asNumber(attribute(node, CLUSTER_SWITCH, "numberOfPositions"));
    const value: ValueSpec =
      positions !== undefined && positions > 1
        ? { kind: "number", min: 0, max: positions - 1 }
        : { kind: "number", min: 0 };
    states.push({ name: "switch_position", value });
  }
  // Latching or momentary, when the device claimed one. See `SWITCH_KINDS`: a
  // position is a lasting fact about a latching switch and a fleeting one about a
  // button, whose presses are Matter events this controller does not subscribe to.
  if (switchKindOf(node) !== undefined) {
    states.push({ name: "switch_kind", value: { kind: "enum", values: [...SWITCH_KINDS] } });
  }

  return states;
}

export function describeNode(node: NodeSnapshot): DeviceDescription {
  return {
    // Endpoint included, or every bridged child describes itself under its hub's id.
    device_id: deviceIdForNode(node.nodeId, node.rootEndpoint),
    // The same projection the device list uses, so a description and a listing
    // can never disagree about what a device is.
    device_type: nodeToDevice(node).device_type,
    capabilities: capabilitiesOf(node),
    sensors: sensorsOf(node),
    vendor_clusters: vendorClustersOf(node),
    states: statesOf(node),
  };
}
