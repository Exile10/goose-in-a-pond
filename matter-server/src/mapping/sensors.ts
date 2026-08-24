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
  /**
   * What the numbers mean, for a reading that is an enum rather than a quantity.
   *
   * The value stays the number: a rule comparing "filter change >= 2" needs one,
   * and readings are stored as numbers. This is for the surfaces a person reads,
   * where "2 state" says nothing. The device's own screen shows "Critical" beside
   * the same attribute, and GIAP reporting 2 for it is GIAP knowing the answer and
   * withholding it.
   */
  words?: Record<number, string>;
  /**
   * A featureMap flag the cluster must claim for this reading to exist at all.
   *
   * `describe` lists a sensor whether or not it has reported yet, which is right — a
   * device that has not spoken still measures the thing. But that made cluster presence
   * stand in for attribute presence, and the two differ: a CO-only alarm has the
   * SmokeCoAlarm cluster and no smoke sensor whatsoever, and was credited with a smoke
   * reading it can never produce. Value presence cannot tell "unsupported" from "not yet
   * reported" — both are `undefined` — so the feature map is the only thing that can.
   */
  feature?: string;
}

/** Does the cluster claim the feature this reading needs? Absent claim means yes. */
export function clusterHasFeature(state: Record<string, unknown> | undefined, feature: string): boolean {
  const raw = state?.["featureMap"];
  // matter.js decodes the bitmap to named flags; a raw number is tolerated for the same
  // reason `colorSupport` tolerates one.
  if (typeof raw === "object" && raw !== null) {
    return (raw as Record<string, unknown>)[feature] === true;
  }
  // Nothing stated. Keep the reading rather than withholding one that works -- the same
  // order used for colour capabilities and thermostat setpoints.
  return true;
}

/**
 * Matter's `MeasurementUnitEnum`, however matter.js hands it over.
 *
 * A concentration cluster states the unit its number is in, and substances do not
 * share one: this device reports ozone in ppm where the conventional default is
 * ppb, and pm1 in ppm where the default is ug/m3. Reading only the default gave a
 * number that was right beside a unit that was not.
 */
const MEASUREMENT_UNITS: ReadonlyMap<number, string> = new Map([
  [0, "ppm"],
  [1, "ppb"],
  [2, "ppt"],
  [3, "mg/m3"],
  [4, "ug/m3"],
  [5, "ng/m3"],
  [6, "/m3"],
  [7, "Bq/m3"],
]);

