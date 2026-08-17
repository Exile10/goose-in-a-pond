import { describe, expect, it } from "vitest";

import {
  brightnessToLevel,
  celsiusToSetpoint,
  fanModeFromName,
  hueToMatter,
  planControl,
  positionOpenToLift100ths,
  saturationToMatter,
  FAN_MODE_OFF,
  FAN_MODE_ON,
} from "../src/mapping/control.js";
import { OpError } from "../src/protocol.js";
import { describedNode, endpoint, fanNode, lightNode, node } from "./fixtures.js";

describe("unit conversions", () => {
  it("maps brightness onto Matter's 0-254 level scale", () => {
    expect(brightnessToLevel(0)).toBe(0);
    expect(brightnessToLevel(50)).toBe(127);
    expect(brightnessToLevel(100)).toBe(254);
    // Over-range input is clamped rather than wrapping into a dim bulb.
    expect(brightnessToLevel(200)).toBe(254);
  });

  it("maps Celsius onto hundredths of a degree", () => {
    expect(celsiusToSetpoint(21.5)).toBe(2150);
    expect(celsiusToSetpoint(-5)).toBe(-500);
    // Clamped to the i16 the attribute is, so an absurd value cannot wrap.
    expect(celsiusToSetpoint(100000)).toBe(32767);
  });

  it("wraps hue around the circle", () => {
    expect(hueToMatter(0)).toBe(0);
    expect(hueToMatter(360)).toBe(0);
    expect(hueToMatter(180)).toBe(127);
    expect(saturationToMatter(100)).toBe(254);
  });

  it("converts percent open into hundredths-of-a-percent closed", () => {
    // GIAP speaks in percent open because that is how users phrase it; Matter's
    // GoToLiftPercentage is the other way round.
    expect(positionOpenToLift100ths(100)).toBe(0);
    expect(positionOpenToLift100ths(0)).toBe(10000);
    expect(positionOpenToLift100ths(50)).toBe(5000);
  });

  it("names fan modes the way a user says them", () => {
    expect(fanModeFromName("off")).toBe(FAN_MODE_OFF);
    expect(fanModeFromName("MEDIUM")).toBe(2);
    expect(fanModeFromName("med")).toBe(2);
    expect(fanModeFromName(" auto ")).toBe(5);
    // "on" writes High, not the deprecated FanMode::On (4).
    expect(fanModeFromName("on")).toBe(FAN_MODE_ON);
    expect(FAN_MODE_ON).toBe(3);
    expect(fanModeFromName("turbo")).toBeUndefined();
  });
});

