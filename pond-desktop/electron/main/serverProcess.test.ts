import { describe, it, expect, vi } from "vitest";
import {
  resolveServerUrl,
  resolveServerBinary,
  recoveryBackoffSeconds,
  ServerProcess,
  SPAWNED_POLL_ATTEMPTS,
  PARENT_MANAGED_POLL_ATTEMPTS,
  type ServerDeps,
} from "./serverProcess";

// Ports the 6 tests from src-tauri/src/process.rs and the 2 from main.rs, none
// of which ever ran in CI, and adds the branches they left uncovered -- in
// particular that the two polling budgets are genuinely different, which is
// the bug that shows up as an error screen on a server that was about to work.

describe("resolveServerUrl", () => {
  it("uses port 4000 and self-managed mode when GIAP_SERVER_PORT is unset", () => {
    expect(resolveServerUrl(undefined)).toEqual({
      url: "http://127.0.0.1:4000",
      parentManaged: false,
    });
  });

  it("adopts the parent's port and refuses to spawn", () => {
    expect(resolveServerUrl("8080")).toEqual({
      url: "http://127.0.0.1:8080",
      parentManaged: true,
    });
  });

  // An exported-but-blank variable must not put us into parent-managed mode
  // pointed at a malformed URL.
  it("treats a blank value as unset", () => {
    expect(resolveServerUrl("").parentManaged).toBe(false);
    expect(resolveServerUrl("   ").parentManaged).toBe(false);
  });
});

describe("resolveServerBinary", () => {
  const base = {
    isPackaged: false,
    resourcesPath: "/App.app/Contents/Resources",
    repoRoot: "/repo",
    platform: "darwin" as NodeJS.Platform,
  };

  it("prefers POND_SERVER_BIN, even inside a packaged app", () => {
    const path = resolveServerBinary({
      ...base,
      isPackaged: true,
      override: "/custom/pond-server",
      exists: (p) => p === "/custom/pond-server",
    });
    expect(path).toBe("/custom/pond-server");
  });

  it("ignores an override that does not exist", () => {
    const path = resolveServerBinary({
      ...base,
      override: "/gone/pond-server",
      exists: (p) => p === "/repo/target/release/pond-server",
    });
    expect(path).toBe("/repo/target/release/pond-server");
  });

  // The whole reason the Rust probed the running executable's siblings first
  // was to stop a packaged app falling back to a stray binaries/ folder in the
  // cwd. app.isPackaged is a hard boolean, so a packaged app looks in exactly
  // one place and nowhere else.
  it("looks only in Resources when packaged", () => {
    const seen: string[] = [];
    const path = resolveServerBinary({
      ...base,
      isPackaged: true,
      exists: (p) => {
        seen.push(p);
        return p === "/App.app/Contents/Resources/pond-server";
      },
    });
    expect(path).toBe("/App.app/Contents/Resources/pond-server");
    expect(seen).toEqual(["/App.app/Contents/Resources/pond-server"]);
  });

  it("returns null rather than a dev path when packaged and absent", () => {
    expect(resolveServerBinary({ ...base, isPackaged: true, exists: () => false })).toBe(null);
  });

  it("prefers a release build over a debug one in dev", () => {
    const path = resolveServerBinary({ ...base, exists: () => true });
    expect(path).toBe("/repo/target/release/pond-server");
  });

  it("falls back to the staged sidecar in dev", () => {
    const path = resolveServerBinary({
      ...base,
      exists: (p) => p === "/repo/pond-desktop/resources/pond-server",
    });
    expect(path).toBe("/repo/pond-desktop/resources/pond-server");
  });

  it("uses the .exe name on Windows", () => {
    const path = resolveServerBinary({
      ...base,
      platform: "win32",
      exists: (p) => p.endsWith("pond-server.exe"),
    });
    expect(path).toContain("pond-server.exe");
  });

  it("returns null when nothing is anywhere", () => {
    expect(resolveServerBinary({ ...base, exists: () => false })).toBe(null);
  });
});

describe("recoveryBackoffSeconds", () => {
  it("starts at five seconds and doubles", () => {
    expect(recoveryBackoffSeconds(1)).toBe(5);
    expect(recoveryBackoffSeconds(2)).toBe(10);
    expect(recoveryBackoffSeconds(3)).toBe(20);
  });

  it("caps at five minutes so a dead server is not hammered forever", () => {
    expect(recoveryBackoffSeconds(7)).toBe(300);
    expect(recoveryBackoffSeconds(50)).toBe(300);
  });

  it("is zero when nothing has failed", () => {
    expect(recoveryBackoffSeconds(0)).toBe(0);
    expect(recoveryBackoffSeconds(-1)).toBe(0);
  });
});

