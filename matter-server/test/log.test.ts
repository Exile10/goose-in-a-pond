import { describe, expect, it } from "vitest";

import { describeError, redactSetupCode, setupCodeKind } from "../src/log.js";

describe("setup code redaction", () => {
  // A Matter pairing code grants fabric access. A leaked one in a log file is a
  // working credential for anyone who reads it, so these are the shapes that must
  // never survive into a log line or an error message the API serves.
  it("removes QR payloads", () => {
    expect(redactSetupCode("commissioning MT:Y.K9042C00KA0648G00 failed")).toBe(
      "commissioning [redacted:setup-code] failed",
    );
  });

  it("removes manual pairing codes and passcodes", () => {
    expect(redactSetupCode("code 34970112332 rejected")).toBe("code [redacted:setup-code] rejected");
    expect(redactSetupCode("passcode 20202021 rejected")).toBe(
      "passcode [redacted:setup-code] rejected",
    );
    expect(redactSetupCode("long 749701123320000000000 x")).toBe(
      "long [redacted:setup-code] x",
    );
  });

  it("leaves digit runs of other lengths alone", () => {
    // Over-eager on length would redact node ids, ports and timestamps, and a log
    // that redacts everything is as useless as one that redacts nothing.
    expect(redactSetupCode("node 18 on port 5580")).toBe("node 18 on port 5580");
    expect(redactSetupCode("took 1234567 ms")).toBe("took 1234567 ms");
  });

  it("does not leave a readable fragment of a code behind", () => {
    // The QR rule runs first for exactly this reason: a digit rule biting a piece out
    // of a payload would blank part of it and print the rest.
    const redacted = redactSetupCode("MT:Y.K9042C00KA0648G00");
    expect(redacted).toBe("[redacted:setup-code]");
    expect(redacted).not.toMatch(/\d{4}/);
  });

  it("is idempotent", () => {
    const once = redactSetupCode("code 34970112332");
    expect(redactSetupCode(once)).toBe(once);
  });

  it("classifies a code without revealing it", () => {
    expect(setupCodeKind("MT:Y.K9042C00KA0648G00")).toBe("qr_payload");
    expect(setupCodeKind("3497-011-2332")).toBe("pairing_code");
    expect(setupCodeKind("20202021")).toBe("passcode");
    expect(setupCodeKind("nonsense")).toBe("unknown");
  });
});

describe("error description", () => {
  it("renders the whole cause chain, not just the outermost layer", () => {
    // The bug this exists for: a failed discovery reported "discovery of node
    // discovery failed" and dropped the cause, which is the only part that says
    // what to do about it.
    const cause = new Error("no usable network interface");
    const wrapped = new Error("discovery of node discovery failed", { cause });

    const described = describeError(wrapped);
    expect(described).toContain("no usable network interface");
    expect(described).toContain("discovery of node");
  });

  it("does not repeat a message already embedded in its wrapper", () => {
    const cause = new Error("EMSGSIZE");
    expect(describeError(new Error("EMSGSIZE", { cause }))).toBe("EMSGSIZE");
  });

  it("survives a cyclic chain", () => {
    const a = new Error("a") as Error & { cause?: unknown };
    const b = new Error("b", { cause: a }) as Error & { cause?: unknown };
    a.cause = b;
    expect(describeError(b)).toBe("b: a");
  });

  it("redacts the chain, not only the outermost message", () => {
    const cause = new Error("PASE failed for MT:Y.K9042C00KA0648G00");
    const described = describeError(new Error("commissioning failed", { cause }));
    expect(described).not.toContain("MT:");
  });

  it("always says something", () => {
    expect(describeError(new Error(""))).toBe("an error with no message");
  });
});
