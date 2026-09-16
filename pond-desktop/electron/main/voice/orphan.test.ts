import { describe, it, expect, vi } from "vitest";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync, rmSync, existsSync } from "node:fs";
import {
  cmdlineIsVoiceChild,
  readPidfile,
  pidIsVoiceChild,
  reapPidfileOrphan,
  pidfilePath,
  writePidfile,
  removePidfile,
  realOrphanDeps,
  type OrphanDeps,
} from "./orphan";

function deps(over: Partial<OrphanDeps> = {}): OrphanDeps {
  return {
    isAlive: vi.fn().mockReturnValue(false),
    commandLine: vi.fn().mockReturnValue(null),
    kill: vi.fn(),
    readFile: vi.fn().mockReturnValue(null),
    removeFile: vi.fn(),
    warn: vi.fn(),
    ...over,
  };
}

describe("cmdlineIsVoiceChild", () => {
  it("matches a voice child", () => {
    // The exact production invocation.
    expect(
      cmdlineIsVoiceChild("/opt/app/pond-server chat --voice --json-events --session-id abc"),
    ).toBe(true);
    // A macOS bundle sidecar path.
    expect(
      cmdlineIsVoiceChild("/Applications/Goose In A Pond.app/Contents/MacOS/pond-server chat --voice"),
    ).toBe(true);
    // The pre-rename invocation still matches, because orphan recovery has to
    // reap a child spawned by a shell that was running before an upgrade. The
    // matcher keys on the binary and the subcommand, never the flags, and that
    // is exactly what makes an upgrade survivable.
    expect(
      cmdlineIsVoiceChild("/opt/app/pond-server chat --input whisper --json-events --session-id abc"),
    ).toBe(true);
  });

  it("does not match the dashboard server or unrelated processes", () => {
    // `serve` must never be reaped as a voice child.
    expect(cmdlineIsVoiceChild("/opt/app/pond-server serve --port 4000")).toBe(false);
    // `chat` as a substring of another token is not the subcommand.
    expect(cmdlineIsVoiceChild("/usr/bin/pond-server-chatterbox serve")).toBe(false);
    expect(cmdlineIsVoiceChild("/usr/bin/node /some/other/app.js")).toBe(false);
    expect(cmdlineIsVoiceChild("")).toBe(false);
    // `chat` with no pond-server token must not match a reused pid running
    // something else that happens to take a `chat` argument.
    expect(cmdlineIsVoiceChild("/usr/bin/irc-client chat")).toBe(false);
  });
});

describe("readPidfile", () => {
  it("parses a valid pid, including surrounding whitespace", () => {
    expect(readPidfile("  12345\n")).toBe(12345);
  });

  it("rejects junk rather than producing a wild kill target", () => {
    expect(readPidfile("not-a-pid")).toBe(null);
    expect(readPidfile("")).toBe(null);
    expect(readPidfile("   ")).toBe(null);
    expect(readPidfile(null)).toBe(null);
    expect(readPidfile("12.5")).toBe(null);
    expect(readPidfile("1e5")).toBe(null);
  });

  // This is the one that matters most. POSIX kill(0, sig) signals the entire
  // process group and kill(-n, sig) signals group n, so a truncated or
  // zero-filled pidfile would have the shell SIGKILL itself and every process
  // it spawned. The Rust this replaces parsed into u32 and accepted "0".
  it("rejects zero and negative pids", () => {
    expect(readPidfile("0")).toBe(null);
    expect(readPidfile("  0  ")).toBe(null);
    expect(readPidfile("-1")).toBe(null);
    expect(readPidfile("-4242")).toBe(null);
  });
});

