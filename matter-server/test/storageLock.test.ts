import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  decide,
  executableOf,
  inspectStorageLock,
  mayBeController,
  parseBootRecord,
  parseLockOwner,
  reclaimStaleStorageLock,
  recordStorageLockBoot,
  type LockFacts,
} from "../src/storageLock.js";

const STORAGE = "/data/matter-server/storage-js";
const CONTROLLER =
  "node --import tsx /data/matter-server/app/src/server.ts --port 5580 --storage-path /data/matter-server/storage-js";

/** A lock held by a live controller of ours this boot: the baseline every case varies. */
function facts(overrides: Partial<LockFacts> = {}): LockFacts {
  return {
    lockExists: true,
    owner: { pid: 2184, token: "aaaa" },
    selfPid: 4321,
    ownerAlive: true,
    ownerCommand: CONTROLLER,
    record: { pid: 2184, token: "aaaa", bootId: "boot-now" },
    bootId: "boot-now",
    storagePath: STORAGE,
    ...overrides,
  };
}

describe("the staleness decision", () => {
  it("does nothing without a lock", () => {
    expect(decide(facts({ lockExists: false }))).toEqual({ action: "none" });
  });

  it("respects a live controller of ours", () => {
    expect(decide(facts())).toEqual({ action: "respect", reason: "live_controller" });
  });

  // The 2026-10-27 Jetson failure: after a power cut pid 2184 went to an unrelated process.
  it("reclaims a lock whose pid now runs something that is not Node", () => {
    const verdict = decide(facts({ ownerCommand: "/usr/sbin/NetworkManager --no-daemon", record: undefined }));
    expect(verdict).toEqual({ action: "reclaim", reason: "pid_reused" });
  });

  it("reclaims a lock whose pid is a kernel thread or zombie with no command line", () => {
    expect(decide(facts({ ownerCommand: "", record: undefined }))).toEqual({
      action: "reclaim",
      reason: "pid_reused",
    });
  });

  it("reclaims a lock from a previous boot even when the pid is now a Node process", () => {
    const verdict = decide(facts({ ownerCommand: "node /opt/other/app.js", bootId: "boot-next" }));
    expect(verdict).toEqual({ action: "reclaim", reason: "previous_boot" });
  });

  it("ignores a boot record written for a different lock", () => {
    const otherToken = facts({ record: { pid: 2184, token: "bbbb", bootId: "boot-old" } });
    expect(decide(otherToken)).toEqual({ action: "respect", reason: "live_controller" });
    const otherPid = facts({ record: { pid: 99, token: "aaaa", bootId: "boot-old" } });
    expect(decide(otherPid)).toEqual({ action: "respect", reason: "live_controller" });
  });

  it("does not use the boot record where this boot has no readable identity", () => {
    const verdict = decide(facts({ bootId: undefined, record: { pid: 2184, token: "aaaa", bootId: "boot-old" } }));
    expect(verdict).toEqual({ action: "respect", reason: "live_controller" });
  });

  it("reclaims a lock whose owner is gone", () => {
    expect(decide(facts({ ownerAlive: false, ownerCommand: undefined }))).toEqual({
      action: "reclaim",
      reason: "owner_gone",
    });
  });

  it("reclaims a lock recorded under this process's own pid", () => {
    expect(decide(facts({ owner: { pid: 4321, token: "old" }, ownerCommand: undefined }))).toEqual({
      action: "reclaim",
      reason: "pid_is_self",
    });
  });

  it("reclaims a lock with no owner recorded", () => {
    expect(decide(facts({ owner: undefined }))).toEqual({ action: "reclaim", reason: "no_owner_recorded" });
  });

  it("respects a live owner it cannot identify", () => {
    expect(decide(facts({ ownerCommand: undefined, record: undefined }))).toEqual({
      action: "respect",
      reason: "owner_unidentified",
    });
  });

  it("respects any live Node process, since a controller may be run by hand", () => {
    expect(decide(facts({ ownerCommand: "node ./src/server.ts --storage-path ./store", record: undefined }))).toEqual({
      action: "respect",
      reason: "live_controller",
    });
  });
});

describe("recognising a possible controller", () => {
  it("knows Node under its usual names and paths", () => {
    for (const command of [
      "node a.js",
      "/usr/bin/node a.js",
      "/home/u/.nvm/versions/node/v22.13.0/bin/node a.js",
      "nodejs a.js",
      "node22 a.js",
    ]) {
      expect(mayBeController(command, STORAGE), command).toBe(true);
    }
  });

  it("does not mistake look-alikes for Node", () => {
    for (const command of ["node_exporter --web.listen-address=:9100", "nodered", "/usr/lib/systemd/systemd-journald"]) {
      expect(mayBeController(command, STORAGE), command).toBe(false);
    }
  });

  it("accepts anything naming this store, such as a Node that renamed itself", () => {
    expect(mayBeController(`giap-matter --storage-path ${STORAGE}`, STORAGE)).toBe(true);
  });

  it("logs only the executable of a foreign command line", () => {
    expect(executableOf("/usr/bin/python3 secret-tool --token abc")).toBe("python3");
    expect(executableOf("")).toBe("");
  });
});

