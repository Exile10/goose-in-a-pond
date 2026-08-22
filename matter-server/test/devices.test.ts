import { describe, expect, it } from "vitest";

import { deviceTypeFromDescriptor, nodeToDevice } from "../src/mapping/devices.js";
import { describedNode, endpoint, fanNode, lightNode, named, node } from "./fixtures.js";

describe("device typing", () => {
  it("keeps a fan with no On/Off cluster powerable, and types it as a fan", () => {
    const device = nodeToDevice(fanNode());
    expect(device.device_type).toBe("fan");
    // Power comes from FanMode here; fan_speed from PercentSetting.
    expect(device.capabilities).toEqual(["power", "fan_speed"]);
    expect(device.name).toBe("Living Room Fan");
    expect(device.id).toBe("matter-18");
  });

  it("reports power once when a fan does implement On/Off", () => {
    const withSwitch = node(18, [
      named("Living Room Fan"),
      endpoint(1, { fanControl: { fanMode: 0 }, onOff: { onOff: false } }),
    ]);
    const device = nodeToDevice(withSwitch);
    expect(device.device_type).toBe("fan");
    expect(device.capabilities).toEqual(["power", "fan_speed"]);
  });

  it("tells a plug from a bulb, which the clusters alone cannot", () => {
    // The regression that started this: both are On/Off, so cluster inference called
    // every plug a light and the UI drew a lightbulb on it. The node says which it is.
    const plug = describedNode(30, 0x010a, { onOff: { onOff: false } });
    expect(nodeToDevice(plug).device_type).toBe("plug");

    const bulb = describedNode(31, 0x0100, { onOff: { onOff: false } });
    expect(nodeToDevice(bulb).device_type).toBe("light");
  });

  it("gives appliances, alarms and coverings their own types", () => {
    const cases: [number, string][] = [
      [0x0075, "appliance"], // Dishwasher
      [0x0073, "appliance"], // Laundry Washer
      [0x0303, "pump"], // Pump
      [0x0028, "media"], // Basic Video Player
      [0x0074, "vacuum"], // Robotic Vacuum
      [0x0076, "alarm"], // Smoke/CO Alarm
      [0x0202, "covering"], // Window Covering
      [0x002c, "air"], // Air Purifier
      [0x002d, "sensor"], // Air Quality Sensor
    ];
    for (const [id, want] of cases) {
      expect(nodeToDevice(describedNode(40, id)).device_type, `device type 0x${id.toString(16)}`)
        .toBe(want);
    }
  });

  it("does not mistake the root endpoint for the device", () => {
    // Endpoint 0 is the Root Node (0x0016) on every device. Reading it would type the
    // whole fabric as one thing.
    expect(deviceTypeFromDescriptor(describedNode(41, 0x0075))).toBe("appliance");
    const rootOnly = node(42, [endpoint(0, {}, [0x0016])]);
    expect(deviceTypeFromDescriptor(rootOnly)).toBeUndefined();
  });

  it("falls back to cluster inference without a usable descriptor", () => {
    expect(deviceTypeFromDescriptor(lightNode())).toBeUndefined();
    expect(nodeToDevice(lightNode()).device_type).toBe("light");

    // An unknown device type id falls through to the clusters too.
    const unknown = describedNode(43, 0xbeef, { onOff: { onOff: false } });
    expect(nodeToDevice(unknown).device_type).toBe("light");
  });

  it("prefers the user's name, then the vendor's, then a speakable fallback", () => {
    const both = node(7, [
      endpoint(0, { basicInformation: { nodeLabel: "Hallway", productName: "Hue color lamp" } }),
      endpoint(1, { onOff: { onOff: false } }),
    ]);
    expect(nodeToDevice(both).name).toBe("Hallway");

    const vendorOnly = node(7, [
      endpoint(0, { basicInformation: { productName: "Hue color lamp" } }),
      endpoint(1, { onOff: { onOff: false } }),
    ]);
    expect(nodeToDevice(vendorOnly).name).toBe("Hue color lamp");

    expect(nodeToDevice(lightNode()).name).toBe("Light 2");
  });

  it("treats a blank name as no name at all", () => {
    // An empty nodeLabel is what a device ships with; taking it verbatim would name
    // every unnamed device the empty string.
    const blank = node(9, [
      endpoint(0, { basicInformation: { nodeLabel: "   ", productName: "Acme Plug" } }),
      endpoint(1, { onOff: { onOff: false } }),
    ]);
    expect(nodeToDevice(blank).name).toBe("Acme Plug");
  });

  it("leaves an unmappable device typed matter with no capabilities", () => {
    // The signal that a mapping is missing, not that the device is broken.
    const opaque = node(50, [endpoint(0, {}), endpoint(1, {})]);
    const device = nodeToDevice(opaque);
    expect(device.device_type).toBe("matter");
    expect(device.capabilities).toEqual([]);
  });
});
