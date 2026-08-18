import { describe, expect, it } from "vitest";

import { describeNode } from "../src/mapping/describe.js";
import { stateOf } from "../src/mapping/state.js";
import { endpoint, laundryWasherNode, named, node } from "./fixtures.js";

/** The value reported for a name, or undefined if it was not reported at all. */
function valueOf(n: Parameters<typeof stateOf>[0], name: string) {
  return stateOf(n).values.find(v => v.name === name)?.value;
}

describe("device state", () => {
  it("reports every setting by the label the device chose", () => {
    // The washer fixture sits at its defaults: Normal, spin Off, one rinse.
    const washer = laundryWasherNode();

    expect(valueOf(washer, "power")).toBe("off");
    expect(valueOf(washer, "laundry washer mode")).toBe("Normal");
    expect(valueOf(washer, "spin speed")).toBe("Low");
    expect(valueOf(washer, "operation")).toBe("stopped");
  });

  it("names state with the same words that change it", () => {
    // The invariant that makes one call follow from the other: every name reported
    // here is a name `describe` offers, so "spin speed is Low" leads straight to the
    // call that makes it High, with no second lookup and no guessing.
    const washer = laundryWasherNode();
    const settable = new Set(
      describeNode(washer).capabilities.map(c => c.setting ?? c.verb),
    );

    for (const { name } of stateOf(washer).values) {
      expect(settable.has(name), `'${name}' is reported but nothing can set it`).toBe(true);
    }
  });

  it("reads a mode by the device's own code, not its position in the list", () => {
    // ModeBase codes need not be 0,1,2: this device's second mode is code 7, and
    // indexing into the label list would report the wrong cycle entirely.
    const oven = node(80, [
      named("Oven"),
      endpoint(1, {
        ovenMode: {
          currentMode: 7,
          supportedModes: [
            { label: "Bake", mode: 0 },
            { label: "Grill", mode: 7 },
          ],
        },
      }),
    ]);

    expect(valueOf(oven, "oven mode")).toBe("Grill");
  });

  it("reports a covering as percent open, matching how it is set", () => {
    // WindowCovering counts percent CLOSED. Reporting its raw number would say 25%
    // for a blind that is three-quarters open.
    const blind = node(81, [
      named("Blind"),
      endpoint(1, { windowCovering: { currentPositionLiftPercent100ths: 2500 } }),
    ]);

    expect(valueOf(blind, "position")).toBe("75% open");
  });

  it("says nothing about what the device did not report", () => {
    // An invented "unknown" is indistinguishable from a real reading one layer up.
    const bare = node(82, [named("Mystery"), endpoint(1, {})]);
    expect(stateOf(bare).values).toEqual([]);

    const lamp = node(83, [named("Lamp"), endpoint(1, { onOff: { onOff: true } })]);
    expect(stateOf(lamp).values).toEqual([{ name: "power", value: "on" }]);
  });
  it("reports the system mode a thermostat is in", () => {
    const thermostat = node(95, [
      named("Thermostat"),
      endpoint(1, { thermostat: { systemMode: 0, occupiedHeatingSetpoint: 1200 } }),
    ]);

    // Off is code 0 and a real answer, not an absent one: a thermostat that is off
    // is the reason a setpoint appears to do nothing.
    expect(valueOf(thermostat, "system mode")).toBe("off");
    expect(valueOf(thermostat, "target_temp")).toBe("12 C");
  });
});
