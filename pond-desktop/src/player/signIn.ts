// Starting a service's sign-in from outside the window. The shell calls this with a user gesture,
// because Apple's sign-in is a popup and Chromium refuses a popup nobody clicked for.

import type { PlayerAdapter, PlayerState } from "./types";

/**
 * Whether this window should be raised for the person. A sleeping service offers a sign-in in the
 * settings row, but has not asked for anything yet, so it does not pull a window up at every launch.
 */
export function needsPerson(state: PlayerState): boolean {
  return state.need === "authorization" && state.dormant !== true;
}

export interface SignInStart {
  started: boolean;
  /** Why not, in words for a person. */
  message?: string;
}

/**
 * Begins the sign-in of `service`. A service that sleeps until wanted is woken first, and that is
 * awaited: it is a token and a script, a second or two, and if it fails the person is told now, not
 * left watching a spinner. The popup itself is not awaited: the person finishes it, which can take
 * minutes, and the caller watches the adapter's state for the outcome. Only a service that is
 * waiting for a sign-in starts; one that needs setting up says what is missing, and one that is
 * signed in says so instead of asking again.
 */
export async function startSignIn(adapters: PlayerAdapter[], service: string): Promise<SignInStart> {
  const adapter = adapters.find((a) => a.service === service);
  if (!adapter) return { started: false, message: `There is no player for ${service}.` };

  const state = adapter.state();
  if (state.need === "setup") {
    return { started: false, message: state.message ?? `${adapter.label} is not set up.` };
  }
  if (state.need === "none") {
    return { started: false, message: `${adapter.label} is already signed in.` };
  }

  try {
    await adapter.prepare?.();
  } catch (error) {
    return {
      started: false,
      message: error instanceof Error ? error.message : `${adapter.label} could not start.`,
    };
  }
  // The adapter puts a failure in its own state, which the caller reads; nothing to add here.
  void adapter.authorize().catch(() => undefined);
  return { started: true };
}
