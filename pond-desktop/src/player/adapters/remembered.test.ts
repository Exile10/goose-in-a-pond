import { describe, expect, it } from "vitest";
import { rememberedIn } from "./remembered";

function memory() {
  const data = new Map<string, string>();
  return {
    getItem: (k: string) => (data.has(k) ? (data.get(k) as string) : null),
    setItem: (k: string, v: string) => void data.set(k, v),
    removeItem: (k: string) => void data.delete(k),
    data,
  };
}

describe("rememberedIn", () => {
  it("is no until someone signs in, then yes, and forgetting makes it no again", () => {
    const store = memory();
    const r = rememberedIn("k", store);
    expect(r.remembered()).toBe(false);
    r.remember();
    expect(r.remembered()).toBe(true);
    expect(store.data.get("k")).toBe("1");
    r.forget();
    expect(r.remembered()).toBe(false);
  });

  it("is kept under the key it is given, so services do not remember each other", () => {
    const store = memory();
    rememberedIn("apple", store).remember();
    expect(rememberedIn("apple", store).remembered()).toBe(true);
    expect(rememberedIn("tidal", store).remembered()).toBe(false);
  });

  it("reads anything but a plain yes as no", () => {
    const store = memory();
    store.setItem("k", "true");
    expect(rememberedIn("k", store).remembered()).toBe(false);
  });

  it("never breaks a sign-in when storage throws: it reads as no and writes are dropped", () => {
    const broken = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
      removeItem: () => {
        throw new Error("blocked");
      },
    };
    const r = rememberedIn("k", broken);
    expect(r.remembered()).toBe(false);
    expect(() => r.remember()).not.toThrow();
    expect(() => r.forget()).not.toThrow();
  });

  it("reads as no when there is no storage at all", () => {
    const r = rememberedIn("k", undefined);
    expect(r.remembered()).toBe(false);
    expect(() => r.remember()).not.toThrow();
  });
});
