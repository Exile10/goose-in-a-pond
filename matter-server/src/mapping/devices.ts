/**
 * A commissioned node projected onto GIAP's `Device`.
 *
 * Works for any Matter device — real bulbs, locks, thermostats, or virtual test
 * devices. Nothing here is specific to a vendor or to test tooling: the type comes
 * from what the node says it is, and the capabilities from the clusters it exposes.
 */

import type { Device } from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  applicationEndpoints,
  endpointWith,
  hasCluster,
  rootAttribute,
  type EndpointSnapshot,
  type NodeSnapshot,
} from "./snapshot.js";
import { operationsOf, settingsOf } from "./settings.js";
import { applianceSetpoint } from "./thermostat.js";

export const CLUSTER_ON_OFF = "onOff";
export const CLUSTER_LEVEL_CONTROL = "levelControl";
export const CLUSTER_COLOR_CONTROL = "colorControl";
export const CLUSTER_THERMOSTAT = "thermostat";
export const CLUSTER_DOOR_LOCK = "doorLock";
export const CLUSTER_FAN_CONTROL = "fanControl";
export const CLUSTER_WINDOW_COVERING = "windowCovering";
export const CLUSTER_BASIC_INFORMATION = "basicInformation";
export const CLUSTER_OCCUPANCY = "occupancySensing";
export const CLUSTER_BOOLEAN_STATE = "booleanState";
export const CLUSTER_TEMPERATURE = "temperatureMeasurement";
export const CLUSTER_HUMIDITY = "relativeHumidityMeasurement";
export const CLUSTER_SMOKE_CO_ALARM = "smokeCoAlarm";
export const CLUSTER_SWITCH = "switch";
/**
 * A bridged device's own identity and its own reachability, as its hub reports them.
 *
 * Dropped from the snapshot until now, which is why a hub's children would have been
 * nameless and the hub's own liveness the only liveness there was.
 */
export const CLUSTER_BRIDGED_DEVICE_INFO = "bridgedDeviceBasicInformation";

/**
 * Every cluster this module names, for the snapshot allowlist.
 *
 * The same contract as `sensorClusters()` and `settingClusters()`: a cluster named here
 * is a cluster the snapshot admits, so the two cannot drift apart.
 */
export function deviceClusters(): ReadonlySet<string> {
  return new Set([
    CLUSTER_ON_OFF,
    CLUSTER_LEVEL_CONTROL,
    CLUSTER_COLOR_CONTROL,
    CLUSTER_THERMOSTAT,
    CLUSTER_DOOR_LOCK,
    CLUSTER_FAN_CONTROL,
    CLUSTER_WINDOW_COVERING,
    CLUSTER_BASIC_INFORMATION,
    CLUSTER_OCCUPANCY,
    CLUSTER_BOOLEAN_STATE,
    CLUSTER_TEMPERATURE,
    CLUSTER_HUMIDITY,
    CLUSTER_SMOKE_CO_ALARM,
    CLUSTER_SWITCH,
    CLUSTER_BRIDGED_DEVICE_INFO,
  ]);
}

/** Matter's Speaker device type. Its Level Control is volume, not brightness. */
export const SPEAKER_DEVICE_TYPE = 0x0022;

/**
 * The endpoint whose Level Control is a volume, if the device has one.
 *
 * A Basic Video Player is composed: the player on one endpoint, a Speaker on another,
 * and Level Control lives on the speaker. Searching the node for the cluster found it
 * and called it brightness, so a television advertised a brightness control that would
 * have turned the sound down instead. The endpoint's own device type is what tells them
 * apart, and it is already in the snapshot.
 */
export function speakerEndpoint(node: NodeSnapshot): EndpointSnapshot | undefined {
  return applicationEndpoints(node).find(
    e => e.deviceTypes.includes(SPEAKER_DEVICE_TYPE) && CLUSTER_LEVEL_CONTROL in e.clusters,
  );
}

