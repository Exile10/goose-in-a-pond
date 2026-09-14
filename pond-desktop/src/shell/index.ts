// The renderer's view of the desktop shell.
//
// Everything the UI needs from the native side goes through here, so there is
// exactly one place that knows the bridge exists. Under Tauri this was six
// copies of `"__TAURI_INTERNALS__" in window` plus direct imports of
// `@tauri-apps/api/core` and `/event` in five files.
//
// Outside the shell -- the browser dev surface, Vitest, Playwright -- there is
// no bridge. `isDesktopShell()` is the one check for that, `listen()` is a
// no-op so browser callers need no guard, and `invoke()` rejects rather than
// throwing synchronously so a missing bridge surfaces as a failed promise at
// the call site rather than a render-time crash.

import type {
  ShellCommand,
  ShellCommands,
  ShellEvent,
  ShellEvents,
} from "./contract";

/**
 * Arguments for a command, as a tuple, so commands taking `void` are called
 * with no second argument at all rather than an explicit `undefined`.
 */
type ArgsFor<C extends ShellCommand> = ShellCommands[C]["args"] extends void
  ? []
  : [ShellCommands[C]["args"]];

/** The object the preload publishes on `window.giap`. */
export interface DesktopShellApi {
  /**
   * The pond-server base URL the shell settled on. The preload also mirrors
   * this to `window.__GIAP_SERVER_URL__`, which PondApiClient reads directly.
   */
  readonly serverUrl: string;

  invoke<C extends ShellCommand>(
    command: C,
    ...args: ArgsFor<C>
  ): Promise<ShellCommands[C]["result"]>;

  /**
   * Subscribe to a shell event. Synchronous, and returns the unsubscribe
   * function directly -- see the note on `listen()` below.
   */
  listen<E extends ShellEvent>(
    event: E,
    handler: (payload: ShellEvents[E]) => void,
  ): () => void;
}

declare global {
  interface Window {
    giap?: DesktopShellApi;
  }
}

/**
 * Are we running inside the desktop shell?
 *
 * This is a property check on the bridge itself, not a string sniff for a
 * framework global, so it says what the caller actually wants to know: is
 * there a native side to talk to.
 */
export function isDesktopShell(): boolean {
  return typeof window !== "undefined" && window.giap !== undefined;
}

/**
 * Call a shell command. Rejects when there is no bridge -- callers that can
 * legitimately run in a browser should guard with `isDesktopShell()` first.
 */
export function invoke<C extends ShellCommand>(
  command: C,
  ...args: ArgsFor<C>
): Promise<ShellCommands[C]["result"]> {
  const shell = window.giap;
  if (!shell) {
    return Promise.reject(
      new Error(`shell command "${command}" called outside the desktop shell`),
    );
  }
  return shell.invoke(command, ...args);
}

/**
 * Subscribe to a shell event. Returns the unsubscribe function, and is a no-op
 * returning a no-op outside the shell.
 *
 * Note this is SYNCHRONOUS, where Tauri's `listen()` returned
 * `Promise<UnlistenFn>` because registration round-tripped into Rust. Two
 * classes of bug existed only because of that asynchrony -- an unlisten
 * resolving after teardown had to be caught by a cancelled-flag register, and
 * events fired between mount and registration were simply lost, which is why
 * the server-status path races a polling loop against its own listener.
 * `ipcRenderer.on` needs no round-trip, so neither bug is expressible here.
 */
export function listen<E extends ShellEvent>(
  event: E,
  handler: (payload: ShellEvents[E]) => void,
): () => void {
  return window.giap?.listen(event, handler) ?? (() => {});
}

export type {
  ShellCommand,
  ShellCommands,
  ShellEvent,
  ShellEvents,
  VoiceEndReason,
} from "./contract";
