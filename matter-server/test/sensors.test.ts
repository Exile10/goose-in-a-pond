import { describe, expect, it } from "vitest";

import { SENSORS, readingFor } from "../src/mapping/sensors.js";

const NODE = 4n;

function value(cluster: string, attribute: string, raw: unknown): number | undefined {
  return readingFor(NODE, cluster, attribute, raw)?.value;
}

describe("sensor readings", () => {
  it("scales the measurements onto the units GIAP records", () => {
    expect(value("temperatureMeasurement", "measuredValue", 2137)).toBe(21.37);
    expect(value("relativeHumidityMeasurement", "measuredValue", 4550)).toBe(45.5);
    expect(value("pressureMeasurement", "measuredValue", 1013)).toBe(101.3);
    expect(value("flowMeasurement", "measuredValue", 125)).toBe(12.5);
    // Lux is passed through: the raw measurement is what a rule threshold compares.
    expect(value("illuminanceMeasurement", "measuredValue", 8200)).toBe(8200);
  });

  it("reads occupancy out of the bitmap matter.js decodes", () => {
    expect(value("occupancySensing", "occupancy", { occupied: true })).toBe(1);
    expect(value("occupancySensing", "occupancy", { occupied: false })).toBe(0);
    // Some devices report the raw bitmap; bit 0 still carries the answer.
    expect(value("occupancySensing", "occupancy", 1)).toBe(1);
    expect(value("occupancySensing", "occupancy", 0)).toBe(0);
  });

  it("reads contact as a boolean", () => {
    expect(value("booleanState", "stateValue", true)).toBe(1);
    expect(value("booleanState", "stateValue", false)).toBe(0);
  });

  it("carries both filter signals, which answer different questions", () => {
    // How worn the filter is, and whether the device is asking for it to be changed.
    expect(value("hepaFilterMonitoring", "condition", 62)).toBe(62);
    expect(value("hepaFilterMonitoring", "changeIndication", 2)).toBe(2);
    expect(value("activatedCarbonFilterMonitoring", "condition", 80)).toBe(80);
    expect(value("activatedCarbonFilterMonitoring", "changeIndication", 0)).toBe(0);
  });

  it("names the reading against the device it came from", () => {
    const reading = readingFor(NODE, "temperatureMeasurement", "measuredValue", 2000);
    expect(reading).toMatchObject({
      device_id: "matter-4",
      sensor_type: "temperature",
      unit: "C",
      value: 20,
    });
    expect(Date.parse(reading?.at ?? "")).not.toBeNaN();
  });

  it("ignores clusters and attributes it does not map", () => {
    // A light confirming its own state is not a sensor reading.
    expect(readingFor(NODE, "onOff", "onOff", true)).toBeUndefined();
    expect(readingFor(NODE, "temperatureMeasurement", "minMeasuredValue", 100)).toBeUndefined();
  });

  it("drops a value whose shape is not what the mapping expects", () => {
    // A null measured value is how Matter says "I do not know", and it must not
    // become a reading of zero.
    expect(readingFor(NODE, "temperatureMeasurement", "measuredValue", null)).toBeUndefined();
    expect(readingFor(NODE, "booleanState", "stateValue", 1)).toBeUndefined();
  });

  it("keeps every sensor type distinct", () => {
    // Two clusters minting the same name would make readings from different devices
    // indistinguishable downstream. The filter pair is the near miss this guards.
    const names = SENSORS.map(s => s.sensorType);
    expect(new Set(names).size).toBe(names.length);
  });

  it("has no duplicate cluster/attribute pairs", () => {
    // A duplicate would be silently shadowed by whichever entry the map built last.
    const paths = SENSORS.map(s => `${s.cluster}.${s.attribute}`);
    expect(new Set(paths).size).toBe(paths.length);
  });
});