describe("control planning", () => {
  it("switches a light through On/Off", () => {
    const plan = planControl(lightNode(), "matter-2", "power", true);
    expect(plan.actions).toEqual([
      { kind: "command", endpoint: 13, cluster: "onOff", command: "on", payload: {} },
    ]);
    expect(plan.applied).toEqual({ on: true });
  });

  it("switches a fan through FanMode when it has no On/Off cluster", () => {
    // Without this branch "turn on the fan" could only fail: most fans, the Matter
    // Virtual Device's included, implement no On/Off cluster at all.
    const plan = planControl(fanNode(), "matter-18", "power", true);
    expect(plan.actions).toEqual([
      { kind: "write", endpoint: 1, cluster: "fanControl", attribute: "fanMode", value: FAN_MODE_ON },
    ]);
    expect(plan.applied).toEqual({ on: true });

    const off = planControl(fanNode(), "matter-18", "power", false);
    expect(off.actions[0]).toMatchObject({ value: FAN_MODE_OFF });
  });

  it("prefers On/Off over FanMode when a fan has both", () => {
    const both = node(18, [
      endpoint(0, {}),
      endpoint(1, { fanControl: { fanMode: 0 }, onOff: { onOff: false } }),
    ]);
    expect(planControl(both, "matter-18", "power", true).actions[0]).toMatchObject({
      kind: "command",
      cluster: "onOff",
    });
  });

  it("writes setpoints and fan speeds as attributes, not commands", () => {
    const thermostat = describedNode(5, 0x0301, { thermostat: { occupiedHeatingSetpoint: 2000 } });
    expect(planControl(thermostat, "matter-5", "target_temp", 21.5).actions).toEqual([
      {
        kind: "write",
        endpoint: 1,
        cluster: "thermostat",
        attribute: "occupiedHeatingSetpoint",
        value: 2150,
      },
    ]);

    expect(planControl(fanNode(), "matter-18", "fan_speed", 40).actions).toEqual([
      { kind: "write", endpoint: 1, cluster: "fanControl", attribute: "percentSetting", value: 40 },
    ]);
  });

  it("reports fan_speed zero as off", () => {
    expect(planControl(fanNode(), "matter-18", "fan_speed", 0).applied).toEqual({
      fan_speed: 0,
      on: false,
    });
  });

  it("locks and unlocks with the matching command", () => {
    const lock = describedNode(6, 0x000a, { doorLock: { lockState: 1 } });
    expect(planControl(lock, "matter-6", "locked", true).actions[0]).toMatchObject({
      command: "lockDoor",
    });
    expect(planControl(lock, "matter-6", "locked", false).actions[0]).toMatchObject({
      command: "unlockDoor",
    });
  });

  it("refuses a verb the device has no cluster for", () => {
    // Refusing is the point: the logging stub answering every verb with success is
    // what let the agent tell a user a fan was on when nothing had been sent anywhere.
    expect(() => planControl(lightNode(), "matter-2", "position", 50)).toThrow(OpError);
    try {
      planControl(lightNode(), "matter-2", "position", 50);
    } catch (error) {
      expect((error as OpError).code).toBe("capability_unsupported");
    }
  });

  it("refuses a device that can be neither switched nor moded", () => {
    const opaque = node(50, [endpoint(0, {}), endpoint(1, {})]);
    try {
      planControl(opaque, "matter-50", "power", true);
      expect.unreachable("a device with neither cluster must not report success");
    } catch (error) {
      expect((error as OpError).code).toBe("capability_unsupported");
    }
  });

  it("rejects values of the wrong shape rather than coercing them", () => {
    for (const bad of [["power", "yes"], ["brightness", "half"], ["fan_mode", 3]] as const) {
      try {
        planControl(lightNode(), "matter-2", bad[0], bad[1]);
        expect.unreachable(`${bad[0]} accepted ${String(bad[1])}`);
      } catch (error) {
        expect((error as OpError).code).toBe("bad_request");
      }
    }
  });

  it("gives commands that take no fields an EMPTY payload", () => {
    // The contract `controller.ts` reads: an empty payload means invoke the
    // command with NO argument. matter.js validates against the cluster schema
    // and rejects `{}` on a void command with "Expected void, got object", so
    // On, Off, LockDoor and UnlockDoor all failed while the commands that do
    // take fields worked — a half-working state that is very hard to read from
    // the outside. Anything added here with a void command must keep this shape.
    const voidCommands: [ReturnType<typeof planControl>, string][] = [
      [planControl(lightNode(), "matter-2", "power", true), "on"],
      [planControl(lightNode(), "matter-2", "power", false), "off"],
    ];
    for (const [plan, name] of voidCommands) {
      const action = plan.actions[0];
      expect(action?.kind).toBe("command");
      if (action?.kind !== "command") continue;
      expect(action.command).toBe(name);
      expect(Object.keys(action.payload)).toEqual([]);
    }

    const lock = describedNode(6, 0x000a, { doorLock: { lockState: 1 } });
    const locking = planControl(lock, "matter-6", "locked", true).actions[0];
    expect(locking?.kind === "command" && Object.keys(locking.payload)).toEqual([]);
  });

  it("gives commands that DO take fields a populated payload", () => {
    // The other half of the same contract: these must not be invoked bare.
    const dim = planControl(lightNode(), "matter-2", "brightness", 40).actions[0];
    expect(dim?.kind === "command" && Object.keys(dim.payload).length).toBeGreaterThan(0);
  });

  it("names the modes it accepts when given one it does not", () => {
    try {
      planControl(fanNode(), "matter-18", "fan_mode", "turbo");
      expect.unreachable("turbo is not a fan mode");
    } catch (error) {
      expect((error as OpError).message).toContain("off, low, medium, high, on, auto or smart");
    }
  });
});
