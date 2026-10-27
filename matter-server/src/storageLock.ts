/**
 * Clears a matter.js storage lock that no live controller holds, before matter.js opens storage.
 *
 * matter.js (`@matter/nodejs`, `fs/lock-utils.ts`) keeps `matter.lock` and `matter.pid`
 * (`"<pid> <token>"`) in the node's storage directory, and honours the lock for as long as
 * `process.kill(pid, 0)` succeeds. A power cut leaves both files behind; on the next boot an
 * unrelated process can be handed the recorded pid, and matter.js then refuses to start for as
 * long as that process lives. Its per-process token only catches a pid recycled to itself.
 *
 * A lock is reclaimed only when its owner is provably not a controller: it belongs to a previous
 * boot, the pid is gone or is this process, or the pid is running something other than Node.
 * Anything else is respected, because two controllers on one fabric store corrupt it.
 *
 * No wall clock is compared: a board without an RTC battery boots with a wrong clock that NTP
 * later moves forward, which makes a lock written this boot look older than the boot itself.
 */

import { execFile } from "node:child_process";
import { readFile, unlink, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { promisify } from "node:util";

import { describeError, log } from "./log.js";

const LOCK_FILE = "matter.lock";
const PID_FILE = "matter.pid";

const run = promisify(execFile);

/** Who `matter.pid` says holds the lock. */
export interface LockOwner {
  pid: number;
  /** matter.js's per-process random token; absent in files written by older matter.js. */
  token: string | undefined;
}

/** What GIAP recorded once it held the lock: the owner, and the boot it was taken in. */
export interface BootRecord {
  pid: number;
  token: string;
  bootId: string;
}

/** Everything the decision needs, gathered by [`inspectStorageLock`]. */
export interface LockFacts {
  lockExists: boolean;
  owner: LockOwner | undefined;
  /** This process's pid: a recorded owner with it is a previous incarnation, not us. */
  selfPid: number;
  ownerAlive: boolean;
  /** The owner's command line, read only for a live owner other than us; undefined if unreadable. */
  ownerCommand: string | undefined;
  record: BootRecord | undefined;
  /** This boot's identity; undefined where the platform has none GIAP can read. */
  bootId: string | undefined;
  /** The `--storage-path` this controller was given. */
  storagePath: string;
}

/** Why a lock is not a live controller's. */
export type StaleReason =
  | "no_owner_recorded"
  | "previous_boot"
  | "owner_gone"
  | "pid_is_self"
  | "pid_reused";

/** Why a lock is left for its owner. */
export type HeldReason = "live_controller" | "owner_unidentified";

export type LockVerdict =
  | { action: "none" }
  | { action: "reclaim"; reason: StaleReason }
  | { action: "respect"; reason: HeldReason };

/** Parse `matter.pid`; undefined for an empty or malformed file, as matter.js treats it. */
export function parseLockOwner(content: string): LockOwner | undefined {
  const [rawPid, token] = content.trim().split(/\s+/);
  const pid = Number(rawPid);
  if (!Number.isInteger(pid) || pid <= 0) return undefined;
  return { pid, token: token === undefined || token.length === 0 ? undefined : token };
}

/** Parse the boot record; undefined unless every field is present and well-typed. */
export function parseBootRecord(content: string): BootRecord | undefined {
  try {
    const value = JSON.parse(content) as Partial<Record<keyof BootRecord, unknown>>;
    const { pid, token, bootId } = value;
    if (typeof pid !== "number" || typeof token !== "string" || typeof bootId !== "string") {
      return undefined;
    }
    return { pid, token, bootId };
  } catch {
    return undefined;
  }
}

/** The executable's file name, the only part of a foreign command line that is safe to log. */
export function executableOf(command: string): string {
  const first = command.trim().split(/\s+/)[0] ?? "";
  return basename(first);
}

/**
 * Could this command line be a matter.js controller? Node under any of its usual names, or
 * anything naming this store (a wrapper, or a Node that renamed itself via `process.title`).
 */
export function mayBeController(command: string, storagePath: string): boolean {
  if (/^node(js)?[\d.]*$/.test(executableOf(command))) return true;
  return storagePath.length > 0 && command.includes(storagePath);
}

/** The decision, pure so that each signal can be tested on its own. */
export function decide(facts: LockFacts): LockVerdict {
  if (!facts.lockExists) return { action: "none" };

  const { owner } = facts;
  // matter.js writes the pid just after creating the lock, so only a crash between them leaves this.
  if (owner === undefined) return { action: "reclaim", reason: "no_owner_recorded" };

  // A record only speaks for the exact lock it was written for: same pid and same token.
  const record = facts.record;
  if (
    record !== undefined &&
    facts.bootId !== undefined &&
    record.pid === owner.pid &&
    record.token === owner.token &&
    record.bootId !== facts.bootId
  ) {
    return { action: "reclaim", reason: "previous_boot" };
  }

  if (!facts.ownerAlive) return { action: "reclaim", reason: "owner_gone" };
  if (owner.pid === facts.selfPid) return { action: "reclaim", reason: "pid_is_self" };

  if (facts.ownerCommand === undefined) return { action: "respect", reason: "owner_unidentified" };
  if (mayBeController(facts.ownerCommand, facts.storagePath)) {
    return { action: "respect", reason: "live_controller" };
  }
  return { action: "reclaim", reason: "pid_reused" };
}

/** `kill(pid, 0)`: only ESRCH means gone. EPERM is a live process someone else owns. */
function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code !== "ESRCH";
  }
}

