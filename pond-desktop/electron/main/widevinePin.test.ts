import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";

// The script is CommonJS, run by Electron; its pure parts are exported for this.
const pin = createRequire(import.meta.url)("../../scripts/widevine-pin.cjs") as {
  compareVersions(a: string, b: string): number;
  installedVersions(names: string[]): string[];
  pickModule(c: Array<{ version: string; dir: string }>): { version: string; dir: string } | undefined;
  lockOwner(target: unknown): number | undefined;
  chromeModules(
    fsLike: unknown,
    home: string,
    platformDir: string,
  ): Array<{ version: string; dir: string }>;
};

describe("compareVersions", () => {
  it("orders the versions that matter here", () => {
    expect(pin.compareVersions("4.10.3112.0", "4.10.3050.0")).toBeGreaterThan(0);
    expect(pin.compareVersions("4.10.3050.1", "4.10.3050.0")).toBeGreaterThan(0);
    expect(pin.compareVersions("4.10.3050.0", "4.10.3112.0")).toBeLessThan(0);
    expect(pin.compareVersions("4.10.3050.0", "4.10.3050.0")).toBe(0);
  });

  it("compares numbers, not text, and treats a missing part as zero", () => {
    expect(pin.compareVersions("4.10.999.0", "4.10.3050.0")).toBeLessThan(0);
    expect(pin.compareVersions("4.10.3050", "4.10.3050.0")).toBe(0);
    expect(pin.compareVersions("4.9.0.0", "4.10.0.0")).toBeLessThan(0);
  });
});

describe("installedVersions", () => {
  it("keeps only version folders, newest first", () => {
    expect(
      pin.installedVersions(["4.10.3050.0", "latest-component-updated-widevine-cdm", "4.10.3112.0", ".DS_Store", "x.y"]),
    ).toEqual(["4.10.3112.0", "4.10.3050.0"]);
    expect(pin.installedVersions([])).toEqual([]);
  });
});

describe("pickModule", () => {
  it("takes the newest and ignores anything that is not a version", () => {
    const best = pin.pickModule([
      { version: "4.10.3050.0", dir: "/a" },
      { version: "4.10.3112.0", dir: "/b" },
      { version: "garbage", dir: "/c" },
      { version: "4.10.3112.0", dir: "/d" },
    ]);
    expect(best).toEqual({ version: "4.10.3112.0", dir: "/b" });
  });

  it("says none when there is none", () => {
    expect(pin.pickModule([])).toBeUndefined();
    expect(pin.pickModule([{ version: "nope", dir: "/x" }])).toBeUndefined();
  });
});

describe("lockOwner", () => {
  it("reads the pid from a Chromium SingletonLock target", () => {
    expect(pin.lockOwner("Jerrys-MacBook.local-4242")).toBe(4242);
    expect(pin.lockOwner("host-with-dashes-77")).toBe(77);
  });

  it("names no one for anything else", () => {
    for (const bad of ["", "nohyphenpid", "host-", undefined, null, 42]) {
      expect(pin.lockOwner(bad), String(bad)).toBeUndefined();
    }
  });
});

describe("chromeModules", () => {
  /** A tiny fake filesystem: paths to file text, and directories derived from them. */
  function fakeFs(files: Record<string, string>) {
    const dirs = new Map<string, Set<string>>();
    for (const f of Object.keys(files)) {
      const parts = f.split("/").filter(Boolean);
      for (let i = 1; i <= parts.length; i += 1) {
        const parent = "/" + parts.slice(0, i - 1).join("/");
        (dirs.get(parent) ?? dirs.set(parent, new Set()).get(parent)!).add(parts[i - 1]!);
      }
    }
    return {
      readFileSync: (p: string) => {
        if (!(p in files)) throw new Error("ENOENT");
        return files[p]!;
      },
      existsSync: (p: string) => p in files || dirs.has(p),
      readdirSync: (p: string) => {
        const d = dirs.get(p === "/" ? "/" : p);
        if (!d) throw new Error("ENOENT");
        return [...d];
      },
    };
  }
  const FW = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions";
  const lib = (dir: string) => `${dir}/_platform_specific/mac_arm64/libwidevinecdm.dylib`;

  it("finds the module bundled with each Chrome and the one Chrome updated itself", () => {
    const home = "/Users/j";
    const updated = `${home}/Library/Application Support/Google/Chrome/WidevineCdm/4.10.3200.0`;
    const files: Record<string, string> = {
      [`${FW}/153.0.1/Libraries/WidevineCdm/manifest.json`]: JSON.stringify({ version: "4.10.3112.0" }),
      [lib(`${FW}/153.0.1/Libraries/WidevineCdm`)]: "",
      [`${updated}/manifest.json`]: JSON.stringify({ version: "4.10.3200.0" }),
      [lib(updated)]: "",
    };
    const found = pin.chromeModules(fakeFs(files), home, "mac_arm64");
    expect(found.map((m) => m.version).sort()).toEqual(["4.10.3112.0", "4.10.3200.0"]);
    expect(pin.pickModule(found)?.version).toBe("4.10.3200.0");
  });

  it("skips a folder with no library for this machine's architecture, or a broken manifest", () => {
    const files: Record<string, string> = {
      [`${FW}/153.0.1/Libraries/WidevineCdm/manifest.json`]: JSON.stringify({ version: "4.10.3112.0" }),
      [lib(`${FW}/153.0.1/Libraries/WidevineCdm`).replace("mac_arm64", "mac_x64")]: "",
      [`${FW}/154.0.1/Libraries/WidevineCdm/manifest.json`]: "{not json",
      [lib(`${FW}/154.0.1/Libraries/WidevineCdm`)]: "",
    };
    expect(pin.chromeModules(fakeFs(files), "/Users/j", "mac_arm64")).toEqual([]);
  });

  it("finds nothing, without failing, when Chrome is not installed", () => {
    expect(pin.chromeModules(fakeFs({ "/Applications/Other.app/x": "" }), "/Users/j", "mac_arm64")).toEqual([]);
  });
});
