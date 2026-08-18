/**
 * What a device currently is, in the same words as what it can be told to do.
 *
 * `describe` answers "what can this device do"; nothing answered "what is it doing".
 * A model asked whether the washer was running had to drive it to find out, and
 * whether a light was on was equally unanswerable — the state was in the controller
 * the whole time, with no way to ask for it.
 *
 * Every name here is one `describe` also uses: the control verb for a scalar
 * ("power", "brightness"), the setting name for a selectable ("spin speed"). So a
 * reading names the thing that changes it, and a reader can go from "spin speed is
 * Low" to the call that makes it High without a second lookup.
 *
 * Values are read through the inverses of the conversions `control` writes with, so
 * a position reported as 40% open is the same 40% that put it there. Anything the
 * device does not report is absent rather than guessed: "unknown" invented here is
 * indistinguishable from a real reading further up.
 */

import type { DeviceState, StateValue } from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  fanModeName,
  levelToBrightness,
  lift100thsToPositionOpen,
  setpointToCelsius,
} from "./control.js";
import {
  CLUSTER_DOOR_LOCK,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_WINDOW_COVERING,
} from "./devices.js";
import { observedOperation, settingsOf } from "./settings.js";
import { targetSetpoint } from "./thermostat.js";
import { endpointWith, type NodeSnapshot } from "./snapshot.js";

/** DoorLock's `lockState`: 0 is not-fully-locked, which is neither of the two. */
const LOCK_STATES: Record<number, string> = {
  0: "jammed between locked and unlocked",
  1: "locked",
  2: "unlocked",
};

function numberAt(node: NodeSnapshot, cluster: string, attribute: string): number | undefined {
  const value = endpointWith(node, cluster)?.clusters[cluster]?.[attribute];
  return typeof value === "number" ? value : undefined;
}

/** Everything this device currently reports, in the order a person would ask. */
export function stateOf(node: NodeSnapshot): DeviceState {
  const values: StateValue[] = [];
  const add = (name: string, value: string | undefined) => {
    if (value !== undefined) values.push({ name, value });
  };

  const on = endpointWith(node, CLUSTER_ON_OFF)?.clusters[CLUSTER_ON_OFF]?.["onOff"];
  if (typeof on === "boolean") add("power", on ? "on" : "off");

  const level = numberAt(node, CLUSTER_LEVEL_CONTROL, "currentLevel");
  if (level !== undefined) add("brightness", `${levelToBrightness(level)}%`);

  // The setpoint `target_temp` would write, which is the one the mode has live.
  // Reporting the heating one to a cooling thermostat describes a number that is
  // not currently steering anything.
  const target = targetSetpoint(node);
  const setpoint = target === undefined ? undefined : numberAt(node, CLUSTER_THERMOSTAT, target.attribute);
  if (setpoint !== undefined) add("target_temp", `${setpointToCelsius(setpoint)} C`);

  const lock = numberAt(node, CLUSTER_DOOR_LOCK, "lockState");
  if (lock !== undefined) add("locked", LOCK_STATES[lock]);

  const speed = numberAt(node, CLUSTER_FAN_CONTROL, "percentCurrent");
  if (speed !== undefined) add("fan_speed", `${speed}%`);
  const fanMode = numberAt(node, CLUSTER_FAN_CONTROL, "fanMode");
  if (fanMode !== undefined) add("fan_mode", fanModeName(fanMode));

  // Reported as percent open, matching how `position` is written and how people say
  // it, rather than WindowCovering's percent closed.
  const lift = numberAt(node, CLUSTER_WINDOW_COVERING, "currentPositionLiftPercent100ths");
  if (lift !== undefined) add("position", `${lift100thsToPositionOpen(lift)}% open`);

  // Selectable settings, named exactly as `describe` names them and as `control`
  // takes them, each carrying the label the device chose for its current value.
  for (const setting of settingsOf(node)) {
    add(setting.name, currentLabel(node, setting));
  }

  add("operation", observedOperation(node));

  return { device_id: deviceIdForNode(node.nodeId), values };
}

/** The label a setting is currently on, as the device words it. */
function currentLabel(
  node: NodeSnapshot,
  setting: ReturnType<typeof settingsOf>[number],
): string | undefined {
  const state = node.endpoints.find(e => e.number === setting.endpoint)?.clusters[setting.cluster];
  if (state === undefined) return undefined;

  // Where the value lives depends on how it is written: ModeBase keeps the choice in
  // `currentMode` and matches it against the codes the device published, while the
  // attribute-written ones are an index into the labels themselves.
  const current =
    setting.write.kind === "command"
      ? state["currentMode"]
      : state[setting.write.attribute];
  if (typeof current !== "number") return undefined;

  // ModeBase codes need not be positions in the list, so ask the setting which label
  // carries this code rather than indexing into it.
  return setting.values.find(label => setting.valueFor(label) === current);
}
