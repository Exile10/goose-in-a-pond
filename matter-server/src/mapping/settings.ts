/**
 * Selectable settings and operational commands — the vocabulary appliances speak.
 *
 * A washer has a wash mode, a spin speed, a rinse count and a temperature level; a
 * dishwasher and a robot vacuum have their own. Adding a verb per appliance would
 * mean twenty-eight of them, each obsolete the moment a device offers something
 * slightly different.
 *
 * Matter already solved this: most appliance controls are ModeBase derivatives,
 * which all publish `supportedModes` — a list of `{label, mode}` the device chose —
 * and a writable `currentMode`. So settings are found **structurally**, by shape
 * rather than by a list of cluster names, and a cluster nobody has heard of works
 * the day a device ships it.
 *
 * The two that are not ModeBase are handled explicitly because their shape differs,
 * not because they are special: Temperature Control names its levels in a parallel
 * array, and Laundry Washer Controls keeps spin speeds and rinses side by side.
 */

import type { EndpointSnapshot, NodeSnapshot } from "./snapshot.js";
import { applicationEndpoints } from "./snapshot.js";

/** How a chosen value reaches the device. */
export type SettingWrite =
  | { kind: "command"; command: string; field: string }
  | { kind: "attribute"; attribute: string };

/**
 * One thing a user can choose on a device, in the device's own words.
 *
 * `values` are the labels the device published, so "Heavy" is offered because the
 * washer said "Heavy" — not because GIAP has a list of wash cycles.
 */
export interface Setting {
  /** What to call it in a sentence: "laundry washer mode", "spin speed". */
  name: string;
  endpoint: number;
  cluster: string;
  values: string[];
  write: SettingWrite;
  /** The number to send for a label, or undefined if the device never offered it. */
  valueFor: (choice: string) => number | undefined;
}

/** Operational State's commands, which every appliance that runs a cycle shares. */
export interface Operations {
  endpoint: number;
  cluster: string;
  /** Commands the cluster defines. Matter names them exactly these. */
  values: string[];
}

const OPERATIONAL_STATE = "operationalState";

/** OperationalStateEnum's own values, for a device that labels a state with nothing. */
const STANDARD_STATES: Record<number, string> = {
  0: "stopped",
  1: "running",
  2: "paused",
  3: "error",
};
const TEMPERATURE_CONTROL = "temperatureControl";
const LAUNDRY_WASHER_CONTROLS = "laundryWasherControls";

/** `laundryWasherMode` → `laundry washer mode`. The device's own word, made speakable. */
function spokenName(clusterId: string): string {
  return clusterId
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .toLowerCase();
}

function labelsOf(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const labels = value.filter((v): v is string => typeof v === "string");
  return labels.length === value.length ? labels : undefined;
}

/** Match a label the way a user says it: case and spacing are not the point. */
function looseEquals(a: string, b: string): boolean {
  return a.trim().toLowerCase() === b.trim().toLowerCase();
}

function indexSetting(
  name: string,
  endpoint: number,
  cluster: string,
  labels: string[],
  write: SettingWrite,
): Setting {
  return {
    name,
    endpoint,
    cluster,
    values: labels,
    write,
    valueFor: choice => {
      const index = labels.findIndex(label => looseEquals(label, choice));
      return index === -1 ? undefined : index;
    },
  };
}

/**
 * A ModeBase cluster: `supportedModes` of `{label, mode}` plus a `currentMode`.
 *
 * Detected by that shape rather than by name, so laundry washers, dishwashers,
 * ovens, vacuums and whatever ships next are all read by the same code.
 */
function modeSetting(endpoint: EndpointSnapshot, cluster: string): Setting | undefined {
  const state = endpoint.clusters[cluster];
  const supported = state?.["supportedModes"];
  if (!Array.isArray(supported) || supported.length === 0) return undefined;

  const entries: { label: string; mode: number }[] = [];
  for (const entry of supported) {
    if (typeof entry !== "object" || entry === null) return undefined;
    const label = (entry as { label?: unknown }).label;
    const mode = (entry as { mode?: unknown }).mode;
    if (typeof label !== "string" || typeof mode !== "number") return undefined;
    entries.push({ label, mode });
  }

  return {
    name: spokenName(cluster),
    endpoint: endpoint.number,
    cluster,
    values: entries.map(e => e.label),
    // ModeBase changes by command, not by writing currentMode: the device may
    // refuse a transition, and the command is how it says so.
    write: { kind: "command", command: "changeToMode", field: "newMode" },
    valueFor: choice => entries.find(e => looseEquals(e.label, choice))?.mode,
  };
}

/** Temperature Control's level variant: levels named in a parallel array. */
function temperatureLevelSetting(endpoint: EndpointSnapshot): Setting | undefined {
  const levels = labelsOf(endpoint.clusters[TEMPERATURE_CONTROL]?.["supportedTemperatureLevels"]);
  if (levels === undefined || levels.length === 0) return undefined;

  return indexSetting("temperature level", endpoint.number, TEMPERATURE_CONTROL, levels, {
    kind: "command",
    command: "setTemperature",
    field: "targetTemperatureLevel",
  });
}

