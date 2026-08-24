/**
 * Which setpoint a thermostat is actually being asked about.
 *
 * A thermostat has two: one it heats up to and one it cools down to. `target_temp`
 * always wrote the heating one, whatever the device was doing — so a thermostat
 * sitting in Cool, told "set it to 20", had its *heating* setpoint moved to 20 and
 * carried on cooling to whatever its cooling setpoint said. The write succeeded,
 * the reported value was 20, and nothing about the device changed in the way the
 * person meant.
 *
 * The mode decides. Cool means the cooling setpoint, Heat means the heating one.
 * Auto and Off run neither exclusively, so there the value picks: whichever setpoint
 * it is nearer to is the one being adjusted, which is how a person naming a
 * temperature means it — "make it 28" against a 12/26 pair is plainly about cooling.
 *
 * The two carry different limits, and each is bounded by the other. They must stay
 * `minSetpointDeadBand` apart wherever the device supports Auto, so raising one
 * lowers the other's headroom: this is why a thermostat advertising a 30 degree
 * maximum refuses 24, and why the honest answer to "what will it take" depends on
 * which setpoint the question is about.
 */

import type { EndpointSnapshot, NodeSnapshot } from "./snapshot.js";
import { endpointWith } from "./snapshot.js";

export const CLUSTER_THERMOSTAT = "thermostat";

/** Matter's SystemModeEnum, for the modes that name one setpoint unambiguously. */
const MODE_COOL = 3;
const MODE_HEAT = 4;
const MODE_EMERGENCY_HEAT = 5;

/** One of a thermostat's two setpoints, with the range it will actually accept. */
export interface Setpoint {
  /** "heating" or "cooling", for saying which one moved. */
  which: "heating" | "cooling";
  endpoint: number;
  attribute: string;
  /** Hundredths of a degree, as Matter reports them. Absent where unstated. */
  min?: number;
  max?: number;
}

function attr(endpoint: EndpointSnapshot, name: string): number | undefined {
  const value = endpoint.clusters[CLUSTER_THERMOSTAT]?.[name];
  return typeof value === "number" ? value : undefined;
}

/** Matter's SystemModeEnum by name, for the encoding matter.js may hand over instead. */
const SYSTEM_MODE_NAMES: ReadonlyMap<string, number> = new Map([
  ["off", 0],
  ["auto", 1],
  ["cool", MODE_COOL],
  ["heat", MODE_HEAT],
  ["emergencyheat", MODE_EMERGENCY_HEAT],
  ["precooling", 6],
  ["fanonly", 7],
  ["dry", 8],
  ["sleep", 9],
]);

/**
 * The mode the thermostat is in, whichever way matter.js decoded it.
 *
 * Read as a number only, every comparison against it failed for a device that reported
 * "Cool" — so a cooling-only air conditioner sitting in Cool mode was treated as a
 * thermostat with no settled mode, and got the union of both setpoints' ranges: 7 to
 * 32 C on a device that cannot go anywhere near 7. The same tolerance `fanModes` and
 * `doorStateWord` already apply, for the same reason.
 */
export function systemMode(endpoint: EndpointSnapshot): number | undefined {
  const raw = endpoint.clusters[CLUSTER_THERMOSTAT]?.["systemMode"];
  if (typeof raw === "number") return raw;
  if (typeof raw === "string") {
    return SYSTEM_MODE_NAMES.get(raw.toLowerCase().replace(/[\s_-]/g, ""));
  }
  return undefined;
}

/**
 * Which setpoints this thermostat actually has.
 *
 * `controlSequenceOfOperation` is MANDATORY on the cluster and says exactly this —
 * whether the device cools, heats, or both — and nothing here read it. Presence was
 * inferred with `"occupiedCoolingSetpoint" in clusters` instead, which is a KEY check:
 * matter.js populates a key for every attribute in the cluster model and leaves the
 * unsupported ones `undefined`, so every thermostat looked like it had both. A
 * cooling-only air conditioner was offered a heating setpoint it does not implement.
 *
 * Claims first, evidence second, and neither is allowed to STRIP a control that might
 * work: a device stating nothing at all keeps both, because withholding a setpoint on
 * silence would break a thermostat that under-reports. Same order as `colorSupport`.
 */
