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
import { clusterHasFeature } from "./sensors.js";
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
/** A valve: open, shut, and -- where it says so -- how far. */
export const CLUSTER_VALVE = "valveConfigurationAndControl";

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
    CLUSTER_VALVE,
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
  // The in-wall equivalents of the two above: a module behind a faceplate switching
  // or dimming whatever is wired to it. A plug rather than a light, because what it
  // drives is the installer's business and not something Matter states.
  [0x010f, "plug"], // Mounted On/Off Control
  [0x0110, "plug"], // Mounted Dimmable Load Control
  // Closures
  [0x000a, "lock"], // Door Lock
  [0x0202, "covering"], // Window Covering
  // Climate and air
  [0x0301, "thermostat"], // Thermostat
  [0x0072, "thermostat"], // Room Air Conditioner
  [0x002b, "fan"], // Fan
  [0x002c, "air"], // Air Purifier
  // Both carry a Thermostat and are driven by asking for a temperature, which is what
  // the word has to convey — a water heater's own mode cluster is read by the ModeBase
  // rule and needs no entry of its own.
  [0x0309, "thermostat"], // Heat Pump
  [0x050f, "thermostat"], // Water Heater
  // Sensors
  [0x0015, "sensor"], // Contact Sensor
  [0x002d, "sensor"], // Air Quality Sensor
  [0x0106, "sensor"], // Light Sensor
  [0x0107, "sensor"], // Occupancy Sensor
  [0x0302, "sensor"], // Temperature Sensor
  [0x0305, "sensor"], // Pressure Sensor
  [0x0306, "sensor"], // Flow Sensor
  [0x0307, "sensor"], // Humidity Sensor
  // Boolean-state detectors. They report one bit and take no orders, so they read as
  // sensors rather than as alarms: nothing on them sounds.
  [0x0041, "sensor"], // Water Freeze Detector
  [0x0043, "sensor"], // Water Leak Detector
  [0x0044, "sensor"], // Rain Sensor
  // An alarm is not a sensor to a user: it is the thing that wakes them.
  [0x0076, "alarm"], // Smoke/CO Alarm
  // A switch reports which way it is thrown and takes no orders, so it is its own
  // type rather than a light with the controls missing.
  //
  // Deliberately NOT the CLIENT device types, and the omission is the whole family
  // rather than an oversight in it: 0x0103 On/Off Light Switch, 0x0104 Dimmer Switch,
  // 0x0105 Colour Dimmer Switch, 0x000b Door Lock Controller, 0x0203 Window Covering
  // Controller, 0x0304 Pump Controller, 0x030a Thermostat Controller, 0x0840 Control
  // Bridge, 0x0850 On/Off Sensor, 0x0029 Casting Video Client, 0x002a Video Remote
  // Control. Every one of those is a remote: it binds to another device and issues
  // commands, and holds no server cluster GIAP could read or drive. Typing them would
  // put a row on the wall that answers "cannot be controlled" for every verb -- and
  // claiming a mapping nobody has held a device against is how a plug arrived wearing
  // a lightbulb.
  [0x000f, "switch"], // Generic Switch
  // A hub that speaks for other devices. Not drivable itself, and deliberately a
  // device anyway: it is the physical thing on the shelf, it owns the fabric
  // membership, and it is the only thing `decommission` can act on.
  [0x000e, "bridge"], // Aggregator
  // Appliances
  [0x0073, "appliance"], // Laundry Washer
  [0x0075, "appliance"], // Dishwasher
  [0x007c, "appliance"], // Laundry Dryer
  [0x0079, "appliance"], // Microwave Oven
  [0x0078, "appliance"], // Cooktop
  // Composed appliances, whose own endpoint carries little or nothing: an oven's
  // function lives in its cabinets, a refrigerator's in its compartments. The type is
  // still worth stating, because the alternative is the node arriving as whatever its
  // first cabinet claims -- or, with nothing to claim, as the monitor fallback.
  [0x007b, "appliance"], // Oven
  [0x0070, "appliance"], // Refrigerator
  // And the parts themselves, for the same reason in reverse: a cabinet or a hob ring
  // commissioned on its own is still an appliance, not an unknown.
  [0x0071, "appliance"], // Temperature Controlled Cabinet
  [0x0077, "appliance"], // Cook Surface
  // A cooker hood is a fan with a filter, and Fan Control is what it publishes.
  [0x007a, "fan"], // Extractor Hood
  [0x0074, "vacuum"], // Robotic Vacuum Cleaner
  [0x0303, "pump"], // Pump
  // A valve is not a plug with water in it: it takes `open` and `close` rather than
  // On/Off, and an irrigation system is one or several of them.
  [0x0042, "valve"], // Water Valve
  [0x0040, "valve"], // Irrigation System
  // Media
  [0x0023, "media"], // Casting Video Player
  [0x0028, "media"], // Basic Video Player
  // A speaker on its own, rather than as a video player's part. Its Level Control is
  // a volume either way -- `volumeEndpoint` already tells the two apart by this very
  // device type -- so the only thing missing was the word for it.
  [0x0022, "media"], // Speaker
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

