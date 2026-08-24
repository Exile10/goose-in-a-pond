import { describe, expect, it } from "vitest";

import {
  brightnessToLevel,
  celsiusToSetpoint,
  fanModeFromName,
  hueToMatter,
  kelvinToMireds,
  VERBS,
  matterToHue,
  observedFor,
  matterToSaturation,
  miredsToKelvin,
  planControl,
  positionOpenToLift100ths,
  saturationToMatter,
  FAN_MODE_OFF,
  FAN_MODE_ON,
} from "../src/mapping/control.js";
import { OpError } from "../src/protocol.js";
import {
  describedNode,
  endpoint,
  extendedColorLightNode,
  fanNode,
  lightNode,
  named,
  node,
  tunableWhiteNode,
  videoPlayerNode,
} from "./fixtures.js";

describe("unit conversions", () => {
  it("maps brightness onto Matter's 0-254 level scale", () => {
    expect(brightnessToLevel(0)).toBe(0);
    expect(brightnessToLevel(50)).toBe(127);
    expect(brightnessToLevel(100)).toBe(254);
    // Over-range input is clamped rather than wrapping into a dim bulb.
    expect(brightnessToLevel(200)).toBe(254);
  });

  it("maps kelvin onto mireds, inverting the order", () => {
    // Mireds are reciprocal megakelvin, so the mapping is its own inverse and hotter is
    // SMALLER. A conversion that preserved order would put warm white where cool goes.
    expect(kelvinToMireds(2700)).toBe(370);
    expect(kelvinToMireds(6500)).toBe(154);
    expect(miredsToKelvin(370)).toBe(2703);
    expect(miredsToKelvin(154)).toBe(6494);

    // Clamped to the cluster's own field range, and zero is not a colour: the spec's
    // defaults include 0 mireds, which would divide to infinity.
    expect(kelvinToMireds(0)).toBe(0xfeff);
    expect(kelvinToMireds(-1)).toBe(0xfeff);
    expect(miredsToKelvin(0)).toBe(0);
  });

  it("reads a hue back as one that would put the device where it is", () => {
    // NOT numeric equality, and the difference is the point. ColorControl quantises 360
    // degrees onto 0-254, so a step is ~1.4 degrees and 90 comes back as 91 -- there is
    // no conversion that avoids that. What the doc actually promises is that a value
    // read here would put the device back where it is, and THAT is exact: writing the
    // read-back lands on the same raw value.
    for (const degrees of [0, 45, 90, 180, 300, 359]) {
      const raw = hueToMatter(degrees);
      expect(hueToMatter(matterToHue(raw))).toBe(raw);
      // And it is never off by more than a step, so a reading is never misleading.
      expect(Math.abs(matterToHue(raw) - degrees)).toBeLessThanOrEqual(2);
    }
  });

  it("reads saturation back exactly", () => {
    // 0-100 onto 0-254 and back is exact at every whole percent, unlike hue.
    for (const pct of [0, 1, 50, 99, 100]) {
      expect(matterToSaturation(saturationToMatter(pct))).toBe(pct);
    }
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

describe("colour temperature control", () => {
  it("sends the device mireds for the kelvin it was asked for", () => {
    const plan = planControl(extendedColorLightNode(), "matter-51", "color_temp", 2700);

    expect(plan.actions).toEqual([
      {
        kind: "command",
        endpoint: 1,
        cluster: "colorControl",
        command: "moveToColorTemperature",
        payload: {
          colorTemperatureMireds: 370,
          transitionTime: 0,
          optionsMask: {},
          optionsOverride: {},
        },
      },
    ]);
  });

  it("reports the kelvin the device will sit at, not the one requested", () => {
    // The round trip through whole mireds is lossy. Echoing 2700 back would overstate
    // the precision -- the device is actually at 2703 -- and `applied` exists precisely
    // so the answer is what happened rather than what was asked.
    const plan = planControl(extendedColorLightNode(), "matter-51", "color_temp", 2700);

    expect(plan.applied).toEqual({ color_temp: 2703 });
  });

  it("refuses a colour temperature that is not a positive number", () => {
    for (const bad of ["warm", 0, -100, null]) {
      expect(() => planControl(tunableWhiteNode(), "matter-52", "color_temp", bad)).toThrow(
        OpError,
      );
    }
  });
});

describe("reading back what the device actually did", () => {
  it("reports the fan speed the device settled on, not the one requested", () => {
    // The report this came from. Asked for 85%, the fan quantised onto its High mode and
    // sat at 90 -- and GIAP said "speed is set to 85%", which is the number the user
    // typed. A result that echoes the request cannot show that anything happened.
    const fan = node(1, [
      named("Fan"),
      endpoint(1, { fanControl: { fanMode: 3, percentSetting: 85, percentCurrent: 90 } }),
    ]);

    expect(observedFor(fan, "fan_speed")).toEqual({ fan_speed: 90 });
  });

  it("reads percentCurrent rather than the setting that was written", () => {
    // percentSetting is the request stored on the device. Reading it back would echo the
    // request with extra steps and look like it had been verified.
    const disagreeing = node(2, [
      named("Fan"),
      endpoint(1, { fanControl: { percentSetting: 20, percentCurrent: 55 } }),
    ]);

    expect(observedFor(disagreeing, "fan_speed")).toEqual({ fan_speed: 55 });
  });

  it("reads every verb whose result can differ from the request", () => {
    const light = node(3, [
      named("Lamp"),
      endpoint(1, {
        levelControl: { currentLevel: 127 },
        colorControl: { currentHue: 84, currentSaturation: 254, colorTemperatureMireds: 370 },
      }),
    ]);

    expect(observedFor(light, "brightness")).toEqual({ brightness: 50 });
    expect(observedFor(light, "color_temp")).toEqual({ color_temp: 2703 });
    expect(observedFor(light, "color")).toEqual({ hue: 119, saturation: 100 });
  });

  it("says nothing for a verb that cannot land somewhere else", () => {
    // A boolean has nowhere else to land, so waiting for a report buys nothing. Empty
    // here is what tells the controller not to wait.
    expect(observedFor(lightNode(), "power")).toEqual({});
    expect(observedFor(lightNode(), "locked")).toEqual({});
  });

  it("says nothing when the device reports no value for the verb", () => {
    // Absent is not evidence of a different value: the plan's own applied stands rather
    // than a reading being invented.
    expect(observedFor(lightNode(), "fan_speed")).toEqual({});
    expect(observedFor(lightNode(), "tilt")).toEqual({});
  });

  it("accepts every verb at the wire boundary", () => {
    // `server.ts` rejects a verb this set does not hold, and `color_temp` was missing
    // from it for a whole commit -- unreachable in production while every unit test
    // passed, because these tests call planControl directly and never cross that check.
    for (const verb of ["power", "brightness", "color", "color_temp", "fan_speed", "mode"]) {
      expect(VERBS.has(verb), `'${verb}' would be refused as an unknown verb`).toBe(true);
    }
  });
});

describe("media control", () => {
  it("writes the volume to the speaker's endpoint, not the player's", () => {
    const plan = planControl(videoPlayerNode(), "matter-81", "volume", 50);

    expect(plan.actions).toEqual([
      { kind: "write", endpoint: 2, cluster: "levelControl", attribute: "currentLevel", value: 127 },
    ]);
    expect(plan.applied).toEqual({ volume: 50 });
  });

  it("refuses a volume on a device with no speaker", () => {
    expect(() => planControl(lightNode(), "matter-2", "volume", 50)).toThrow(OpError);
  });

  it("sends playback commands to MediaPlayback", () => {
    const plan = planControl(videoPlayerNode(), "matter-81", "operation", "pause");

    expect(plan.actions).toEqual([
      { kind: "command", endpoint: 1, cluster: "mediaPlayback", command: "pause", payload: {} },
    ]);
  });

  it("selects an input by the index behind the device's own label", () => {
    // The label is the device's; the index is what goes on the wire.
    const plan = planControl(videoPlayerNode(), "matter-81", "mode", {
      setting: "input",
      value: "HDMI 2",
    });

    expect(plan.actions).toEqual([
      { kind: "command", endpoint: 1, cluster: "mediaInput", command: "selectInput", payload: { index: 2 } },
    ]);
  });

  it("refuses an input the television never offered", () => {
    expect(() =>
      planControl(videoPlayerNode(), "matter-81", "mode", { setting: "input", value: "SCART" }),
    ).toThrow(OpError);
  });

  it("reads the volume back off the speaker", () => {
    expect(observedFor(videoPlayerNode(), "volume")).toEqual({ volume: 50 });
    // And does not report the same level under the wrong name: a television has no
    // brightness to read, so reading one would be the volume wearing a disguise.
    expect(observedFor(videoPlayerNode(), "brightness")).toEqual({});
    // A bulb is unaffected -- its level is still a brightness.
    expect(observedFor(lightNode(), "brightness")).toEqual({ brightness: 50 });
  });
});
