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
  levelIsBrightness,
  speakerEndpoint,
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_WINDOW_COVERING,
} from "./devices.js";
import { operationsOf, settingNamed, settingsOf } from "./settings.js";
import { endpointWith, type NodeSnapshot } from "./snapshot.js";
import { applianceSetpoint, targetSetpoint } from "./thermostat.js";

// `Verb` is protocol vocabulary — it names what a `control` op may ask for — so it
// lives in protocol.ts and is re-exported here, where every caller already looks.
export type { Verb };

/**
 * Every verb, as a record rather than a list — so the COMPILER enforces coverage.
 *
 * `new Set<Verb>([...])` type-checks happily while missing an entry, and it did: adding
 * `color_temp` to the union left it out of this set, and `server.ts` rejects any verb the
 * set does not hold. The control was unreachable at the wire boundary while every unit
 * test passed, because the tests call `planControl` directly and never cross it.
 *
 * A `Record<Verb, true>` cannot be missing a key. Add a verb to the union without adding
 * it here and the build fails, which is the only guard that survives someone in a hurry.
 */
const ALL_VERBS: Record<Verb, true> = {
  power: true,
  brightness: true,
  volume: true,
  target_temp: true,
  locked: true,
  color: true,
  color_temp: true,
  fan_speed: true,
  fan_mode: true,
  position: true,
  tilt: true,
  mode: true,
  operation: true,
};

export const VERBS: ReadonlySet<string> = new Set(Object.keys(ALL_VERBS));

/** What the server must actually do to the device. */
export type Action =
  | { kind: "command"; endpoint: number; cluster: string; command: string; payload: Record<string, unknown> }
  | { kind: "write"; endpoint: number; cluster: string; attribute: string; value: unknown };

export interface Plan {
  actions: Action[];
  /** The state the device is in once the actions succeed. */
  applied: DeviceStatePatch;
}

/**
 * What the device NOW reports for a verb, in the verb's own units.
 *
 * `applied` is supposed to be what the device did rather than what it was asked for —
 * the protocol doc says so, and it is the reason the field exists at all. Until now only
 * `operation` honoured it: every other verb echoed the request back, so a fan told to run
 * at 85% reported 85% while the device had quantised it to its High mode and was sitting
 * at 90. The number the user reads was the number they typed, which makes it worthless
 * for noticing that anything happened at all.
 *
 * These are the verbs whose result can differ from the request: a value quantised onto a
 * cluster's own scale, clamped to a device's stated limits, or still travelling. Power
 * and lock are absent deliberately — a boolean cannot land somewhere else, so making
 * them wait for a report would be latency bought for nothing.
 *
 * Read through the same inverses `state` reads with, so the number reported here is the
 * number that would put the device back where it is.
 */