function deps(over: Partial<ServerDeps> = {}): ServerDeps {
  return {
    lookup: {
      isPackaged: false,
      resourcesPath: "/res",
      repoRoot: "/repo",
      platform: "darwin",
      exists: () => true,
    },
    env: {},
    fetchFn: vi.fn().mockRejectedValue(new Error("refused")) as unknown as typeof fetch,
    spawnFn: vi.fn().mockReturnValue({ exitCode: null, kill: vi.fn() }) as never,
    sleep: () => Promise.resolve(),
    ...over,
  };
}

/** A fetch that refuses `failures` times, then reports healthy. */
function fetchHealthyAfter(failures: number) {
  let n = 0;
  return vi.fn(async () => {
    if (n++ < failures) throw new Error("refused");
    return { ok: true } as Response;
  }) as unknown as typeof fetch;
}

describe("ServerProcess", () => {
  it("attaches to an already-healthy server without spawning", async () => {
    const d = deps({ fetchFn: vi.fn().mockResolvedValue({ ok: true }) as never });
    const s = new ServerProcess(d);
    expect(await s.ensureRunning()).toBe("http://127.0.0.1:4000");
    expect(d.spawnFn).not.toHaveBeenCalled();
  });

  it("spawns with the serve argv when nothing is listening", async () => {
    const d = deps({ fetchFn: fetchHealthyAfter(1) });
    await new ServerProcess(d).ensureRunning();
    expect(d.spawnFn).toHaveBeenCalledWith(
      "/repo/target/release/pond-server",
      ["serve", "--port", "4000"],
      expect.objectContaining({ cwd: "/repo" }),
    );
  });

  // The failure this prevents is two pond-servers fighting for port 4000, which
  // presents as a blank window rather than as an error.
  it("never spawns in parent-managed mode", async () => {
    const d = deps({
      env: { GIAP_SERVER_PORT: "8080" },
      fetchFn: fetchHealthyAfter(3),
    });
    const s = new ServerProcess(d);
    expect(s.parentManaged).toBe(true);
    expect(await s.ensureRunning()).toBe("http://127.0.0.1:8080");
    expect(d.spawnFn).not.toHaveBeenCalled();
  });

  // Two distinct budgets, easy to collapse into one by accident. A cold
  // parent loading face recognition, Whisper and TTS routinely needs more than
  // the 10 seconds a self-spawned server gets.
  it("waits far longer for a parent-managed server than for its own", async () => {
    expect(PARENT_MANAGED_POLL_ATTEMPTS).toBe(240);
    expect(SPAWNED_POLL_ATTEMPTS).toBe(20);

    const slow = deps({
      env: { GIAP_SERVER_PORT: "8080" },
      fetchFn: fetchHealthyAfter(100),
    });
    // 100 refusals is far past the self-spawned budget and well inside the
    // parent-managed one.
    await expect(new ServerProcess(slow).ensureRunning()).resolves.toBe("http://127.0.0.1:8080");

    const own = deps({ fetchFn: fetchHealthyAfter(100) });
    await expect(new ServerProcess(own).ensureRunning()).rejects.toThrow(/within 10 s/);
  });

  it("reports a missing binary rather than spawning nothing quietly", async () => {
    const d = deps({ lookup: { ...deps().lookup, exists: () => false } });
    await expect(new ServerProcess(d).ensureRunning()).rejects.toThrow(/no binary found/);
  });

  // The startup probe and the periodic health check both call this. Without
  // serialisation they race into two children fighting for one port.
  it("serialises concurrent recovery into a single spawn", async () => {
    const d = deps({ fetchFn: fetchHealthyAfter(1) });
    const s = new ServerProcess(d);
    await Promise.all([s.ensureRunning(), s.ensureRunning(), s.ensureRunning()]);
    expect(d.spawnFn).toHaveBeenCalledTimes(1);
  });

  it("keeps serving later calls after one fails", async () => {
    const d = deps({ lookup: { ...deps().lookup, exists: () => false } });
    const s = new ServerProcess(d);
    await expect(s.ensureRunning()).rejects.toThrow();
    await expect(s.ensureRunning()).rejects.toThrow();
  });

  it("kills only a server it spawned itself", async () => {
    const kill = vi.fn();
    const d = deps({
      fetchFn: fetchHealthyAfter(1),
      spawnFn: vi.fn().mockReturnValue({ exitCode: null, kill }) as never,
    });
    const s = new ServerProcess(d);
    await s.ensureRunning();
    s.shutdown();
    expect(kill).toHaveBeenCalled();

    const parent = new ServerProcess(
      deps({ env: { GIAP_SERVER_PORT: "8080" }, fetchFn: vi.fn().mockResolvedValue({ ok: true }) as never }),
    );
    await parent.ensureRunning();
    expect(() => parent.shutdown()).not.toThrow();
  });

  it("treats a non-ok health response as unhealthy", async () => {
    const d = deps({ fetchFn: vi.fn().mockResolvedValue({ ok: false }) as never });
    expect(await new ServerProcess(d).healthCheck()).toBe(false);
  });
});
