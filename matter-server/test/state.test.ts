import { describe, expect, it } from "vitest";

import { planControl } from "../src/mapping/control.js";
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

  it("names state with words the description also uses", () => {
    // The invariant that makes one call follow from the other: every name reported
    // here appears in the description, either as something settable or as something
    // measured. So "spin speed is Low" leads straight to the call that makes it
    // High, and a measurement is named the same way `list_sensors` names it.
    //
    // A device with both halves, because the earlier version of this test used a
    // washer -- which has no sensors, so it never checked the measured half at all.
    const purifier = node(96, [
      named("Air Purifier"),
      endpoint(1, {
        onOff: { onOff: true },
        fanControl: { percentCurrent: 50, fanMode: 2 },
        hepaFilterMonitoring: { condition: 100, changeIndication: 0 },
        activatedCarbonFilterMonitoring: { condition: 80, changeIndication: 0 },
      }),
    ]);

    const described = describeNode(purifier);
    const settable = new Set(described.capabilities.map(c => c.setting ?? c.verb));
    const measured = new Set(described.sensors.map(s => s.sensor_type));

    const reported = stateOf(purifier).values.map(v => v.name);
    // It has to report both kinds, or this passes by reporting nothing.
    expect(reported).toContain("power");
    expect(reported).toContain("hepa_filter_condition");

    for (const name of reported) {
      expect(
        settable.has(name) || measured.has(name),
        `'${name}' is reported but the description neither sets nor measures it`,
      ).toBe(true);
    }
  });

  it("reports what a device measures, not only what it can be told to be", () => {
    // Asked for an air purifier's state, GIAP answered power and fan speed and had
    // to add that the filter conditions "are not measured in this reading" -- while
    // both sat in the snapshot, and describe was already listing them.
    const purifier = node(97, [
      named("Air Purifier"),
      endpoint(1, {
        onOff: { onOff: true },
        hepaFilterMonitoring: { condition: 100 },
        activatedCarbonFilterMonitoring: { condition: 80 },
      }),
    ]);

    // Formatted as the controls beside it are: no gap before a percent sign.
    expect(valueOf(purifier, "hepa_filter_condition")).toBe("100%");
    expect(valueOf(purifier, "carbon_filter_condition")).toBe("80%");
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
  it("says what an enum reading means, in the device's own words", () => {
    // The purifier's own screen shows "Critical" for a spent filter and "OK" for a
    // good one. GIAP reported "2 state" and "0 state" -- the same fact with the
    // meaning removed, which is the whole of what was being asked for.
    const purifier = node(98, [
      named("Air Purifier"),
      endpoint(1, {
        hepaFilterMonitoring: { condition: 0, changeIndication: 2 },
        activatedCarbonFilterMonitoring: { condition: 100, changeIndication: 0 },
      }),
    ]);

    expect(valueOf(purifier, "hepa_filter_change")).toBe("Critical");
    expect(valueOf(purifier, "carbon_filter_change")).toBe("OK");
    // The quantities beside them are unaffected.
    expect(valueOf(purifier, "hepa_filter_condition")).toBe("0%");
    expect(valueOf(purifier, "carbon_filter_condition")).toBe("100%");
  });

  it("keeps the number when a value is outside the enum it knows", () => {
    // A device reporting something this table has no word for must not be
    // described as any of the words it does have.
    const odd = node(99, [
      named("Purifier"),
      endpoint(1, { hepaFilterMonitoring: { changeIndication: 7 } }),
    ]);
    expect(valueOf(odd, "hepa_filter_change")).toBe("7 state");
  });
  it("says where a covering is heading when that is not where it is", () => {
    // 0 hundredths is fully OPEN in Matter: UpOrOpen sets it to 0.00%, DownOrClose
    // to 100.00%. A covering told to close therefore sits at "100% open" with a
    // target of fully closed until it travels -- and on a device that accepts the
    // command without moving, that is the only sign the command landed at all.
    const closing = node(110, [
      named("Blind"),
      endpoint(1, {
        windowCovering: {
          currentPositionLiftPercent100ths: 0,
          targetPositionLiftPercent100ths: 10000,
        },
      }),
    ]);
    expect(valueOf(closing, "position")).toBe("100% open, moving to 0% open");

    // Arrived: one fact, not two.
    const settled = node(111, [
      named("Blind"),
      endpoint(1, {
        windowCovering: {
          currentPositionLiftPercent100ths: 3000,
          targetPositionLiftPercent100ths: 3000,
        },
      }),
    ]);
    expect(valueOf(settled, "position")).toBe("70% open");

    // Still one name, and one `describe` offers -- the thing reported is the thing
    // `position` sets.
    const settable = new Set(describeNode(closing).capabilities.map(c => c.setting ?? c.verb));
    for (const { name } of stateOf(closing).values) {
      expect(settable.has(name), `'${name}' is reported but nothing sets it`).toBe(true);
    }
  });
  it("offers and reports a covering's second axis, where it has one", () => {
    // Lift and tilt are separate axes: how far a blind is lowered, and how far its
    // slats are turned. A venetian blind is routinely down with its slats open, and
    // position alone cannot ask for that.
    const venetian = node(112, [
      named("Blind"),
      endpoint(1, {
        windowCovering: {
          currentPositionLiftPercent100ths: 0,
          targetPositionLiftPercent100ths: 0,
          currentPositionTiltPercent100ths: 10000,
          targetPositionTiltPercent100ths: 3000,
        },
      }),
    ]);

    expect(describeNode(venetian).capabilities.map(c => c.verb)).toEqual(["position", "tilt"]);
    expect(valueOf(venetian, "position")).toBe("100% open");
    expect(valueOf(venetian, "tilt")).toBe("0% open, turning to 70% open");

    // Sent as its own command, on the axis it belongs to.
    const plan = planControl(venetian, "matter-2", "tilt", 40);
    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "windowCovering",
        command: "goToTiltPercentage",
        payload: { tiltPercent100thsValue: 6000 },
      },
    ]);
    expect(plan.applied).toEqual({ tilt: 40 });
  });

  it("does not offer tilt to a covering with no slats", () => {
    // A roller blind has nothing to turn, and offering a control the device will
    // reject is the failure this area exists to stop.
    const roller = node(113, [
      named("Roller"),
      endpoint(1, { windowCovering: { currentPositionLiftPercent100ths: 5000 } }),
    ]);

    expect(describeNode(roller).capabilities.map(c => c.verb)).toEqual(["position"]);
    expect(stateOf(roller).values.find(v => v.name === "tilt")).toBeUndefined();
    expect(() => planControl(roller, "matter-3", "tilt", 40)).not.toThrow();
  });
});
