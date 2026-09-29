// Starting a service's sign-in from outside the window. The shell calls this with a user gesture,
// because Apple's sign-in is a popup and Chromium refuses a popup nobody clicked for.

import type { PlayerAdapter } from "./types";

export interface SignInStart {
  started: boolean;
  /** Why not, in words for a person. */
  message?: string;
}

/**
 * Begins the sign-in of `service`, without waiting for it: the person finishes it in a popup, which
 * can take minutes, and the caller watches the adapter's state for the outcome. Only a service that
 * is waiting for one starts; one that needs setting up says what is missing, and one that is signed
 * in says so instead of asking again.
 */
export function startSignIn(adapters: PlayerAdapter[], service: string): SignInStart {
  const adapter = adapters.find((a) => a.service === service);
  if (!adapter) return { started: false, message: `There is no player for ${service}.` };

  const state = adapter.state();
  if (state.need === "setup") {
    return { started: false, message: state.message ?? `${adapter.label} is not set up.` };
  }
  if (state.need === "none") {
    return { started: false, message: `${adapter.label} is already signed in.` };
  }
  // The adapter puts a failure in its own state, which the caller reads; nothing to add here.
  void adapter.authorize().catch(() => undefined);
  return { started: true };
}
