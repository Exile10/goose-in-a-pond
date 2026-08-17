/**
 * GIAP control verbs resolved onto Matter cluster actions.
 *
 * Pure: `planControl` decides the endpoint, the cluster, whether the change is a
 * command or an attribute write, and what the device state becomes — and returns that
 * as data. `controller.ts` is the only thing that touches the network, so every
 * decision below is unit-testable against a recorded device.
 */

import { OpError, type DeviceStatePatch, type Verb } from "../protocol.js";
import {
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_WINDOW_COVERING,
} from "./devices.js";
import { endpointWith, type NodeSnapshot } from "./snapshot.js";

// `Verb` is protocol vocabulary — it names what a `control` op may ask for — so it
// lives in protocol.ts and is re-exported here, where every caller already looks.
export type { Verb };

export const VERBS: ReadonlySet<string> = new Set<Verb>([
  "power",
  "brightness",
  "target_temp",
  "locked",
  "color",
  "fan_speed",
  "fan_mode",
  "position",
]);

/** What the server must actually do to the device. */
export type Action =
  | { kind: "command"; endpoint: number; cluster: string; command: string; payload: Record<string, unknown> }
  | { kind: "write"; endpoint: number; cluster: string; attribute: string; value: unknown };

export interface Plan {
  actions: Action[];
  /** The state the device is in once the actions succeed. */
  applied: DeviceStatePatch;
}

// ── Unit conversions ─────────────────────────────────────────────────────────

/** A 0-100 GIAP brightness percentage onto Matter's 0-254 level scale. */
export function brightnessToLevel(percent: number): number {
  const pct = clampPercent(percent);
  return Math.floor((pct * 254 + 50) / 100);
}

/** Celsius onto a Matter thermostat setpoint (hundredths of a degree). */
export function celsiusToSetpoint(celsius: number): number {
  return Math.min(32767, Math.max(-32768, Math.round(celsius * 100)));
}

/** A 0-360 degree hue onto ColorControl's 0-254 scale (360 wraps to 0, matching the
 *  circular hue space). */
export function hueToMatter(degrees: number): number {
  const wrapped = ((Math.round(degrees) % 360) + 360) % 360;
  return Math.floor((wrapped * 254 + 180) / 360);
}

/** A 0-100 saturation percentage onto Matter's 0-254 scale. */
export function saturationToMatter(percent: number): number {
  const pct = clampPercent(percent);
  return Math.floor((pct * 254 + 50) / 100);
}

/**
 * A GIAP covering position (0-100 percent OPEN) onto WindowCovering's lift value in
 * hundredths-of-a-percent CLOSED, which is what `GoToLiftPercentage` takes: 0 is fully
 * open, 10000 fully closed. GIAP speaks in percent open because that is how users
 * phrase it ("open the blinds 50%").
 */
export function positionOpenToLift100ths(percentOpen: number): number {
  return (100 - clampPercent(percentOpen)) * 100;
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, Math.round(value)));
}

// ── Fan modes ────────────────────────────────────────────────────────────────

export const FAN_MODE_OFF = 0;

/**
 * "Turn the fan on" writes **High**, not `FanMode.On`.
 *
 * `On` is 4, and it is the obvious choice until you read `FanModeSequence`: it was
 * deprecated in Matter 1.2 and appears in none of the sequences a current device
 * advertises (`OffLowMedHigh`, `OffLowHigh`, `OffLowMedHighAuto`, `OffLowHighAuto`,
 * `OffHighAuto`, `OffHigh`). Writing an unsupported mode is a write a conforming fan
 * may reject — so the fix for "turn on the fan does nothing" would have shipped still
 * not turning on the fan.
 *
 * High is the only non-Off value present in EVERY sequence, which is what makes it the
 * safe universal choice without reading `FanModeSequence` first. Reading that attribute
 * and picking the gentlest supported mode is the better behaviour and a bigger change;
 * it belongs with the `fan_mode` validation follow-up, which has the same gap.
 */
export const FAN_MODE_ON = 3;

/**
 * `FanMode` by the name a user says it. Auto and Smart are not points on the
 * percentage scale — they hand the choice back to the device — which is why a fan
 * needs modes as well as a speed.
 */
export function fanModeFromName(name: string): number | undefined {
  switch (name.trim().toLowerCase()) {
    case "off":
      return FAN_MODE_OFF;
    case "low":
      return 1;
    case "medium":
    case "med":
      return 2;
    case "high":
      return 3;
    case "on":
      return FAN_MODE_ON;
    case "auto":
      return 5;
    case "smart":
      return 6;
    default:
      return undefined;
  }
}

// ── Planning ─────────────────────────────────────────────────────────────────

function endpointFor(node: NodeSnapshot, cluster: string, deviceId: string): number {
  const endpoint = endpointWith(node, cluster);
  if (endpoint === undefined) {
    throw new OpError(
      "capability_unsupported",
      `Matter device '${deviceId}' does not support this capability`,
    );
  }
  return endpoint.number;
}

function asPercent(value: unknown, verb: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new OpError("bad_request", `${verb} needs a number from 0 to 100`);
  }
  return clampPercent(value);
}

function asBoolean(value: unknown, verb: string): boolean {
  if (typeof value !== "boolean") {
    throw new OpError("bad_request", `${verb} needs true or false`);
  }
  return value;
}

