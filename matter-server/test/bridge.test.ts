import { describe, expect, it } from "vitest";

import { planControl } from "../src/mapping/control.js";
import { describeNode } from "../src/mapping/describe.js";
import { nodeToDevice } from "../src/mapping/devices.js";
import { deviceSlices } from "../src/mapping/snapshot.js";
import { stateOf } from "../src/mapping/state.js";
import { bridgeNode, bridgedComposedNode, endpoint, named, node } from "./fixtures.js";

/**
 * A Matter bridge, end to end through the mappings.
 *
 * Every assertion here is something the code got confidently wrong before: a hub
 * collapsed into one device that was simultaneously a light and a lock, whose state
 * was whichever child held the lowest endpoint number, and which could only ever be
 * driven at that one child. "Turn off the hall lamp" turned off the kitchen lamp and
 * reported success.
 */
describe("a bridge", () => {
  const devices = () => deviceSlices(bridgeNode()).map(nodeToDevice);

  it("becomes the hub plus one device per child", () => {
    expect(devices().map(d => d.id)).toEqual([
      "matter-90",
      "matter-90-3",
      "matter-90-4",
      "matter-90-5",
    ]);
  });

  it("gives each child the name its hub reports for it", () => {
    expect(devices().map(d => d.name)).toEqual([
      "Living Room Hub",
      "Kitchen Lamp",
      "Hall Lamp",
      "Front Door",
    ]);
  });

  it("types each child as what it is, not as what its sibling is", () => {
    // The whole node used to type as `light` — the lowest child's type — so the lock
    // was invisible and the hub was a light.
    expect(devices().map(d => d.device_type)).toEqual(["bridge", "light", "light", "lock"]);
  });

  it("gives each child only its own capabilities", () => {
    // One device advertising `power`, `brightness` AND `lock` is a device that does
    // not exist, and nothing in `control` could have said which lamp anyway.
    expect(devices().map(d => d.capabilities)).toEqual([
      [],
      ["power", "brightness"],
      ["power", "brightness"],
      ["lock"],
    ]);
  });

  it("describes a child as one device, with its own verbs", () => {
    const [, kitchen, , front] = deviceSlices(bridgeNode());

    expect(describeNode(kitchen!).capabilities.map(c => c.verb)).toEqual([
      "power",
      "brightness",
    ]);
    expect(describeNode(front!).capabilities.map(c => c.verb)).toEqual(["locked"]);
    // And the lock's door state stays the lock's.
    expect(describeNode(kitchen!).states).toEqual([]);
    expect(describeNode(front!).states.map(s => s.name)).toEqual(["door"]);
  });

  it("reports each child's own state, not the first child's", () => {
    // The fixture is deliberately contradictory: the kitchen lamp is on at full and
    // the hall lamp is off at 4%. One snapshot for the node could only ever report
    // one of them, and reported it for both.
    const [, kitchen, hall] = deviceSlices(bridgeNode());

    const read = (slice: Parameters<typeof stateOf>[0], name: string) =>
      stateOf(slice).values.find(v => v.name === name)?.value;

    expect(read(kitchen!, "power")).toBe("on");
    expect(read(kitchen!, "brightness")).toBe("100%");
    expect(read(hall!, "power")).toBe("off");
    expect(read(hall!, "brightness")).toBe("4%");
  });

  it("drives the child that was asked for", () => {
    // The failure this whole change exists for. `planControl` chose "the lowest
    // endpoint carrying onOff", so every command on this hub went to endpoint 3 —
    // and it LOOKED like it worked, because a light did go off.
    const [, kitchen, hall, front] = deviceSlices(bridgeNode());

    expect(planControl(kitchen!, "matter-90-3", "power", false).actions[0]?.endpoint).toBe(3);
    expect(planControl(hall!, "matter-90-4", "power", false).actions[0]?.endpoint).toBe(4);
    expect(planControl(front!, "matter-90-5", "locked", true).actions[0]?.endpoint).toBe(5);
  });

  it("refuses a capability the addressed child has not got", () => {
    // Previously the union of every child's capabilities was offered on one device,
    // so "lock the kitchen lamp" planned an action against the lock.
    const [, kitchen] = deviceSlices(bridgeNode());

    expect(() => planControl(kitchen!, "matter-90-3", "locked", true)).toThrow();
  });

  it("keeps the hub itself undrivable", () => {
    const [hub] = deviceSlices(bridgeNode());

    expect(describeNode(hub!).capabilities).toEqual([]);
    expect(nodeToDevice(hub!).capabilities).toEqual([]);
  });
});

describe("a composed device behind a bridge", () => {
  it("is typed by the device, not by whichever endpoint the hub numbered lowest", () => {
    // The player is at endpoint 7 and its Speaker part at 3, because the hub
    // allocates its children's numbers in its own order. Ascending order would type
    // this a `media`-less speaker; root-first types it as the player it is.
    const [, telly] = deviceSlices(bridgedComposedNode());

    expect(nodeToDevice(telly!).device_type).toBe("media");
    expect(nodeToDevice(telly!).id).toBe("matter-91-7");
  });

  it("keeps the part's cluster reachable as the device's own", () => {
    // A television with no volume is the failure if the part is left behind.
    const [, telly] = deviceSlices(bridgedComposedNode());

    expect(nodeToDevice(telly!).capabilities).toContain("volume");
    expect(planControl(telly!, "matter-91-7", "volume", 50).actions[0]?.endpoint).toBe(3);
  });
});

describe("a bridged device's reachability", () => {
  const hubWith = (reachable: unknown) =>
    node(98, [
      named("Hub"),
      endpoint(1, {}, [0x000e], [], [2]),
      endpoint(
        2,
        {
          onOff: { onOff: true },
          bridgedDeviceBasicInformation: { nodeLabel: "Bulb", reachable },
        },
        [0x0013, 0x0100],
      ),
    ]);

  it("is offline when its hub says it cannot be reached", () => {
    // A Zigbee bulb whose battery died is still behind a healthy bridge, and the
    // node's own liveness cannot say so.
    const [hub, bulb] = deviceSlices(hubWith(false)).map(nodeToDevice);

    expect(hub!.online).toBe(true);
    expect(bulb!.online).toBe(false);
  });

  it("is online when the hub has not said yet", () => {
    // Fails OPEN. `bridgedDeviceBasicInformation` arrives in a subscription report
    // like everything else, so treating "not said yet" as unreachable would show a
    // dozen offline devices behind a perfectly healthy hub for its first seconds.
    const [, unstated] = deviceSlices(hubWith(undefined)).map(nodeToDevice);
    expect(unstated!.online).toBe(true);
  });

  it("is offline when the hub itself is", () => {
    const dead = { ...hubWith(true), online: false };
    expect(deviceSlices(dead).map(nodeToDevice).every(d => !d.online)).toBe(true);
  });
});
