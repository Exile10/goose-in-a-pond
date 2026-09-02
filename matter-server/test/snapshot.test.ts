import { describe, expect, it } from "vitest";

import { applicationEndpoints, isVendorCluster } from "../src/mapping/snapshot.js";
import { deviceIdForNode, partsOfDeviceId } from "../src/protocol.js";
import { endpoint, node } from "./fixtures.js";

describe("vendor clusters", () => {
  it("reads manufacturer ownership off the cluster id, as the spec defines it", () => {
    // A Matter cluster id is 32 bits with the vendor code in the upper 16; a standard
    // cluster's is zero. Nothing here depends on what the controller happens to name
    // the cluster, which is what makes it survive a matter.js upgrade.
    expect(isVendorCluster(0x0006)).toBe(false); // OnOff
    expect(isVendorCluster(0x0008)).toBe(false); // LevelControl
    expect(isVendorCluster(0x0051)).toBe(false); // LaundryWasherMode, high but standard
    expect(isVendorCluster(0xfff1fc01)).toBe(true); // a test vendor's own
  });

  it("does not treat a standard cluster GIAP simply ignores as the maker's own", () => {
    // The snapshot drops identify, groups, powerSource and a dozen more by name. They
    // are not what the user is looking at in the maker's app, and listing them as
    // things the device can do would bury the one entry that is.
    for (const standard of [0x0003, 0x0004, 0x002f, 0x0038]) {
      expect(isVendorCluster(standard)).toBe(false);
    }
  });
});

describe("a slice's endpoint order", () => {
  it("puts the slice's own endpoint first, not the lowest-numbered one", () => {
    // Behind a bridge, endpoint numbers are allocated by the HUB in its own
    // discovery order — so a bridged device can sit ABOVE one of its own parts. Every
    // lookup downstream resolves ties by taking the first endpoint, and the
    // justification for that ("whatever its first endpoint claims, which is what its
    // own UI calls it") holds for a composed device and not for a hub's numbering.
    const bridged = {
      ...node(90, [
        endpoint(0, {}),
        endpoint(3, { levelControl: { currentLevel: 10 } }, [0x0022]),
        endpoint(7, { onOff: { onOff: true } }, [0x0013, 0x0028]),
      ]),
      rootEndpoint: 7,
    };

    expect(applicationEndpoints(bridged).map(e => e.number)).toEqual([7, 3]);
  });

  it("is plain ascending order for a node that is not a slice", () => {
    // The property that makes this change inert until slices exist.
    const plain = node(2, [endpoint(0, {}), endpoint(13, {}), endpoint(4, {})]);

    expect(applicationEndpoints(plain).map(e => e.number)).toEqual([4, 13]);
  });

  it("does not reorder when the slice's own endpoint is already first", () => {
    const bridged = {
      ...node(90, [endpoint(0, {}), endpoint(2, {}), endpoint(9, {})]),
      rootEndpoint: 2,
    };

    expect(applicationEndpoints(bridged).map(e => e.number)).toEqual([2, 9]);
  });
});

describe("device ids", () => {
  it("leaves an ordinary node's id exactly as it was", () => {
    // Existing registry rows and fabric state key off this string, so a node that is
    // not a bridge must not acquire an endpoint component.
    expect(deviceIdForNode(18n)).toBe("matter-18");
    expect(partsOfDeviceId("matter-18")).toEqual({ nodeId: 18n });
  });

  it("names one bridged device of a hub, and reads it back", () => {
    expect(deviceIdForNode(90n, 2)).toBe("matter-90-2");
    expect(partsOfDeviceId("matter-90-2")).toEqual({ nodeId: 90n, rootEndpoint: 2 });
  });

  it("accepts only the canonical spelling", () => {
    // `BigInt("01")` is `1n`, so without the round-trip check `matter-01` and
    // `matter-1` would be two ids for one device — and the registry keys rows on the
    // string. The Rust side refuses the same spellings for the same reason.
    expect(partsOfDeviceId("matter-01")).toBeUndefined();
    expect(partsOfDeviceId("matter-1-02")).toBeUndefined();
    expect(partsOfDeviceId("matter-1-")).toBeUndefined();
    expect(partsOfDeviceId("matter-1-2-3")).toBeUndefined();
    // An endpoint is a u16.
    expect(partsOfDeviceId("matter-1-70000")).toBeUndefined();
    // And not a Matter id at all.
    expect(partsOfDeviceId("mqtt-lamp")).toBeUndefined();
  });
});
