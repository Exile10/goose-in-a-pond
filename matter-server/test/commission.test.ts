import { describe, expect, it } from "vitest";

import { commissioningOptions } from "../src/controller.js";
import { setupCodeKind } from "../src/log.js";
import { OpError } from "../src/protocol.js";

/** What `commission` does with a code: classify it, then choose matter.js options. */
function optionsFor(code: string) {
  return commissioningOptions(code.trim(), setupCodeKind(code.trim()));
}

describe("commissioning options", () => {
  it("decodes a QR payload here, because matter.js cannot", () => {
    // Google's Matter Virtual Device shows exactly this payload under its QRCode
    // button, and commissioning it failed in TWO MILLISECONDS -- before anything
    // reached the network -- with "Invalid pairing code". matter.js's
    // `commission({pairingCode})` runs `ManualPairingCodeCodec.decode`
    // unconditionally, and that codec strips every non-digit before checking the
    // length, so this payload arrives as twelve stray digits and is rejected.
    //
    // The values are the ones the payload really carries: vendor 0xfff1, product
    // 0x8000, long discriminator 3840, passcode 20202021.
    expect(optionsFor("MT:Y.K9042C00KA0648G00")).toEqual({
      passcode: 20202021,
      discriminator: 3840,
    });
  });

  it("takes the long discriminator, which the manual form cannot carry", () => {
    // Worth asserting separately: the manual code for the same device carries only a
    // SHORT discriminator (the top 4 bits), so a QR payload narrows the mDNS browse
    // to one device where the manual form cannot. Passing `pairingCode` through would
    // have thrown away the difference even if the decoder had accepted it.
    const options = optionsFor("MT:Y.K9042C00KA0648G00");
    expect(options).toHaveProperty("discriminator", 3840);
    expect(options).not.toHaveProperty("pairingCode");
  });

  it("accepts a QR payload typed in lower case", () => {
    // The base-38 alphabet is uppercase, and the QR codec matches `MT:`
    // case-sensitively, so normalising is lossless and spares the user a retype.
    expect(optionsFor("mt:y.k9042c00ka0648g00")).toEqual({
      passcode: 20202021,
      discriminator: 3840,
    });
  });

  it("passes a manual pairing code to matter.js, whose decoder reads that form", () => {
    expect(optionsFor("3497-011-2332")).toEqual({ pairingCode: "3497-011-2332" });
  });

  it("passes a bare passcode as a number", () => {
    expect(optionsFor(" 2020-2021 ")).toEqual({ passcode: 20202021 });
  });

  it("calls a malformed QR payload an invalid code, not a failed commission", () => {
    // `commission_failed` means pairing was attempted and did not complete. A payload
    // that cannot be decoded never leaves this process, and reporting it as a failed
    // commission is what produced "commissioning failed: Invalid pairing code:
    // commission_failed" -- three layers, none of them the actionable one.
    let raised: unknown;
    try {
      optionsFor("MT:...");
    } catch (error) {
      raised = error;
    }
    expect(raised).toBeInstanceOf(OpError);
    expect((raised as OpError).code).toBe("invalid_setup_code");
  });

  it("refuses a payload carrying several devices rather than silently taking one", () => {
    let raised: unknown;
    try {
      optionsFor("MT:Y.K9042C00KA0648G00*Y.K9042C00KA0648G00");
    } catch (error) {
      raised = error;
    }
    expect(raised).toBeInstanceOf(OpError);
    expect((raised as OpError).code).toBe("invalid_setup_code");
    expect((raised as OpError).message).toContain("one at a time");
  });
});