/**
 * Matter device type ids (Descriptor DeviceTypeList), grouped onto the GIAP types the
 * UI has icons for. Ids are from the Matter Device Library; the grouping is ours — a
 * dishwasher and a washing machine are both "appliance" as far as anything GIAP shows
 * or says is concerned.
 */
const DEVICE_TYPES: ReadonlyMap<number, string> = new Map([
  // Lighting
  [0x0100, "light"], // On/Off Light
  [0x0101, "light"], // Dimmable Light
  [0x010c, "light"], // Colour Temperature Light
  [0x010d, "light"], // Extended Colour Light
  // Plugs — the pair of clusters alone cannot tell these from a bulb, which is why a
  // plug used to arrive wearing a lightbulb.
  [0x010a, "plug"], // On/Off Plug-in Unit
  [0x010b, "plug"], // Dimmable Plug-in Unit
  // Closures
  [0x000a, "lock"], // Door Lock
  [0x0202, "covering"], // Window Covering
  // Climate and air
  [0x0301, "thermostat"], // Thermostat
  [0x0072, "thermostat"], // Room Air Conditioner
  [0x002b, "fan"], // Fan
  [0x002c, "air"], // Air Purifier
  // Sensors
  [0x0015, "sensor"], // Contact Sensor
  [0x002d, "sensor"], // Air Quality Sensor
  [0x0106, "sensor"], // Light Sensor
  [0x0107, "sensor"], // Occupancy Sensor
  [0x0302, "sensor"], // Temperature Sensor
  [0x0305, "sensor"], // Pressure Sensor
  [0x0306, "sensor"], // Flow Sensor
  [0x0307, "sensor"], // Humidity Sensor
  // An alarm is not a sensor to a user: it is the thing that wakes them.
  [0x0076, "alarm"], // Smoke/CO Alarm
  // A switch reports which way it is thrown and takes no orders, so it is its own
  // type rather than a light with the controls missing. Deliberately NOT the switch
  // CLIENT types (0x0103 On/Off Light Switch, 0x0104 Dimmer Switch, 0x0105 Colour
  // Dimmer Switch): those drive other devices, and claiming a mapping nobody has
  // held a device against is how a plug arrived wearing a lightbulb.
  [0x000f, "switch"], // Generic Switch
  // Appliances
  [0x0073, "appliance"], // Laundry Washer
  [0x0075, "appliance"], // Dishwasher
  [0x0074, "vacuum"], // Robotic Vacuum Cleaner
  [0x0303, "pump"], // Pump
  // Media
  [0x0023, "media"], // Casting Video Player
  [0x0028, "media"], // Basic Video Player
]);

/**
 * The GIAP device type stated by the node itself, if it says.
 *
 * Endpoint 0 is the Root Node (0x0016) on every device and never describes the
 * application, so it is skipped. The first application endpoint that names a type
 * GIAP knows wins; a composed device (a fan inside an air purifier) is reported as
 * whatever its first endpoint claims, which is what its own UI calls it.
 */
export function deviceTypeFromDescriptor(node: NodeSnapshot): string | undefined {
  for (const endpoint of applicationEndpoints(node)) {
    for (const id of endpoint.deviceTypes) {
      const known = DEVICE_TYPES.get(id);
      if (known !== undefined) return known;
    }
  }
  return undefined;
}

/** Local to this module: matter.js hands numbers over as numbers, or not at all. */
function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/**
 * Which colour controls the device says it has.
 *
 * `colorCapabilities` is a bitmap, and matter.js may hand it over decoded into named
 * flags or as the raw number, so both are read — the same tolerance `fanModes` applies
 * to `fanModeSequence`.
 *
 * Lives here rather than in `describe.ts` because both the short capability list and
 * the description need it, and `describe.ts` already imports this module — the other
 * direction would be a cycle.
 *
 * Claims first, evidence second. Where the bitmap names a capability it is believed;
 * where it claims NOTHING — absent, or every flag false — the attributes the device
 * actually publishes decide instead. That fallback is not a nicety: Google's Matter
 * Virtual Device offers hue/saturation, XY and colour temperature in its own Controller
 * tab while claiming none of them here, and trusting the bitmap outright described that
 * light as having no colour whatsoever. A device that exposes `currentHue` has a hue
 * whatever its bitmap says.
 *
 * Presence of the CLUSTER is still not evidence of either, which is the bug this
 * replaced: a tunable-white bulb has ColorControl and no hue at all, and was being told
 * it accepted one it would reject. matter.js omits the attributes a device's features
 * do not cover, so `currentHue` is absent on exactly those bulbs — the same signal
 * `tilt` reads to tell a venetian blind from a roller.
 */
