import { describe, expect, it } from "vitest";

import { describeNode } from "../src/mapping/describe.js";
import {
  airConditionerNode,
  bareLockNode,
  customLightNode,
  describedNode,
  doorLockNode,
  endpoint,
  extendedColorLightNode,
  fanNode,
  lightNode,
  mvdColorLightNode,
  named,
  node,
  tunableWhiteNode,
} from "./fixtures.js";

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

  it("offers every colour control the device claims, not just hue", () => {
    // The report this came from: asked what the Extended Color Light could do, GIAP
    // answered power, brightness and hue/saturation -- for a device whose own Color mode
    // dropdown offered hue/saturation, XY and colour temperature. Temperature was
    // missing entirely, and it is the one a person actually asks for ("warmer").
    const verbs = describeNode(extendedColorLightNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("color");
    expect(verbs).toContain("color_temp");
  });

  it("states the kelvin range the device says it can reach", () => {
    // Mireds invert: the SMALLEST mired value is the HOTTEST colour, so 153..500 mireds
    // is 2000..6536 K and not the other way round. Getting it backwards yields a range
    // whose minimum exceeds its maximum, which reads as a broken device.
    const temp = describeNode(extendedColorLightNode()).capabilities.find(
      c => c.verb === "color_temp",
    );

    expect(temp?.value).toEqual({ kind: "number", unit: "K", min: 2000, max: 6536 });
  });

  it("believes the attributes when the capability bitmap claims nothing", () => {
    // Google's Matter Virtual Device, exactly: three colour modes in its own Controller
    // tab and a colorCapabilities bitmap claiming none of them. Trusting the bitmap
    // outright described that light as having no colour at all -- strictly worse than
    // the over-claiming it replaced, because the control is right there in the app.
    const verbs = describeNode(mvdColorLightNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("color");
    expect(verbs).toContain("color_temp");
  });

  it("does not offer a hue to a bulb that only does white", () => {
    // A tunable-white bulb has ColorControl and no hue whatsoever. Offering one is a
    // command the device rejects -- the same failure as offering tilt to a roller blind.
    const verbs = describeNode(tunableWhiteNode()).capabilities.map(c => c.verb);

    expect(verbs).toContain("color_temp");
    expect(verbs).not.toContain("color");
  });

  it("invents no kelvin range when the device states none", () => {
    // The spec's own default for colorTempPhysicalMinMireds is 0, which converts to
    // infinite kelvin. The capability stands; the range does not get made up.
    const silent = node(53, [
      named("Bulb"),
      endpoint(1, { colorControl: { colorCapabilities: 0x10 } }, [0x010c]),
    ]);
    const temp = describeNode(silent).capabilities.find(c => c.verb === "color_temp");

    expect(temp?.value).toEqual({ kind: "number", unit: "K" });
  });

  it("describes only what the device has", () => {
    const description = describeNode(lightNode());
    const verbs = description.capabilities.map(c => c.verb);

    expect(verbs).toEqual(["power", "brightness"]);
    expect(verbs).not.toContain("fan_mode");
    expect(verbs).not.toContain("locked");
    expect(description.sensors).toEqual([]);
    // The overwhelmingly common case, and the one a renderer must not print a line for.
    expect(description.vendor_clusters).toEqual([]);
    expect(description.states).toEqual([]);
  });

  it("says a device has a custom cluster rather than implying it has none", () => {
    // Google's Matter Virtual Device shows a Flip-Flop toggle and an Emoticon field
    // under Custom Clusters. Asked what this light could do, the agent answered "power
    // and brightness" -- true of what GIAP could see, and read by the user as a claim
    // that the two controls in front of them did not exist.
    const description = describeNode(customLightNode());

    expect(description.vendor_clusters).toEqual([{ cluster_id: 0xfff1fc01, endpoint: 1 }]);
  });

  it("keeps a custom cluster out of the verbs, since none of them can drive it", () => {
    // Anything describable is callable, by construction. A vendor cluster has no verb,
    // no name for its attributes, and no command matter.js can resolve -- so admitting
    // one here would trade that property for a control the agent still cannot work.
    const description = describeNode(customLightNode());

    expect(description.capabilities.map(c => c.verb)).toEqual(["power", "brightness"]);
  });

  it("ignores a custom cluster on the root endpoint, which is not the device", () => {
    // Endpoint 0 is the node's own plumbing. A vendor cluster there is not something
    // the light does, and reporting it as such would send the user looking for a
    // control their app does not show.
    const rootOnly = node(31, [
      endpoint(0, { basicInformation: { nodeLabel: "Custom Light" } }, [0x0016], [
        { id: 0xfff1fc02 },
      ]),
      endpoint(1, { onOff: { onOff: false } }, [0x0100]),
    ]);

    expect(describeNode(rootOnly).vendor_clusters).toEqual([]);
  });

  it("names what a lock reports and cannot be told to be", () => {
    // The report this came from: asked what the Door Lock could do, GIAP answered
    // "locked or unlocked. It does not measure any data" -- for a device whose own app
    // showed a door state and a PIN requirement beside the lock state. Both sat in the
    // snapshot the whole time; there was no slot in the description to put them in.
    const description = describeNode(doorLockNode());

    expect(description.capabilities.map(c => c.verb)).toEqual(["locked"]);
    expect(description.states).toEqual([
      {
        name: "door",
        value: {
          kind: "enum",
          values: ["open", "closed", "jammed", "forced open", "unspecified error", "ajar"],
        },
      },
      { name: "pin_required", value: { kind: "enum", values: ["required", "not required"] } },
    ]);
  });

  it("keeps a lock's PIN requirement out of the verbs", () => {
    // Read, never written. Every writable attribute on DoorLock is a security control,
    // and a verb for one puts "turn off the pin requirement" a sentence away.
    const verbs = describeNode(doorLockNode()).capabilities;

    expect(verbs.map(c => c.verb)).not.toContain("mode");
    expect(verbs.map(c => c.setting)).not.toContain("pin_required");
  });

  it("promises no door reading for a lock that has no position sensor", () => {
    // DoorPositionSensor and PinCredential are both optional. Declaring either on a
    // plain deadbolt would promise a reading that never arrives -- the same failure as
    // offering tilt to a roller blind.
    expect(describeNode(bareLockNode()).states).toEqual([]);
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

  it("offers an air conditioner only the range it can cool to", () => {
    // The report this came from: "you can set its target temperature between 7 and 32 C"
    // for a cooling-only air conditioner. 7 is the floor of a HEATING setpoint the device
    // does not implement, unioned in because presence was inferred from a key rather than
    // a value -- and matter.js keys every attribute in the cluster model, unsupported
    // ones included. Verified against a live device: controlSequenceOfOperation says
    // CoolingOnly and occupiedHeatingSetpoint has no value at all.
    expect(capability(airConditionerNode(), "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      min: 16,
      max: 32,
      // Names which setpoint moved, and carries no "reaches ... across its modes" tail:
      // there is no other mode, and the existing heat-only case settled that the label
      // itself stays. What was wrong was the RANGE, not the qualifier.
      when: "while cooling",
    });
  });

  it("reads a system mode matter.js decoded to its enum name", () => {
    // "Cool" compared against 3 is never equal, so the mode read as unsettled and the
    // description fell back to the union of both setpoints' ranges.
    const spec = capability(airConditionerNode(), "target_temp")?.value;

    // Settled on the cooling setpoint, so the floor is the COOLING minimum (16) and not
    // the heating one (7) that the union would have supplied.
    expect(spec).toMatchObject({ min: 16 });
  });

  it("keeps the setpoints from crossing when no deadband is stated", () => {
    // An absent deadband means zero, not "no rule": heating still may not pass
    // cooling.
    const noDeadband = node(92, [
      named("Thermostat"),
      endpoint(1, {
        // States that it both heats and cools (controlSequenceOfOperation 4), which is
        // the premise the deadband rule needs: the cap exists to stop TWO setpoints
        // crossing. Without it this fixture reads as cool-only -- it has a cooling
        // setpoint and no heating one -- and a cool-only device has no heating ceiling
        // to report.
        thermostat: {
          controlSequenceOfOperation: 4,
          absMaxHeatSetpointLimit: 3000,
          occupiedCoolingSetpoint: 2400,
        },
      }),
    ]);

    expect(capability(noDeadband, "target_temp")?.value).toEqual({
      kind: "number",
      unit: "C",
      max: 24,
    });
  });
});