export function observedFor(node: NodeSnapshot, verb: Verb): DeviceStatePatch {
  const at = (cluster: string, attribute: string): number | undefined => {
    const raw = endpointWith(node, cluster)?.clusters[cluster]?.[attribute];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : undefined;
  };

  switch (verb) {
    case "fan_speed": {
      // percentCURRENT, not percentSetting: the setting is what was written, the current
      // is what the fan is doing. Reading the setting back would echo the request with
      // extra steps.
      const pct = at(CLUSTER_FAN_CONTROL, "percentCurrent");
      return pct === undefined ? {} : { fan_speed: clampPercent(pct) };
    }
    case "brightness": {
      // Mirrors the split in `describe`: a television's Level Control belongs to its
      // speaker, and reading it back as a brightness would report the volume under the
      // wrong name -- the same confusion at the other end of the same command.
      if (!levelIsBrightness(node)) return {};
      const level = at(CLUSTER_LEVEL_CONTROL, "currentLevel");
      return level === undefined ? {} : { brightness: levelToBrightness(level) };
    }
    case "volume": {
      const speaker = speakerEndpoint(node);
      const level = speaker?.clusters[CLUSTER_LEVEL_CONTROL]?.["currentLevel"];
      return typeof level === "number" ? { volume: levelToBrightness(level) } : {};
    }
    case "color": {
      const hue = at(CLUSTER_COLOR_CONTROL, "currentHue");
      const saturation = at(CLUSTER_COLOR_CONTROL, "currentSaturation");
      if (hue === undefined || saturation === undefined) return {};
      return { hue: matterToHue(hue), saturation: matterToSaturation(saturation) };
    }
    case "color_temp": {
      const mireds = at(CLUSTER_COLOR_CONTROL, "colorTemperatureMireds");
      if (mireds === undefined) return {};
      const kelvin = miredsToKelvin(mireds);
      return kelvin > 0 ? { color_temp: kelvin } : {};
    }
    case "target_temp": {
      // Whichever setpoint is live, by the same rule `target_temp` writes with: an
      // appliance's own, or the thermostat setpoint its mode has running.
      if (applianceSetpoint(node) !== undefined) {
        const set = at("temperatureControl", "temperatureSetpoint");
        return set === undefined ? {} : { target_temp: setpointToCelsius(set) };
      }
      const target = targetSetpoint(node);
      const setpoint = target === undefined ? undefined : at(CLUSTER_THERMOSTAT, target.attribute);
      return setpoint === undefined ? {} : { target_temp: setpointToCelsius(setpoint) };
    }
    case "position": {
      const lift = at(CLUSTER_WINDOW_COVERING, "currentPositionLiftPercent100ths");
      return lift === undefined ? {} : { position: lift100thsToPositionOpen(lift) };
    }
    case "tilt": {
      const tilt = at(CLUSTER_WINDOW_COVERING, "currentPositionTiltPercent100ths");
      return tilt === undefined ? {} : { tilt: lift100thsToPositionOpen(tilt) };
    }
    default:
      // power, locked, fan_mode, mode, operation. `operation` has its own settle path
      // in the controller; the rest cannot land on a value other than the one asked for.
      return {};
  }
}

// ── Unit conversions ─────────────────────────────────────────────────────────

/** A 0-100 GIAP brightness percentage onto Matter's 0-254 level scale. */
export function brightnessToLevel(percent: number): number {
  const pct = clampPercent(percent);
  return Math.floor((pct * 254 + 50) / 100);
}

/** Matter's 0-254 level back to a 0-100 GIAP percentage, for reading state. */
export function levelToBrightness(level: number): number {
  return clampPercent((level * 100) / 254);
}

/**
 * Kelvin onto ColorControl's mireds, and back.
 *
 * Mireds are reciprocal megakelvin — 1e6/K — so the mapping is its own inverse and the
 * ORDER INVERTS: fewer mireds is a hotter, bluer white. Kelvin is what a person says
 * ("2700K", "warm white") and mireds is what the cluster takes, which is the whole
 * reason this conversion exists rather than the wire carrying mireds.
 *
 * Clamped to the cluster's own field range (1..0xfeff). Zero mireds is not a colour and
 * would divide to infinity; the spec's own defaults include it, so it has to be handled
 * rather than assumed away.
 */
export function kelvinToMireds(kelvin: number): number {
  if (!Number.isFinite(kelvin) || kelvin <= 0) return 0xfeff;
  return Math.min(0xfeff, Math.max(1, Math.round(1_000_000 / kelvin)));
}

/** Mireds back to kelvin, rounded to a whole degree — no device is that precise. */
export function miredsToKelvin(mireds: number): number {
  if (!Number.isFinite(mireds) || mireds <= 0) return 0;
  return Math.round(1_000_000 / mireds);
}

/** Celsius onto a Matter thermostat setpoint (hundredths of a degree). */
export function celsiusToSetpoint(celsius: number): number {
  return Math.min(32767, Math.max(-32768, Math.round(celsius * 100)));
}

/** A Matter thermostat setpoint back to Celsius. */
export function setpointToCelsius(setpoint: number): number {
  return Math.round(setpoint) / 100;
}

/** A 0-360 degree hue onto ColorControl's 0-254 scale (360 wraps to 0, matching the
 *  circular hue space). */