export function setpointsAvailable(endpoint: EndpointSnapshot): {
  heating: boolean;
  cooling: boolean;
} {
  // 0 CoolingOnly, 1 CoolingWithReheat, 2 HeatingOnly, 3 HeatingWithReheat,
  // 4 CoolingAndHeating, 5 CoolingAndHeatingWithReheat.
  const sequence = attr(endpoint, "controlSequenceOfOperation");
  if (sequence !== undefined) {
    return { cooling: sequence <= 1 || sequence >= 4, heating: sequence >= 2 };
  }

  const heating = attr(endpoint, "occupiedHeatingSetpoint") !== undefined;
  const cooling = attr(endpoint, "occupiedCoolingSetpoint") !== undefined;
  if (heating || cooling) return { heating, cooling };

  // Says nothing and reports nothing. Keep offering both rather than describing a
  // thermostat as having no temperature control at all.
  return { heating: true, cooling: true };
}

/** The tighter of a configured limit and the absolute one the hardware states. */
function floor(endpoint: EndpointSnapshot, configured: string, absolute: string) {
  return attr(endpoint, configured) ?? attr(endpoint, absolute);
}

/**
 * The gap the two setpoints must keep, in hundredths.
 *
 * `minSetpointDeadBand` is in TENTHS of a degree — an int8 whose legal range is 0
 * to 25, meaning 0 to 2.5 degrees. Reading it as whole degrees turns a 2.5 degree
 * band into 25, which is wrong rather than absurd and shows up nowhere but here.
 */
function deadband(endpoint: EndpointSnapshot): number {
  return (attr(endpoint, "minSetpointDeadBand") ?? 0) * 10;
}

function heatingSetpoint(endpoint: EndpointSnapshot): Setpoint {
  const cooling = attr(endpoint, "occupiedCoolingSetpoint");
  let max = floor(endpoint, "maxHeatSetpointLimit", "absMaxHeatSetpointLimit");

  // Capped below the cooling setpoint. Absent deadband means zero, not "no rule":
  // the two still may not cross.
  if (cooling !== undefined) {
    const ceiling = cooling - deadband(endpoint);
    max = max === undefined ? ceiling : Math.min(max, ceiling);
  }

  const min = floor(endpoint, "minHeatSetpointLimit", "absMinHeatSetpointLimit");
  return {
    which: "heating",
    endpoint: endpoint.number,
    attribute: "occupiedHeatingSetpoint",
    ...(min === undefined ? {} : { min }),
    ...(max === undefined ? {} : { max }),
  };
}

function coolingSetpoint(endpoint: EndpointSnapshot): Setpoint {
  const heating = attr(endpoint, "occupiedHeatingSetpoint");
  let min = floor(endpoint, "minCoolSetpointLimit", "absMinCoolSetpointLimit");

  // The mirror image: held above the heating setpoint by the same band.
  if (heating !== undefined) {
    const bottom = heating + deadband(endpoint);
    min = min === undefined ? bottom : Math.max(min, bottom);
  }

  const max = floor(endpoint, "maxCoolSetpointLimit", "absMaxCoolSetpointLimit");
  return {
    which: "cooling",
    endpoint: endpoint.number,
    attribute: "occupiedCoolingSetpoint",
    ...(min === undefined ? {} : { min }),
    ...(max === undefined ? {} : { max }),
  };
}

/**
 * The setpoint a request is about.
 *
 * `celsius` is the value being asked for, where there is one. Without it — as when
 * describing a device rather than driving it — a mode that names one setpoint still
 * answers definitely, and Auto or Off fall back to heating, which is the setpoint
 * every thermostat has.
 */
