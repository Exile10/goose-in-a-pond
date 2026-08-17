import { describe, expect, it } from "vitest";

import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import { operationsOf, settingsOf } from "../src/mapping/settings.js";
import { endpoint, laundryWasherNode, named, node } from "./fixtures.js";

/** The capabilities of one verb, in the order the device offered them. */
function verbs(n: Parameters<typeof describeNode>[0], verb: string) {
  return describeNode(n).capabilities.filter(c => c.verb === verb);
}

describe("appliance settings", () => {
  it("finds every setting a washer offers, in the washer's own words", () => {
    const names = settingsOf(laundryWasherNode()).map(s => s.name);

    // Four settings, none of which had a verb before: this is the whole gap that
    // left "set the spin speed to high" answerable only with "it only turns on".
    expect(names).toContain("laundry washer mode");
    expect(names).toContain("temperature level");
    expect(names).toContain("spin speed");
    expect(names).toContain("rinses");
  });

  it("offers the values the device published, not a list of our own", () => {
    const described = verbs(laundryWasherNode(), "mode");
    const washMode = described.find(c => c.setting === "laundry washer mode");

    expect(washMode?.value).toEqual({
      kind: "enum",
      values: ["Normal", "Heavy", "Delicate", "Whites"],
    });
    // "Whites" is offered because this washer said "Whites". A device with a
    // different cycle list gets its own, with no code change.
    expect(described.find(c => c.setting === "spin speed")?.value).toEqual({
      kind: "enum",
      values: ["Low", "Medium", "High"],
    });
  });

  it("changes a ModeBase setting by command, carrying the device's own code", () => {
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "laundry washer mode",
      value: "heavy",
    });

    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "laundryWasherMode",
        // ModeBase changes by command: the device may refuse a transition, and
        // the command is how it says so. Writing currentMode would lose that.
        command: "changeToMode",
        payload: { newMode: 1 },
      },
    ]);
    // Reported with the label the device uses, not the lowercase the user typed.
    expect(plan.applied.mode).toEqual({ setting: "laundry washer mode", value: "Heavy" });
  });

  it("writes an indexed setting as its index", () => {
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "spin speed",
      value: "High",
    });
    expect(plan.actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "laundryWasherControls",
        attribute: "spinSpeedCurrent",
        value: 2,
      },
    ]);
  });

  it("takes the name a user would say for a setting", () => {
    // "washer mode" for "laundry washer mode": people name the distinguishing
    // part, not the cluster's full title.
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "washer mode",
      value: "Delicate",
    });
    expect(plan.applied.mode).toEqual({ setting: "laundry washer mode", value: "Delicate" });
  });

  it("refuses a value the device never offered, and says what it does offer", () => {
    expect(() =>
      planControl(laundryWasherNode(), "matter-50", "mode", {
        setting: "spin speed",
        value: "turbo",
      }),
    ).toThrowError(/accepts: Low, Medium, High/);

    // Nearest-match guessing is how a wash ends up on the wrong cycle.
    expect(() =>
      planControl(laundryWasherNode(), "matter-50", "mode", {
        setting: "colour",
        value: "blue",
      }),
    ).toThrowError(/is not a setting/);
  });

  it("starts and stops a device that runs cycles", () => {
    expect(operationsOf(laundryWasherNode())?.values).toEqual([
      "start",
      "stop",
      "pause",
      "resume",
    ]);

    const plan = planControl(laundryWasherNode(), "matter-50", "operation", "start");
    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "operationalState",
        command: "start",
        payload: {},
      },
    ]);
    expect(plan.applied.operation).toBe("start");
  });

  it("says a device does not run cycles rather than failing obscurely", () => {
    const bulb = node(60, [named("Lamp"), endpoint(1, { onOff: { onOff: false } })]);
    expect(operationsOf(bulb)).toBeUndefined();
    expect(() => planControl(bulb, "matter-60", "operation", "start")).toThrowError(
      /does not run cycles/,
    );
  });

  it("finds a mode cluster it has never heard of, by its shape", () => {
    // The point of reading structurally: a cluster nobody wrote code for works
    // the day a device ships it.
    const unknown = node(61, [
      named("Something New"),
      endpoint(1, {
        astonishinglyNovelMode: {
          currentMode: 0,
          supportedModes: [
            { label: "Gentle", mode: 0 },
            { label: "Vigorous", mode: 7 },
          ],
        },
      }),
    ]);

    const setting = settingsOf(unknown)[0];
    expect(setting?.name).toBe("astonishingly novel mode");
    expect(setting?.values).toEqual(["Gentle", "Vigorous"]);
    // And the device's own code is sent, not the position in the list.
    expect(setting?.valueFor("Vigorous")).toBe(7);
  });
});