/** The command line of `pid`; `""` for a process without one (a kernel thread, a zombie). */
async function commandOf(pid: number): Promise<string | undefined> {
  try {
    if (process.platform === "linux") {
      const raw = await readFile(`/proc/${pid}/cmdline`, "utf8");
      return raw.split("\0").filter(part => part.length > 0).join(" ");
    }
    const { stdout } = await run("ps", ["-p", String(pid), "-o", "command="]);
    return stdout.trim();
  } catch (error) {
    log.debug("storage_lock_owner_unread", "could not read the lock owner's command line", {
      pid,
      error: describeError(error),
    });
    return undefined;
  }
}

/** This boot's identity: Linux's `boot_id`, macOS's boot session UUID. */
async function currentBootId(): Promise<string | undefined> {
  try {
    if (process.platform === "linux") {
      const id = (await readFile("/proc/sys/kernel/random/boot_id", "utf8")).trim();
      return id.length > 0 ? id : undefined;
    }
    if (process.platform === "darwin") {
      const { stdout } = await run("sysctl", ["-n", "kern.bootsessionuuid"]);
      const id = stdout.trim();
      return id.length > 0 ? id : undefined;
    }
  } catch (error) {
    log.debug("boot_id_unread", "could not read this boot's identity", {
      error: describeError(error),
    });
  }
  return undefined;
}

async function readOptional(path: string): Promise<string | undefined> {
  try {
    return await readFile(path, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
}

/** Beside the node directory, not in it: matter.js owns every file inside. */
function bootRecordPath(storagePath: string, nodeId: string): string {
  return join(storagePath, `${nodeId}.lock-boot.json`);
}

/** Gather [`LockFacts`] for the node directory `<storagePath>/<nodeId>`. */
export async function inspectStorageLock(storagePath: string, nodeId: string): Promise<LockFacts> {
  const dir = join(storagePath, nodeId);
  const lockExists = (await readOptional(join(dir, LOCK_FILE))) !== undefined;
  const pidContent = lockExists ? await readOptional(join(dir, PID_FILE)) : undefined;
  const owner = pidContent === undefined ? undefined : parseLockOwner(pidContent);
  const ownerAlive = owner !== undefined && isAlive(owner.pid);
  const ownerCommand =
    owner !== undefined && ownerAlive && owner.pid !== process.pid
      ? await commandOf(owner.pid)
      : undefined;
  const recordContent = owner === undefined ? undefined : await readOptional(bootRecordPath(storagePath, nodeId));

  return {
    lockExists,
    owner,
    selfPid: process.pid,
    ownerAlive,
    ownerCommand,
    record: recordContent === undefined ? undefined : parseBootRecord(recordContent),
    bootId: owner === undefined ? undefined : await currentBootId(),
    storagePath,
  };
}

async function unlinkIfPresent(path: string): Promise<void> {
  try {
    await unlink(path);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
}

/**
 * Before matter.js opens storage: remove a lock no live controller holds, and say so; leave a
 * live one alone, and say that too, since matter.js is about to fail on it.
 */
export async function reclaimStaleStorageLock(storagePath: string, nodeId: string): Promise<void> {
  const facts = await inspectStorageLock(storagePath, nodeId);
  const verdict = decide(facts);
  const pid = facts.owner?.pid;

  if (verdict.action === "none") return;

  if (verdict.action === "respect") {
    log.warn(
      "matter_storage_lock_held",
      "the fabric store is locked by a running process that may be a controller; not starting a second one on it",
      { pid, reason: verdict.reason, storage: nodeId },
    );
    return;
  }

  const dir = join(storagePath, nodeId);
  const fields: Record<string, unknown> = { pid, reason: verdict.reason, storage: nodeId };
  if (verdict.reason === "pid_reused" && facts.ownerCommand !== undefined) {
    fields.owner_executable = executableOf(facts.ownerCommand) || "(none)";
  }
  try {
    // The pid file first, as matter.js does: a lock without one is still stale, never the reverse.
    await unlinkIfPresent(join(dir, PID_FILE));
    await unlinkIfPresent(join(dir, LOCK_FILE));
  } catch (error) {
    log.warn("matter_stale_lock_reclaim_failed", "could not remove a stale storage lock", {
      ...fields,
      error: describeError(error),
    });
    return;
  }
  log.warn("matter_stale_lock_reclaimed", "removed a storage lock no running controller held", fields);
}

/**
 * Once storage is open: record which boot this lock was taken in, so after a power cut the next
 * start knows it is stale whatever process has since been given the pid.
 */
export async function recordStorageLockBoot(storagePath: string, nodeId: string): Promise<void> {
  try {
    const pidContent = await readOptional(join(storagePath, nodeId, PID_FILE));
    const owner = pidContent === undefined ? undefined : parseLockOwner(pidContent);
    const bootId = await currentBootId();
    if (owner === undefined || owner.pid !== process.pid || owner.token === undefined || bootId === undefined) {
      log.debug("storage_lock_boot_unrecorded", "the storage lock's boot was not recorded", {
        has_owner: owner !== undefined,
        has_boot_id: bootId !== undefined,
      });
      return;
    }
    const record: BootRecord = { pid: owner.pid, token: owner.token, bootId };
    await writeFile(bootRecordPath(storagePath, nodeId), `${JSON.stringify(record)}\n`);
  } catch (error) {
    log.warn("matter_lock_boot_record_failed", "could not record the storage lock's boot", {
      error: describeError(error),
    });
  }
}
