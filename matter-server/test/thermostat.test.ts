import { describe, expect, it } from "vitest";

import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import { stateOf } from "../src/mapping/state.js";
import { reachableRange, targetSetpoint } from "../src/mapping/thermostat.js";
import { endpoint, named, node } from "./fixtures.js";

/** The Matter Virtual Device thermostat, with the values it actually publishes. */
function thermostat(systemMode: number) {
  return node(100, [
    named("Thermostat"),
    endpoint(1, {
      thermostat: {
        systemMode,
        occupiedHeatingSetpoint: 1200,
        occupiedCoolingSetpoint: 2600,
        absMinHeatSetpointLimit: 700,
        absMaxHeatSetpointLimit: 3000,
        absMinCoolSetpointLimit: 1600,
        absMaxCoolSetpointLimit: 3200,
        minHeatSetpointLimit: 700,
        // Tenths of a degree: 2.5.
        minSetpointDeadBand: 25,
      },
    }),
  ]);
}

const OFF = 0;
const AUTO = 1;
const COOL = 3;
const HEAT = 4;

describe("which setpoint a thermostat request is about", () => {
  it("writes the cooling setpoint when the thermostat is cooling", () => {
    // The bug: a thermostat sitting in Cool, told "set it to 20", had its HEATING
    // setpoint moved and carried on cooling to 26. The write succeeded and the
    // reported value was 20, so nothing looked wrong except the device.
    const plan = planControl(thermostat(COOL), "matter-1", "target_temp", 20);

    expect(plan.actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "thermostat",
        attribute: "occupiedCoolingSetpoint",
        value: 2000,
      },
    ]);
  });

  it("writes the heating setpoint when the thermostat is heating", () => {
    const plan = planControl(thermostat(HEAT), "matter-1", "target_temp", 20);
    expect(plan.actions[0]).toMatchObject({ attribute: "occupiedHeatingSetpoint", value: 2000 });
  });

  it("lets the value decide when the mode does not", () => {
    // Auto runs both, so neither setpoint is the obvious one. Against a 12/26 pair,
    // "make it 28" is plainly about cooling and "make it 10" plainly about heating.
    expect(targetSetpoint(thermostat(AUTO), 28)?.which).toBe("cooling");
    expect(targetSetpoint(thermostat(AUTO), 10)?.which).toBe("heating");
    expect(targetSetpoint(thermostat(OFF), 25)?.which).toBe("cooling");

    // A tie goes to heating rather than being arbitrary: 19 is equidistant.
    expect(targetSetpoint(thermostat(AUTO), 19)?.which).toBe("heating");
  });

  it("bounds each setpoint by the other across the deadband", () => {
    // Heating is capped 2.5 below cooling; cooling is floored 2.5 above heating.
    // This is why a thermostat advertising 30 refuses 24.
    const heating = targetSetpoint(thermostat(HEAT));
    expect(heating).toMatchObject({ which: "heating", min: 700, max: 2350 });

    const cooling = targetSetpoint(thermostat(COOL));
    expect(cooling).toMatchObject({ which: "cooling", min: 1600, max: 3200 });
  });

  it("describes the range of the setpoint the mode has live", () => {
    const cooling = describeNode(thermostat(COOL)).capabilities.find(c => c.verb === "target_temp");
    expect(cooling?.value).toEqual({ kind: "number", unit: "C", min: 16, max: 32 });

    const heating = describeNode(thermostat(HEAT)).capabilities.find(c => c.verb === "target_temp");
    expect(heating?.value).toEqual({ kind: "number", unit: "C", min: 7, max: 23.5 });

    // Auto reaches either, so the range spans both rather than advertising one.
    const auto = describeNode(thermostat(AUTO)).capabilities.find(c => c.verb === "target_temp");
    expect(auto?.value).toEqual({ kind: "number", unit: "C", min: 7, max: 32 });
    expect(reachableRange(thermostat(AUTO))).toEqual({ min: 700, max: 3200 });
  });

  it("reports the setpoint that is steering, not the other one", () => {
    const value = (n: number) =>
      stateOf(thermostat(n)).values.find(v => v.name === "target_temp")?.value;

    expect(value(COOL)).toBe("26 C");
    expect(value(HEAT)).toBe("12 C");
  });

  it("leaves a heat-only thermostat alone", () => {
    // Nothing to choose between, and no deadband to cap it.
    const heatOnly = node(101, [
      named("Boiler"),
      endpoint(1, {
        thermostat: { systemMode: 4, occupiedHeatingSetpoint: 2000, absMaxHeatSetpointLimit: 3000 },
      }),
    ]);

    expect(targetSetpoint(heatOnly, 28)).toMatchObject({
      which: "heating",
      attribute: "occupiedHeatingSetpoint",
      max: 3000,
    });
  });
});