/**
 * Resolve `verb` against what `node` actually exposes.
 *
 * @throws {OpError} `capability_unsupported` when the node has no cluster for the
 * verb, `bad_request` when the value is the wrong shape.
 */
export function planControl(
  node: NodeSnapshot,
  deviceId: string,
  verb: Verb,
  value: unknown,
): Plan {
  switch (verb) {
    case "power": {
      const on = asBoolean(value, "power");
      const onOff = endpointWith(node, CLUSTER_ON_OFF);
      if (onOff !== undefined) {
        return {
          actions: [
            { kind: "command", endpoint: onOff.number, cluster: CLUSTER_ON_OFF, command: on ? "on" : "off", payload: {} },
          ],
          applied: { on },
        };
      }
      // A fan's power is FanMode, written rather than commanded. Most fans (the
      // Matter Virtual Device's included) implement no On/Off cluster at all, so
      // without this branch "turn on the fan" could only fail.
      const fan = endpointWith(node, CLUSTER_FAN_CONTROL);
      if (fan !== undefined) {
        return {
          actions: [
            { kind: "write", endpoint: fan.number, cluster: CLUSTER_FAN_CONTROL, attribute: "fanMode", value: on ? FAN_MODE_ON : FAN_MODE_OFF },
          ],
          applied: { on },
        };
      }
      throw new OpError(
        "capability_unsupported",
        `Matter device '${deviceId}' cannot be switched on or off`,
      );
    }

    case "brightness": {
      const pct = asPercent(value, "brightness");
      const endpoint = endpointFor(node, CLUSTER_LEVEL_CONTROL, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_LEVEL_CONTROL,
            command: "moveToLevelWithOnOff",
            payload: { level: brightnessToLevel(pct), transitionTime: 0, optionsMask: {}, optionsOverride: {} },
          },
        ],
        applied: { brightness: pct, on: pct > 0 },
      };
    }

    case "target_temp": {
      if (typeof value !== "number" || !Number.isFinite(value)) {
        throw new OpError("bad_request", "target_temp needs a temperature in Celsius");
      }
      const endpoint = endpointFor(node, CLUSTER_THERMOSTAT, deviceId);
      // Setpoints are attribute writes, not commands.
      return {
        actions: [
          { kind: "write", endpoint, cluster: CLUSTER_THERMOSTAT, attribute: "occupiedHeatingSetpoint", value: celsiusToSetpoint(value) },
        ],
        applied: { target_temp: value },
      };
    }

    case "locked": {
      const locked = asBoolean(value, "locked");
      const endpoint = endpointFor(node, CLUSTER_DOOR_LOCK, deviceId);
      return {
        actions: [
          { kind: "command", endpoint, cluster: CLUSTER_DOOR_LOCK, command: locked ? "lockDoor" : "unlockDoor", payload: {} },
        ],
        applied: { locked },
      };
    }

    case "color": {
      const { hue, saturation } = readColor(value);
      const endpoint = endpointFor(node, CLUSTER_COLOR_CONTROL, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_COLOR_CONTROL,
            command: "moveToHueAndSaturation",
            payload: {
              hue: hueToMatter(hue),
              saturation: saturationToMatter(saturation),
              transitionTime: 0,
              optionsMask: {},
              optionsOverride: {},
            },
          },
        ],
        applied: { hue: ((Math.round(hue) % 360) + 360) % 360, saturation },
      };
    }

    case "fan_speed": {
      const pct = asPercent(value, "fan_speed");
      const endpoint = endpointFor(node, CLUSTER_FAN_CONTROL, deviceId);
      // Fan speed is the `percentSetting` attribute (0-100), not a command.
      return {
        actions: [
          { kind: "write", endpoint, cluster: CLUSTER_FAN_CONTROL, attribute: "percentSetting", value: pct },
        ],
        applied: { fan_speed: pct, on: pct > 0 },
      };
    }

    case "fan_mode": {
      if (typeof value !== "string") {
        throw new OpError("bad_request", "fan_mode needs a mode name");
      }
      const code = fanModeFromName(value);
      if (code === undefined) {
        throw new OpError(
          "bad_request",
          `'${value}' is not a fan mode — use off, low, medium, high, on, auto or smart`,
        );
      }
      const endpoint = endpointFor(node, CLUSTER_FAN_CONTROL, deviceId);
      return {
        actions: [
          { kind: "write", endpoint, cluster: CLUSTER_FAN_CONTROL, attribute: "fanMode", value: code },
        ],
        // Off is the one mode that says something definite about power.
        applied: { fan_mode: value.trim().toLowerCase(), on: code !== FAN_MODE_OFF },
      };
    }

    case "position": {
      const pct = asPercent(value, "position");
      const endpoint = endpointFor(node, CLUSTER_WINDOW_COVERING, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_WINDOW_COVERING,
            command: "goToLiftPercentage",
            payload: { liftPercent100thsValue: positionOpenToLift100ths(pct) },
          },
        ],
        applied: { position: pct },
      };
    }
  }
}

function readColor(value: unknown): { hue: number; saturation: number } {
  if (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { hue?: unknown }).hue === "number" &&
    typeof (value as { saturation?: unknown }).saturation === "number"
  ) {
    const color = value as { hue: number; saturation: number };
    return { hue: color.hue, saturation: clampPercent(color.saturation) };
  }
  throw new OpError("bad_request", "color needs { hue, saturation }");
}
