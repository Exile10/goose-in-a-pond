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

  const heating = heatingSetpoint(endpoint);
  const hasCooling = "occupiedCoolingSetpoint" in (endpoint.clusters[CLUSTER_THERMOSTAT] ?? {});
  // A heat-only thermostat has nothing to choose between.
  if (!hasCooling) return heating;

  const cooling = coolingSetpoint(endpoint);
  switch (attr(endpoint, "systemMode")) {
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

  const heating = heatingSetpoint(endpoint);
  if (!("occupiedCoolingSetpoint" in (endpoint.clusters[CLUSTER_THERMOSTAT] ?? {}))) {
    return { ...(heating.min === undefined ? {} : { min: heating.min }),
             ...(heating.max === undefined ? {} : { max: heating.max }) };
  }

  const cooling = coolingSetpoint(endpoint);
  const min = heating.min ?? cooling.min;
  const max = cooling.max ?? heating.max;
  return { ...(min === undefined ? {} : { min }), ...(max === undefined ? {} : { max }) };
}
