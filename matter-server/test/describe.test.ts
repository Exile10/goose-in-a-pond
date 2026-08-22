import { describe, expect, it } from "vitest";

import { describeNode } from "../src/mapping/describe.js";
import { describedNode, endpoint, fanNode, lightNode, named, node } from "./fixtures.js";

/** The capability for a verb, or undefined if the device does not offer it. */
function capability(n: Parameters<typeof describeNode>[0], verb: string) {
  return describeNode(n).capabilities.find(c => c.verb === verb);
}

describe("device description", () => {
  it("tells a model what a fan accepts, not just that it has one", () => {
    // The Matter Virtual Device's fan: FanControl, no On/Off, no mode sequence.
    const description = describeNode(fanNode());

    expect(description.device_type).toBe("fan");
    expect(capability(fanNode(), "power")).toEqual({ verb: "power", value: { kind: "boolean" } });
    expect(capability(fanNode(), "fan_speed")).toEqual({
      verb: "fan_speed",
      value: { kind: "percent" },
    });
    // The named modes are the point: "fan_speed" alone cannot say that auto exists.
    expect(capability(fanNode(), "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "medium", "high", "on", "auto", "smart"],
    });
  });

  it("offers only the fan modes the device says it has", () => {
    // FanModeSequence 1 = Off/Low/High. Offering Auto here would produce a failure
    // the user cannot act on, from a promise the device never made.
    const limited = node(18, [
      named("Desk Fan"),
      endpoint(1, { fanControl: { fanMode: 0, fanModeSequence: 1 } }),
    ]);
    expect(capability(limited, "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "high"],
    });

    // 2 = Off/Low/Med/High/Auto.
    const withAuto = node(19, [
      named("Tower Fan"),
      endpoint(1, { fanControl: { fanMode: 0, fanModeSequence: 2 } }),
    ]);
    expect(capability(withAuto, "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "medium", "high", "auto"],
    });
  });

  it("reads a mode sequence matter.js decoded to its name", () => {
    const named_ = node(20, [
      named("Named Sequence"),
      endpoint(1, { fanControl: { fanMode: 0, fanModeSequence: "OffLowMedHighAuto" } }),
    ]);
    expect(capability(named_, "fan_mode")?.value).toEqual({
      kind: "enum",
      values: ["off", "low", "medium", "high", "auto"],
    });
  });

  it("carries a thermostat's own limits, and omits what it does not state", () => {
    const stated = describedNode(30, 0x0301, {
      thermostat: { absMinHeatSetpointLimit: 700, absMaxHeatSetpointLimit: 3000 },
    });
    expect(capability(stated, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 7,
      max: 30,
    });

    // A device that states no limits gets none invented for it: an absent
    // constraint is honest, a guessed one gets believed.
    const silent = describedNode(31, 0x0301, { thermostat: {} });
    expect(capability(silent, "target_temp")?.value).toEqual({ kind: "number", unit: "C" });
  });

  it("describes only what the device has", () => {
    const description = describeNode(lightNode());
    const verbs = description.capabilities.map(c => c.verb);

    expect(verbs).toEqual(["power", "brightness"]);
    expect(verbs).not.toContain("fan_mode");
    expect(verbs).not.toContain("locked");
    expect(description.sensors).toEqual([]);
  });

  it("lists what a sensor measures before it has reported anything", () => {
    // The Air Quality Sensor: this is the question "what is the reading?" could not
    // answer, because a device that has not reported yet had nothing to show.
    const airQuality = describedNode(40, 0x002d, {
      temperatureMeasurement: { measuredValue: 2150 },
      relativeHumidityMeasurement: { measuredValue: 4500 },
      airQuality: { airQuality: 1 },
      carbonDioxideConcentrationMeasurement: { measuredValue: 636 },
    });

    const description = describeNode(airQuality);
    const types = description.sensors.map(s => s.sensor_type);
    expect(types).toContain("temperature");
    expect(types).toContain("humidity");
    expect(types).toContain("air_quality");
    expect(types).toContain("carbon_dioxide");
  });

  it("prefers the unit the device declares over the substance default", () => {
    // CO2 defaults to ppm; this device says ppb. Labelled ppm it is wrong by a
    // factor of a thousand the moment a rule compares it.
    const declared = describedNode(41, 0x002d, {
      carbonDioxideConcentrationMeasurement: { measuredValue: 636, measurementUnit: 1 },
    });
    const co2 = describeNode(declared).sensors.find(s => s.sensor_type === "carbon_dioxide");
    expect(co2?.unit).toBe("ppb");

    const assumed = describedNode(42, 0x002d, {
      carbonDioxideConcentrationMeasurement: { measuredValue: 636 },
    });
    const fallback = describeNode(assumed).sensors.find(s => s.sensor_type === "carbon_dioxide");
    expect(fallback?.unit).toBe("ppm");
  });
  it("reports the ceiling the deadband imposes, not the one the limits advertise", () => {
    // Measured on Google's Matter Virtual Device: it advertised 7 to 30, accepted
    // 23, and answered "Constraint error" from 24 up. In Auto a thermostat keeps
    // its two setpoints minSetpointDeadBand apart, so the heating setpoint cannot
    // come within that of the cooling one -- a ceiling that appears in no limit
    // attribute. Advertising 30 is how a model comes to try 30.
    const auto = node(90, [
      named("Thermostat"),
      endpoint(1, {
        thermostat: {
          absMinHeatSetpointLimit: 700,
          absMaxHeatSetpointLimit: 3000,
          occupiedCoolingSetpoint: 2600,
          // TENTHS of a degree, where the setpoints are hundredths: an int8 whose
          // legal range is 0 to 25, so this is 2.5 degrees. These are the values
          // the Matter Virtual Device actually reports.
          minSetpointDeadBand: 25,
          occupiedHeatingSetpoint: 2000,
        },
      }),
    ]);

    // 26 less 2.5, which is where that device stops accepting: 23 goes in, 24 does
    // not. Read as whole degrees this came out as 1.
    expect(capability(auto, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 7,
      max: 23.5,
    });
  });

  it("prefers the limits a device is configured with over what it could ever do", () => {
    const configured = node(91, [
      named("Thermostat"),
      endpoint(1, {
        thermostat: {
          absMinHeatSetpointLimit: 700,
          absMaxHeatSetpointLimit: 3000,
          minHeatSetpointLimit: 1000,
          maxHeatSetpointLimit: 2500,
        },
      }),
    ]);

    expect(capability(configured, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 10,
      max: 25,
    });
  });

  it("keeps the setpoints from crossing when no deadband is stated", () => {
    // An absent deadband means zero, not "no rule": heating still may not pass
    // cooling.
    const noDeadband = node(92, [
      named("Thermostat"),
      endpoint(1, {
        thermostat: { absMaxHeatSetpointLimit: 3000, occupiedCoolingSetpoint: 2400 },
      }),
    ]);

    expect(capability(noDeadband, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      max: 24,
    });
  });
});