export function colorSupport(node: NodeSnapshot): { hueSaturation: boolean; temperature: boolean } {
  const raw = endpointWith(node, CLUSTER_COLOR_CONTROL)?.clusters[CLUSTER_COLOR_CONTROL]?.[
    "colorCapabilities"
  ];

  // Bit 0 HueSaturation, bit 4 ColorTemperature (Matter 1.4, ColorControl 5.2.2.9).
  // matter.js decodes the bitmap to named flags, but a raw number is read too — the
  // same tolerance `fanModes` applies, and neither shape is guaranteed by the wire.
  const numeric = asNumber(raw);
  const claimed =
    numeric !== undefined
      ? { hueSaturation: (numeric & 0x01) !== 0, temperature: (numeric & 0x10) !== 0 }
      : typeof raw === "object" && raw !== null
        ? {
            hueSaturation: (raw as { hueSaturation?: unknown }).hueSaturation === true,
            temperature: (raw as { colorTemperature?: unknown }).colorTemperature === true,
          }
        : { hueSaturation: false, temperature: false };

  if (claimed.hueSaturation || claimed.temperature) return claimed;

  // The device claimed nothing. Read what it publishes instead of concluding it has no
  // colour: an attribute is only there because a feature covers it.
  const state = endpointWith(node, CLUSTER_COLOR_CONTROL)?.clusters[CLUSTER_COLOR_CONTROL];
  return {
    hueSaturation: state?.["currentHue"] !== undefined || state?.["currentSaturation"] !== undefined,
    temperature: state?.["colorTemperatureMireds"] !== undefined,
  };
}

/**
 * Is there a Level Control that is NOT a speaker's?
 *
 * A composed device can have both — a television with a backlight would — so this asks
 * whether any endpoint carries the cluster without claiming to be a speaker, rather than
 * treating the two as alternatives.
 */
export function levelIsBrightness(node: NodeSnapshot): boolean {
  return applicationEndpoints(node).some(
    e => CLUSTER_LEVEL_CONTROL in e.clusters && !e.deviceTypes.includes(SPEAKER_DEVICE_TYPE),
  );
}

