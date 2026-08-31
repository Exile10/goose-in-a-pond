import { describe, expect, it } from "vitest";

import {
  assertAccepted,
  changeObservables,
  isSnapshotCluster,
  refusalOrFault,
  settleTo,
  wordValueSpec,
} from "../src/controller.js";
import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import {
  observedOperation,
  operationsOf,
  settingClusters,
  settingsOf,
} from "../src/mapping/settings.js";
import { deviceClusters } from "../src/mapping/devices.js";
import { sensorClusters } from "../src/mapping/sensors.js";
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

  it("lets appliance clusters into the snapshot at all", () => {
    // The bug that made everything above invisible in a running Pond: settings are
    // read by shape, but the snapshot dropped these clusters BY NAME before anything
    // could look at their shape. Every test above passed while a paired washer still
    // reported nothing but power. They build snapshots by hand, so they never went
    // through the filter that was discarding the clusters.
    expect(isSnapshotCluster("laundryWasherMode")).toBe(true);
    expect(isSnapshotCluster("temperatureControl")).toBe(true);
    expect(isSnapshotCluster("laundryWasherControls")).toBe(true);
    expect(isSnapshotCluster("operationalState")).toBe(true);

    // The same bug, a year and a device class later: these three were declared in
    // settings.ts as module-private constants and never reached the allowlist, so every
    // television described nothing but power and volume while all 158 tests passed.
    expect(isSnapshotCluster("mediaPlayback")).toBe(true);
    expect(isSnapshotCluster("mediaInput")).toBe(true);
    expect(isSnapshotCluster("audioOutput")).toBe(true);

    // The `*Mode` rule is what keeps the promise for devices nobody has coded for.
    expect(isSnapshotCluster("dishwasherMode")).toBe(true);
    expect(isSnapshotCluster("rvcRunMode")).toBe(true);
    expect(isSnapshotCluster("astonishinglyNovelMode")).toBe(true);

    // And it stays bounded: a snapshot is rebuilt on every node event.
    expect(isSnapshotCluster("timeSynchronization")).toBe(false);
    expect(isSnapshotCluster("diagnosticLogs")).toBe(false);
  });

  it("admits every cluster the mappings say they read", () => {
    // Tautological while the allowlist is DERIVED from these three helpers, and that is
    // the point: it is what fails the moment someone goes back to hand-listing a cluster
    // in controller.ts, which is how the media clusters came to be dropped. The check
    // costs nothing and the failure it guards cost a whole feature.
    for (const cluster of [...deviceClusters(), ...settingClusters(), ...sensorClusters()]) {
      expect(isSnapshotCluster(cluster), `'${cluster}' is read but never snapshotted`).toBe(
        true,
      );
    }
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

  it("offers only the operations the device says it has", () => {
    // Start, Stop, Pause and Resume are each optional. The spec ties them to the
    // state list: a device "shall expose the set of states matching the commands
    // that are also supported", so a washer with no Paused state cannot be paused.
    const noPause = node(70, [
      named("Basic Washer"),
      endpoint(1, {
        operationalState: {
          operationalState: 0,
          operationalStateList: [
            { operationalStateId: 0, operationalStateLabel: "Stopped" },
            { operationalStateId: 3, operationalStateLabel: "Error" },
          ],
        },
      }),
    ]);

    expect(operationsOf(noPause)?.values).toEqual(["stop"]);
    expect(() => planControl(noPause, "matter-70", "operation", "start")).toThrowError(
      /is not an operation/,
    );
  });

  it("reports the state the device is in, not the verb it was sent", () => {
    // The whole complaint: GIAP said a washer was running while the washer said
    // Stopped, because it echoed the request back instead of looking.
    expect(observedOperation(laundryWasherNode())).toBe("stopped");

    const running = node(71, [
      named("Washer"),
      endpoint(1, {
        operationalState: {
          operationalState: 1,
          // Its own word for the state, which need not be the standard one.
          operationalStateList: [{ operationalStateId: 1, operationalStateLabel: "Washing" }],
        },
      }),
    ]);
    expect(observedOperation(running)).toBe("washing");
  });

  it("treats a refusal as a failure, in the device's own words", () => {
    // A refused command is a perfectly successful invocation carrying a non-zero
    // code. Nothing throws, so nothing looked -- and a washer that never started
    // was reported as running.
    expect(() =>
      assertAccepted("matter-50", "start", {
        commandResponseState: { errorStateId: 3, errorStateLabel: "CommandInvalidInState" },
      }),
    ).toThrowError(/refused start: CommandInvalidInState/);

    // ModeBase says no differently, with a status and a statusText.
    expect(() =>
      assertAccepted("matter-50", "changeToMode", { status: 2, statusText: "Door is open" }),
    ).toThrowError(/refused changeToMode: Door is open/);

    // And an acceptance is left alone, in both shapes.
    expect(() =>
      assertAccepted("matter-50", "start", { commandResponseState: { errorStateId: 0 } }),
    ).not.toThrow();
    expect(() => assertAccepted("matter-50", "changeToMode", { status: 0 })).not.toThrow();
    expect(() => assertAccepted("matter-50", "off", undefined)).not.toThrow();
  });
  it("takes the cluster's name for a setting when only one can be meant", () => {
    // What Goose actually sent: the setting reads "temperature level", but the
    // cluster is called TemperatureControl, so it asked for "temperature control"
    // first and had to be told no before retrying -- which is why one answer both
    // denied setting it and reported it set.
    const plan = planControl(laundryWasherNode(), "matter-50", "mode", {
      setting: "temperature control",
      value: "Hot",
    });
    expect(plan.applied.mode).toEqual({ setting: "temperature level", value: "Hot" });
  });

  it("still refuses a name that could mean two settings", () => {
    // "mode" alone is not an answer on a device with two of them: picking one is
    // how a wash ends up on the wrong cycle.
    const twoModes = node(72, [
      named("Combo"),
      endpoint(1, {
        laundryWasherMode: {
          currentMode: 0,
          supportedModes: [{ label: "Normal", mode: 0 }],
        },
        dryerMode: {
          currentMode: 0,
          supportedModes: [{ label: "Timed", mode: 0 }],
        },
      }),
    ]);

    expect(() =>
      planControl(twoModes, "matter-72", "mode", { setting: "mode", value: "Normal" }),
    ).toThrowError(/is not a setting/);
  });
  it("waits for the device to report the command's effect", async () => {
    // Measured against the Matter Virtual Device washer: the command answers in
    // about 13ms and the state arrives around 500ms later. Reading straight after
    // the invocation returns the state BEFORE the command, which reported a washer
    // that started perfectly well as having stayed stopped.
    let reads = 0;
    const washer = () => (++reads < 3 ? "stopped" : "running");

    expect(await settleTo("running", washer, 500, 1)).toBe("running");
    expect(reads).toBeGreaterThan(1);
  });

  it("gives up and reports what the device actually is", async () => {
    // A device that takes the command and does nothing is reported as it is, not
    // waited on forever and not assumed to have obeyed.
    expect(await settleTo("running", () => "stopped", 20, 1)).toBe("stopped");

    // A verb with no state of its own to reach is not waited on at all.
    let reads = 0;
    expect(
      await settleTo(
        undefined,
        () => {
          reads++;
          return "idle";
        },
        20,
        1,
      ),
    ).toBe("idle");
    expect(reads).toBe(1);
  });
  it("calls a refusal a refusal, not an unreachable device", () => {
    // The thermostat answered "Constraint error" in milliseconds, from the same
    // machine, and was reported as unreachable -- which sends the reader looking at
    // the network for a fault that is not there.
    const refused = refusalOrFault("matter-1", new Error("Constraint error"));
    expect(refused.code).toBe("device_refused");
    expect(refused.message).toMatch(/outside what it will accept/);
    // The device's own words are kept alongside the explanation.
    expect(refused.message).toMatch(/Constraint error/);

    // A real fault stays one: guessing that an unfamiliar error was a refusal
    // would hide an outage.
    const fault = refusalOrFault("matter-1", new Error("socket hang up"));
    expect(fault.code).toBe("device_unreachable");
    expect(fault.message).toBe("socket hang up");
  });

  it("tells a refusal everything the description already knew", () => {
    // The gap: describe said "49 to 82 C in steps of 1" while the refusal said
    // "49 to 82 C" -- true of a request for 50.5, and no use, because it does not
    // say what was wrong with it. A refusal knowing less than the description is
    // how a caller guesses twice.
    expect(
      wordValueSpec({ kind: "number", unit: "C", min: 49, max: 82, step: 1 }),
    ).toBe("49 to 82 C, in steps of 1");

    // A condition travels too: a thermostat's range is a different range a mode
    // later, so quoting one without it is wrong as soon as it is repeated.
    expect(
      wordValueSpec({ kind: "number", unit: "C", min: 7, max: 23.5, when: "while heating" }),
    ).toBe("7 to 23.5 C (while heating)");

    // An increment with no ends is still worth saying on its own.
    expect(wordValueSpec({ kind: "number", unit: "C", step: 5 })).toBe("values in steps of 5 C");

    // And a device that stated nothing has nothing quoted at it.
    expect(wordValueSpec({ kind: "number" })).toBeUndefined();
  });

  it("says what the device will take, on the refusal itself", () => {
    // A caller that did not read the description first is exactly the caller who
    // gets here. Told only that 30 was wrong, it asked for 30 again.
    const refused = refusalOrFault("matter-1", new Error("Constraint error"), "7 to 23.5 C");
    expect(refused.message).toMatch(/It accepts 7 to 23\.5 C\./);

    // Nothing useful to add is not a reason to invent something.
    const bare = refusalOrFault("matter-1", new Error("Constraint error"));
    expect(bare.message).not.toMatch(/It accepts/);
  });
  it("offers the one thermostat control a person can see on the device", () => {
    // A setpoint change may show nowhere on the thermostat's own screen; system
    // mode is the control it does display. It is also the mode that decides whether
    // a setpoint means anything: aiming a thermostat that is Off at 20 does nothing.
    const thermostat = node(93, [
      named("Thermostat"),
      endpoint(1, { thermostat: { systemMode: 1, occupiedHeatingSetpoint: 2000 } }),
    ]);

    const setting = settingsOf(thermostat).find(s => s.name === "system mode");
    expect(setting?.values).toEqual(["off", "auto", "cool", "heat"]);
    // Matter's codes, which are not positions in the list: heat is 4, not 3.
    expect(setting?.valueFor("heat")).toBe(4);
    expect(setting?.valueFor("cool")).toBe(3);

    const plan = planControl(thermostat, "matter-1", "mode", {
      setting: "system mode",
      value: "Heat",
    });
    expect(plan.actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "thermostat",
        attribute: "systemMode",
        value: 4,
      },
    ]);
  });

  it("does not offer a system mode on a device that has no thermostat", () => {
    const bulb = node(94, [named("Lamp"), endpoint(1, { onOff: { onOff: true } })]);
    expect(settingsOf(bulb).find(s => s.name === "system mode")).toBeUndefined();
  });
});

describe("wiring attribute changes", () => {
  it("finds the level that holds the change observables", () => {
    // matter.js hands these back nested: the outer object's single key is `events`.
    // Iterating the outer level found one key not ending in `$Changed` and wired
    // nothing, for every cluster, with no error -- so readings only ever refreshed
    // when the bridge re-subscribed, and a thermostat measuring 47.33 answered 100.
    const nested = {
      events: { localTemperature$Changed: {}, systemMode$Changed: {}, systemMode$Changing: {} },
    };
    expect(Object.keys(changeObservables(nested))).toContain("localTemperature$Changed");

    // A flat shape is taken as it comes: the nesting is matter.js's business and
    // may change back.
    const flat = { measuredValue$Changed: {} };
    expect(changeObservables(flat)).toBe(flat);

    // Neither level has any: returned unchanged, so the caller wires nothing rather
    // than reaching into something it does not understand.
    const barren = { events: { somethingElse: {} } };
    expect(changeObservables(barren)).toBe(barren);
  });
});
