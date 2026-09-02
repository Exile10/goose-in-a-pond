import { describe, expect, it } from "vitest";

import { parseArgs, parseDevice } from "../tools/virtual-device-args.js";

/**
 * The harness's command line, which is the part of it that can be tested without a
 * fabric. Everything else needs a running `ServerNode`, so this is where the grammar
 * is pinned — the three device shapes it exists to express, and the errors that
 * would otherwise be discovered by reading a matter.js stack trace.
 */
describe("virtual device arguments", () => {
  it("builds an ordinary device", () => {
    const spec = parseArgs(["--device", "dimmable-light"]);

    expect(spec.device?.device).toBe("dimmable-light");
    expect(spec.bridged).toEqual([]);
    expect(spec.parts).toEqual([]);
  });

  it("builds a composed device, whose parts are parts and not devices", () => {
    // An oven carries nothing but Identify itself -- its own matter.js definition
    // says it "contains one or more cabinets". The negative control for bridging:
    // this must stay ONE device however many endpoints it has.
    const spec = parseArgs([
      "--device", "oven",
      "--part", "temperature-controlled-cabinet",
      "--part", "cook-surface",
    ]);

    expect(spec.device?.device).toBe("oven");
    expect(spec.device?.parts.map(p => p.device)).toEqual([
      "temperature-controlled-cabinet",
      "cook-surface",
    ]);
    expect(spec.bridged).toEqual([]);
  });

  it("builds a bridge, where each child is its own device", () => {
    const spec = parseArgs([
      "--bridged", "dimmable-light=Kitchen",
      "--bridged", "door-lock=Front Door",
    ]);

    expect(spec.device).toBeUndefined();
    expect(spec.bridged.map(b => [b.device, b.label, b.id])).toEqual([
      ["dimmable-light", "Kitchen", "kitchen"],
      ["door-lock", "Front Door", "front-door"],
    ]);
  });

  it("attaches a part to whatever was named last", () => {
    // The rule that makes a composed device BEHIND a bridge expressible, which is
    // the shape where endpoint numbering stops being a usable guide.
    const spec = parseArgs([
      "--bridged", "dimmable-light=Kitchen",
      "--bridged", "basic-video-player=Telly",
      "--part", "speaker",
    ]);

    expect(spec.bridged[0]?.parts).toEqual([]);
    expect(spec.bridged[1]?.parts.map(p => p.device)).toEqual(["speaker"]);
  });

  it("forces endpoint numbers, so a part can sit below its own parent", () => {
    // A real hub allocates its children's numbers in its own order, so a bridged
    // device can be at a HIGHER endpoint than one of its parts. Anything reading
    // "the lowest endpoint carrying this cluster" answers wrongly there, and without
    // this the case cannot be built at all.
    const spec = parseArgs(["--bridged", "basic-video-player=Telly@7", "--part", "speaker@3"]);

    expect(spec.bridged[0]?.number).toBe(7);
    expect(spec.bridged[0]?.parts[0]?.number).toBe(3);
  });

  it("keeps every endpoint id distinct, because stdin addresses them by id", () => {
    const spec = parseArgs([
      "--bridged", "dimmable-light",
      "--bridged", "dimmable-light",
      "--bridged", "dimmable-light",
    ]);

    expect(spec.bridged.map(b => b.id)).toEqual([
      "dimmable-light",
      "dimmable-light-2",
      "dimmable-light-3",
    ]);
  });

  it("routes --attr to the endpoint it names, parts included", () => {
    // matter.js enforces Matter conformance device-side, so several device types
    // refuse to start without their mandatory attributes. This is how they are given.
    const spec = parseArgs([
      "--bridged", "door-lock=Front",
      "--part", "speaker",
      "--attr", "front.doorLock.lockType=0",
      "--attr", "front.doorLock.actuatorEnabled=true",
      "--attr", "speaker.levelControl.currentLevel=128",
    ]);

    expect(spec.bridged[0]?.state).toEqual({
      doorLock: { lockType: 0, actuatorEnabled: true },
    });
    expect(spec.bridged[0]?.parts[0]?.state).toEqual({
      levelControl: { currentLevel: 128 },
    });
  });

  it("reads vendor and product ids as hex, the way MVD's own form shows them", () => {
    const spec = parseArgs(["--device", "on-off-light", "--vendor-id", "0xFFF1", "--product-id", "0x8000"]);

    expect(spec.vendorId).toBe(0xfff1);
    expect(spec.productId).toBe(0x8000);
  });

  it("gives each instance its own storage, so two can run at once", () => {
    const one = parseArgs(["--device", "on-off-light", "--id", "one"]);
    const two = parseArgs(["--device", "on-off-light", "--id", "two"]);

    expect(one.storage).not.toBe(two.storage);
  });

  it("refuses a device with nothing to be", () => {
    expect(() => parseArgs([])).toThrow(/give --device/);
  });

  it("refuses a part with nothing to attach to", () => {
    expect(() => parseArgs(["--part", "speaker"])).toThrow(/needs a --device or --bridged/);
  });

  it("refuses an --attr naming an endpoint that does not exist", () => {
    // Silently dropping it would leave a device that refuses to start for a reason
    // the flag was meant to fix.
    expect(() => parseArgs(["--bridged", "door-lock=Front", "--attr", "kitchen.onOff.onOff=true"]))
      .toThrow(/not one of: front/);
  });

  it("names the mistake for a flag it does not know", () => {
    expect(() => parseArgs(["--device", "on-off-light", "--colour", "red"])).toThrow(/unknown option/);
  });

  it("refuses a device type that is not a module name", () => {
    expect(() => parseDevice("DimmableLight", "x")).toThrow(/kebab-case/);
  });
});
