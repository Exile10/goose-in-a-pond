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
  hasCluster,
  rootAttribute,
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
  if (hasCluster(node, CLUSTER_LEVEL_CONTROL)) capabilities.push("brightness");
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
  if (hasCluster(node, CLUSTER_WINDOW_COVERING)) capabilities.push("position");

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