/**
 * Does this valve say it has a level, rather than only open and shut?
 *
 * Valve Configuration and Control's LVL feature is optional. Claims first, evidence
 * second -- the same order `colorSupport` uses: the feature map is believed where it
 * speaks, and a device that publishes a `currentLevel` has one whatever it claims.
 */
export function valveHasLevel(node: NodeSnapshot): boolean {
  const state = endpointWith(node, CLUSTER_VALVE)?.clusters[CLUSTER_VALVE];
  if (state === undefined) return false;
  if (clusterHasFeature(state, "level")) return true;
  return state["currentLevel"] !== undefined || state["targetLevel"] !== undefined;
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
  if (hasCluster(node, CLUSTER_VALVE)) {
    capabilities.push("valve");
    // Only a valve that says it has a level. The LVL feature is optional and a plain
    // solenoid has none, so offering one is a control the device would reject -- the
    // same rule `tilt` follows for a roller blind with no slats.
    if (valveHasLevel(node)) capabilities.push("position");
  }
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

function nonEmpty(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function basicInfo(node: NodeSnapshot, attribute: string): string | undefined {
  return nonEmpty(rootAttribute(node, CLUSTER_BASIC_INFORMATION, attribute));
}

/** What a bridged device says about itself, which its hub relays on its behalf. */
function bridgedInfo(node: NodeSnapshot, attribute: string): string | undefined {
  if (node.rootEndpoint === undefined) return undefined;
  const own = node.endpoints.find(e => e.number === node.rootEndpoint);
  return nonEmpty(own?.clusters[CLUSTER_BRIDGED_DEVICE_INFO]?.[attribute]);
}

/**
 * Is this bridged device reachable, as its hub reports it?
 *
 * Fails OPEN: an unstated `reachable` means yes. `bridgedDeviceBasicInformation`
 * populates from a subscription report like everything else, so treating "not said
 * yet" as unreachable would show a dozen offline devices behind a perfectly healthy
 * hub for the first seconds of its life. Same rule, and the same reason, as
 * `clusterHasFeature`: keep the answer that works rather than withholding it.
 */
function bridgedReachable(node: NodeSnapshot): boolean {
  if (node.rootEndpoint === undefined) return true;
  const own = node.endpoints.find(e => e.number === node.rootEndpoint);
  return own?.clusters[CLUSTER_BRIDGED_DEVICE_INFO]?.["reachable"] !== false;
}

/**
 * Naming follows what production controllers do — take the device's own identity,
 * best source first. For a bridged device that identity is on its own endpoint,
 * relayed by the hub, and only then does the hub's own name apply:
 *   1. `bridgedDeviceBasicInformation` `nodeLabel`, `productName`, `vendorName`;
 *   2. Basic Information `nodeLabel` — the user-assigned name;
 *   3. Basic Information `productName` — the vendor's ("Hue color lamp");
 *   4. `"<Type> <node_id>"`, plus the endpoint for a bridged device.
 *
 * The fallback must be UNIQUE, which is why it carries the endpoint. Hubs that
 * report nothing about their children are ordinary, and twelve devices all named
 * "Light 90" would make `resolve_device` answer `Ambiguous` for every one of them —
 * and because its last tier matches on device type, "the light" would stop resolving
 * for standalone lights on entirely different nodes. A bridge would break devices it
 * has nothing to do with.
 */
export function nodeToDevice(node: NodeSnapshot): Device {
  const device_type = typeOf(node);
  const suffix =
    node.rootEndpoint === undefined
      ? node.nodeId.toString()
      : `${node.nodeId.toString()}-${node.rootEndpoint}`;
  const name =
    bridgedInfo(node, "nodeLabel") ??
    bridgedInfo(node, "productName") ??
    bridgedInfo(node, "vendorName") ??
    basicInfo(node, "nodeLabel") ??
    basicInfo(node, "productName") ??
    `${device_type.charAt(0).toUpperCase()}${device_type.slice(1)} ${suffix}`;

  return {
    id: deviceIdForNode(node.nodeId, node.rootEndpoint),
    name,
    device_type,
    capabilities: capabilitiesOf(node),
    // A bridged device is only as reachable as its hub, and its hub may know it is
    // not: a Zigbee bulb whose battery died is still behind a healthy bridge.
    online: node.online && bridgedReachable(node),
  };
}
