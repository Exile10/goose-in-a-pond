// Finding, starting and watching pond-server.
//
// A port of src-tauri/src/process.rs. Three things here are load-bearing and
// each has cost someone a debugging session before:
//
//   * Parent-managed mode. `pond-server serve --native` launches this shell
//     and pins the port via GIAP_SERVER_PORT. The parent has already bound the
//     socket, so spawning our own would either fail or fight for it, and the
//     window comes up blank -- "the app launches but it shows nothing".
//
//   * Two different patience budgets. A server we spawned ourselves gets 10
//     seconds; a parent-managed one gets 120, because a cold start loading
//     face recognition, Whisper and TTS routinely takes over a minute. These
//     are easy to collapse into one constant by accident, and the symptom is
//     an error screen on a server that was about to be fine.
//
//   * Recovery is serialised. The startup probe and the periodic health check
//     both call ensureRunning, and without the lock they race into two
//     children fighting for one port.

import { join } from "node:path";
import { existsSync } from "node:fs";
import { spawn, type ChildProcess } from "node:child_process";

export const DEFAULT_PORT = 4000;
const HEALTH_TIMEOUT_MS = 2_000;
const POLL_MS = 500;

/** Attempts when we spawned the server ourselves: 10 seconds. */
export const SPAWNED_POLL_ATTEMPTS = 20;

/**
 * Attempts when a parent owns the server: 120 seconds. Deliberately far longer
 * -- see the note above.
 */
export const PARENT_MANAGED_POLL_ATTEMPTS = 240;

/**
 * Decide the server URL and whether a parent owns it.
 *
 * An empty string counts as unset, matching the Rust: an exported-but-blank
 * variable must not put us into parent-managed mode with a malformed URL.
 */
export function resolveServerUrl(port: string | undefined): {
  url: string;
  parentManaged: boolean;
} {
  if (typeof port === "string" && port.trim() !== "") {
    return { url: `http://127.0.0.1:${port.trim()}`, parentManaged: true };
  }
  return { url: `http://127.0.0.1:${DEFAULT_PORT}`, parentManaged: false };
}

export interface BinaryLookup {
  /** POND_SERVER_BIN, kept because dev and tests both rely on it. */
  override?: string | undefined;
  isPackaged: boolean;
  /** Electron's process.resourcesPath -- Contents/Resources inside a bundle. */
  resourcesPath: string;
  /** Repo root, for the dev build. */
  repoRoot: string;
  platform: NodeJS.Platform;
  exists?: (path: string) => boolean;
}

/**
 * Locate the pond-server binary.
 *
 * Three branches, two of which are KNOWN rather than probed. The Rust had to
 * guess -- it walked the running executable's siblings first specifically so a
 * packaged app could not fall back to a stray `binaries/` folder in the cwd --
 * because it had no reliable way to ask whether it was packaged. `app.isPackaged`
 * is a hard boolean, so that whole heuristic goes.
 */
export function resolveServerBinary(opts: BinaryLookup): string | null {
  const exists = opts.exists ?? existsSync;
  const name = opts.platform === "win32" ? "pond-server.exe" : "pond-server";

  // Highest priority so the dev flow keeps working even inside a packaged app.
  if (opts.override && opts.override.trim() !== "" && exists(opts.override)) {
    return opts.override;
  }

  if (opts.isPackaged) {
    const bundled = join(opts.resourcesPath, name);
    return exists(bundled) ? bundled : null;
  }

  for (const candidate of [
    join(opts.repoRoot, "target", "release", name),
    join(opts.repoRoot, "target", "debug", name),
    join(opts.repoRoot, "pond-desktop", "resources", name),
  ]) {
    if (exists(candidate)) return candidate;
  }
  return null;
}

/**
 * How long to wait before the next recovery attempt, in seconds.
 *
 * Exponential from 5 seconds, capped at 5 minutes, so a server that cannot
 * start does not get hammered forever.
 */
export function recoveryBackoffSeconds(consecutiveFailures: number): number {
  const BASE = 5;
  const MAX = 300;
  if (consecutiveFailures <= 0) return 0;
  const shift = Math.min(consecutiveFailures - 1, 6);
  return Math.min(BASE * 2 ** shift, MAX);
}

