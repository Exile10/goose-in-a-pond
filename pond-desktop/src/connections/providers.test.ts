import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { PROVIDERS, describeLastSync, describeStatus } from "./providers";

const REPO = join(__dirname, "..", "..", "..");

function rustSource(path: string): string {
  return readFileSync(join(REPO, path), "utf8");
}

describe("provider list", () => {
  /**
   * The ids are written to `context_sources.provider` and read back to rebuild
   * the adapter, so a value this UI offers that the backend cannot parse
   * produces a source that stores fine and can never sync.
   */
  it("offers only calendar providers the CalDAV adapter can rebuild", () => {
    const rust = rustSource("crates/pond-adapters-caldav/src/provider.rs");
    // `"name" =>` matches only the from_stored arms: as_str is the other way
    // round (`Self::Google => "google"`), and the self-hosted arms return
    // `base_url.map(...)` rather than `Some(Self::...)`, so matching on the
    // literal is the shape that catches all five.
    const known = [...rust.matchAll(/"([a-z]+)" =>/g)].map((m) => m[1]);
    expect(known.length).toBeGreaterThan(2);
    for (const p of PROVIDERS.filter((p) => p.kind === "calendar")) {
      expect(known, `calendar provider "${p.id}" is not in from_stored`).toContain(p.id);
    }
  });

  it("offers only mail providers the IMAP adapter can rebuild", () => {
    const rust = rustSource("crates/pond-adapters-imap/src/provider.rs");
    const known = [...rust.matchAll(/"([a-z]+)" =>/g)].map((m) => m[1]);
    expect(known.length).toBeGreaterThan(2);
    for (const p of PROVIDERS.filter((p) => p.kind === "mail")) {
      expect(known, `mail provider "${p.id}" is not in from_stored`).toContain(p.id);
    }
  });

  /** A provider whose self-hosted server cannot be supplied cannot connect. */
  it("asks for a server address for exactly the self-hosted providers", () => {
    const selfHosted = PROVIDERS.filter((p) => p.needsServer).map((p) => p.id);
    expect(selfHosted.sort()).toEqual(["custom", "nextcloud"]);
    for (const p of PROVIDERS.filter((p) => p.needsServer)) {
      expect(p.serverPlaceholder, `${p.label} has no placeholder`).toBeTruthy();
    }
  });

  /** The hint is the whole reason a household gets past the password box. */
  it("gives every provider a setup hint", () => {
    for (const p of PROVIDERS) {
      expect(p.hint.length, `${p.label} has no hint`).toBeGreaterThan(30);
    }
  });
});

describe("status wording", () => {
  it("tells somebody what to do about a refused password", () => {
    const s = describeStatus("needs_reauth");
    expect(s.tone).toBe("warn");
    expect(s.detail).toMatch(/reconnect/i);
  });

  /** Offline is a choice the operator made, not a fault to badge. */
  it("does not present an offline pond as broken", () => {
    const s = describeStatus("paused");
    expect(s.tone).toBe("muted");
    expect(s.detail).toMatch(/nothing is broken/i);
  });

  it("treats an unknown status as needing attention rather than as fine", () => {
    expect(describeStatus("wat").tone).toBe("warn");
  });
});

describe("last sync wording", () => {
  /**
   * "Never checked" and "checked, found nothing" look identical in a list and
   * are completely different problems.
   */
  it("says so when the pond has never reached the account", () => {
    expect(describeLastSync(null)).toMatch(/not checked yet/i);
    expect(describeLastSync("not-a-date")).toMatch(/not checked yet/i);
  });

  it("reads in the units a person would use", () => {
    const now = Date.parse("2026-08-17T12:00:00Z");
    expect(describeLastSync("2026-08-17T11:58:00Z", now)).toMatch(/2 minutes ago/);
    expect(describeLastSync("2026-08-17T09:00:00Z", now)).toMatch(/3 hours ago/);
    expect(describeLastSync("2026-08-15T12:00:00Z", now)).toMatch(/2 days ago/);
    expect(describeLastSync("2026-08-17T11:59:50Z", now)).toMatch(/just now/i);
  });
});
