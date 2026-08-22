/**
 * Matter measurement clusters projected onto GIAP `SensorReading`s.
 *
 * This file holds the whole vocabulary of sensor names the Pond mints for itself.
 * `crates/pond-core/tests/context_producer_tracks_the_sensor_vocabulary.rs` reads it
 * as source text and fails the build if a `sensorType` here is dispositioned neither
 * as a transition (`DISCRETE_SENSOR_TYPES`) nor as a measurement (its own
 * `KNOWN_CONTINUOUS`). That tripwire exists because the producer's rule is an
 * ALLOW-list: an unlisted signal is discarded forever, with no error anywhere, and a
 * household's new device becomes the one thing the assistant never mentions.
 *
 * So: keep the table one entry per line, each with its sensor name as a plain string
 * literal in the sensorType field, because that is the shape the tripwire scans for. A
 * mapping written some other way is invisible to it, and an invisible mapping is the
 * failure it was built to catch.
 */

import type { Reading } from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";

/** How a decoded attribute value becomes a number. `undefined` skips the reading. */
type Read = (value: unknown) => number | undefined;

export interface SensorMapping {
  /** matter.js behavior id. */
  cluster: string;
  /** matter.js attribute name. */
  attribute: string;
  sensorType: string;
  unit: string;
  read: Read;
}

// ── Value readers ────────────────────────────────────────────────────────────

const asNumber: Read = v => (typeof v === "number" && Number.isFinite(v) ? v : undefined);

/** Hundredths of a unit — Matter's scale for temperature and humidity. */
const hundredths: Read = v => {
  const n = asNumber(v);
  return n === undefined ? undefined : n / 100;
};

/** Tenths of a unit — Matter's scale for pressure and flow. */
const tenths: Read = v => {
  const n = asNumber(v);
  return n === undefined ? undefined : n / 10;
};

const asBool: Read = v => (typeof v === "boolean" ? (v ? 1 : 0) : undefined);

/**
 * OccupancySensing's `occupancy` is a bitmap whose bit 0 is "occupied". matter.js
 * decodes bitmaps into objects, so this reads the flag rather than masking an integer
 * as the schema-11 adapter had to.
 */
const occupied: Read = v => {
  if (typeof v === "object" && v !== null && "occupied" in v) {
    return (v as { occupied?: unknown }).occupied === true ? 1 : 0;
  }
  // Some devices report the raw bitmap; bit 0 still carries the answer.
  const n = asNumber(v);
  return n === undefined ? undefined : (n & 1) === 1 ? 1 : 0;
};

// ── The table ────────────────────────────────────────────────────────────────