export interface ServerDeps {
  lookup: Omit<BinaryLookup, "override">;
  env?: NodeJS.ProcessEnv;
  fetchFn?: typeof fetch;
  spawnFn?: typeof spawn;
  sleep?: (ms: number) => Promise<void>;
  log?: { info(m: string): void; warn(m: string): void };
}

const noopLog = { info() {}, warn() {} };

export class ServerProcess {
  private child: ChildProcess | null = null;
  private recovery: Promise<unknown> = Promise.resolve();
  readonly url: string;
  readonly parentManaged: boolean;

  constructor(private readonly deps: ServerDeps) {
    const env = deps.env ?? process.env;
    const resolved = resolveServerUrl(env["GIAP_SERVER_PORT"]);
    this.url = resolved.url;
    this.parentManaged = resolved.parentManaged;
  }

  private get log() {
    return this.deps.log ?? noopLog;
  }

  private sleep(ms: number): Promise<void> {
    return this.deps.sleep ? this.deps.sleep(ms) : new Promise((r) => setTimeout(r, ms));
  }

  /** Is pond-server answering right now? */
  async healthCheck(url = this.url): Promise<boolean> {
    const doFetch = this.deps.fetchFn ?? fetch;
    try {
      const res = await doFetch(`${url}/api/v1/health`, {
        signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
      });
      return res.ok;
    } catch {
      return false;
    }
  }

  /**
   * Make sure pond-server is reachable, spawning it if this shell owns it.
   *
   * Serialised, so the startup probe and the periodic health check cannot race
   * into two children fighting for one port.
   */
  ensureRunning(): Promise<string> {
    const run = this.recovery.then(
      () => this.ensureRunningInner(),
      () => this.ensureRunningInner(),
    );
    this.recovery = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  private async ensureRunningInner(): Promise<string> {
    if (await this.healthCheck()) {
      this.log.info(`connected to an existing pond-server at ${this.url}`);
      return this.url;
    }

    if (this.parentManaged) {
      // The parent bound the socket; we must not spawn. Just wait, patiently.
      this.log.info(`parent-managed pond-server detected; waiting for ${this.url}`);
      for (let i = 0; i < PARENT_MANAGED_POLL_ATTEMPTS; i++) {
        await this.sleep(POLL_MS);
        if (await this.healthCheck()) {
          this.log.info(`parent pond-server is ready at ${this.url}`);
          return this.url;
        }
      }
      throw new Error(
        `Parent-managed pond-server at ${this.url} did not become healthy within 120 s`,
      );
    }

    this.reapExitedChild();

    const binary = resolveServerBinary({
      ...this.deps.lookup,
      override: (this.deps.env ?? process.env)["POND_SERVER_BIN"],
    });
    if (binary === null) {
      throw new Error(
        `No pond-server running at ${this.url} and no binary found. ` +
          "Stage one with `npm run stage:server`, or set POND_SERVER_BIN.",
      );
    }

    this.log.info(`spawning pond-server from ${binary}`);
    const spawnFn = this.deps.spawnFn ?? spawn;
    // Grounding the child's cwd at the repo root in dev is what lets the
    // GooseAdapter inside it resolve extension paths like
    // extensions/music/src/server.ts regardless of where the shell was
    // started. A no-op in a packaged app, where those paths are absolute.
    this.child = spawnFn(binary, ["serve", "--port", String(DEFAULT_PORT)], {
      stdio: "inherit",
      ...(this.deps.lookup.isPackaged ? {} : { cwd: this.deps.lookup.repoRoot }),
    });

    for (let i = 0; i < SPAWNED_POLL_ATTEMPTS; i++) {
      await this.sleep(POLL_MS);
      if (await this.healthCheck()) {
        this.log.info(`spawned pond-server is ready at ${this.url}`);
        return this.url;
      }
    }
    throw new Error("Spawned pond-server did not become healthy within 10 s");
  }

  private reapExitedChild(): void {
    if (this.child && this.child.exitCode !== null) this.child = null;
  }

  /** Kill the server we spawned. Never touches a parent-managed one. */
  shutdown(): void {
    if (!this.child) return;
    try {
      this.child.kill();
    } catch {
      // Already gone.
    }
    this.child = null;
  }
}