describe("pidIsVoiceChild", () => {
  it("confirms a live pond-server chat process", () => {
    const d = deps({
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue("/opt/app/pond-server chat --voice\n"),
    });
    expect(pidIsVoiceChild(4242, d)).toBe(true);
  });

  it("reports a dead pid without spawning ps at all", () => {
    const d = deps({ isAlive: vi.fn().mockReturnValue(false) });
    expect(pidIsVoiceChild(4242, d)).toBe(false);
    // The common case is a stale pidfile naming a long-dead pid. Paying for a
    // subprocess there is exactly what fails on a memory-pressured board.
    expect(d.commandLine).not.toHaveBeenCalled();
  });

  it("reports a live but unrelated process as not ours", () => {
    const d = deps({
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue("/usr/bin/node server.js"),
    });
    expect(pidIsVoiceChild(4242, d)).toBe(false);
  });

  it("stays indeterminate when the command line cannot be read", () => {
    const d = deps({
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue(null),
    });
    expect(pidIsVoiceChild(4242, d)).toBe(null);
  });

  it("stays indeterminate when the liveness check itself fails", () => {
    const d = deps({
      isAlive: vi.fn().mockImplementation(() => {
        throw new Error("EMFILE");
      }),
    });
    expect(pidIsVoiceChild(4242, d)).toBe(null);
  });

  it("never confirms a non-positive pid", () => {
    expect(pidIsVoiceChild(0, deps())).toBe(false);
    expect(pidIsVoiceChild(-1, deps())).toBe(false);
  });
});

describe("reapPidfileOrphan", () => {
  const PATH = "/tmp/giap-test.pid";

  it("does nothing when there is no pidfile", () => {
    const d = deps();
    reapPidfileOrphan(d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
    expect(d.removeFile).not.toHaveBeenCalled();
  });

  it("kills a confirmed orphan and clears the file", () => {
    const d = deps({
      readFile: vi.fn().mockReturnValue("4242"),
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue("/opt/app/pond-server chat --voice"),
    });
    reapPidfileOrphan(d, PATH);
    expect(d.kill).toHaveBeenCalledWith(4242);
    expect(d.removeFile).toHaveBeenCalledWith(PATH);
  });

  it("clears a stale record without killing anything", () => {
    const d = deps({
      readFile: vi.fn().mockReturnValue("4242"),
      isAlive: vi.fn().mockReturnValue(false),
    });
    reapPidfileOrphan(d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
    expect(d.removeFile).toHaveBeenCalledWith(PATH);
  });

  // The rule this pins: an indeterminate result must KEEP the pidfile. A real
  // orphan may still hold the microphone, and the file is the only record of
  // it -- deleting it orphans the child permanently.
  it("keeps the pidfile when liveness is indeterminate", () => {
    const d = deps({
      readFile: vi.fn().mockReturnValue("4242"),
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue(null),
    });
    reapPidfileOrphan(d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
    expect(d.removeFile).not.toHaveBeenCalled();
    expect(d.warn).toHaveBeenCalledWith(expect.stringContaining("retry recovery"));
  });

  it("never kills on a malformed pidfile", () => {
    const d = deps({ readFile: vi.fn().mockReturnValue("0") });
    reapPidfileOrphan(d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
  });
});

describe("the real pidfile primitives", () => {
  const PATH = join(tmpdir(), `giap-pidfile-test-${process.pid}.pid`);

  it("round-trips write, read and remove, and remove is idempotent", () => {
    rmSync(PATH, { force: true });
    writePidfile(4242, PATH);
    expect(readPidfile(readFileSync(PATH, "utf8"))).toBe(4242);
    removePidfile(PATH);
    expect(existsSync(PATH)).toBe(false);
    // A second remove is a no-op, never a throw.
    expect(() => removePidfile(PATH)).not.toThrow();
  });

  it("scopes the path per user, with a filesystem-safe name", () => {
    const name = pidfilePath().split("/").pop() ?? "";
    expect(name).toMatch(/^giap-voice-child-[A-Za-z0-9_]+\.pid$/);
  });

  it("reports this very process as alive, and pid 1 as not ours", () => {
    expect(realOrphanDeps.isAlive(process.pid)).toBe(true);
    // pid 1 exists on every POSIX host but is launchd/init, never our child.
    expect(pidIsVoiceChild(1, realOrphanDeps)).toBe(false);
  });
});
