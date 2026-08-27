import { describe, expect, it } from "vitest";

import { isVendorCluster } from "../src/mapping/snapshot.js";

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