describe("parsing", () => {
  it("reads matter.js's pid file", () => {
    expect(parseLockOwner("2184 c8698de2bb66237e\n")).toEqual({ pid: 2184, token: "c8698de2bb66237e" });
    expect(parseLockOwner("2184")).toEqual({ pid: 2184, token: undefined });
  });

  it("rejects a pid file that names no process", () => {
    for (const content of ["", "  ", "x 1", "0 t", "-5 t", "1.5 t"]) {
      expect(parseLockOwner(content), JSON.stringify(content)).toBeUndefined();
    }
  });

  it("reads a boot record and rejects a damaged one", () => {
    expect(parseBootRecord('{"pid":1,"token":"t","bootId":"b"}')).toEqual({ pid: 1, token: "t", bootId: "b" });
    expect(parseBootRecord('{"pid":"1","token":"t","bootId":"b"}')).toBeUndefined();
    expect(parseBootRecord("{")).toBeUndefined();
  });
});

describe("against a real storage directory", () => {
  const NODE = "giap-controller";
  let storage: string;
  let children: ChildProcess[];

  beforeEach(async () => {
    storage = await mkdtemp(join(tmpdir(), "giap-lock-"));
    await mkdir(join(storage, NODE));
    children = [];
  });

  afterEach(async () => {
    for (const child of children) child.kill("SIGKILL");
    await rm(storage, { recursive: true, force: true });
  });

  /** A live process to hold the recorded pid, waited on until the OS reports it. */
  async function live(command: string, args: string[]): Promise<number> {
    const child = spawn(command, args, { stdio: "ignore" });
    children.push(child);
    await new Promise<void>((resolve, reject) => {
      child.once("spawn", () => resolve());
      child.once("error", reject);
    });
    if (child.pid === undefined) throw new Error(`${command} did not start`);
    return child.pid;
  }

  async function lockAs(pid: number, token = "feedface"): Promise<void> {
    await writeFile(join(storage, NODE, "matter.lock"), "");
    await writeFile(join(storage, NODE, "matter.pid"), `${pid} ${token}`);
  }

  const locked = () => existsSync(join(storage, NODE, "matter.lock"));

  it("clears a lock whose pid was recycled to a process that is not Node", async () => {
    await lockAs(await live("sleep", ["30"]));
    await reclaimStaleStorageLock(storage, NODE);
    expect(locked()).toBe(false);
    expect(existsSync(join(storage, NODE, "matter.pid"))).toBe(false);
  });

  it("leaves a lock held by a live Node process", async () => {
    await lockAs(await live(process.execPath, ["-e", "setTimeout(() => {}, 30000)"]));
    await reclaimStaleStorageLock(storage, NODE);
    expect(locked()).toBe(true);
  });

  it("leaves the store untouched when there is no lock", async () => {
    await writeFile(join(storage, NODE, "fabrics.fabrics"), "kept");
    await reclaimStaleStorageLock(storage, NODE);
    expect(await readFile(join(storage, NODE, "fabrics.fabrics"), "utf8")).toBe("kept");
  });

  it("records the boot of a lock this process holds, and the record marks it stale on the next boot", async () => {
    await lockAs(process.pid, "mine");
    await recordStorageLockBoot(storage, NODE);

    // Both platforms a Pond runs on have a boot identity; a silent skip would hide losing it.
    const seen = await inspectStorageLock(storage, NODE);
    expect(seen.bootId).toBeDefined();
    expect(seen.record).toEqual({ pid: process.pid, token: "mine", bootId: seen.bootId });

    // The same lock seen from the next boot, with its pid handed to a live Node process.
    const nextBoot = { ...seen, selfPid: process.pid + 1, bootId: `${seen.bootId}-next`, ownerCommand: "node x.js" };
    expect(decide(nextBoot)).toEqual({ action: "reclaim", reason: "previous_boot" });
  });

  it("does not record a lock some other process holds", async () => {
    await lockAs(await live("sleep", ["30"]));
    await recordStorageLockBoot(storage, NODE);
    expect(existsSync(join(storage, `${NODE}.lock-boot.json`))).toBe(false);
  });
});