/** Laundry Washer Controls: spin speed and rinse count, side by side. */
function washerControlSettings(endpoint: EndpointSnapshot): Setting[] {
  const state = endpoint.clusters[LAUNDRY_WASHER_CONTROLS];
  if (state === undefined) return [];
  const settings: Setting[] = [];

  const speeds = labelsOf(state["spinSpeeds"]);
  if (speeds !== undefined && speeds.length > 0) {
    settings.push(
      indexSetting("spin speed", endpoint.number, LAUNDRY_WASHER_CONTROLS, speeds, {
        kind: "attribute",
        attribute: "spinSpeedCurrent",
      }),
    );
  }

  // `supportedRinses` is an enum list; matter.js may hand over names or numbers.
  const rinses = state["supportedRinses"];
  if (Array.isArray(rinses) && rinses.length > 0) {
    const labels = rinses.map(String);
    settings.push({
      name: "rinses",
      endpoint: endpoint.number,
      cluster: LAUNDRY_WASHER_CONTROLS,
      values: labels,
      write: { kind: "attribute", attribute: "numberOfRinses" },
      valueFor: choice => {
        const index = labels.findIndex(label => looseEquals(label, choice));
        if (index === -1) return undefined;
        // Numeric entries are the value itself; named ones are their position.
        const raw = rinses[index];
        return typeof raw === "number" ? raw : index;
      },
    });
  }

  return settings;
}

/** Everything selectable on this device, in endpoint order. */
export function settingsOf(node: NodeSnapshot): Setting[] {
  const settings: Setting[] = [];

  for (const endpoint of applicationEndpoints(node)) {
    for (const cluster of Object.keys(endpoint.clusters)) {
      const mode = modeSetting(endpoint, cluster);
      if (mode !== undefined) settings.push(mode);
    }
    const temperature = temperatureLevelSetting(endpoint);
    if (temperature !== undefined) settings.push(temperature);
    settings.push(...washerControlSettings(endpoint));
  }

  return settings;
}

/**
 * The setting matching a name the user said, if exactly one does.
 *
 * Widening from exact match to shared words, in that order, because a device's own
 * vocabulary is not the one people use for it: "temperature control" is what the
 * cluster is called, but the setting reads "temperature level", and a caller that
 * says the former has still named it unambiguously. "Exactly one" is the guard
 * throughout — a name matching two settings is answered as unknown rather than
 * guessed at, since guessing puts a wash on the wrong cycle.
 */
export function settingNamed(node: NodeSnapshot, name: string): Setting | undefined {
  const settings = settingsOf(node);
  const exact = settings.find(s => looseEquals(s.name, name));
  if (exact !== undefined) return exact;

  // "washer mode" for "laundry washer mode": a user names the part that
  // distinguishes it, not the cluster's full title.
  const wanted = name.trim().toLowerCase();
  const partial = settings.filter(s => s.name.includes(wanted) || wanted.includes(s.name));
  if (partial.length === 1) return partial[0];

  // "temperature control" for "temperature level": the caller named it by its
  // cluster rather than by the setting, which is a reasonable thing to do.
  const words = new Set(wanted.split(/\s+/).filter(w => w !== ""));
  const shared = settings.filter(s => s.name.split(" ").some(w => words.has(w)));
  return shared.length === 1 ? shared[0] : undefined;
}

/**
 * Which operational states this device says it has, lowercased.
 *
 * `operationalStateList` entries are `{operationalStateId, operationalStateLabel}`;
 * a device may add its own beyond Stopped / Running / Paused / Error.
 */
function operationalStates(endpoint: EndpointSnapshot): Map<number, string> {
  const list = endpoint.clusters[OPERATIONAL_STATE]?.["operationalStateList"];
  const states = new Map<number, string>();
  if (!Array.isArray(list)) return states;

  for (const entry of list) {
    if (typeof entry !== "object" || entry === null) continue;
    const id = (entry as { operationalStateId?: unknown }).operationalStateId;
    const label = (entry as { operationalStateLabel?: unknown }).operationalStateLabel;
    if (typeof id !== "number") continue;
    states.set(id, typeof label === "string" && label !== "" ? label.toLowerCase() : STANDARD_STATES[id] ?? `state ${id}`);
  }
  return states;
}

/**
 * The operations this device actually accepts.
 *
 * Every one of these commands is optional — the spec says Start "shall be supported
 * if the device supports remotely starting the operation" — so offering all four to
 * every appliance advertises things a device will refuse.
 *
 * They are derived from `operationalStateList` because the spec ties the two
 * together: a device "shall, at a minimum, expose the set of states matching the
 * commands that are also supported". So a washer that lists Running accepts Start,
 * one that lists Paused accepts Pause and Resume, and one that lists neither is not
 * remotely startable however much we would like it to be.
 */
export function operationsOf(node: NodeSnapshot): Operations | undefined {
  const endpoint = applicationEndpoints(node).find(e => OPERATIONAL_STATE in e.clusters);
  if (endpoint === undefined) return undefined;

  const states = new Set(operationalStates(endpoint).values());
  const values: string[] = [];
  if (states.has("running")) values.push("start");
  if (states.has("stopped")) values.push("stop");
  if (states.has("paused")) values.push("pause", "resume");

  // A device that publishes no state list at all has told us nothing, which is not
  // the same as telling us "no". Offer the standard four and let it refuse.
  return {
    endpoint: endpoint.number,
    cluster: OPERATIONAL_STATE,
    values: values.length > 0 ? values : ["start", "stop", "pause", "resume"],
  };
}

/**
 * The state the device reports being in, in its own words.
 *
 * This is what an operation reports back, rather than the verb that was asked for.
 * Echoing the request is how GIAP told a user a washer was running while the washer
 * sat there saying Stopped.
 */
export function observedOperation(node: NodeSnapshot): string | undefined {
  const endpoint = applicationEndpoints(node).find(e => OPERATIONAL_STATE in e.clusters);
  if (endpoint === undefined) return undefined;

  const current = endpoint.clusters[OPERATIONAL_STATE]?.["operationalState"];
  if (typeof current !== "number") return undefined;
  return operationalStates(endpoint).get(current) ?? STANDARD_STATES[current];
}
