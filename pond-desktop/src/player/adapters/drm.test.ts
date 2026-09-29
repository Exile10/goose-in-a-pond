import { afterEach, describe, expect, it, vi } from "vitest";
import { KNOWN_BAD_MODULES, widevineProblem } from "./drm";

afterEach(() => vi.unstubAllGlobals());

function browser(access: () => Promise<unknown>) {
  vi.stubGlobal("navigator", { requestMediaKeySystemAccess: vi.fn(access) });
  return (navigator as unknown as { requestMediaKeySystemAccess: ReturnType<typeof vi.fn> })
    .requestMediaKeySystemAccess;
}

describe("widevineProblem", () => {
  it("refuses a module the services are known to reject, by name, before it asks the browser", async () => {
    const ask = browser(async () => ({}));
    const problem = await widevineProblem("Spotify", "4.10.3050.0");
    expect(problem).toContain("4.10.3050.0");
    expect(problem).toContain("Spotify");
    expect(problem).toContain("castlabs/electron-releases#237");
    expect(ask).not.toHaveBeenCalled();
  });

  it("lets a module that is not on the list through to the browser's own answer", async () => {
    const ask = browser(async () => ({}));
    for (const version of ["4.10.3112.0", "4.10.3050.1", undefined]) {
      expect(await widevineProblem("Apple Music", version), String(version)).toBeNull();
    }
    expect(ask).toHaveBeenCalled();
  });

  it("still says when the browser has no Widevine at all", async () => {
    browser(async () => {
      throw new Error("no key system");
    });
    expect(await widevineProblem("Apple Music", "4.10.3112.0")).toContain("no Widevine module");
    vi.stubGlobal("navigator", {});
    expect(await widevineProblem("Apple Music")).toContain("no DRM support");
  });

  it("lists only versions with a reason, so a fixed build is never caught by name", () => {
    expect(Object.keys(KNOWN_BAD_MODULES)).toEqual(["4.10.3050.0"]);
    for (const reason of Object.values(KNOWN_BAD_MODULES)) expect(reason.length).toBeGreaterThan(20);
  });
});