function capabilitiesOf(node: NodeSnapshot): string[] {
  const capabilities: string[] = [];
  const hasOnOff = hasCluster(node, CLUSTER_ON_OFF);

  if (hasOnOff) capabilities.push("power");
  if (hasCluster(node, CLUSTER_FAN_CONTROL)) {
    // A Matter fan need not implement On/Off at all — the Matter Virtual Device's fan
    // does not — so without this it advertised no capabilities and "turn on the fan"
    // had nothing to aim at. FanMode is its power switch.
    if (!hasOnOff) capabilities.push("power");
    capabilities.push("fan_speed");
  }
  // Volume where the level belongs to a speaker, brightness where it does not.
  const speaker = speakerEndpoint(node);
  if (speaker !== undefined) capabilities.push("volume");
  if (levelIsBrightness(node)) capabilities.push("brightness");
  // Either source of a temperature target. Gating on the thermostat alone listed a
  // dishwasher as "power, mode, operation" while `describe` offered it 49 to 82
  // degrees -- and the listing is what a model reads before deciding whether to ask
  // for the description at all, so the fuller answer was never reached.
  if (hasCluster(node, CLUSTER_THERMOSTAT) || applianceSetpoint(node) !== undefined) {
    capabilities.push("temperature");
  }
  if (hasCluster(node, CLUSTER_DOOR_LOCK)) capabilities.push("lock");
  // A covering was listed with no capabilities at all while `describe` offered it a
  // position, so the short answer said a controllable device could not be driven.
  if (hasCluster(node, CLUSTER_WINDOW_COVERING)) {
    capabilities.push("position");
    // Only a covering with slats to turn.
    const tilting = endpointWith(node, CLUSTER_WINDOW_COVERING)?.clusters[CLUSTER_WINDOW_COVERING]?.[
      "currentPositionTiltPercent100ths"
    ];
    if (tilting !== undefined) capabilities.push("tilt");
  }

  // Colour, absent from this list entirely until now: `describe` offered a colour bulb
  // hue and saturation while `list_registered_devices` said "power, brightness", and the
  // short list is what the model reads before deciding whether to look closer. Split the
  // same way `describe` splits it, and gated on the same claim, so the two cannot
  // disagree about what a tunable-white bulb can do.
  if (hasCluster(node, CLUSTER_COLOR_CONTROL)) {
    const colour = colorSupport(node);
    if (colour.hueSaturation) capabilities.push("color");
    if (colour.temperature) capabilities.push("color_temp");
  }

  // Appliance vocabulary, found the same structural way `settingsOf` finds it rather
  // than from a second list that could disagree with the first. Without these a
  // washer was listed as "capabilities: power", which is what the model reads before
  // it decides whether to look closer -- so it never looked.
  if (settingsOf(node).length > 0) capabilities.push("mode");
  if (operationsOf(node) !== undefined) capabilities.push("operation");

  return capabilities;
}

function typeOf(node: NodeSnapshot): string {
  // The node's own word first. Clusters can only say what is drivable, which is why
  // every On/Off appliance used to arrive as a light.
  const stated = deviceTypeFromDescriptor(node);
  if (stated !== undefined) return stated;

  if (hasCluster(node, CLUSTER_DOOR_LOCK)) return "lock";
  if (hasCluster(node, CLUSTER_THERMOSTAT)) return "thermostat";
  // Ahead of the On/Off check: a fan that does implement On/Off is still a fan, and
  // calling it a light gives the model the wrong vocabulary.
  if (hasCluster(node, CLUSTER_FAN_CONTROL)) return "fan";
  if (hasCluster(node, CLUSTER_ON_OFF)) return "light";
  if (
    hasCluster(node, CLUSTER_OCCUPANCY) ||
    hasCluster(node, CLUSTER_BOOLEAN_STATE) ||
    hasCluster(node, CLUSTER_TEMPERATURE) ||
    hasCluster(node, CLUSTER_HUMIDITY)
  ) {
    return "sensor";
  }
  if (hasCluster(node, CLUSTER_SWITCH)) return "switch";
  // A cluster GIAP does not map leaves the device typed `matter` with no
  // capabilities: it appears in the device list, but the model has nothing it can do
  // with it. That is the signal a mapping is missing, not that the device is broken.
  return "matter";
}

function basicInfo(node: NodeSnapshot, attribute: string): string | undefined {
  const value = rootAttribute(node, CLUSTER_BASIC_INFORMATION, attribute);
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

/**
 * Naming follows what production controllers do — take the device's own identity,
 * best source first:
 *   1. Basic Information `nodeLabel` — the user-assigned name;
 *   2. Basic Information `productName` — the vendor's ("Hue color lamp");
 *   3. `"<Type> <node_id>"` (e.g. "Light 2") — a clean, speakable fallback.
 */
export function nodeToDevice(node: NodeSnapshot): Device {
  const device_type = typeOf(node);
  const name =
    basicInfo(node, "nodeLabel") ??
    basicInfo(node, "productName") ??
    `${device_type.charAt(0).toUpperCase()}${device_type.slice(1)} ${node.nodeId.toString()}`;

  return {
    id: deviceIdForNode(node.nodeId),
    name,
    device_type,
    capabilities: capabilitiesOf(node),
    online: node.online,
  };
}