export function hueToMatter(degrees: number): number {
  const wrapped = ((Math.round(degrees) % 360) + 360) % 360;
  return Math.floor((wrapped * 254 + 180) / 360);
}

/** ColorControl's 0-254 hue back to degrees, for reading state. */
export function matterToHue(raw: number): number {
  const clamped = Math.min(254, Math.max(0, Math.round(raw)));
  return Math.round((clamped * 360) / 254) % 360;
}

/** A 0-100 saturation percentage onto Matter's 0-254 scale. */
export function saturationToMatter(percent: number): number {
  const pct = clampPercent(percent);
  return Math.floor((pct * 254 + 50) / 100);
}

/** Matter's 0-254 saturation back to a percentage. */
export function matterToSaturation(raw: number): number {
  return clampPercent((Math.min(254, Math.max(0, raw)) * 100) / 254);
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

/** WindowCovering's hundredths-of-a-percent CLOSED back to GIAP percent OPEN. */
export function lift100thsToPositionOpen(lift100ths: number): number {
  return clampPercent(100 - lift100ths / 100);
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, Math.round(value)));
}

// ── Fan modes ────────────────────────────────────────────────────────────────

const CLUSTER_TEMPERATURE_CONTROL = "temperatureControl";

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

/** `FanMode` back to the name it is sent by, for reading state. */
export function fanModeName(code: number): string | undefined {
  switch (code) {
    case FAN_MODE_OFF:
      return "off";
    case 1:
      return "low";
    case 2:
      return "medium";
    case 3:
      return "high";
    case 4:
      return "on";
    case 5:
      return "auto";
    case 6:
      return "smart";
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

    case "volume": {
      const pct = asPercent(value, "volume");
      const speaker = speakerEndpoint(node);
      if (speaker === undefined) {
        throw new OpError(
          "capability_unsupported",
          `Matter device '${deviceId}' has no speaker to set a volume on`,
        );
      }
      // The speaker's own Level Control, on the speaker's own endpoint. Written the same
      // way brightness is because it is the same cluster and the same 0-254 scale -- what
      // differs is whose level it is, and that is settled by the endpoint.
      return {
        actions: [
          {
            kind: "write",
            endpoint: speaker.number,
            cluster: CLUSTER_LEVEL_CONTROL,
            attribute: "currentLevel",
            value: brightnessToLevel(pct),
          },
        ],
        applied: { volume: pct },
      };
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
      // Which setpoint depends on what the thermostat is doing: writing the
      // heating one to a device that is cooling moves a number nobody asked about
      // and leaves the cooling unchanged.
      // An appliance keeps its target in Temperature Control and takes it by
      // command, not by writing an attribute.
      const appliance = applianceSetpoint(node);
      if (appliance !== undefined) {
        return {
          actions: [
            {
              kind: "command",
              endpoint: appliance.endpoint,
              cluster: CLUSTER_TEMPERATURE_CONTROL,
              command: "setTemperature",
              payload: { targetTemperature: celsiusToSetpoint(value) },
            },
          ],
          applied: { target_temp: value },
        };
      }

      const setpoint = targetSetpoint(node, value);
      if (setpoint === undefined) {
        throw new OpError(
          "capability_unsupported",
          `Matter device '${deviceId}' does not support this capability`,
        );
      }
      // Setpoints are attribute writes, not commands.
      return {
        actions: [
          { kind: "write", endpoint: setpoint.endpoint, cluster: CLUSTER_THERMOSTAT, attribute: setpoint.attribute, value: celsiusToSetpoint(value) },
        ],
        applied: { target_temp: value },
      };
    }

    case "tilt": {
      // A covering's second axis: how far the slats are turned, independent of how
      // far the blind is raised. A venetian blind is routinely down with its slats
      // open, which `position` alone cannot ask for.
      //
      // Same convention as lift, and the spec is explicit about it: zero is treated
      // as UpOrOpen. So GIAP speaks percent OPEN here too, and the same conversion
      // serves both.
      const pct = asPercent(value, "tilt");
      const endpoint = endpointFor(node, CLUSTER_WINDOW_COVERING, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_WINDOW_COVERING,
            command: "goToTiltPercentage",
            payload: { tiltPercent100thsValue: positionOpenToLift100ths(pct) },
          },
        ],
        applied: { tilt: pct },
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

    case "color_temp": {
      if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
        throw new OpError(
          "bad_request",
          "color_temp needs a colour temperature in kelvin, e.g. 2700 for warm white",
        );
      }
      const kelvin = value;
      const endpoint = endpointFor(node, CLUSTER_COLOR_CONTROL, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_COLOR_CONTROL,
            command: "moveToColorTemperature",
            payload: {
              colorTemperatureMireds: kelvinToMireds(kelvin),
              transitionTime: 0,
              optionsMask: {},
              optionsOverride: {},
            },
          },
        ],
        // Reported as the kelvin the device will actually sit at, not the kelvin that
        // was asked for: the round trip through mireds is lossy at whole-mired
        // granularity, and echoing the request would overstate the precision by a few
        // degrees at the warm end and rather more at the cool one.
        applied: { color_temp: miredsToKelvin(kelvinToMireds(kelvin)) },
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

    case "mode": {
      const { setting: wanted, value: choice } = readModeRequest(value);
      const setting = settingNamed(node, wanted);
      if (setting === undefined) {
        const available = settingsOf(node).map(s => s.name);
        throw new OpError(
          "capability_unsupported",
          available.length === 0
            ? `Matter device '${deviceId}' has no settings that can be chosen`
            : `'${wanted}' is not a setting on '${deviceId}' — it has: ${available.join(", ")}`,
        );
      }

      // The device published these labels; anything else was never on offer, and
      // guessing at the nearest one is how a wash ends up on the wrong cycle.
      const encoded = setting.valueFor(choice);
      if (encoded === undefined) {
        throw new OpError(
          "bad_request",
          `'${choice}' is not a ${setting.name} on '${deviceId}' — it accepts: ${setting.values.join(", ")}`,
        );
      }

      const action: Action =
        setting.write.kind === "command"
          ? {
              kind: "command",
              endpoint: setting.endpoint,
              cluster: setting.cluster,
              command: setting.write.command,
              payload: { [setting.write.field]: encoded },
            }
          : {
              kind: "write",
              endpoint: setting.endpoint,
              cluster: setting.cluster,
              attribute: setting.write.attribute,
              value: encoded,
            };

      return {
        actions: [action],
        // Reported with the label the device uses, not the one the user typed.
        applied: {
          mode: {
            setting: setting.name,
            value: setting.values.find(v => v.toLowerCase() === choice.trim().toLowerCase()) ?? choice,
          },
        },
      };
    }

    case "operation": {
      const wanted = asString(value, "operation").toLowerCase();
      const operations = operationsOf(node);
      if (operations === undefined) {
        throw new OpError(
          "capability_unsupported",
          `Matter device '${deviceId}' does not run cycles, so it cannot be started or stopped`,
        );
      }
      if (!operations.values.includes(wanted)) {
        throw new OpError(
          "bad_request",
          `'${wanted}' is not an operation — use ${operations.values.join(", ")}`,
        );
      }
      return {
        actions: [
          {
            kind: "command",
            endpoint: operations.endpoint,
            cluster: operations.cluster,
            command: wanted,
            payload: {},
          },
        ],
        applied: { operation: wanted },
      };
    }
  }
}

/** A `mode` request names the setting and the choice, both as the device words them. */
function readModeRequest(value: unknown): { setting: string; value: string } {
  if (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { setting?: unknown }).setting === "string" &&
    typeof (value as { value?: unknown }).value === "string"
  ) {
    const request = value as { setting: string; value: string };
    return { setting: request.setting.trim(), value: request.value.trim() };
  }
  throw new OpError(
    "bad_request",
    "a mode needs both the setting and the value, as {setting, value}",
  );
}

function asString(value: unknown, verb: string): string {
  if (typeof value !== "string") {
    throw new OpError("bad_request", `${verb} takes a name, not ${typeof value}`);
  }
  return value.trim();
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