export function targetSetpoint(node: NodeSnapshot, celsius?: number): Setpoint | undefined {
  const endpoint = endpointWith(node, CLUSTER_THERMOSTAT);
  if (endpoint === undefined) return undefined;

  const available = setpointsAvailable(endpoint);
  const heating = heatingSetpoint(endpoint);
  // A cool-only device has no heating setpoint to write, and vice versa. Naming one the
  // device does not implement is a write it refuses.
  if (!available.cooling) return available.heating ? heating : undefined;
  if (!available.heating) return coolingSetpoint(endpoint);

  const cooling = coolingSetpoint(endpoint);
  switch (systemMode(endpoint)) {
    case MODE_COOL:
      return cooling;
    case MODE_HEAT:
    case MODE_EMERGENCY_HEAT:
      return heating;
    default:
      break;
  }

  // Auto or Off: neither setpoint is the obvious one, so the value decides. A
  // request nearer the cooling setpoint is a request about cooling.
  if (celsius === undefined) return heating;
  const hundredths = celsius * 100;
  const toHeating = Math.abs(hundredths - (attr(endpoint, "occupiedHeatingSetpoint") ?? 0));
  const toCooling = Math.abs(hundredths - (attr(endpoint, "occupiedCoolingSetpoint") ?? 0));
  return toCooling < toHeating ? cooling : heating;
}

/**
 * Both setpoints' ranges together, for describing a device whose mode does not
 * settle which one a request would land on.
 */
export function reachableRange(node: NodeSnapshot): { min?: number; max?: number } | undefined {
  const endpoint = endpointWith(node, CLUSTER_THERMOSTAT);
  if (endpoint === undefined) return undefined;

  const available = setpointsAvailable(endpoint);
  const heating = heatingSetpoint(endpoint);
  if (!available.cooling) {
    return { ...(heating.min === undefined ? {} : { min: heating.min }),
             ...(heating.max === undefined ? {} : { max: heating.max }) };
  }

  const cooling = coolingSetpoint(endpoint);
  // Cool-only: the union below would take its floor from a heating setpoint that does
  // not exist, which is how an air conditioner came to advertise 7 C.
  if (!available.heating) {
    return { ...(cooling.min === undefined ? {} : { min: cooling.min }),
             ...(cooling.max === undefined ? {} : { max: cooling.max }) };
  }

  const min = heating.min ?? cooling.min;
  const max = cooling.max ?? heating.max;
  return { ...(min === undefined ? {} : { min }), ...(max === undefined ? {} : { max }) };
}

/**
 * An appliance's own temperature setpoint, where it keeps one.
 *
 * Temperature Control has two shapes and this is the other one. The washer named
 * levels — Low, Medium, High — and `settingsOf` reads those as a mode. A dishwasher
 * states a number instead: `temperatureSetpoint` in hundredths, with its own
 * minimum, maximum and step. Reading only the levels, GIAP told a household "no,
 * you cannot set a temperature for the dishwasher" about a device showing a 49 to
 * 82 degree slider on its own screen.
 *
 * It answers to `target_temp` rather than a verb of its own. GIAP already has a
 * verb meaning "a temperature in Celsius", and a dishwasher's wash temperature is
 * that: adding `dishwasher_temp` would be the per-appliance vocabulary this whole
 * area exists to avoid.
 */
export interface ApplianceSetpoint {
  endpoint: number;
  /** Hundredths of a degree, as Matter states them. Absent where unstated. */
  min?: number;
  max?: number;
  /** The increment the device accepts, if it says. */
  step?: number;
}

const TEMPERATURE_CONTROL = "temperatureControl";

export function applianceSetpoint(node: NodeSnapshot): ApplianceSetpoint | undefined {
  const endpoint = endpointWith(node, TEMPERATURE_CONTROL);
  const state = endpoint?.clusters[TEMPERATURE_CONTROL];
  if (endpoint === undefined || state === undefined) return undefined;

  // The number feature, not the level one: a device offering levels is read as a
  // mode, and one offering neither has nothing to set.
  const number = (name: string) => {
    const value = state[name];
    return typeof value === "number" ? value : undefined;
  };
  if (number("temperatureSetpoint") === undefined) return undefined;

  const min = number("minTemperature");
  const max = number("maxTemperature");
  const step = number("step");
  return {
    endpoint: endpoint.number,
    ...(min === undefined ? {} : { min }),
    ...(max === undefined ? {} : { max }),
    ...(step === undefined ? {} : { step }),
  };
}