export const SENSORS: readonly SensorMapping[] = [
  // Presence and contact. Both are transitions: a household cares when they change.
  { cluster: "occupancySensing", attribute: "occupancy", sensorType: "occupancy", unit: "bool", read: occupied },
  { cluster: "booleanState", attribute: "stateValue", sensorType: "contact", unit: "bool", read: asBool },

  // Ambient measurements.
  { cluster: "temperatureMeasurement", attribute: "measuredValue", sensorType: "temperature", unit: "C", read: hundredths },
  { cluster: "relativeHumidityMeasurement", attribute: "measuredValue", sensorType: "humidity", unit: "%", read: hundredths },
  // Lux, reported log-scaled. Passed through unconverted: the raw measurement is what
  // a rule threshold compares against.
  { cluster: "illuminanceMeasurement", attribute: "measuredValue", sensorType: "illuminance", unit: "lux", read: asNumber },
  { cluster: "pressureMeasurement", attribute: "measuredValue", sensorType: "pressure", unit: "kPa", read: tenths },
  { cluster: "flowMeasurement", attribute: "measuredValue", sensorType: "flow", unit: "m3/h", read: tenths },

  // An ordinal: 0 unknown, 1 good, rising to 6 extremely poor. Kept as the ordinal
  // rather than invented units, so the scale stays the device's own.
  { cluster: "airQuality", attribute: "airQuality", sensorType: "air_quality", unit: "level", read: asNumber },
  // Alarm state: 0 normal, non-zero means it is sounding.
  { cluster: "smokeCoAlarm", attribute: "smokeState", sensorType: "smoke_alarm", unit: "state", read: asNumber },

  // Concentrations are floats in each substance's own unit, passed through unscaled —
  // the number the device shows is the number a rule threshold should compare against.
  //
  // These units are the DEFAULTS for each substance. The device also publishes a
  // `measurementUnit` attribute which is authoritative and which GIAP does not read
  // yet: a device reporting CO2 in ppb rather than ppm would be labelled wrongly.
  // Worth reading before this is trusted for anything but display.
  { cluster: "carbonMonoxideConcentrationMeasurement", attribute: "measuredValue", sensorType: "carbon_monoxide", unit: "ppm", read: asNumber },
  { cluster: "carbonDioxideConcentrationMeasurement", attribute: "measuredValue", sensorType: "carbon_dioxide", unit: "ppm", read: asNumber },
  { cluster: "nitrogenDioxideConcentrationMeasurement", attribute: "measuredValue", sensorType: "nitrogen_dioxide", unit: "ppb", read: asNumber },
  { cluster: "ozoneConcentrationMeasurement", attribute: "measuredValue", sensorType: "ozone", unit: "ppb", read: asNumber },
  { cluster: "formaldehydeConcentrationMeasurement", attribute: "measuredValue", sensorType: "formaldehyde", unit: "mg/m3", read: asNumber },
  { cluster: "pm1ConcentrationMeasurement", attribute: "measuredValue", sensorType: "pm1", unit: "ug/m3", read: asNumber },
  { cluster: "pm25ConcentrationMeasurement", attribute: "measuredValue", sensorType: "pm2_5", unit: "ug/m3", read: asNumber },
  { cluster: "pm10ConcentrationMeasurement", attribute: "measuredValue", sensorType: "pm10", unit: "ug/m3", read: asNumber },
  { cluster: "radonConcentrationMeasurement", attribute: "measuredValue", sensorType: "radon", unit: "ppm", read: asNumber },
  { cluster: "totalVolatileOrganicCompoundsConcentrationMeasurement", attribute: "measuredValue", sensorType: "total_volatile_organic_compounds", unit: "ppb", read: asNumber },

  // Resource monitoring — an air purifier's two filters. Same cluster shape, one
  // instance per filter, and two attributes each because they answer different
  // questions: how worn the filter is, and whether the device is asking for it to be
  // changed. The change indication is 0 OK, 1 Warning, 2 Critical.
  { cluster: "hepaFilterMonitoring", attribute: "condition", sensorType: "hepa_filter_condition", unit: "%", read: asNumber },
  { cluster: "hepaFilterMonitoring", attribute: "changeIndication", sensorType: "hepa_filter_change", unit: "state", read: asNumber },
  { cluster: "activatedCarbonFilterMonitoring", attribute: "condition", sensorType: "carbon_filter_condition", unit: "%", read: asNumber },
  { cluster: "activatedCarbonFilterMonitoring", attribute: "changeIndication", sensorType: "carbon_filter_change", unit: "state", read: asNumber },
];

const BY_PATH: ReadonlyMap<string, SensorMapping> = new Map(
  SENSORS.map(mapping => [`${mapping.cluster}.${mapping.attribute}`, mapping]),
);

/**
 * The reading a cluster attribute carries, or `undefined` when it is not one GIAP
 * understands — a light confirming its own state, a utility cluster, an attribute
 * whose decoded value is not the shape the mapping expects.
 */
export function readingFor(
  nodeId: bigint,
  cluster: string,
  attribute: string,
  value: unknown,
  at: Date = new Date(),
): Reading | undefined {
  const mapping = BY_PATH.get(`${cluster}.${attribute}`);
  if (mapping === undefined) return undefined;

  const reading = mapping.read(value);
  if (reading === undefined) return undefined;

  return {
    device_id: deviceIdForNode(nodeId),
    sensor_type: mapping.sensorType,
    value: reading,
    unit: mapping.unit,
    at: at.toISOString(),
  };
}

/** Every cluster carrying at least one mapped sensor attribute. */
export function sensorClusters(): ReadonlySet<string> {
  return new Set(SENSORS.map(mapping => mapping.cluster));
}