/** The unit a device declared, from its raw `measurementUnit`, if it declared one. */
export function declaredUnitOf(raw: unknown): string | undefined {
  if (typeof raw === "number" && Number.isFinite(raw)) return MEASUREMENT_UNITS.get(raw);

  // matter.js may decode the enum to its name.
  if (typeof raw === "string") {
    return [...MEASUREMENT_UNITS.values()].find(
      unit => unit.replace("/", "").toLowerCase() === raw.replace("/", "").toLowerCase(),
    );
  }
  return undefined;
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

// ── What the enum readings mean ──────────────────────────────────────────────
//
// Matter's own names for these values. Kept beside the sensors that use them so a
// reading and its meaning cannot drift apart, and so adding a sensor with an enum
// has an obvious place to say what its numbers are.

/** ResourceMonitoring's ChangeIndicationEnum: does this filter need replacing. */
const CHANGE_INDICATION: Record<number, string> = {
  0: "OK",
  1: "Warning",
  2: "Critical",
};

/** SmokeCoAlarm's AlarmStateEnum. */
const ALARM_STATE: Record<number, string> = {
  0: "Normal",
  1: "Warning",
  2: "Critical",
};

/** AirQuality's AirQualityEnum, the ordinal the device grades itself on. */
const AIR_QUALITY: Record<number, string> = {
  0: "Unknown",
  1: "Good",
  2: "Fair",
  3: "Moderate",
  4: "Poor",
  5: "Very poor",
  6: "Extremely poor",
};

// ── The table ────────────────────────────────────────────────────────────────

export const SENSORS: readonly SensorMapping[] = [
  // Presence and contact. Both are transitions: a household cares when they change.
  { cluster: "occupancySensing", attribute: "occupancy", sensorType: "occupancy", unit: "bool", read: occupied },
  { cluster: "booleanState", attribute: "stateValue", sensorType: "contact", unit: "bool", read: asBool },

  // Ambient measurements.
  { cluster: "temperatureMeasurement", attribute: "measuredValue", sensorType: "temperature", unit: "C", read: hundredths },
  // A thermostat measures the room it is in, and publishes it here rather than
  // through TemperatureMeasurement -- so asking a thermostat for the temperature
  // got "none recorded", from the one device in the house whose whole job is
  // knowing it. Its setpoint is a separate thing, reachable through `target_temp`.
  { cluster: "thermostat", attribute: "localTemperature", sensorType: "temperature", unit: "C", read: hundredths },
  { cluster: "relativeHumidityMeasurement", attribute: "measuredValue", sensorType: "humidity", unit: "%", read: hundredths },
  // Lux, reported log-scaled. Passed through unconverted: the raw measurement is what
  // a rule threshold compares against.
  { cluster: "illuminanceMeasurement", attribute: "measuredValue", sensorType: "illuminance", unit: "lux", read: asNumber },
  { cluster: "pressureMeasurement", attribute: "measuredValue", sensorType: "pressure", unit: "kPa", read: tenths },
  { cluster: "flowMeasurement", attribute: "measuredValue", sensorType: "flow", unit: "m3/h", read: tenths },

  // An ordinal: 0 unknown, 1 good, rising to 6 extremely poor. Kept as the ordinal
  // rather than invented units, so the scale stays the device's own.
  { cluster: "airQuality", attribute: "airQuality", sensorType: "air_quality", unit: "level", read: asNumber, words: AIR_QUALITY },
  // Alarm state: 0 normal, non-zero means it is sounding.
  //
  // Smoke and CO are separate sensors on the same device because they are separate
  // dangers with separate responses -- one says leave, the other says ventilate. Only
  // smoke was read, so an alarm sounding for carbon monoxide reported nothing at all,
  // and a CO-only alarm looked like a device that measures nothing. Both are gated by
  // the cluster's feature map, so a device without one simply has no value there.
  { cluster: "smokeCoAlarm", attribute: "smokeState", sensorType: "smoke_alarm", unit: "state", read: asNumber, words: ALARM_STATE, feature: "smokeAlarm" },
  { cluster: "smokeCoAlarm", attribute: "coState", sensorType: "co_alarm", unit: "state", read: asNumber, words: ALARM_STATE, feature: "coAlarm" },
  // The same ordinal, about the thing that makes the alarm able to sound at all. A
  // life-safety device with a flat battery is the failure everyone already knows about
  // and nobody is told about.
  { cluster: "smokeCoAlarm", attribute: "batteryAlert", sensorType: "alarm_battery", unit: "state", read: asNumber, words: ALARM_STATE },

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
  { cluster: "hepaFilterMonitoring", attribute: "changeIndication", sensorType: "hepa_filter_change", unit: "state", read: asNumber, words: CHANGE_INDICATION },
  { cluster: "activatedCarbonFilterMonitoring", attribute: "condition", sensorType: "carbon_filter_condition", unit: "%", read: asNumber },
  { cluster: "activatedCarbonFilterMonitoring", attribute: "changeIndication", sensorType: "carbon_filter_change", unit: "state", read: asNumber, words: CHANGE_INDICATION },
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
  // What the cluster says its numbers are in. `describe` has always read this;
  // readings did not, so the same substance was described in one unit and reported
  // in another -- ozone declared ppm and reported ppb, from one device, at once.
  declaredUnit?: unknown,
): Reading | undefined {
  const mapping = BY_PATH.get(`${cluster}.${attribute}`);
  if (mapping === undefined) return undefined;

  const reading = mapping.read(value);
  if (reading === undefined) return undefined;

  return {
    device_id: deviceIdForNode(nodeId),
    sensor_type: mapping.sensorType,
    value: reading,
    unit: declaredUnitOf(declaredUnit) ?? mapping.unit,
    at: at.toISOString(),
  };
}

/** Every cluster carrying at least one mapped sensor attribute. */
export function sensorClusters(): ReadonlySet<string> {
  return new Set(SENSORS.map(mapping => mapping.cluster));
}
